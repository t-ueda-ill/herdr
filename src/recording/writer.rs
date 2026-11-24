//! The thread that owns every recording file, and the handle each pane uses
//! to hand it bytes without waiting for the disk.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use bytes::Bytes;

use super::cast::EventCode;
use super::sink::{start_stamp, CastFile, CastIdentity};
use super::PaneRecordingStatus;

/// How stale a file read with `zless` during recording may be.
const FLUSH_INTERVAL: Duration = Duration::from_secs(1);
/// How long a server shutdown waits for the trailers to reach the disk.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub(crate) struct RecorderConfig {
    /// The session's folder under the `--log` folder.
    pub dir: PathBuf,
    pub session: String,
    pub input: bool,
    pub part_limit_bytes: u64,
    /// Bytes waiting for the writer across all panes before new bytes are
    /// dropped. Why drop instead of wait: a PTY reader that waits on a slow
    /// or full disk would freeze the pane.
    pub queue_limit_bytes: usize,
    /// The share of that one pane may hold. Why: without it a pane printing
    /// a flood fills the queue, and a quiet pane next to it, often the one
    /// worth auditing, loses its bytes too.
    pub pane_queue_limit_bytes: usize,
}

enum Command {
    Open {
        id: u64,
        pane: u32,
        at: Instant,
        cols: u16,
        rows: u16,
        shared: Arc<PaneShared>,
    },
    Data {
        id: u64,
        at: Instant,
        code: EventCode,
        bytes: Bytes,
    },
    Resize {
        id: u64,
        at: Instant,
        cols: u16,
        rows: u16,
    },
    Close {
        id: u64,
        at: Instant,
    },
    Shutdown {
        reply: mpsc::Sender<()>,
    },
}

#[derive(Default)]
struct PaneShared {
    status: Mutex<PaneRecordingStatus>,
    dropped_bytes: AtomicU64,
    queued_bytes: AtomicUsize,
    /// Mirrors `status.error.is_some()` for readers that must not lock.
    failed: AtomicBool,
    /// Runs once when recording fails, so clients redraw the sidebar mark.
    on_failure: Option<FailureNotice>,
}

/// Called from the writer thread; must only flag work for other threads.
pub(crate) type FailureNotice = Box<dyn Fn() + Send + Sync>;

impl PaneShared {
    fn update(&self, change: impl FnOnce(&mut PaneRecordingStatus)) {
        change(&mut self.status.lock().unwrap_or_else(|p| p.into_inner()));
    }

    fn fail(&self, err: &std::io::Error) {
        self.update(|status| status.error = Some(err.to_string()));
        if !self.failed.swap(true, Ordering::AcqRel) {
            if let Some(notice) = &self.on_failure {
                notice();
            }
        }
    }
}

