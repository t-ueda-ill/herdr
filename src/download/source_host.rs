//! The server's own filesystem as a download source. A thread walks and reads;
//! the worker only waits on the queue it fills. Why not read from the worker:
//! a stalled NFS or FUSE `read`, or a path swapped for a FIFO, would stop the
//! worker too, and with it heartbeats and cancellation.

use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use crate::api::schema::PaneUploadEntryKind;
use crate::platform::open_regular_nofollow;
use crate::upload::path::{validate_names, Failure};
use crate::upload::source::{
    mtime, scan_limited_with, DuplicateNames, Scan, ScanError, ScanOptions, SourceEntry,
};
use crate::upload::worker::Control;

use super::frame::{Changed, Frame, SkipReason};
use super::source::{
    cancelled, frame_queue, Began, Closed, DownloadSource, FrameFeed, FrameSender,
};
use super::{notice_text, FRAME_QUEUE, MAX_DOWNLOAD_BYTES, MAX_DOWNLOAD_ENTRIES};

/// Bytes read from a file per frame.
const READ_BYTES: usize = 64 * 1024;

/// A test's view of every file read.
#[cfg(test)]
pub(crate) type ReadHook = Arc<dyn Fn(&Path) + Send + Sync>;

#[derive(Clone)]
pub(crate) struct HostOptions {
    pub(crate) max_entries: u64,
    pub(crate) max_bytes: u64,
    /// How long the thread may wait on the filesystem before the stream fails.
    pub(crate) stall: Duration,
    /// How often a wait for a frame looks at the cancel flag.
    pub(crate) tick: Duration,
    /// How long `begin` waits for the walk.
    pub(crate) walk: Duration,
    pub(crate) queue: usize,
    pub(crate) thread_name: String,
    /// Runs before every file read, so a test can change a file between reads
    /// or hold the thread as a stalled filesystem would.
    #[cfg(test)]
    pub(crate) before_read: Option<ReadHook>,
}

impl Default for HostOptions {
    fn default() -> Self {
        Self {
            max_entries: MAX_DOWNLOAD_ENTRIES,
            max_bytes: MAX_DOWNLOAD_BYTES,
            stall: Duration::from_secs(120),
            tick: Duration::from_millis(100),
            walk: Duration::from_secs(120),
            queue: FRAME_QUEUE,
            thread_name: "herdr-download-host".into(),
            #[cfg(test)]
            before_read: None,
        }
    }
}

enum State {
    Idle,
    Streaming {
        feed: FrameFeed,
        stop: Arc<AtomicBool>,
    },
    Stopped,
}

pub(crate) struct HostSource {
    paths: Vec<PathBuf>,
    options: HostOptions,
    state: State,
}

impl HostSource {
    pub(crate) fn new(paths: Vec<PathBuf>, options: HostOptions) -> Self {
        Self {
            paths,
            options,
            state: State::Idle,
        }
    }
}

fn failed(message: impl Into<String>) -> Failure {
    ("download_failed", message.into())
}

fn changed(message: impl Into<String>) -> Failure {
    ("download_source_changed", message.into())
}

fn shown(path: &Path) -> String {
    notice_text(&path.to_string_lossy())
}

/// What `begin` promised for one top-level path, before the walk.
fn check_top_level(path: &Path) -> Result<(), Failure> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(("not_found", format!("{} does not exist", shown(path))));
        }
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            return Err((
                "permission_denied",
                format!("{} cannot be read: {error}", shown(path)),
            ));
        }
        Err(error) => {
            return Err((
                "invalid_params",
                format!("{} cannot be read: {error}", shown(path)),
            ));
        }
    };
    let readable = if metadata.is_dir() {
        fs::read_dir(path).map(drop)
    } else if metadata.is_file() {
        fs::File::open(path).map(drop)
    } else {
        return Err((
            "invalid_params",
            format!("{} is not a file or folder", shown(path)),
        ));
    };
    readable.map_err(|error| {
        if error.kind() == std::io::ErrorKind::PermissionDenied {
            (
                "permission_denied",
                format!("{} cannot be read: {error}", shown(path)),
            )
        } else {
            failed(format!("{} cannot be read: {error}", shown(path)))
        }
    })
}

/// A scan error as a message that never mentions uploading, which the scan's
/// own refusals do.
fn scan_failure(error: ScanError, max_entries: u64) -> Failure {
    match error {
        ScanError::TooManyEntries(_) => (
            "download_too_large",
            format!("the selection holds more than {max_entries} entries"),
        ),
        ScanError::Other(message) => {
            if message.to_ascii_lowercase().contains("upload") {
                failed("the selected paths cannot be read")
            } else {
                failed(notice_text(&message))
            }
        }
    }
}

