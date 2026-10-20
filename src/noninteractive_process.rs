use std::{
    ffi::OsStr,
    io::{self, Read, Write as _},
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError},
    time::{Duration, Instant},
};

const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Builds a subprocess whose stdio is controlled by the caller and which never opens a Windows console.
pub(crate) fn command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    crate::platform::configure_background_command(&mut command);
    command
}

pub(crate) fn curl_command() -> Command {
    command("curl")
}

#[derive(Debug)]
pub(crate) struct ProcessOutput {
    pub(crate) status: Option<i32>,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

/// Like [`ProcessOutput`] without the lossy text conversion, for callers that
/// must see raw bytes (UTF-8 validation, NUL-separated records).
#[derive(Debug)]
pub(crate) struct RawProcessOutput {
    pub(crate) status: Option<i32>,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
}

#[derive(Debug)]
pub(crate) enum RunError {
    NotFound,
    Timeout,
    /// Spawn, wait, or input failures, and a success exit after the child
    /// left part of `stdin` unread.
    Failed {
        status: Option<i32>,
        detail: String,
    },
}

/// Runs one CLI call to completion. `stdin` is streamed from its own thread
/// so a child that never reads it cannot block past `deadline`.
pub(crate) fn run_with_deadline<S: AsRef<OsStr>>(
    program: &Path,
    args: &[S],
    stdin: Option<Vec<u8>>,
    deadline: Instant,
) -> Result<ProcessOutput, RunError> {
    run_with_deadline_raw(program, args, stdin, deadline).map(|output| ProcessOutput {
        status: output.status,
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// [`run_with_deadline`] returning stdout and stderr as bytes.
pub(crate) fn run_with_deadline_raw<S: AsRef<OsStr>>(
    program: &Path,
    args: &[S],
    stdin: Option<Vec<u8>>,
    deadline: Instant,
) -> Result<RawProcessOutput, RunError> {
    let mut child = command(program)
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| match err.kind() {
            io::ErrorKind::NotFound => RunError::NotFound,
            _ => RunError::Failed {
                status: None,
                detail: err.to_string(),
            },
        })?;

    let stdout = drain(child.stdout.take(), "herdr-cli-stdout");
    let stderr = drain(child.stderr.take(), "herdr-cli-stderr");
    let feeder = match stdin.map(|input| feed(child.stdin.take(), input)) {
        Some(Ok(feeder)) => Some(feeder),
        Some(Err(err)) => {
            // Why not let the child run: closing stdin early would hand it
            // an empty or truncated input that it treats as complete.
            let _ = child.kill();
            let _ = child.wait();
            return Err(RunError::Failed {
                status: None,
                detail: format!("could not stream input: {err}"),
            });
        }
        None => None,
    };

    let status = match wait_until(&mut child, deadline) {
        Ok(Some(status)) => status,
        Ok(None) => return Err(RunError::Timeout),
        Err(err) => {
            return Err(RunError::Failed {
                status: None,
                detail: err.to_string(),
            })
        }
    };

    // Why not kill the process group or descendants on timeout: the ssh
    // callers use `ControlMaster=auto`, so a group kill could take down a
    // ControlPersist master that an interactive pane just joined. The
    // direct child is already reaped here, so nothing leaks. A detached
    // collector thread exits by itself once the descendant closes its pipe,
    // so lingering threads are bounded by live descendants.
    let Some(stdout) = recv_until(stdout, deadline, Vec::new) else {
        return Err(RunError::Timeout);
    };
    let Some(stderr) = recv_until(stderr, deadline, Vec::new) else {
        return Err(RunError::Timeout);
    };
    let input_error = match feeder {
        Some(feeder) => {
            let Some(result) = recv_until(Some(feeder), deadline, || {
                Err(io::Error::other("input thread panicked"))
            }) else {
                return Err(RunError::Timeout);
            };
            result.err().map(|err| err.to_string())
        }
        None => None,
    };
    let output = RawProcessOutput {
        status: status.code(),
        stdout,
        stderr,
    };
    match input_error {
        // A successful exit after a short write means the child consumed a
        // truncated input, so success must not be reported.
        Some(err) if output.status == Some(0) => Err(RunError::Failed {
            status: output.status,
            detail: format!("process did not read all input: {err}"),
        }),
        _ => Ok(output),
    }
}

/// A child that keeps its stdin open for the caller to stream into.
pub(crate) struct StreamingChild {
    pub(crate) child: Child,
    pub(crate) stdin: ChildStdin,
    pub(crate) stdout: std::process::ChildStdout,
    /// The first [`STREAMING_STDERR_LIMIT`] bytes of stderr, once it closes.
    pub(crate) stderr: Receiver<Vec<u8>>,
}

/// Why a bound: a remote that writes errors in a loop must not grow the
/// server's memory; the first lines are the ones that explain the failure.
pub(crate) const STREAMING_STDERR_LIMIT: usize = 64 * 1024;

/// Starts `program` with piped stdio and returns it without waiting. Unlike
/// [`run_with_deadline`], the caller owns the deadline and the stdout reader.
pub(crate) fn spawn_streaming<S: AsRef<OsStr>>(
    program: &Path,
    args: &[S],
) -> Result<StreamingChild, RunError> {
    let mut child = command(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| match err.kind() {
            io::ErrorKind::NotFound => RunError::NotFound,
            _ => RunError::Failed {
                status: None,
                detail: err.to_string(),
            },
        })?;
    let (Some(stdin), Some(stdout), Some(stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(RunError::Failed {
            status: None,
            detail: "child stdio was not piped".into(),
        });
    };
    let (tx, stderr_rx) = mpsc::channel();
    let spawned = crate::thread_spawn::spawn_named("herdr-stream-stderr", move || {
        let mut kept = Vec::new();
        let mut buf = [0u8; 8192];
        let mut stderr = stderr;
        loop {
            match stderr.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    let room = STREAMING_STDERR_LIMIT.saturating_sub(kept.len());
                    kept.extend_from_slice(&buf[..read.min(room)]);
                }
            }
        }
        let _ = tx.send(kept);
    });
    if let Err(err) = spawned {
        let _ = child.kill();
        let _ = child.wait();
        return Err(RunError::Failed {
            status: None,
            detail: err.to_string(),
        });
    }
    Ok(StreamingChild {
        child,
        stdin,
        stdout,
        stderr: stderr_rx,
    })
}

fn feed(stdin: Option<ChildStdin>, input: Vec<u8>) -> io::Result<Receiver<io::Result<()>>> {
    let mut stdin = stdin.ok_or_else(|| io::Error::other("stdin was not piped"))?;
    let (tx, rx) = mpsc::channel();
    crate::thread_spawn::spawn_named("herdr-cli-stdin", move || {
        // Dropping `stdin` after the write sends EOF.
        let _ = tx.send(stdin.write_all(&input));
    })?;
    Ok(rx)
}

/// Polls instead of blocking in `wait` because std has no `wait_timeout`, and a
/// tokio runtime is unavailable on the synchronous callers.
fn wait_until(
    child: &mut Child,
    deadline: Instant,
) -> io::Result<Option<std::process::ExitStatus>> {
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        let now = Instant::now();
        if now >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        std::thread::sleep(POLL_INTERVAL.min(deadline - now));
    }
}

fn drain<R: Read + Send + 'static>(stream: Option<R>, name: &str) -> Option<Receiver<Vec<u8>>> {
    let mut stream = stream?;
    let (tx, rx) = mpsc::channel();
    // A failed spawn drops `tx`, which reads as an empty result below.
    crate::thread_spawn::spawn_named(name, move || {
        let mut buf = Vec::new();
        let _ = stream.read_to_end(&mut buf);
        let _ = tx.send(buf);
    })
    .ok()?;
    Some(rx)
}