pub(crate) struct Recorder {
    tx: mpsc::Sender<Command>,
    queued_bytes: Arc<AtomicUsize>,
    next_id: AtomicU64,
    config: RecorderConfig,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Recorder {
    pub(crate) fn spawn(config: RecorderConfig) -> std::io::Result<Self> {
        let (tx, rx) = mpsc::channel();
        let queued_bytes = Arc::new(AtomicUsize::new(0));
        let mut writer = Writer {
            config: config.clone(),
            queued_bytes: queued_bytes.clone(),
            panes: HashMap::new(),
        };
        let thread = std::thread::Builder::new()
            .name("herdr-recording".into())
            .spawn(move || writer.run(rx))?;
        Ok(Self {
            tx,
            queued_bytes,
            next_id: AtomicU64::new(1),
            config,
            thread: Mutex::new(Some(thread)),
        })
    }

    #[cfg(test)]
    pub(crate) fn pane(&self, pane: u32, cols: u16, rows: u16) -> PaneRecorder {
        self.pane_with_failure_notice(pane, cols, rows, None)
    }

    pub(crate) fn pane_with_failure_notice(
        &self,
        pane: u32,
        cols: u16,
        rows: u16,
        on_failure: Option<FailureNotice>,
    ) -> PaneRecorder {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let shared = Arc::new(PaneShared {
            on_failure,
            ..PaneShared::default()
        });
        shared.update(|status| status.input = self.config.input);
        let _ = self.tx.send(Command::Open {
            id,
            pane,
            at: Instant::now(),
            cols,
            rows,
            shared: shared.clone(),
        });
        PaneRecorder {
            inner: Arc::new(PaneRecorderInner {
                id,
                input: self.config.input,
                queue_limit_bytes: self.config.queue_limit_bytes,
                pane_queue_limit_bytes: self.config.pane_queue_limit_bytes,
                tx: self.tx.clone(),
                queued_bytes: self.queued_bytes.clone(),
                shared,
            }),
        }
    }

    /// Finishes every open file so each ends with a valid gzip trailer.
    pub(crate) fn shutdown(&self) {
        let (reply, done) = mpsc::channel();
        if self.tx.send(Command::Shutdown { reply }).is_ok()
            && done.recv_timeout(SHUTDOWN_GRACE).is_err()
        {
            tracing::warn!("pane recordings were not finished before shutdown");
            return;
        }
        let thread = self.thread.lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some(thread) = thread {
            let _ = thread.join();
        }
    }
}

/// A pane's way into the recorder. Cloned into the PTY reader; the file is
/// finished when the last clone is gone.
#[derive(Clone)]
pub(crate) struct PaneRecorder {
    inner: Arc<PaneRecorderInner>,
}

struct PaneRecorderInner {
    id: u64,
    input: bool,
    queue_limit_bytes: usize,
    pane_queue_limit_bytes: usize,
    tx: mpsc::Sender<Command>,
    queued_bytes: Arc<AtomicUsize>,
    shared: Arc<PaneShared>,
}

impl Drop for PaneRecorderInner {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Close {
            id: self.id,
            at: Instant::now(),
        });
    }
}

impl PaneRecorder {
    /// Bytes the pane's process wrote to the terminal.
    pub(crate) fn output(&self, bytes: &[u8]) {
        if self.reserve(bytes.len()) {
            self.send_reserved(EventCode::Output, Bytes::copy_from_slice(bytes));
        }
    }

    /// Bytes herdr wrote to the pane's process; kept only with `--log-input`.
    pub(crate) fn input(&self, bytes: &Bytes) {
        if self.inner.input && self.reserve(bytes.len()) {
            self.send_reserved(EventCode::Input, bytes.clone());
        }
    }

    pub(crate) fn resize(&self, cols: u16, rows: u16) {
        let _ = self.inner.tx.send(Command::Resize {
            id: self.inner.id,
            at: Instant::now(),
            cols,
            rows,
        });
    }

    /// Whether recording stopped or never started; cheap enough for every
    /// snapshot, unlike [`Self::status`].
    pub(crate) fn failed(&self) -> bool {
        self.inner.shared.failed.load(Ordering::Acquire)
    }

    pub(crate) fn status(&self) -> PaneRecordingStatus {
        let mut status = self
            .inner
            .shared
            .status
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        // Bytes dropped since the writer last caught up count as dropped too.
        status.dropped_bytes += self.inner.shared.dropped_bytes.load(Ordering::Relaxed);
        status
    }

    fn reserve(&self, len: usize) -> bool {
        if len == 0 {
            return false;
        }
        let inner = &self.inner;
        let pane = inner.shared.queued_bytes.fetch_add(len, Ordering::AcqRel);
        if pane.saturating_add(len) > inner.pane_queue_limit_bytes {
            inner.shared.queued_bytes.fetch_sub(len, Ordering::AcqRel);
            inner
                .shared
                .dropped_bytes
                .fetch_add(len as u64, Ordering::Relaxed);
            return false;
        }
        let all = inner.queued_bytes.fetch_add(len, Ordering::AcqRel);
        if all.saturating_add(len) > inner.queue_limit_bytes {
            inner.queued_bytes.fetch_sub(len, Ordering::AcqRel);
            inner.shared.queued_bytes.fetch_sub(len, Ordering::AcqRel);
            inner
                .shared
                .dropped_bytes
                .fetch_add(len as u64, Ordering::Relaxed);
            return false;
        }
        true
    }

    fn send_reserved(&self, code: EventCode, bytes: Bytes) {
        let len = bytes.len();
        let sent = self.inner.tx.send(Command::Data {
            id: self.inner.id,
            at: Instant::now(),
            code,
            bytes,
        });
        if sent.is_err() {
            self.inner.queued_bytes.fetch_sub(len, Ordering::AcqRel);
            self.inner
                .shared
                .queued_bytes
                .fetch_sub(len, Ordering::AcqRel);
        }
    }
}

