use std::io::{self, Read};
use std::path::Path;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use super::labels::parse_inspect_output;
use super::{ContainerInfo, ContainerListError, ContainerRef};

/// Total budget for one resolution (`ps` followed by `inspect`).
pub(crate) const DOCKER_CLI_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(25);

#[derive(Debug)]
struct DockerOutput {
    status: Option<i32>,
    stdout: String,
    stderr: String,
}

pub(crate) fn list_running_containers() -> Result<Vec<ContainerInfo>, ContainerListError> {
    list_running_containers_with("docker", DOCKER_CLI_TIMEOUT)
}

pub(crate) fn inspect_reference(
    reference: &ContainerRef,
) -> Result<Option<ContainerInfo>, ContainerListError> {
    inspect_reference_with("docker", reference, DOCKER_CLI_TIMEOUT)
}

fn list_running_containers_with(
    program: impl AsRef<Path>,
    timeout: Duration,
) -> Result<Vec<ContainerInfo>, ContainerListError> {
    let deadline = Instant::now() + timeout;
    let ids = running_ids(program.as_ref(), &[], deadline)?;
    inspect_ids(program.as_ref(), &ids, deadline)
}

fn inspect_reference_with(
    program: impl AsRef<Path>,
    reference: &ContainerRef,
    timeout: Duration,
) -> Result<Option<ContainerInfo>, ContainerListError> {
    let program = program.as_ref();
    let deadline = Instant::now() + timeout;
    match reference {
        ContainerRef::Compose { project, service } => {
            let filters = [
                format!("label=com.docker.compose.project={project}"),
                format!("label=com.docker.compose.service={service}"),
            ];
            let ids = running_ids(program, &filters, deadline)?;
            let mut found = inspect_ids(program, &ids, deadline)?;
            // Compose names end in `-<container-number>`; the name is a stable
            // proxy that avoids widening ContainerInfo for a scale-out edge case.
            found.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(found.into_iter().next())
        }
        ContainerRef::Name(name) => {
            let args = ["inspect", "--type", "container", name.as_str()];
            let output = run_docker_with(program, &args, deadline)?;
            if output.status != Some(0) {
                if output.stderr.contains("No such") {
                    return Ok(None);
                }
                return Err(failed(output));
            }
            Ok(parse_inspect_output(&output.stdout)?.into_iter().next())
        }
    }
}

fn running_ids(
    program: &Path,
    filters: &[String],
    deadline: Instant,
) -> Result<Vec<String>, ContainerListError> {
    let mut args = vec!["ps", "-q", "--no-trunc", "--filter", "status=running"];
    for filter in filters {
        args.push("--filter");
        args.push(filter);
    }
    let output = run_docker_with(program, &args, deadline)?;
    if output.status != Some(0) {
        return Err(failed(output));
    }
    Ok(output
        .stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect())
}

fn inspect_ids(
    program: &Path,
    ids: &[String],
    deadline: Instant,
) -> Result<Vec<ContainerInfo>, ContainerListError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut args = vec!["inspect", "--type", "container"];
    args.extend(ids.iter().map(String::as_str));
    let output = run_docker_with(program, &args, deadline)?;
    if output.status != Some(0) {
        return Err(failed(output));
    }
    parse_inspect_output(&output.stdout)
}

fn failed(output: DockerOutput) -> ContainerListError {
    ContainerListError::Failed {
        status: output.status,
        stderr: output.stderr.trim().to_owned(),
    }
}

