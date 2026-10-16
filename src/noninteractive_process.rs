use std::{
    ffi::OsStr,
    io::{self, Read, Write as _},
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
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
    let input_error = feeder.and_then(|feeder| match feeder.join() {
        Ok(Ok(())) => None,
        Ok(Err(err)) => Some(err.to_string()),
        Err(_) => Some("input thread panicked".to_owned()),
    });
    let output = ProcessOutput {
        status: status.code(),
        stdout: join_output(stdout),
        stderr: join_output(stderr),
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

fn feed(
    stdin: Option<ChildStdin>,
    input: Vec<u8>,
) -> io::Result<std::thread::JoinHandle<io::Result<()>>> {
    let mut stdin = stdin.ok_or_else(|| io::Error::other("stdin was not piped"))?;
    crate::thread_spawn::spawn_named("herdr-cli-stdin", move || {
        stdin.write_all(&input)
        // Dropping `stdin` here sends EOF.
    })
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

type Reader = Option<std::thread::JoinHandle<Vec<u8>>>;

fn drain<R: Read + Send + 'static>(stream: Option<R>, name: &str) -> Reader {
    let mut stream = stream?;
    crate::thread_spawn::spawn_named(name, move || {
        let mut buf = Vec::new();
        let _ = stream.read_to_end(&mut buf);
        buf
    })
    .ok()
}

fn join_output(reader: Reader) -> String {
    reader
        .and_then(|handle| handle.join().ok())
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default()
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