struct Writer {
    config: RecorderConfig,
    queued_bytes: Arc<AtomicUsize>,
    panes: HashMap<u64, PaneEntry>,
}

struct PaneEntry {
    /// `None` after a write failed; later bytes for the pane are discarded.
    file: Option<CastFile>,
    shared: Arc<PaneShared>,
}

impl Writer {
    fn run(&mut self, rx: mpsc::Receiver<Command>) {
        let mut last_flush = Instant::now();
        loop {
            match rx.recv_timeout(FLUSH_INTERVAL) {
                Ok(Command::Shutdown { reply }) => {
                    self.finish_all();
                    let _ = reply.send(());
                    return;
                }
                Ok(command) => self.handle(command),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    self.finish_all();
                    return;
                }
            }
            if last_flush.elapsed() >= FLUSH_INTERVAL {
                self.flush_all();
                last_flush = Instant::now();
            }
        }
    }

    fn handle(&mut self, command: Command) {
        match command {
            Command::Open {
                id,
                pane,
                at,
                cols,
                rows,
                shared,
            } => self.open(id, pane, at, cols, rows, shared),
            Command::Data {
                id,
                at,
                code,
                bytes,
            } => {
                self.queued_bytes.fetch_sub(bytes.len(), Ordering::AcqRel);
                if let Some(entry) = self.panes.get(&id) {
                    entry
                        .shared
                        .queued_bytes
                        .fetch_sub(bytes.len(), Ordering::AcqRel);
                }
                self.write(id, at, |file| file.write_bytes(at, code, &bytes));
            }
            Command::Resize { id, at, cols, rows } => {
                self.write(id, at, |file| file.resize(at, cols, rows));
            }
            Command::Close { id, at } => {
                if let Some(entry) = self.panes.remove(&id) {
                    finish(entry, at);
                }
            }
            Command::Shutdown { .. } => unreachable!("handled by run"),
        }
    }

    fn open(
        &mut self,
        id: u64,
        pane: u32,
        at: Instant,
        cols: u16,
        rows: u16,
        shared: Arc<PaneShared>,
    ) {
        let identity = CastIdentity {
            dir: self.config.dir.clone(),
            stem: format!("{}-pane-{pane}", start_stamp(SystemTime::now())),
            title: format!("herdr {} pane {pane}", self.config.session),
        };
        let file = match CastFile::create(identity, self.config.part_limit_bytes, cols, rows, at) {
            Ok(file) => {
                let path = file.path().to_path_buf();
                tracing::info!(pane, path = %path.display(), "recording pane");
                shared.update(|status| status.path = Some(path));
                Some(file)
            }
            Err(err) => {
                tracing::warn!(pane, %err, dir = %self.config.dir.display(), "could not start pane recording");
                shared.fail(&err);
                None
            }
        };
        self.panes.insert(id, PaneEntry { file, shared });
    }

    fn write(
        &mut self,
        id: u64,
        at: Instant,
        op: impl FnOnce(&mut CastFile) -> std::io::Result<()>,
    ) {
        let Some(entry) = self.panes.get_mut(&id) else {
            return;
        };
        let Some(file) = entry.file.as_mut() else {
            return;
        };
        let result = (|| {
            let dropped = entry.shared.dropped_bytes.swap(0, Ordering::Relaxed);
            if dropped > 0 {
                entry
                    .shared
                    .update(|status| status.dropped_bytes += dropped);
                file.write_marker(at, &format!("herdr: {dropped} bytes not recorded"))?;
            }
            op(file)
        })();
        match result {
            Ok(()) => {
                if entry
                    .shared
                    .status
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .path
                    .as_deref()
                    != Some(file.path())
                {
                    let path = file.path().to_path_buf();
                    entry.shared.update(|status| status.path = Some(path));
                }
            }
            Err(err) => {
                let path = file.path().display().to_string();
                tracing::warn!(%err, %path, "pane recording stopped after a write failed");
                entry.shared.fail(&err);
                entry.file = None;
            }
        }
    }

    fn flush_all(&mut self) {
        for entry in self.panes.values_mut() {
            let Some(file) = entry.file.as_mut() else {
                continue;
            };
            if let Err(err) = file.flush() {
                tracing::warn!(%err, path = %file.path().display(), "pane recording stopped after a flush failed");
                entry.shared.fail(&err);
                entry.file = None;
            }
        }
    }

    fn finish_all(&mut self) {
        let at = Instant::now();
        for (_, entry) in self.panes.drain() {
            finish(entry, at);
        }
    }
}

