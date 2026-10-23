//! A pane's off-host environment (a container, or an ssh host) as a download
//! source. One helper process per download ([`super::sender_script`]) walks
//! the selection and streams it; a reader thread parses its output into frames
//! and hands them to the worker through the same bounded queue the host source
//! uses, so a helper that outruns the client is held back by a full pipe.

use std::io::Write as _;
use std::process::{Child, ChildStdin};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::noninteractive_process::StreamingChild;
use crate::offhost::launcher::OffhostLauncher;
use crate::upload::path::Failure;
use crate::upload::worker::Control;

use super::frame::Frame;
use super::sender_script::{Event, SenderParser, EMIT_SCRIPT};
use super::source::{cancelled, frame_queue, Began, DownloadSource, FrameFeed, FrameSender};
use super::{notice_text, FRAME_QUEUE, MAX_DOWNLOAD_BYTES, MAX_DOWNLOAD_ENTRIES};

/// Bytes read from the helper's stdout at a time.
const READ_BYTES: usize = 64 * 1024;
/// How much of what the helper wrote to stderr becomes a warning.
const STDERR_WARNING_BYTES: usize = 512;

#[derive(Debug, Clone)]
pub(crate) struct RemoteOptions {
    pub(crate) max_entries: u64,
    pub(crate) max_bytes: u64,
    /// How long the helper may go without output before the stream fails.
    pub(crate) stall: Duration,
    /// How often a wait for a frame looks at the cancel flag.
    pub(crate) tick: Duration,
    /// Added to the launcher's own budget: how long `begin` waits for the walk.
    pub(crate) walk: Duration,
    /// How long a stopping helper gets after EOF on its stdin, and again after TERM.
    pub(crate) grace: Duration,
    pub(crate) queue: usize,
    pub(crate) thread_name: String,
}