/// Waits for a collector's result until `deadline`. `None` means the deadline
/// passed; a disconnected channel (panicked or unspawned thread) yields
/// `on_disconnect()`, as does an absent receiver (stream was not piped).
fn recv_until<T>(
    rx: Option<Receiver<T>>,
    deadline: Instant,
    on_disconnect: impl FnOnce() -> T,
) -> Option<T> {
    let Some(rx) = rx else {
        return Some(on_disconnect());
    };
    match rx.try_recv() {
        Ok(value) => return Some(value),
        Err(TryRecvError::Disconnected) => return Some(on_disconnect()),
        Err(TryRecvError::Empty) => {}
    }
    match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(value) => Some(value),
        Err(RecvTimeoutError::Timeout) => None,
        Err(RecvTimeoutError::Disconnected) => Some(on_disconnect()),
    }
}

/// A fake CLI executable for tests that drive `run_with_deadline` callers.
#[cfg(all(test, unix))]
pub(crate) mod fake_program {
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    pub(crate) struct FakeProgram {
        dir: PathBuf,
        program: PathBuf,
    }

    impl FakeProgram {
        /// Writes `script` as `/bin/sh` code to an executable named `program`
        /// in a directory unique to this process and `name`.
        pub(crate) fn new(program: &str, name: &str, script: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "herdr-fake-{program}-{}-{name}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join(program);
            std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            Self { dir, program: path }
        }