fn prepare(paths: &[PathBuf], options: &HostOptions) -> Result<Scan, Failure> {
    let mut names = Vec::with_capacity(paths.len());
    for path in paths {
        let name = path
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .ok_or_else(|| {
                (
                    "invalid_params",
                    format!("{} has no usable name", shown(path)),
                )
            })?;
        names.push(name.to_owned());
    }
    validate_names(&names)?;
    for path in paths {
        check_top_level(path)?;
    }
    let scan = scan_limited_with(
        paths,
        DuplicateNames::Reject,
        options.max_entries,
        ScanOptions {
            skip_unreadable_dirs: true,
            follow_top_level_symlinks: true,
        },
    )
    .map_err(|error| scan_failure(error, options.max_entries))?;
    // Why compare: the walk drops names it cannot carry and says only in its
    // warnings. A name missing here would never arrive, so refuse it now.
    for name in &names {
        if !scan.names.contains(name) {
            return Err((
                "invalid_params",
                format!("{} cannot be downloaded", notice_text(name)),
            ));
        }
    }
    if scan.bytes > options.max_bytes {
        return Err((
            "download_too_large",
            format!("the selection holds more than {} bytes", options.max_bytes),
        ));
    }
    Ok(scan)
}

struct Streamer<'a> {
    sender: &'a FrameSender,
    stop: &'a AtomicBool,
    options: &'a HostOptions,
}

impl Streamer<'_> {
    fn send(&self, frame: Frame) -> Result<(), Closed> {
        self.sender.send(Ok(frame))
    }

    fn fail(&self, failure: Failure) -> Result<bool, Closed> {
        self.sender.send(Err(failure))?;
        Ok(false)
    }

    fn run(&self, scan: Scan) -> Result<(), Closed> {
        if scan.skipped_special > 0 {
            self.send(Frame::WalkWarning(format!(
                "skipped {} item(s) that are not regular files, folders, or links",
                scan.skipped_special
            )))?;
        }
        for warning in scan.warnings {
            self.send(Frame::WalkWarning(warning))?;
        }
        for entry in scan.entries {
            if self.stop.load(Ordering::Acquire) {
                return Ok(());
            }
            let keep_going = match entry.header.kind {
                PaneUploadEntryKind::File => self.file(entry)?,
                PaneUploadEntryKind::Directory | PaneUploadEntryKind::Symlink => {
                    let link_target = entry
                        .header
                        .link_target
                        .as_ref()
                        .map(|target| target.clone().into_bytes());
                    self.send(Frame::Entry {
                        rel: entry.rel.into_bytes(),
                        header: entry.header,
                        link_target,
                    })?;
                    true
                }
                PaneUploadEntryKind::Unknown => true,
            };
            if !keep_going {
                return Ok(());
            }
        }
        self.send(Frame::End)
    }

    /// Streams one file. `false`: the stream failed and ends here.
    fn file(&self, entry: SourceEntry) -> Result<bool, Closed> {
        self.sender.touch();
        let rel = entry.rel.clone().into_bytes();
        let shown_path = shown(&entry.local);
        let (mut file, identity) = match open_regular_nofollow(&entry.local) {
            Ok(opened) => opened,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                self.send(Frame::Skipped {
                    rel,
                    reason: SkipReason::PermissionDenied,
                })?;
                return Ok(true);
            }
            Err(error) => {
                return self.fail(changed(format!(
                    "{shown_path} changed while it was being downloaded: {error}"
                )));
            }
        };
        let size = entry.header.size.unwrap_or(0);
        let opened = match file.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                return self.fail(changed(format!(
                    "{shown_path} cannot be inspected: {error}"
                )));
            }
        };
        let swapped = entry.identity.is_some_and(|expected| expected != identity);
        if swapped || opened.len() != size || mtime(&opened) != entry.header.mtime {
            return self.fail(changed(format!(
                "{shown_path} changed while it was being downloaded"
            )));
        }
        let opened_modified = opened.modified().ok();
        self.send(Frame::Entry {
            rel,
            header: entry.header,
            link_target: None,
        })?;

        let mut buffer = vec![0u8; READ_BYTES];
        let mut remaining = size;
        while remaining > 0 {
            let want = buffer
                .len()
                .min(usize::try_from(remaining).unwrap_or(usize::MAX));
            let read = match self.read(&mut file, &entry.local, &mut buffer[..want]) {
                Ok(read) => read,
                Err(error) => {
                    return self.end_file(Err(Changed(format!(
                        "{shown_path} could not be read: {error}"
                    ))));
                }
            };
            if read == 0 {
                return self.end_file(Err(Changed(format!(
                    "{shown_path} became shorter while it was being downloaded"
                ))));
            }
            self.sender.touch();
            remaining -= read as u64;
            self.send(Frame::Data(buffer[..read].to_vec()))?;
        }
        // Why one more read: a file that grew has bytes past the size the
        // client was promised, which a length check at the end could miss.
        match self.read(&mut file, &entry.local, &mut buffer[..1]) {
            Ok(0) => {}
            Ok(_) => {
                return self.end_file(Err(Changed(format!(
                    "{shown_path} became longer while it was being downloaded"
                ))));
            }
            Err(error) => {
                return self.end_file(Err(Changed(format!(
                    "{shown_path} could not be read: {error}"
                ))));
            }
        }
        let closing = file.metadata();
        let same = closing
            .as_ref()
            .is_ok_and(|now| now.len() == size && now.modified().ok() == opened_modified);
        if same {
            self.end_file(Ok(()))
        } else {
            self.end_file(Err(Changed(format!(
                "{shown_path} was modified while it was being downloaded"
            ))))
        }
    }

    fn read(&self, file: &mut fs::File, path: &Path, buffer: &mut [u8]) -> std::io::Result<usize> {
        #[cfg(test)]
        if let Some(hook) = &self.options.before_read {
            hook(path);
        }
        #[cfg(not(test))]
        let _ = (path, &self.options);
        loop {
            match file.read(buffer) {
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                other => return other,
            }
        }
    }

    /// Why not stop after a changed trailer: the Packer ignores the trailer
    /// of a file it is discarding, and a stream that then ended without `End`
    /// would fail a download that should have gone on. The Packer decides.
    fn end_file(&self, result: Result<(), Changed>) -> Result<bool, Closed> {
        self.send(Frame::FileEnd(result))?;
        Ok(true)
    }
}