impl Default for RemoteOptions {
    fn default() -> Self {
        Self {
            max_entries: MAX_DOWNLOAD_ENTRIES,
            max_bytes: MAX_DOWNLOAD_BYTES,
            stall: Duration::from_secs(120),
            tick: Duration::from_millis(100),
            walk: Duration::from_secs(120),
            grace: Duration::from_secs(2),
            queue: FRAME_QUEUE,
            thread_name: "herdr-download-remote-out".into(),
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn failed(message: impl Into<String>) -> Failure {
    ("download_failed", message.into())
}

fn wait_exit(child: &Mutex<Child>, within: Duration) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + within;
    loop {
        if let Ok(Some(status)) = lock(child).try_wait() {
            return Some(status);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Sends TERM only while the child is known to be unreaped. Why not signal the
/// bare pid: the reader thread may reap the helper at any moment, and a pid
/// signalled after that could already belong to an unrelated process.
fn terminate(child: &Mutex<Child>, pid: u32) {
    let mut child = lock(child);
    if matches!(child.try_wait(), Ok(None)) {
        crate::platform::signal_processes(&[pid], crate::platform::Signal::Terminate);
    }
}

/// What the reader thread needs to explain why the helper is gone.
struct Ending {
    child: Arc<Mutex<Child>>,
    launcher: Arc<dyn OffhostLauncher>,
    stderr: Receiver<Vec<u8>>,
    grace: Duration,
    aborted: Arc<AtomicBool>,
}

enum Outcome {
    /// The helper exited cleanly: its stderr, if any, is a warning.
    Clean(String),
    Failure(Failure),
}

impl Ending {
    fn explain(&self, ended_properly: bool) -> Outcome {
        let status = wait_exit(&self.child, self.grace);
        let stderr = self
            .stderr
            .recv_timeout(self.grace)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default();
        let code = status.and_then(|status| status.code());
        let clean = status.is_none_or(|status| status.success());
        if ended_properly && clean {
            return Outcome::Clean(stderr);
        }
        if status.is_some_and(|status| status.success()) && stderr.trim().is_empty() {
            return Outcome::Failure(failed("the sender stopped before the stream was complete"));
        }
        Outcome::Failure(self.launcher.classify(code, &stderr))
    }
}

/// Delivers a failure to whoever is waiting: `begin` until the estimate
/// arrived, the stream after that.
struct Reporter {
    sender: FrameSender,
    began: Option<SyncSender<Result<Began, Failure>>>,
}

impl Reporter {
    fn estimate(&mut self, began: Began) -> bool {
        match self.began.take() {
            Some(tx) => tx.send(Ok(began)).is_ok(),
            None => false,
        }
    }

    fn failure(&mut self, failure: Failure) {
        match self.began.take() {
            Some(tx) => {
                let _ = tx.send(Err(failure));
            }
            None => {
                let _ = self.sender.send(Err(failure));
            }
        }
    }
}

fn read_output(mut stdout: impl std::io::Read, mut reporter: Reporter, ending: Ending) {
    let mut parser = SenderParser::default();
    let mut buffer = vec![0u8; READ_BYTES];
    let mut ended = false;
    'read: loop {
        let read = match stdout.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        reporter.sender.touch();
        let events = match parser.feed(&buffer[..read]) {
            Ok(events) => events,
            Err(failure) => {
                // Why return without waiting for the helper: its output cannot
                // be trusted any more, and dropping stdout ends it.
                reporter.failure(failure);
                return;
            }
        };
        for event in events {
            match event {
                Event::Estimate { entries, bytes } => {
                    if !reporter.estimate(Began { entries, bytes }) {
                        return;
                    }
                }
                // Why hold `End` back: it is only true once the helper also
                // exited cleanly, which is known after its output closed.
                Event::Frame(Frame::End) => {
                    ended = true;
                    break 'read;
                }
                Event::Frame(frame) => {
                    if reporter.sender.send(Ok(frame)).is_err() {
                        return;
                    }
                }
            }
        }
    }
    if ending.aborted.load(Ordering::Acquire) {
        return;
    }
    match ending.explain(ended) {
        Outcome::Clean(stderr) => {
            let text = stderr.trim();
            if !text.is_empty() {
                let mut shown = notice_text(text);
                if shown.len() > STDERR_WARNING_BYTES {
                    let mut end = STDERR_WARNING_BYTES;
                    while !shown.is_char_boundary(end) {
                        end -= 1;
                    }
                    shown.truncate(end);
                }
                if reporter
                    .sender
                    .send(Ok(Frame::WalkWarning(format!(
                        "the sender reported: {shown}"
                    ))))
                    .is_err()
                {
                    return;
                }
            }
            let _ = reporter.sender.send(Ok(Frame::End));
        }
        Outcome::Failure(failure) => reporter.failure(failure),
    }
}

enum State {
    Idle,
    Streaming {
        feed: FrameFeed,
        /// Kept open: the helper waits for `G` on it and exits when it closes.
        stdin: Option<ChildStdin>,
        go_sent: bool,
        process: Process,
    },
    Stopped,
}

/// The helper process, as far as stopping it goes.
struct Process {
    child: Arc<Mutex<Child>>,
    pid: u32,
    aborted: Arc<AtomicBool>,
}

impl Process {
    /// Why escalate on a thread: the worker calls `abort` and must not wait
    /// out two grace periods for a helper that ignores EOF and TERM.
    fn stop(&self, grace: Duration) {
        self.aborted.store(true, Ordering::Release);
        let child = Arc::clone(&self.child);
        let pid = self.pid;
        let spawned = crate::thread_spawn::spawn_named("herdr-download-stop", move || {
            if wait_exit(&child, grace).is_some() {
                return;
            }
            terminate(&child, pid);
            if wait_exit(&child, grace).is_some() {
                return;
            }
            let _ = lock(&child).kill();
            let _ = lock(&child).wait();
        });
        if spawned.is_err() {
            // Without a thread the helper still must not outlive the download.
            terminate(&self.child, self.pid);
            let _ = lock(&self.child).kill();
            let _ = lock(&self.child).wait();
        }
    }
}

pub(crate) struct RemoteSource {
    launcher: Arc<dyn OffhostLauncher>,
    /// Absolute paths in the environment, without a trailing `/`.
    paths: Vec<String>,
    options: RemoteOptions,
    state: State,
}

impl RemoteSource {
    pub(crate) fn new(
        launcher: Box<dyn OffhostLauncher>,
        paths: Vec<String>,
        options: RemoteOptions,
    ) -> Self {
        Self {
            launcher: Arc::from(launcher),
            paths,
            options,
            state: State::Idle,
        }
    }

    fn start(&self) -> Result<StreamingChild, Failure> {
        let limits = [
            self.options.max_entries.to_string(),
            self.options.max_bytes.to_string(),
        ];
        let mut args: Vec<&str> = vec![EMIT_SCRIPT, &limits[0], &limits[1]];
        args.extend(self.paths.iter().map(String::as_str));
        self.launcher.launch(&args)
    }
}

impl DownloadSource for RemoteSource {
    fn begin(&mut self) -> Result<Began, Failure> {
        if !matches!(self.state, State::Idle) {
            return Err(failed("the download was already begun"));
        }
        self.state = State::Stopped;
        let StreamingChild {
            child,
            stdin,
            stdout,
            stderr,
        } = self.start()?;
        let pid = child.id();
        let child = Arc::new(Mutex::new(child));
        let aborted = Arc::new(AtomicBool::new(false));
        let process = Process {
            child: Arc::clone(&child),
            pid,
            aborted: Arc::clone(&aborted),
        };
        let (sender, feed) = frame_queue(self.options.queue, self.options.stall, self.options.tick);
        let (began_tx, began_rx) = mpsc::sync_channel::<Result<Began, Failure>>(1);
        let ending = Ending {
            child,
            launcher: Arc::clone(&self.launcher),
            stderr,
            grace: self.options.grace,
            aborted,
        };
        let reporter = Reporter {
            sender,
            began: Some(began_tx),
        };
        let spawned = crate::thread_spawn::spawn_named(&self.options.thread_name, move || {
            read_output(stdout, reporter, ending);
        });
        if let Err(error) = spawned {
            process.stop(Duration::ZERO);
            return Err(failed(format!("cannot start the reader: {error}")));
        }
        let budget = self.launcher.begin_budget() + self.options.walk;
        match began_rx.recv_timeout(budget) {
            Ok(Ok(began)) => {
                self.state = State::Streaming {
                    feed,
                    stdin: Some(stdin),
                    go_sent: false,
                    process,
                };
                Ok(began)
            }
            Ok(Err(failure)) => {
                drop(stdin);
                process.stop(self.options.grace);
                Err(failure)
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                drop(stdin);
                process.stop(self.options.grace);
                Err(failed("the folders took too long to scan"))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                drop(stdin);
                process.stop(self.options.grace);
                Err(failed("the sender stopped before the scan finished"))
            }
        }
    }

    fn next(&mut self, wait: Duration, control: &Control) -> Result<Option<Frame>, Failure> {
        match &mut self.state {
            State::Streaming {
                feed,
                stdin,
                go_sent,
                ..
            } => {
                if !*go_sent {
                    *go_sent = true;
                    // Why here: the first read is the client saying its stage
                    // exists. A failed write only means the helper is gone,
                    // which the output's end reports with its status.
                    if let Some(stdin) = stdin.as_mut() {
                        let _ = stdin.write_all(b"G\n").and_then(|()| stdin.flush());
                    }
                    // Why restart the clock: the helper idled until now by
                    // design, and that idle time is not a stall.
                    feed.touch();
                }
                feed.next(wait, control)
            }
            State::Idle => Err(failed("the download was not begun")),
            State::Stopped => Err(cancelled()),
        }
    }

    fn abort(&mut self) {
        if let State::Streaming { stdin, process, .. } = &mut self.state {
            drop(stdin.take());
            process.stop(self.options.grace);
        }
        self.state = State::Stopped;
    }
}

impl Drop for RemoteSource {
    fn drop(&mut self) {
        self.abort();
    }
}

#[cfg(all(test, unix))]
mod tests;