        pub(crate) fn program(&self) -> PathBuf {
            self.program.clone()
        }
    }

    impl Drop for FakeProgram {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn sh(
        script: &str,
        stdin: Option<Vec<u8>>,
        limit: Duration,
    ) -> (Result<ProcessOutput, RunError>, Duration) {
        let started = Instant::now();
        let result = run_with_deadline(
            Path::new("/bin/sh"),
            &["-c", script],
            stdin,
            started + limit,
        );
        (result, started.elapsed())
    }

    #[test]
    fn descendant_holding_pipes_times_out_at_the_deadline() {
        let (result, elapsed) = sh("sleep 2 & exit 0", None, Duration::from_millis(100));
        assert!(matches!(result, Err(RunError::Timeout)), "{result:?}");
        assert!(elapsed < Duration::from_secs(1), "{elapsed:?}");
    }

    #[test]
    fn direct_child_times_out_and_is_killed() {
        let (result, elapsed) = sh("sleep 5", None, Duration::from_millis(100));
        assert!(matches!(result, Err(RunError::Timeout)), "{result:?}");
        assert!(elapsed < Duration::from_secs(1), "{elapsed:?}");
    }

    #[test]
    fn collects_stdout_stderr_and_status() {
        let (result, _) = sh(
            "printf out; printf err >&2; exit 3",
            None,
            Duration::from_secs(5),
        );
        let output = result.expect("run should succeed");
        assert_eq!(output.stdout, "out");
        assert_eq!(output.stderr, "err");
        assert_eq!(output.status, Some(3));
    }

    #[test]
    fn short_stdin_write_on_success_is_reported() {
        let (result, _) = sh("exit 0", Some(vec![b'x'; 1 << 20]), Duration::from_secs(5));
        match result {
            Err(RunError::Failed {
                status: Some(0),
                detail,
            }) => {
                assert!(
                    detail.starts_with("process did not read all input"),
                    "{detail}"
                )
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn raw_run_keeps_non_utf8_and_nul_bytes_intact() {
        let output = run_with_deadline_raw(
            Path::new("/bin/sh"),
            &["-c", r"printf 'a\377\000b'; printf '\376' >&2"],
            None,
            Instant::now() + Duration::from_secs(5),
        )
        .expect("run should succeed");

        assert_eq!(output.stdout, b"a\xff\0b");
        assert_eq!(output.stderr, b"\xfe");
    }

    #[test]
    fn a_streaming_child_echoes_what_is_written_and_keeps_only_bounded_stderr() {
        let mut child = spawn_streaming(
            Path::new("/bin/sh"),
            &["-c", "cat; head -c 70000 /dev/zero | tr '\\0' e >&2"],
        )
        .expect("spawn should succeed");
        child.stdin.write_all(b"hello").unwrap();
        drop(child.stdin);
        let mut out = Vec::new();
        child.stdout.read_to_end(&mut out).unwrap();
        let status = child.child.wait().unwrap();
        let stderr = child.stderr.recv_timeout(Duration::from_secs(5)).unwrap();

        assert!(status.success());
        assert_eq!(out, b"hello");
        assert_eq!(stderr.len(), STREAMING_STDERR_LIMIT);
    }

    #[test]
    fn a_missing_streaming_program_is_not_found() {
        let result = spawn_streaming(Path::new("/nonexistent/program"), &["x"]);

        assert!(matches!(result, Err(RunError::NotFound)));
    }

    #[test]
    fn stdin_is_streamed_to_the_child() {
        let (result, _) = sh("cat", Some(b"hello".to_vec()), Duration::from_secs(5));
        assert_eq!(result.expect("run should succeed").stdout, "hello");
    }

    #[test]
    fn missing_program_is_not_found() {
        let result = run_with_deadline(
            Path::new("/nonexistent/herdr-no-such-program"),
            &["x"],
            None,
            Instant::now() + Duration::from_secs(1),
        );
        assert!(matches!(result, Err(RunError::NotFound)), "{result:?}");
    }
}