impl DownloadSource for HostSource {
    fn begin(&mut self) -> Result<Began, Failure> {
        if !matches!(self.state, State::Idle) {
            return Err(failed("the download was already begun"));
        }
        let (sender, feed) = frame_queue(self.options.queue, self.options.stall, self.options.tick);
        let (began_tx, began_rx) = mpsc::channel::<Result<Began, Failure>>();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let paths = self.paths.clone();
        let options = self.options.clone();
        let spawned = std::thread::Builder::new()
            .name(self.options.thread_name.clone())
            .spawn(move || {
                let scan = match prepare(&paths, &options) {
                    Ok(scan) => scan,
                    Err(failure) => {
                        let _ = began_tx.send(Err(failure));
                        return;
                    }
                };
                sender.touch();
                let began = Began {
                    entries: scan.entries.len() as u64,
                    bytes: scan.bytes,
                };
                if began_tx.send(Ok(began)).is_err() {
                    return;
                }
                let streamer = Streamer {
                    sender: &sender,
                    stop: &thread_stop,
                    options: &options,
                };
                let _ = streamer.run(scan);
            });
        if let Err(error) = spawned {
            self.state = State::Stopped;
            return Err(failed(format!("cannot start the reader: {error}")));
        }
        match began_rx.recv_timeout(self.options.walk) {
            Ok(Ok(began)) => {
                self.state = State::Streaming { feed, stop };
                Ok(began)
            }
            Ok(Err(failure)) => {
                self.state = State::Stopped;
                Err(failure)
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                stop.store(true, Ordering::Release);
                self.state = State::Stopped;
                Err(failed("the folders took too long to scan"))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                self.state = State::Stopped;
                Err(failed("the reader stopped before the scan finished"))
            }
        }
    }

    fn next(&mut self, wait: Duration, control: &Control) -> Result<Option<Frame>, Failure> {
        match &mut self.state {
            State::Streaming { feed, .. } => feed.next(wait, control),
            State::Idle => Err(failed("the download was not begun")),
            State::Stopped => Err(cancelled()),
        }
    }

    fn abort(&mut self) {
        if let State::Streaming { stop, .. } = &self.state {
            stop.store(true, Ordering::Release);
        }
        // Dropping the feed makes the thread's next send fail, which ends it.
        self.state = State::Stopped;
    }
}

#[cfg(all(test, unix))]
mod tests;
