//! Uploads into a container or over ssh through one long-lived receiver
//! process per session ([`super::receiver_script`]).
//!
//! Why one process instead of one per file: `docker exec` costs 100-300 ms per
//! start and an ssh channel at least a round trip, which makes thousands of
//! small files take minutes, and sshd's `MaxSessions` caps parallelism. One
//! process also keeps publication and rollback inside one shell.

use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::api::schema::PaneUploadCollision;
use crate::container::ContainerLaunch;
use crate::noninteractive_process::{spawn_streaming, RunError, StreamingChild};
use crate::offhost::exec::{classify_failure, command_argv, ExecError};
use crate::offhost::PaneEnvironment;

use super::path::Failure;
use super::receiver_script::{self as script, Token, TokenReader, RECEIVER_SCRIPT};
use super::session::{DirectoryAttrs, EntryStart};
use super::sink::{kept_stage_failure, upload_failed, Began, Committed, UploadSink};
use super::worker::{Control, PHASE_COMMITTING, PHASE_DONE};

/// Starts the receiver somewhere. The seam lets tests run it with local shells.
pub(crate) trait ReceiverLauncher: Send + Sync {
    fn launch(&self, args: &[&str]) -> Result<StreamingChild, Failure>;
    /// Why the receiver stopped, from its exit status and stderr.
    fn classify(&self, status: Option<i32>, stderr: &str) -> Failure;
    fn timed_out(&self) -> Failure;
    /// Time to start and answer the begin queries.
    fn begin_budget(&self) -> Duration;
}

/// Q14: the message names the cause and what to do.
pub(crate) fn connection_not_shared(host: &str) -> Failure {
    (
        "ssh_unavailable",
        format!(
            "cannot upload to {host}: the pane's ssh connection is not shared \
             (ControlMaster unavailable). Reopen the ssh pane from Herdr so its \
             connection can be shared, then drop again."
        ),
    )
}

/// The receiver's argv in `env`. Why refuse a container on an ssh host
/// without a control path, unlike directory reads: a transfer must never open
/// a second login of its own (Q14).
pub(crate) fn receiver_argv(
    env: &PaneEnvironment,
    args: &[&str],
) -> Result<(&'static str, Vec<OsString>), Failure> {
    if let PaneEnvironment::Container(ContainerLaunch { ssh: Some(ssh), .. }) = env {
        if ssh.control_path.is_none() {
            return Err(connection_not_shared(&ssh.host));
        }
    }
    command_argv(env, RECEIVER_SCRIPT, args).map_err(|error| match error {
        ExecError::SshControlUnavailable { host } => connection_not_shared(&host),
        other => other.failure(),
    })
}

/// Runs the receiver in a pane's environment with the docker or ssh CLI.
pub(crate) struct EnvironmentLauncher {
    pub(crate) env: PaneEnvironment,
    pub(crate) docker: PathBuf,
    pub(crate) ssh: PathBuf,
}

impl EnvironmentLauncher {
    pub(crate) fn system(env: PaneEnvironment) -> Self {
        Self {
            env,
            docker: PathBuf::from("docker"),
            ssh: PathBuf::from("ssh"),
        }
    }

    fn over_ssh(&self) -> bool {
        !matches!(
            &self.env,
            PaneEnvironment::Container(ContainerLaunch { ssh: None, .. })
        )
    }
}

impl ReceiverLauncher for EnvironmentLauncher {
    fn launch(&self, args: &[&str]) -> Result<StreamingChild, Failure> {
        let (program, argv) = receiver_argv(&self.env, args)?;
        let over_ssh = program == "ssh";
        let path = if over_ssh { &self.ssh } else { &self.docker };
        spawn_streaming(path, &argv).map_err(|error| match error {
            RunError::NotFound if over_ssh => ExecError::SshNotFound.failure(),
            RunError::NotFound => ExecError::DockerNotFound.failure(),
            RunError::Timeout => ExecError::Timeout { over_ssh }.failure(),
            RunError::Failed { detail, .. } => ExecError::Failed { over_ssh, detail }.failure(),
        })
    }

    fn classify(&self, status: Option<i32>, stderr: &str) -> Failure {
        classify_failure(&self.env, status, stderr, self.over_ssh()).failure()
    }

    fn timed_out(&self) -> Failure {
        ExecError::Timeout {
            over_ssh: self.over_ssh(),
        }
        .failure()
    }

