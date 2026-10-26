use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::labels::parse_inspect_output;
use super::{ContainerInfo, ContainerListError, ContainerRef};
use crate::noninteractive_process::{run_with_deadline, ProcessOutput, RunError};
use crate::ssh_host::launch::{is_ssh_login_failure, shell_quote, ssh_batch_argv};

/// Total budget for one resolution (`ps` followed by `inspect`).
pub(crate) const DOCKER_CLI_TIMEOUT: Duration = Duration::from_secs(5);

/// Why not `DOCKER_CLI_TIMEOUT`: a remote call may first have to establish the
/// ssh connection, which alone can take longer than a local docker round trip.
pub(crate) const REMOTE_DOCKER_TIMEOUT: Duration = Duration::from_secs(20);

/// Where the docker CLI runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DockerHost {
    Local,
    /// Docker runs on `host`, reached with the local `ssh` client.
    Ssh {
        host: String,
        control_path: Option<PathBuf>,
    },
}

impl DockerHost {
    pub(crate) fn for_launch(ssh: Option<&crate::ssh_host::SshLaunch>) -> Self {
        match ssh {
            Some(launch) => Self::Ssh {
                host: launch.host.clone(),
                control_path: launch.control_path.clone(),
            },
            None => Self::Local,
        }
    }

    pub(crate) fn timeout(&self) -> Duration {
        match self {
            Self::Local => DOCKER_CLI_TIMEOUT,
            Self::Ssh { .. } => REMOTE_DOCKER_TIMEOUT,
        }
    }
}

pub(crate) fn list_running_containers(
    host: &DockerHost,
) -> Result<Vec<ContainerInfo>, ContainerListError> {
    list_running_containers_with(program_for(host), host, host.timeout())
}

pub(crate) fn inspect_reference(
    host: &DockerHost,
    reference: &ContainerRef,
) -> Result<Option<ContainerInfo>, ContainerListError> {
    inspect_reference_with(program_for(host), host, reference, host.timeout())
}

/// The executable that is spawned: docker itself, or the ssh client.
pub(super) fn program_for(host: &DockerHost) -> &'static str {
    match host {
        DockerHost::Local => "docker",
        DockerHost::Ssh { .. } => "ssh",
    }
}

fn list_running_containers_with(
    program: impl AsRef<Path>,
    host: &DockerHost,
    timeout: Duration,
) -> Result<Vec<ContainerInfo>, ContainerListError> {
    let deadline = Instant::now() + timeout;
    let ids = running_ids(program.as_ref(), host, &[], deadline)?;
    inspect_ids(program.as_ref(), host, &ids, deadline)
}

fn inspect_reference_with(
    program: impl AsRef<Path>,
    host: &DockerHost,
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
            let ids = running_ids(program, host, &filters, deadline)?;
            let mut found = inspect_ids(program, host, &ids, deadline)?;
            // Compose names end in `-<container-number>`; the name is a stable
            // proxy that avoids widening ContainerInfo for a scale-out edge case.
            found.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(found.into_iter().next())
        }
        ContainerRef::Name(name) => {
            let args = ["inspect", "--type", "container", name.as_str()];
            let output = run_docker_with(program, host, &args, None, deadline)?;
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
    host: &DockerHost,
    filters: &[String],
    deadline: Instant,
) -> Result<Vec<String>, ContainerListError> {
    let mut args = vec!["ps", "-q", "--no-trunc", "--filter", "status=running"];
    for filter in filters {
        args.push("--filter");
        args.push(filter);
    }
    let output = run_docker_with(program, host, &args, None, deadline)?;
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
    host: &DockerHost,
    ids: &[String],
    deadline: Instant,
) -> Result<Vec<ContainerInfo>, ContainerListError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut args = vec!["inspect", "--type", "container"];
    args.extend(ids.iter().map(String::as_str));
    let output = run_docker_with(program, host, &args, None, deadline)?;
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