fn run_docker_with(
    program: &Path,
    args: &[&str],
    deadline: Instant,
) -> Result<DockerOutput, ContainerListError> {
    let mut child = crate::noninteractive_process::command(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| match err.kind() {
            io::ErrorKind::NotFound => ContainerListError::DockerNotFound,
            _ => ContainerListError::Failed {
                status: None,
                stderr: err.to_string(),
            },
        })?;

    let stdout = drain(child.stdout.take(), "herdr-docker-stdout");
    let stderr = drain(child.stderr.take(), "herdr-docker-stderr");

    let status = match wait_until(&mut child, deadline) {
        Ok(Some(status)) => status,
        Ok(None) => {
            tracing::warn!(?program, "docker timed out");
            return Err(ContainerListError::Timeout);
        }
        Err(err) => {
            return Err(ContainerListError::Failed {
                status: None,
                stderr: err.to_string(),
            })
        }
    };
    Ok(DockerOutput {
        status: status.code(),
        stdout: join_output(stdout),
        stderr: join_output(stderr),
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    struct Shim {
        dir: PathBuf,
    }

    impl Shim {
        fn new(name: &str, script: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("herdr-docker-shim-{}-{name}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("docker");
            std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            Self { dir }
        }

        fn program(&self) -> PathBuf {
            self.dir.join("docker")
        }
    }

    impl Drop for Shim {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    const INSPECT: &str = include_str!("../../tests/fixtures/docker/inspect-compose.json");

    fn ps_inspect_shim(name: &str) -> (Shim, PathBuf) {
        let fixture = std::env::temp_dir().join(format!(
            "herdr-docker-fixture-{}-{name}.json",
            std::process::id()
        ));
        std::fs::write(&fixture, INSPECT).unwrap();
        let script = format!(
            "case \"$1\" in ps) echo abc123; echo ;; inspect) cat {} ;; esac",
            fixture.display()
        );
        (Shim::new(name, &script), fixture)
    }

    #[test]
    fn run_docker_returns_stdout_on_success() {
        let shim = Shim::new("stdout", "echo hello; echo oops >&2");
        let out = run_docker_with(&shim.program(), &["ps"], Instant::now() + secs(5)).unwrap();
        assert_eq!(out.status, Some(0));
        assert_eq!(out.stdout, "hello\n");
        assert_eq!(out.stderr, "oops\n");
    }

    #[test]
    fn run_docker_times_out_and_kills_hanging_process() {
        let shim = Shim::new("hang", "exec sleep 30");
        let start = Instant::now();
        let err = run_docker_with(
            &shim.program(),
            &["ps"],
            Instant::now() + Duration::from_millis(200),
        )
        .unwrap_err();
        assert!(matches!(err, ContainerListError::Timeout));
        assert!(start.elapsed() < secs(5));
    }

    #[test]
    fn run_docker_drains_stderr_larger_than_pipe_buffer() {
        let shim = Shim::new(
            "bigerr",
            "head -c 131072 /dev/zero | tr '\\0' x >&2; echo done",
        );
        let out = run_docker_with(&shim.program(), &["ps"], Instant::now() + secs(5)).unwrap();
        assert_eq!(out.stdout, "done\n");
        assert_eq!(out.stderr.len(), 131072);
    }

    #[test]
    fn list_running_containers_reports_docker_missing() {
        let err = list_running_containers_with("/nonexistent/herdr-docker", secs(5)).unwrap_err();
        assert!(matches!(err, ContainerListError::DockerNotFound));
    }

    #[test]
    fn list_running_containers_reports_failure_on_nonzero_exit() {
        let shim = Shim::new("fail", "echo daemon down >&2; exit 1");
        let err = list_running_containers_with(shim.program(), secs(5)).unwrap_err();
        match err {
            ContainerListError::Failed { status, stderr } => {
                assert_eq!(status, Some(1));
                assert_eq!(stderr, "daemon down");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn list_running_containers_returns_empty_when_nothing_runs() {
        let shim = Shim::new("empty", "case \"$1\" in ps) ;; *) exit 9 ;; esac");
        assert!(list_running_containers_with(shim.program(), secs(5))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn list_running_containers_parses_inspect_output() {
        let (shim, fixture) = ps_inspect_shim("list");
        let infos = list_running_containers_with(shim.program(), secs(5)).unwrap();
        let _ = std::fs::remove_file(fixture);
        assert!(!infos.is_empty());
    }

    #[test]
    fn inspect_reference_resolves_compose_reference_through_ps_filters() {
        let (shim, fixture) = ps_inspect_shim("compose");
        let reference = ContainerRef::parse("shop/db").unwrap();
        let found = inspect_reference_with(shim.program(), &reference, secs(5)).unwrap();
        let _ = std::fs::remove_file(fixture);
        assert!(found.is_some());
    }

    #[test]
    fn inspect_reference_returns_none_when_name_is_unknown() {
        let shim = Shim::new(
            "nosuch",
            "echo 'Error: No such container: x' >&2; echo '[]'; exit 1",
        );
        let reference = ContainerRef::Name("x".into());
        assert!(inspect_reference_with(shim.program(), &reference, secs(5))
            .unwrap()
            .is_none());
    }

    #[test]
    fn inspect_reference_shares_one_deadline_across_ps_and_inspect() {
        let shim = Shim::new(
            "slow",
            "case \"$1\" in ps) sleep 0.4; echo abc ;; inspect) sleep 0.4; echo '[]' ;; esac",
        );
        let reference = ContainerRef::parse("shop/db").unwrap();
        let err = inspect_reference_with(shim.program(), &reference, Duration::from_millis(600))
            .unwrap_err();
        assert!(matches!(err, ContainerListError::Timeout));
    }
}
