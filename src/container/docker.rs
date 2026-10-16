use std::path::Path;
use std::time::{Duration, Instant};

use super::labels::parse_inspect_output;
use super::{ContainerInfo, ContainerListError, ContainerRef};
use crate::noninteractive_process::{run_with_deadline, ProcessOutput, RunError};

/// Total budget for one resolution (`ps` followed by `inspect`).
pub(crate) const DOCKER_CLI_TIMEOUT: Duration = Duration::from_secs(5);

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
            let output = run_docker_with(program, &args, None, deadline)?;
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
    let output = run_docker_with(program, &args, None, deadline)?;
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
    let output = run_docker_with(program, &args, None, deadline)?;
    if output.status != Some(0) {
        return Err(failed(output));
    }
    parse_inspect_output(&output.stdout)
}

fn failed(output: ProcessOutput) -> ContainerListError {
    ContainerListError::Failed {
        status: output.status,
        stderr: output.stderr.trim().to_owned(),
    }
}

/// Runs one docker CLI call. `stdin` is streamed from its own thread so a
/// child that never reads it cannot block past `deadline`.
pub(super) fn run_docker_with(
    program: &Path,
    args: &[&str],
    stdin: Option<Vec<u8>>,
    deadline: Instant,
) -> Result<ProcessOutput, ContainerListError> {
    run_with_deadline(program, args, stdin, deadline).map_err(|err| match err {
        RunError::NotFound => ContainerListError::DockerNotFound,
        RunError::Timeout => {
            tracing::warn!(?program, "docker timed out");
            ContainerListError::Timeout
        }
        RunError::Failed { status, detail } => ContainerListError::Failed {
            status,
            stderr: detail,
        },
    })
}

#[cfg(all(test, unix))]
pub(super) mod tests {
    use super::*;
    use crate::noninteractive_process::fake_program::FakeProgram;
    use std::path::PathBuf;

    /// A fake `docker` executable shared with sibling container tests.
    pub(in crate::container) struct Shim(FakeProgram);

    impl Shim {
        pub(in crate::container) fn new(name: &str, script: &str) -> Self {
            Self(FakeProgram::new("docker", name, script))
        }

        pub(in crate::container) fn program(&self) -> PathBuf {
            self.0.program()
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
        let out =
            run_docker_with(&shim.program(), &["ps"], None, Instant::now() + secs(5)).unwrap();
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
            None,
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
        let out =
            run_docker_with(&shim.program(), &["ps"], None, Instant::now() + secs(5)).unwrap();
        assert_eq!(out.stdout, "done\n");
        assert_eq!(out.stderr.len(), 131072);
    }

    #[test]
    fn run_docker_streams_input_to_stdin_and_closes_it() {
        let shim = Shim::new("stdin", "cat");
        let input = b"image\nbytes without a trailing newline".to_vec();
        let out = run_docker_with(
            &shim.program(),
            &["exec"],
            Some(input.clone()),
            Instant::now() + secs(5),
        )
        .unwrap();
        assert_eq!(out.status, Some(0));
        assert_eq!(out.stdout.as_bytes(), input.as_slice());
    }

    #[test]
    fn run_docker_times_out_when_child_never_reads_large_input() {
        let shim = Shim::new("stdin-hang", "exec sleep 30");
        let start = Instant::now();
        let err = run_docker_with(
            &shim.program(),
            &["exec"],
            Some(vec![0; 1 << 20]),
            Instant::now() + Duration::from_millis(300),
        )
        .unwrap_err();
        assert!(matches!(err, ContainerListError::Timeout));
        assert!(start.elapsed() < secs(5));
    }

    #[test]
    fn run_docker_rejects_success_when_child_skipped_part_of_the_input() {
        let shim = Shim::new("stdin-skip", "exit 0");
        let err = run_docker_with(
            &shim.program(),
            &["exec"],
            Some(vec![0; 1 << 20]),
            Instant::now() + secs(5),
        )
        .unwrap_err();
        match err {
            ContainerListError::Failed { status, stderr } => {
                assert_eq!(status, Some(0));
                assert!(stderr.contains("did not read all input"), "{stderr}");
            }
            other => panic!("unexpected {other:?}"),
        }
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