/// `ssh` running one `docker '<arg>'…` command, see [`ssh_batch_argv`].
///
/// Why one pre-quoted command: ssh joins its remaining arguments with spaces
/// for the remote login shell, which would split filters at `=` and `;`.
pub(super) fn ssh_docker_argv(
    host: &str,
    control_path: Option<&Path>,
    docker_args: &[&str],
) -> Vec<OsString> {
    let mut command = String::from("docker");
    for arg in docker_args {
        command.push(' ');
        command.push_str(&shell_quote(arg));
    }
    ssh_batch_argv(host, control_path, &command)
}

/// Runs one docker CLI call, locally or on the ssh host. `stdin` is streamed
/// from its own thread so a child that never reads it cannot block past
/// `deadline`.
pub(super) fn run_docker_with(
    program: &Path,
    host: &DockerHost,
    args: &[&str],
    stdin: Option<Vec<u8>>,
    deadline: Instant,
) -> Result<ProcessOutput, ContainerListError> {
    let result = match host {
        DockerHost::Local => run_with_deadline(program, args, stdin, deadline),
        DockerHost::Ssh { host, control_path } => run_with_deadline(
            program,
            &ssh_docker_argv(host, control_path.as_deref(), args),
            stdin,
            deadline,
        ),
    };
    let output = result.map_err(|err| match err {
        RunError::NotFound => match host {
            DockerHost::Local => ContainerListError::DockerNotFound,
            DockerHost::Ssh { .. } => ContainerListError::SshNotFound,
        },
        RunError::Timeout => {
            tracing::warn!(?program, "docker timed out");
            ContainerListError::Timeout
        }
        RunError::Failed { status, detail } => ContainerListError::Failed {
            status,
            stderr: detail,
        },
    })?;
    if let DockerHost::Ssh { host, .. } = host {
        if output.status == Some(255) && is_ssh_login_failure(&output.stderr) {
            return Err(ContainerListError::SshLoginRequired { host: host.clone() });
        }
    }
    Ok(output)
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

        /// A fake executable under another name, e.g. `ssh`.
        pub(in crate::container) fn named(program: &str, name: &str, script: &str) -> Self {
            Self(FakeProgram::new(program, name, script))
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
    fn run_docker_drains_stderr_larger_than_pipe_buffer() {
        let shim = Shim::new(
            "bigerr",
            "head -c 131072 /dev/zero | tr '\\0' x >&2; echo done",
        );
        let out = run_docker_with(
            &shim.program(),
            &DockerHost::Local,
            &["ps"],
            None,
            Instant::now() + secs(5),
        )
        .unwrap();
        assert_eq!(out.stdout, "done\n");
        assert_eq!(out.stderr.len(), 131072);
    }

    #[test]
    fn run_docker_rejects_success_when_child_skipped_part_of_the_input() {
        let shim = Shim::new("stdin-skip", "exit 0");
        let err = run_docker_with(
            &shim.program(),
            &DockerHost::Local,
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
        let err =
            list_running_containers_with("/nonexistent/herdr-docker", &DockerHost::Local, secs(5))
                .unwrap_err();
        assert!(matches!(err, ContainerListError::DockerNotFound));
    }

    #[test]
    fn list_running_containers_reports_failure_on_nonzero_exit() {
        let shim = Shim::new("fail", "echo daemon down >&2; exit 1");
        let err =
            list_running_containers_with(shim.program(), &DockerHost::Local, secs(5)).unwrap_err();
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
        assert!(
            list_running_containers_with(shim.program(), &DockerHost::Local, secs(5))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn list_running_containers_parses_inspect_output() {
        let (shim, fixture) = ps_inspect_shim("list");
        let infos =
            list_running_containers_with(shim.program(), &DockerHost::Local, secs(5)).unwrap();
        let _ = std::fs::remove_file(fixture);
        assert!(!infos.is_empty());
    }

    #[test]
    fn inspect_reference_resolves_compose_reference_through_ps_filters() {
        let (shim, fixture) = ps_inspect_shim("compose");
        let reference = ContainerRef::parse("shop/db").unwrap();
        let found = inspect_reference_with(shim.program(), &DockerHost::Local, &reference, secs(5))
            .unwrap();
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
        assert!(
            inspect_reference_with(shim.program(), &DockerHost::Local, &reference, secs(5))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn inspect_reference_shares_one_deadline_across_ps_and_inspect() {
        let shim = Shim::new(
            "slow",
            "case \"$1\" in ps) sleep 0.4; echo abc ;; inspect) sleep 0.4; echo '[]' ;; esac",
        );
        let reference = ContainerRef::parse("shop/db").unwrap();
        let err = inspect_reference_with(
            shim.program(),
            &DockerHost::Local,
            &reference,
            Duration::from_millis(600),
        )
        .unwrap_err();
        assert!(matches!(err, ContainerListError::Timeout));
    }

    fn strings(argv: Vec<OsString>) -> Vec<String> {
        argv.into_iter().map(|a| a.into_string().unwrap()).collect()
    }

    fn prod(control_path: Option<&str>) -> DockerHost {
        DockerHost::Ssh {
            host: "deploy@prod".into(),
            control_path: control_path.map(PathBuf::from),
        }
    }

    const BATCH_OPTIONS: [&str; 16] = [
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=5",
        "-o",
        "RequestTTY=no",
        "-o",
        "RemoteCommand=none",
        "-o",
        "ClearAllForwardings=yes",
        "-o",
        "PermitLocalCommand=no",
        "-o",
        "ForwardX11=no",
        "-o",
        "ForwardAgent=no",
    ];

    #[test]
    fn ssh_docker_argv_joins_the_pane_master_and_quotes_every_docker_argument() {
        let argv = strings(ssh_docker_argv(
            "deploy@prod",
            Some(Path::new("/tmp/hssh-1/abc-%C")),
            &["ps", "--filter", "label=a=b c", "it's"],
        ));
        let mut expected: Vec<String> = [
            "-S",
            "/tmp/hssh-1/abc-%C",
            "-o",
            "ControlMaster=auto",
            "-o",
            "ControlPersist=600",
        ]
        .into_iter()
        .chain(BATCH_OPTIONS)
        .chain(["deploy@prod"])
        .map(str::to_owned)
        .collect();
        expected.push(r"docker 'ps' '--filter' 'label=a=b c' 'it'\''s'".into());
        assert_eq!(argv, expected);
    }

    #[test]
    fn ssh_docker_argv_omits_the_master_options_without_a_control_path() {
        let argv = strings(ssh_docker_argv("prod", None, &["ps"]));
        let mut expected: Vec<String> = BATCH_OPTIONS.iter().map(|s| s.to_string()).collect();
        expected.extend(["prod".to_owned(), "docker 'ps'".to_owned()]);
        assert_eq!(argv, expected);
        assert!(!argv.iter().any(|arg| arg == "-S" || arg == "-t"));
    }

    /// A fake `ssh` that records its argv and evaluates the remote command
    /// the way a login shell would, against a fake `docker` shell function.
    fn remote_shim(name: &str, docker_function: &str) -> (Shim, PathBuf) {
        let log = std::env::temp_dir().join(format!(
            "herdr-ssh-docker-{}-{name}.log",
            std::process::id()
        ));
        let script = format!(
            "printf '%s\\n' \"$@\" > {log}; for last; do :; done; docker() {{ {docker_function} ;}}; eval \"$last\"",
            log = log.display()
        );
        (Shim::named("ssh", name, &script), log)
    }

    #[test]
    fn list_running_containers_runs_ps_and_inspect_on_the_ssh_host() {
        let fixture = std::env::temp_dir().join(format!(
            "herdr-docker-remote-fixture-{}.json",
            std::process::id()
        ));
        std::fs::write(&fixture, INSPECT).unwrap();
        let (shim, log) = remote_shim(
            "list",
            &format!(
                "case \"$1\" in ps) echo abc123; echo ;; inspect) cat {} ;; esac",
                fixture.display()
            ),
        );
        let host = prod(Some("/tmp/ctl-%C"));

        let infos = list_running_containers_with(shim.program(), &host, secs(5)).unwrap();

        let logged = std::fs::read_to_string(&log).unwrap();
        let _ = std::fs::remove_file(&fixture);
        let _ = std::fs::remove_file(&log);
        assert!(!infos.is_empty());
        assert!(logged.contains("ControlMaster=auto"), "{logged}");
        assert!(logged.contains("deploy@prod"), "{logged}");
        assert!(
            logged.contains("docker 'inspect' '--type' 'container' 'abc123'"),
            "{logged}"
        );
    }

    #[test]
    fn inspect_reference_over_ssh_passes_compose_filters_through_the_remote_shell() {
        let fixture = std::env::temp_dir().join(format!(
            "herdr-docker-remote-compose-{}.json",
            std::process::id()
        ));
        std::fs::write(&fixture, INSPECT).unwrap();
        // The filter values must arrive as single words, or `ps` would see none.
        let (shim, log) = remote_shim(
            "compose",
            &format!(
                "case \"$1\" in ps) case \"$*\" in *'label=com.docker.compose.project=shop'*'label=com.docker.compose.service=db'*) echo abc123 ;; esac ;; inspect) cat {} ;; esac",
                fixture.display()
            ),
        );
        let reference = ContainerRef::parse("shop/db").unwrap();

        let found = inspect_reference_with(shim.program(), &prod(None), &reference, secs(5));

        let _ = std::fs::remove_file(&fixture);
        let _ = std::fs::remove_file(&log);
        assert!(found.unwrap().is_some());
    }

    #[test]
    fn ssh_login_failures_ask_for_an_ssh_pane_first() {
        for stderr in [
            "deploy@prod: Permission denied (publickey,password).",
            "Host key verification failed.",
            "ssh: Could not resolve hostname prod: Name or service not known",
        ] {
            let shim = Shim::named("ssh", "login", &format!("echo '{stderr}' >&2; exit 255"));
            let err =
                list_running_containers_with(shim.program(), &prod(None), secs(5)).unwrap_err();
            match &err {
                ContainerListError::SshLoginRequired { host } => assert_eq!(host, "deploy@prod"),
                other => panic!("{stderr}: unexpected {other:?}"),
            }
            assert!(err
                .to_string()
                .contains("open an ssh pane to deploy@prod first"));
        }
    }

    #[test]
    fn ssh_failures_other_than_login_stay_plain_failures() {
        // A remote docker that exits 255 or a docker message that merely
        // mentions permissions must not be reported as a login problem.
        for (script, status) in [
            ("echo 'connection reset' >&2; exit 255", 255),
            ("echo 'permission denied: docker.sock' >&2; exit 1", 1),
            (
                "echo 'Permission denied while connecting to docker' >&2; exit 1",
                1,
            ),
        ] {
            let shim = Shim::named("ssh", "plain-failure", script);
            let err =
                list_running_containers_with(shim.program(), &prod(None), secs(5)).unwrap_err();
            match err {
                ContainerListError::Failed { status: got, .. } => assert_eq!(got, Some(status)),
                other => panic!("{script}: unexpected {other:?}"),
            }
        }
    }

    #[test]
    fn list_running_containers_reports_a_missing_ssh_client() {
        let err = list_running_containers_with("/nonexistent/herdr-ssh", &prod(None), secs(5))
            .unwrap_err();
        assert!(matches!(err, ContainerListError::SshNotFound));
    }

    #[test]
    fn remote_docker_gets_a_longer_budget_than_local_docker() {
        assert!(prod(None).timeout() > DockerHost::Local.timeout());
        assert_eq!(
            DockerHost::for_launch(
                crate::ssh_host::SshLaunch::new("prod".into(), Some("/tmp/c".into())).as_ref()
            ),
            DockerHost::Ssh {
                host: "prod".into(),
                control_path: Some("/tmp/c".into())
            }
        );
        assert_eq!(DockerHost::for_launch(None), DockerHost::Local);
    }
}