    fn begin_budget(&self) -> Duration {
        self.env.deadline_budget(false)
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct RemoteTimeouts {
    /// One stdin write that makes no progress this long means the receiver is stuck.
    pub(crate) stall: Duration,
    /// How long a cancelled or stuck receiver gets after EOF, and again after TERM.
    pub(crate) grace: Duration,
}

impl Default for RemoteTimeouts {
    fn default() -> Self {
        Self {
            stall: Duration::from_secs(60),
            grace: Duration::from_secs(2),
        }
    }
}

#[derive(Default)]
struct Output {
    tokens: Vec<Token>,
    closed: bool,
    malformed: Option<String>,
}

struct Shared {
    output: Mutex<Output>,
    ready: Condvar,
    /// Milliseconds since `epoch` when the current stdin write began; 0 when idle.
    writing_since: AtomicU64,
    stop_watch: AtomicBool,
}

struct Process {
    child: Arc<Mutex<Child>>,
    pid: u32,
    stdin: Option<ChildStdin>,
    stderr: Receiver<Vec<u8>>,
    shared: Arc<Shared>,
    cursor: usize,
    epoch: Instant,
}

enum Next {
    Token(Token),
    Closed,
    Timeout,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn entry_error(what: &str, entry: &str) -> Failure {
    let detail = match what {
        "short" => "a file was written short; the destination may be full",
        "eof" => "the upload ended before a file was complete",
        "mkdir" => "a directory could not be created",
        "symlink" => "a symlink could not be created",
        "create" | "write" => "a file could not be written",
        "chmod" => "permissions could not be applied",
        "attr" => "directory attributes could not be recorded",
        "noname" => "no free numbered name was left for a conflicting name",
        "replace" => "an existing entry could not be moved aside",
        "publish" => "an entry could not be moved into the destination",
        "header" => "the receiver did not understand a request",
        _ => "the receiver failed",
    };
    let at = if entry == "commit" {
        String::new()
    } else {
        format!(" (entry {entry})")
    };
    upload_failed(format!("{detail}{at}; nothing was published"))
}

impl Process {
    fn start(launcher: &dyn ReceiverLauncher, args: &[&str]) -> Result<Self, Failure> {
        let StreamingChild {
            child,
            stdin,
            mut stdout,
            stderr,
        } = launcher.launch(args)?;
        let shared = Arc::new(Shared {
            output: Mutex::new(Output::default()),
            ready: Condvar::new(),
            writing_since: AtomicU64::new(0),
            stop_watch: AtomicBool::new(false),
        });
        let pid = child.id();
        let reader_shared = shared.clone();
        let spawned = crate::thread_spawn::spawn_named("herdr-upload-out", move || {
            let mut reader = TokenReader::default();
            let mut buf = [0u8; 8192];
            loop {
                let read = match stdout.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => read,
                };
                let parsed = reader.feed(&buf[..read]);
                let mut output = lock(&reader_shared.output);
                match parsed {
                    Ok(tokens) => output.tokens.extend(tokens),
                    Err(malformed) => {
                        output.malformed = Some(malformed.0);
                        break;
                    }
                }
                reader_shared.ready.notify_all();
            }
            lock(&reader_shared.output).closed = true;
            reader_shared.ready.notify_all();
        });
        let child = Arc::new(Mutex::new(child));
        if let Err(error) = spawned {
            let _ = lock(&child).kill();
            let _ = lock(&child).wait();
            return Err(upload_failed(format!(
                "could not start the receiver: {error}"
            )));
        }
        Ok(Self {
            child,
            pid,
            stdin: Some(stdin),
            stderr,
            shared,
            cursor: 0,
            epoch: Instant::now(),
        })
    }

    fn next(&mut self, deadline: Instant) -> Next {
        let mut output = lock(&self.shared.output);
        loop {
            if let Some(token) = output.tokens.get(self.cursor) {
                self.cursor += 1;
                return Next::Token(token.clone());
            }
            if output.closed || output.malformed.is_some() {
                return Next::Closed;
            }
            let now = Instant::now();
            if now >= deadline {
                return Next::Timeout;
            }
            output = self
                .shared
                .ready
                .wait_timeout(output, deadline - now)
                .map(|(guard, _)| guard)
                .unwrap_or_else(|poisoned| poisoned.into_inner().0);
        }
    }

    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| io::Error::from(io::ErrorKind::BrokenPipe))?;
        let started = self.epoch.elapsed().as_millis().max(1) as u64;
        self.shared.writing_since.store(started, Ordering::Release);
        let result = stdin.write_all(bytes);
        self.shared.writing_since.store(0, Ordering::Release);
        result
    }