fn finish(entry: PaneEntry, at: Instant) {
    let Some(mut file) = entry.file else {
        return;
    };
    let path = file.path().display().to_string();
    // Why here too: the dropped tail of a pane that closes right after a
    // flood has no later event to carry its marker.
    let dropped = entry.shared.dropped_bytes.swap(0, Ordering::Relaxed);
    let marked = if dropped > 0 {
        entry
            .shared
            .update(|status| status.dropped_bytes += dropped);
        file.write_marker(at, &format!("herdr: {dropped} bytes not recorded"))
    } else {
        Ok(())
    };
    if let Err(err) = marked.and_then(|()| file.finish(at).map(|_| ())) {
        tracing::warn!(%err, %path, "could not finish pane recording");
        entry.shared.fail(&err);
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::path::Path;

    use super::*;

    fn config(name: &str, input: bool) -> RecorderConfig {
        let dir = std::env::temp_dir().join(format!(
            "herdr-recording-writer-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        RecorderConfig {
            dir: dir.join("default"),
            session: "default".into(),
            input,
            part_limit_bytes: u64::MAX,
            queue_limit_bytes: 1024 * 1024,
            pane_queue_limit_bytes: 1024 * 1024,
        }
    }

    fn events(path: &Path) -> Vec<serde_json::Value> {
        let mut text = String::new();
        flate2::read::GzDecoder::new(std::fs::File::open(path).unwrap())
            .read_to_string(&mut text)
            .unwrap();
        text.lines()
            .skip(1)
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn codes_and_data(path: &Path) -> Vec<(String, String)> {
        events(path)
            .into_iter()
            .map(|event| {
                (
                    event[1].as_str().unwrap().to_owned(),
                    event[2].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }

    #[test]
    fn a_closed_pane_leaves_a_finished_file_named_after_the_pane() {
        let config = config("closed", false);
        let root = config.dir.parent().unwrap().to_path_buf();
        let recorder = Recorder::spawn(config).unwrap();
        let pane = recorder.pane(7, 80, 24);
        pane.output(b"hello");
        pane.resize(100, 30);
        let path_holder = pane.clone();
        drop(pane);
        recorder.shutdown();

        let path = path_holder.status().path.unwrap();
        drop(path_holder);
        let name = path.file_name().unwrap().to_str().unwrap().to_owned();
        assert!(name.ends_with("-pane-7.cast.gz"), "{name}");
        assert_eq!(
            codes_and_data(&path),
            vec![
                ("o".to_owned(), "hello".to_owned()),
                ("r".to_owned(), "100x30".to_owned())
            ]
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn input_is_left_out_unless_input_recording_was_asked_for() {
        for input in [false, true] {
            let config = config(&format!("input-{input}"), input);
            let root = config.dir.parent().unwrap().to_path_buf();
            let recorder = Recorder::spawn(config).unwrap();
            let pane = recorder.pane(1, 80, 24);
            pane.input(&Bytes::from_static(b"secret\r"));
            pane.output(b"$ ");
            recorder.shutdown();

            let status = pane.status();
            let codes: Vec<String> = codes_and_data(status.path.as_ref().unwrap())
                .into_iter()
                .map(|(code, _)| code)
                .collect();
            assert_eq!(status.input, input);
            if input {
                assert_eq!(codes, ["i", "o"]);
            } else {
                assert_eq!(codes, ["o"]);
            }
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn bytes_over_the_queue_limit_are_counted_and_marked_instead_of_waiting() {
        let mut config = config("dropped", false);
        config.queue_limit_bytes = 4;
        let root = config.dir.parent().unwrap().to_path_buf();
        let recorder = Recorder::spawn(config).unwrap();
        let pane = recorder.pane(1, 80, 24);
        // Larger than the whole queue, so it is dropped however fast the
        // writer runs.
        pane.output(b"too long");
        assert_eq!(pane.status().dropped_bytes, 8);
        pane.output(b"ok");
        recorder.shutdown();

        let status = pane.status();
        assert_eq!(status.dropped_bytes, 8);
        assert_eq!(
            codes_and_data(status.path.as_ref().unwrap()),
            vec![
                ("m".to_owned(), "herdr: 8 bytes not recorded".to_owned()),
                ("o".to_owned(), "ok".to_owned())
            ]
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_flooding_pane_cannot_take_the_queue_share_of_a_quiet_one() {
        let mut config = config("fair", false);
        config.queue_limit_bytes = 10;
        config.pane_queue_limit_bytes = 6;
        let root = config.dir.parent().unwrap().to_path_buf();
        let recorder = Recorder::spawn(config).unwrap();
        let noisy = recorder.pane(1, 80, 24);
        let quiet = recorder.pane(2, 80, 24);
        // Over its own share, so it is dropped however fast the writer runs.
        noisy.output(b"1234567");
        quiet.output(b"ok");
        recorder.shutdown();

        assert_eq!(noisy.status().dropped_bytes, 7);
        let quiet_status = quiet.status();
        assert_eq!(quiet_status.dropped_bytes, 0);
        assert_eq!(
            codes_and_data(quiet_status.path.as_ref().unwrap()),
            vec![("o".to_owned(), "ok".to_owned())]
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bytes_dropped_right_before_the_pane_closes_still_leave_a_marker() {
        let mut config = config("dropped-tail", false);
        config.queue_limit_bytes = 4;
        let root = config.dir.parent().unwrap().to_path_buf();
        let recorder = Recorder::spawn(config).unwrap();
        let pane = recorder.pane(1, 80, 24);
        pane.output(b"too long");
        let holder = pane.clone();
        drop(pane);
        recorder.shutdown();

        let status = holder.status();
        assert_eq!(status.dropped_bytes, 8);
        assert_eq!(
            codes_and_data(status.path.as_ref().unwrap()),
            vec![("m".to_owned(), "herdr: 8 bytes not recorded".to_owned())]
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_failing_pane_notifies_once_so_clients_can_redraw() {
        let mut config = config("notice", false);
        let root = config.dir.parent().unwrap().to_path_buf();
        std::fs::create_dir_all(&root).unwrap();
        let blocker = root.join("not-a-folder");
        std::fs::write(&blocker, b"").unwrap();
        config.dir = blocker.join("default");
        let notices = Arc::new(AtomicUsize::new(0));
        let counter = notices.clone();
        let recorder = Recorder::spawn(config).unwrap();
        let pane = recorder.pane_with_failure_notice(
            1,
            80,
            24,
            Some(Box::new(move || {
                counter.fetch_add(1, Ordering::Relaxed);
            })),
        );
        pane.output(b"lost");
        recorder.shutdown();

        assert!(pane.failed());
        assert_eq!(notices.load(Ordering::Relaxed), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_folder_that_cannot_be_created_is_reported_on_the_pane_without_panicking() {
        let mut config = config("blocked", false);
        let root = config.dir.parent().unwrap().to_path_buf();
        std::fs::create_dir_all(&root).unwrap();
        let blocker = root.join("not-a-folder");
        std::fs::write(&blocker, b"").unwrap();
        config.dir = blocker.join("default");
        let recorder = Recorder::spawn(config).unwrap();
        let pane = recorder.pane(1, 80, 24);
        pane.output(b"lost");
        recorder.shutdown();

        let status = pane.status();
        assert_eq!(status.path, None);
        assert!(status.error.is_some());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn two_runtimes_of_the_same_pane_record_to_separate_files() {
        let config = config("reconnect", false);
        let root = config.dir.parent().unwrap().to_path_buf();
        let recorder = Recorder::spawn(config).unwrap();
        let first = recorder.pane(4, 80, 24);
        let second = recorder.pane(4, 80, 24);
        first.output(b"before");
        drop(first.clone());
        second.output(b"after");
        recorder.shutdown();

        let first_path = first.status().path.unwrap();
        let second_path = second.status().path.unwrap();
        assert_ne!(first_path, second_path);
        assert_eq!(codes_and_data(&first_path)[0].1, "before");
        assert_eq!(codes_and_data(&second_path)[0].1, "after");
        std::fs::remove_dir_all(root).unwrap();
    }
}