    fn wait_exit(&self, within: Duration) -> Option<std::process::ExitStatus> {
        let deadline = Instant::now() + within;
        loop {
            if let Ok(Some(status)) = lock(&self.child).try_wait() {
                return Some(status);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn stop(&mut self, grace: Duration) {
        drop(self.stdin.take());
        if self.wait_exit(grace).is_some() {
            return;
        }
        crate::platform::signal_processes(&[self.pid], crate::platform::Signal::Terminate);
        if self.wait_exit(grace).is_some() {
            return;
        }
        let _ = lock(&self.child).kill();
        let _ = lock(&self.child).wait();
    }

    fn stderr_text(&self, within: Duration) -> String {
        self.stderr
            .recv_timeout(within)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default()
    }

    fn malformed(&self) -> Option<String> {
        lock(&self.shared.output).malformed.clone()
    }
}

/// Watches a receiver the worker may be blocked on: a cancel or a stalled
/// write gets EOF time, then TERM, then KILL. Why not a cancel in a commit:
/// the receiver ignores signals there and has to finish publishing or rolling
/// back. A stalled commit write is still stopped: `C` ends that write, so the
/// receiver cannot have started publishing while it blocks.
fn spawn_watchdog(
    process: &Process,
    control: Arc<Control>,
    timeouts: RemoteTimeouts,
) -> io::Result<()> {
    let child = process.child.clone();
    let shared = process.shared.clone();
    let pid = process.pid;
    let epoch = process.epoch;
    crate::thread_spawn::spawn_named("herdr-upload-watch", move || {
        let mut trouble_since: Option<Instant> = None;
        let mut terminated = false;
        loop {
            std::thread::sleep(Duration::from_millis(100));
            if shared.stop_watch.load(Ordering::Acquire)
                || matches!(lock(&child).try_wait(), Ok(Some(_)))
            {
                return;
            }
            let phase = control.phase();
            if phase == PHASE_DONE {
                trouble_since = None;
                continue;
            }
            let since = shared.writing_since.load(Ordering::Acquire);
            let stalled = since != 0
                && epoch.elapsed().saturating_sub(Duration::from_millis(since)) >= timeouts.stall;
            let cancelled = phase != PHASE_COMMITTING && control.is_cancelled();
            if !(cancelled || stalled) {
                trouble_since = None;
                continue;
            }
            let began = *trouble_since.get_or_insert_with(Instant::now);
            let waited = began.elapsed();
            // A stalled write never lets the worker close stdin, so skip the EOF grace.
            let term_after = if stalled {
                Duration::ZERO
            } else {
                timeouts.grace
            };
            if !terminated && waited >= term_after {
                crate::platform::signal_processes(&[pid], crate::platform::Signal::Terminate);
                terminated = true;
            } else if terminated && waited >= term_after + timeouts.grace {
                let _ = lock(&child).kill();
                return;
            }
        }
    })
    .map(|_| ())
}

pub(crate) struct RemoteSink {
    launcher: Box<dyn ReceiverLauncher>,
    /// Empty: the receiver starts in the login directory (spec Q6).
    destination: String,
    token: String,
    control: Arc<Control>,
    timeouts: RemoteTimeouts,
    process: Option<Process>,
    /// The physical destination the receiver reported.
    resolved: Option<String>,
    warnings: Vec<String>,
    names: Vec<String>,
}

impl RemoteSink {
    pub(crate) fn new(
        launcher: Box<dyn ReceiverLauncher>,
        destination: String,
        token: String,
        control: Arc<Control>,
        timeouts: RemoteTimeouts,
    ) -> Self {
        Self {
            launcher,
            destination,
            token,
            control,
            timeouts,
            process: None,
            resolved: None,
            warnings: Vec::new(),
            names: Vec::new(),
        }
    }

    fn process(&mut self) -> Result<&mut Process, Failure> {
        self.process
            .as_mut()
            .ok_or_else(|| upload_failed("the receiver is not running"))
    }

    fn note_warning(&mut self, what: &str, detail: &str) {
        self.warnings.push(match what {
            "mtime" => format!("could not set the modification time of {detail}"),
            "chmod" => format!("could not set the permissions of {detail}"),
            "leftover" => {
                format!("a stage from an interrupted upload holds replaced originals: {detail}")
            }
            _ => format!("{what}: {detail}"),
        });
    }

    /// Reads what the receiver reported so far without waiting.
    fn drain(&mut self) -> Result<(), Failure> {
        loop {
            let next = self.process()?.next(Instant::now());
            match next {
                Next::Token(Token::Ok(_) | Token::Step) => {}
                Next::Token(Token::Warning(what, detail)) => self.note_warning(&what, &detail),
                Next::Token(Token::Error(entry, what)) => {
                    self.finish_process();
                    return Err(entry_error(&what, &entry));
                }
                Next::Token(other) => {
                    self.finish_process();
                    return Err(upload_failed(format!(
                        "unexpected receiver report {other:?}"
                    )));
                }
                Next::Closed => return Err(self.closed_failure()),
                Next::Timeout => return Ok(()),
            }
        }
    }

    /// Why the receiver is gone, once its pipe broke or its output ended.
    fn closed_failure(&mut self) -> Failure {
        let grace = self.timeouts.grace;
        let Some(process) = self.process.as_mut() else {
            return upload_failed("the receiver is not running");
        };
        // A broken pipe usually means an `err` token is on its way.
        let deadline = Instant::now() + grace;
        while let Next::Token(token) = process.next(deadline) {
            if let Token::Error(entry, what) = token {
                self.finish_process();
                return entry_error(&what, &entry);
            }
            if let Token::RollbackFailed(stage) = token {
                self.finish_process();
                return kept_stage_failure(&stage);
            }
        }
        if let Some(malformed) = process.malformed() {
            self.finish_process();
            return upload_failed(format!(
                "the receiver's output was not understood: {malformed}"
            ));
        }
        let status = process.wait_exit(grace);
        let stderr = process.stderr_text(grace);
        let failure = self
            .launcher
            .classify(status.and_then(|status| status.code()), &stderr);
        self.finish_process();
        failure
    }

    fn send(&mut self, bytes: &[u8]) -> Result<(), Failure> {
        let written = self.process()?.write(bytes);
        match written {
            Ok(()) => Ok(()),
            Err(_) => Err(self.closed_failure()),
        }
    }

    fn finish_process(&mut self) {
        let grace = self.timeouts.grace;
        if let Some(mut process) = self.process.take() {
            process.shared.stop_watch.store(true, Ordering::Release);
            process.stop(grace);
        }
    }
}

impl UploadSink for RemoteSink {
    fn begin(&mut self, names: &[String]) -> Result<Began, Failure> {
        if !super::is_stage_token(&self.token) {
            return Err(upload_failed("invalid stage token"));
        }
        self.names = names.to_vec();
        let args = [self.destination.as_str(), self.token.as_str()];
        let process = Process::start(self.launcher.as_ref(), &args)?;
        if let Err(error) = spawn_watchdog(&process, self.control.clone(), self.timeouts) {
            self.process = Some(process);
            self.finish_process();
            return Err(upload_failed(format!(
                "could not watch the receiver: {error}"
            )));
        }
        self.process = Some(process);
        let mut queries = String::new();
        for name in names {
            queries.push_str(&script::query_line(name));
        }
        queries.push_str(script::QUERY_END);
        // Why ignore a broken pipe here: a receiver that refuses the
        // destination exits before reading, and its report is in the output.
        let _ = self.process()?.write(queries.as_bytes());
        let deadline = Instant::now() + self.launcher.begin_budget();
        let mut destination = None;
        let mut conflicts = Vec::new();
        loop {
            let next = self.process()?.next(deadline);
            let token = match next {
                Next::Token(token) => token,
                Next::Closed => return Err(self.closed_failure()),
                Next::Timeout => {
                    self.finish_process();
                    return Err(self.launcher.timed_out());
                }
            };
            let shown = if self.destination.is_empty() {
                "the login directory".to_owned()
            } else {
                self.destination.clone()
            };
            let failure = match token {
                Token::Ready(path) => match super::sink::check_resolved_destination(&path) {
                    Ok(()) => {
                        destination = Some(path);
                        continue;
                    }
                    Err(failure) => failure,
                },
                Token::Exists(name) => {
                    conflicts.push(name);
                    continue;
                }
                Token::Warning(what, detail) => {
                    self.note_warning(&what, &detail);
                    continue;
                }
                Token::QueryEnd => match destination {
                    Some(destination) => {
                        self.resolved = Some(destination.clone());
                        return Ok(Began {
                            destination,
                            conflicts,
                        });
                    }
                    None => upload_failed("the receiver answered before it was ready"),
                },
                Token::NotFound => ("not_found", format!("not found: {shown}")),
                Token::NotDirectory => ("not_a_directory", format!("not a directory: {shown}")),
                Token::PermissionDenied => {
                    ("permission_denied", format!("permission denied: {shown}"))
                }
                Token::StageFailed => {
                    upload_failed(format!("cannot create a staging directory in {shown}"))
                }
                other => upload_failed(format!("unexpected receiver report {other:?}")),
            };
            self.finish_process();
            return Err(failure);
        }
    }

    fn start(&mut self, entry: &EntryStart) -> Result<(), Failure> {
        self.drain()?;
        let header = match entry {
            EntryStart::Directory { path } => script::directory_line(path.as_str()),
            EntryStart::Symlink { path, target } => script::symlink_lines(path.as_str(), target),
            EntryStart::File {
                path,
                size,
                mode,
                mtime,
            } => script::file_line(path.as_str(), *size, *mode, *mtime),
        };
        self.send(header.as_bytes())
    }

    fn data(&mut self, bytes: &[u8]) -> Result<(), Failure> {
        self.send(bytes)
    }

    fn end_file(&mut self) -> Result<(), Failure> {
        self.drain()
    }

    fn commit(
        &mut self,
        collision: PaneUploadCollision,
        nested: &[DirectoryAttrs],
        top: &[DirectoryAttrs],
    ) -> Result<Committed, Failure> {
        let Some(commit) = script::commit_line(collision) else {
            return Err(("invalid_params", "unknown collision policy".into()));
        };
        self.drain()?;
        let mut lines = String::new();
        for dir in nested {
            lines.push_str(&script::nested_attrs_line(
                dir.path.as_str(),
                dir.mode,
                dir.mtime,
            ));
        }
        for dir in top {
            lines.push_str(&script::top_attrs_line(
                dir.path.as_str(),
                dir.mode,
                dir.mtime,
            ));
        }
        lines.push_str(commit);
        self.send(lines.as_bytes())?;
        // Why a budget per report rather than one for the commit: each nested
        // directory costs two forks and each name a rename, which no fixed
        // allowance per entry fits on every remote. The receiver reports
        // every step, so only silence means it is stuck.
        let budget = self.launcher.begin_budget();
        let mut deadline = Instant::now() + budget;
        let mut conflicts = Vec::new();
        let mut paths = Vec::new();
        let mut committed = false;
        loop {
            let next = self.process()?.next(deadline);
            let token = match next {
                Next::Token(token) => {
                    deadline = Instant::now() + budget;
                    token
                }
                Next::Closed if committed => break,
                Next::Closed => return Err(self.closed_failure()),
                Next::Timeout => {
                    let base = self.resolved.clone().unwrap_or_default();
                    let base = base.trim_end_matches('/');
                    let stage = format!("{base}/{}{}", super::STAGE_PREFIX, self.token);
                    // Why KILL and keep the stage: originals may sit in its `o/`.
                    if let Some(process) = self.process.take() {
                        process.shared.stop_watch.store(true, Ordering::Release);
                        let _ = lock(&process.child).kill();
                        let _ = lock(&process.child).wait();
                    }
                    return Err(upload_failed(format!(
                        "publishing did not finish in time; check {stage} for anything left behind"
                    )));
                }
            };
            match token {
                Token::Ok(_) | Token::Step => {}
                Token::Warning(what, detail) => self.note_warning(&what, &detail),
                Token::Exists(name) => conflicts.push(name),
                Token::Conflict => return Ok(Committed::Conflict(conflicts)),
                Token::Committed => committed = true,
                Token::Published(name, path) => {
                    paths.push((name, path));
                    if paths.len() == self.names.len() {
                        break;
                    }
                }
                Token::Error(entry, what) => {
                    self.finish_process();
                    return Err(entry_error(&what, &entry));
                }
                Token::RollbackFailed(stage) => {
                    self.finish_process();
                    return Err(kept_stage_failure(&stage));
                }
                other => {
                    self.finish_process();
                    return Err(upload_failed(format!(
                        "unexpected receiver report {other:?}"
                    )));
                }
            }
        }
        self.finish_process();
        Ok(Committed::Published {
            paths,
            warnings: std::mem::take(&mut self.warnings),
        })
    }

    fn abort(&mut self) {
        // EOF makes the receiver remove its stage itself; signals follow only
        // if it does not exit.
        self.finish_process();
    }
}

impl Drop for RemoteSink {
    fn drop(&mut self) {
        self.finish_process();
    }
}

#[cfg(all(test, unix))]
pub(crate) mod tests;
