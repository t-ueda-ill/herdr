//! Runs one fixed `sh` script inside a pane's environment and returns its raw
//! stdout. Three transports: the local docker CLI, docker on an ssh host, and
//! the pane's own ssh host.

use std::ffi::OsString;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::api::schema::{PaneLocation, PaneLocationKind};
use crate::container::docker::{DOCKER_CLI_TIMEOUT, REMOTE_DOCKER_TIMEOUT};
use crate::container::ContainerLaunch;
use crate::noninteractive_process::{run_with_deadline_raw, RunError};
use crate::ssh_host::launch::{is_ssh_login_failure, shell_quote, ssh_batch_argv};
use crate::ssh_host::SshLaunch;

/// Why not the `pane.file.read` default: a background refresh must never hold a
/// request slot long enough to starve the foreground work behind it.
pub(crate) const POLL_DEADLINE: Duration = Duration::from_secs(5);

/// Largest stdout accepted from a script: text limit plus two index streams
/// with headroom. A bigger answer means the remote ignored the scripts' caps.
const MAX_OUTPUT_BYTES: usize = 48 * 1024 * 1024;

/// Where a pane's shell runs. Captured on the app thread; the worker needs
/// nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PaneEnvironment {
    Container(ContainerLaunch),
    Ssh(SshLaunch),
}

impl PaneEnvironment {
    pub(crate) fn location(&self) -> PaneLocation {
        match self {
            Self::Container(launch) => PaneLocation {
                kind: PaneLocationKind::Container,
                ssh_host: launch.ssh.as_ref().map(|ssh| ssh.canonical.clone()),
                container: Some(launch.canonical.clone()),
            },
            Self::Ssh(launch) => PaneLocation {
                kind: PaneLocationKind::Ssh,
                ssh_host: Some(launch.canonical.clone()),
                container: None,
            },
        }
    }

    /// Local docker answers quickly; anything over ssh may first have to
    /// establish the connection.
    pub(crate) fn deadline_budget(&self, poll: bool) -> Duration {
        if poll {
            return POLL_DEADLINE;
        }
        match self {
            Self::Container(ContainerLaunch { ssh: None, .. }) => DOCKER_CLI_TIMEOUT,
            _ => REMOTE_DOCKER_TIMEOUT,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExecError {
    DockerNotFound,
    SshNotFound,
    /// The pane's ControlPath could not be created on this host.
    SshControlUnavailable {
        host: String,
    },
    SshLoginRequired {
        host: String,
    },
    SshFailed {
        host: String,
        detail: String,
    },
    ContainerNotFound {
        container: String,
    },
    ContainerUnavailable {
        detail: String,
    },
    ShellUnavailable,
    Timeout {
        over_ssh: bool,
    },
    OutputTooLarge,
    Failed {
        over_ssh: bool,
        detail: String,
    },
}

impl ExecError {
    /// API error code and message, as `encode_error` takes them.
    pub(crate) fn failure(&self) -> (&'static str, String) {
        match self {
            Self::DockerNotFound => (
                "container_unavailable",
                "docker executable not found".to_owned(),
            ),
            Self::SshNotFound => ("ssh_unavailable", "ssh executable not found".to_owned()),
            Self::SshControlUnavailable { host } => (
                "ssh_unavailable",
                format!("cannot reach {host} without a prompt: ssh connection sharing is unavailable on this host"),
            ),
            Self::SshLoginRequired { host } => (
                "ssh_unavailable",
                format!("cannot reach {host} without a prompt; open an ssh pane to {host} first, then retry"),
            ),
            Self::SshFailed { host, detail } => (
                "ssh_unavailable",
                format!("ssh to {host} failed: {detail}"),
            ),
            Self::ContainerNotFound { container } => (
                "container_not_found",
                format!("no running container matches {container}"),
            ),
            Self::ContainerUnavailable { detail } => ("container_unavailable", detail.clone()),
            Self::ShellUnavailable => (
                "offhost_shell_unavailable",
                "this environment has no `sh`, so its files cannot be listed".to_owned(),
            ),
            Self::Timeout { over_ssh: true } => (
                "ssh_unavailable",
                "the remote host did not respond in time".to_owned(),
            ),
            Self::Timeout { over_ssh: false } => (
                "container_unavailable",
                "docker did not respond in time".to_owned(),
            ),
            Self::OutputTooLarge => (
                "directory_read_failed",
                "the environment returned more data than expected".to_owned(),
            ),
            Self::Failed { over_ssh, detail } => (
                if *over_ssh {
                    "ssh_unavailable"
                } else {
                    "container_unavailable"
                },
                detail.clone(),
            ),
        }
    }
}

/// The executable and arguments that run `sh -c <script> sh <args…>` in `env`.
///
/// Why positional arguments: the script text is fixed, so a path with spaces,
/// quotes, or a leading `-` can never be read as code or as an option.
pub(crate) fn command_argv(
    env: &PaneEnvironment,
    script: &str,
    args: &[&str],
) -> Result<(&'static str, Vec<OsString>), ExecError> {
    match env {
        PaneEnvironment::Container(launch) => {
            let mut docker_args = vec!["exec".to_owned(), "-i".to_owned()];
            if let Some(user) = &launch.user {
                docker_args.push("-u".to_owned());
                docker_args.push(user.clone());
            }
            docker_args.push(launch.id.clone());
            docker_args.extend(["sh".to_owned(), "-c".to_owned(), script.to_owned()]);
            docker_args.push("sh".to_owned());
            docker_args.extend(args.iter().map(|arg| (*arg).to_owned()));
            match &launch.ssh {
                None => Ok(("docker", docker_args.into_iter().map(Into::into).collect())),
                Some(ssh) => {
                    // Why one quoted string: ssh joins its arguments with spaces
                    // for the remote login shell, which would re-split the script.
                    let command = std::iter::once("docker".to_owned())
                        .chain(docker_args.iter().map(|arg| shell_quote(arg)))
                        .collect::<Vec<_>>()
                        .join(" ");
                    Ok((
                        "ssh",
                        ssh_batch_argv(&ssh.host, ssh.control_path.as_deref(), &command),
                    ))
                }
            }
        }
        PaneEnvironment::Ssh(launch) => {
            // Why not run without ControlPath: the pane's login owns the
            // prompts, and a second full login from here could never answer them.
            let Some(control_path) = launch.control_path.as_deref() else {
                return Err(ExecError::SshControlUnavailable {
                    host: launch.host.clone(),
                });
            };
            let command = ["sh", "-c", script, "sh"]
                .into_iter()
                .chain(args.iter().copied())
                .map(shell_quote)
                .collect::<Vec<_>>()
                .join(" ");
            Ok((
                "ssh",
                ssh_batch_argv(&launch.host, Some(control_path), &command),
            ))
        }
    }
}

/// Program names to spawn. Overridable so tests can substitute fakes.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Programs<'a> {
    pub(crate) docker: &'a Path,
    pub(crate) ssh: &'a Path,
}

impl Programs<'static> {
    pub(crate) fn system() -> Self {
        Self {
            docker: Path::new("docker"),
            ssh: Path::new("ssh"),
        }
    }
}

pub(crate) fn run_script_with(
    programs: &Programs<'_>,
    env: &PaneEnvironment,
    script: &str,
    args: &[&str],
    deadline: Instant,
) -> Result<Vec<u8>, ExecError> {
    let (program, argv) = command_argv(env, script, args)?;
    let over_ssh = program == "ssh";
    let path = if over_ssh {
        programs.ssh
    } else {
        programs.docker
    };
    let output = run_with_deadline_raw(path, &argv, None, deadline).map_err(|err| match err {
        RunError::NotFound if over_ssh => ExecError::SshNotFound,
        RunError::NotFound => ExecError::DockerNotFound,
        RunError::Timeout => {
            tracing::warn!(program, "off-host script timed out");
            ExecError::Timeout { over_ssh }
        }
        RunError::Failed { detail, .. } => ExecError::Failed { over_ssh, detail },
    })?;
    if output.status == Some(0) {
        if output.stdout.len() > MAX_OUTPUT_BYTES {
            return Err(ExecError::OutputTooLarge);
        }
        return Ok(output.stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(classify_failure(env, output.status, &stderr, over_ssh))
}

fn classify_failure(
    env: &PaneEnvironment,
    status: Option<i32>,
    stderr: &str,
    over_ssh: bool,
) -> ExecError {
    let ssh_host = match env {
        PaneEnvironment::Ssh(launch) => Some(&launch.host),
        PaneEnvironment::Container(launch) => launch.ssh.as_ref().map(|ssh| &ssh.host),
    };
    if let (true, Some(host), Some(255)) = (over_ssh, ssh_host, status) {
        // Status 255 is ssh's own failure, as opposed to the remote command's.
        if is_ssh_login_failure(stderr) {
            return ExecError::SshLoginRequired { host: host.clone() };
        }
        return ExecError::SshFailed {
            host: host.clone(),
            detail: first_line(stderr),
        };
    }
    if let PaneEnvironment::Container(launch) = env {
        if stderr.contains("No such container") {
            return ExecError::ContainerNotFound {
                container: launch.canonical.clone(),
            };
        }
        if stderr.contains("is not running") || stderr.contains("is paused") {
            return ExecError::ContainerUnavailable {
                detail: first_line(stderr),
            };
        }
    }
    // 126/127 is the shell's (or docker exec's) "cannot run it" status.
    if matches!(status, Some(126 | 127))
        && (stderr.contains("executable file not found")
            || stderr.contains("sh: not found")
            || stderr.contains("sh: command not found")
            || stderr.contains("exec: \"sh\""))
    {
        return ExecError::ShellUnavailable;
    }
    let program = if over_ssh { "ssh" } else { "docker" };
    let detail = match status {
        Some(code) => format!(
            "{program} exited with status {code}: {}",
            first_line(stderr)
        ),
        None => format!("{program} was terminated: {}", first_line(stderr)),
    };
    ExecError::Failed { over_ssh, detail }
}

fn first_line(stderr: &str) -> String {
    stderr.lines().next().unwrap_or("").trim().to_owned()
}

/// Runs scripts in a pane's environment under one shared deadline.
pub(crate) struct EnvironmentRunner<'a> {
    pub(crate) programs: Programs<'a>,
    pub(crate) env: &'a PaneEnvironment,
    pub(crate) deadline: Instant,
}

impl super::ops::ScriptRunner for EnvironmentRunner<'_> {
    fn run(&self, script: &str, args: &[&str]) -> Result<Vec<u8>, ExecError> {
        run_script_with(&self.programs, self.env, script, args, self.deadline)
    }

    fn seconds_left(&self) -> Option<u64> {
        Some(
            self.deadline
                .saturating_duration_since(Instant::now())
                .as_secs(),
        )
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::container::{ContainerRef, ContainerShell};
    use crate::noninteractive_process::fake_program::FakeProgram;

    fn container(user: Option<&str>, ssh: Option<SshLaunch>) -> PaneEnvironment {
        PaneEnvironment::Container(ContainerLaunch {
            reference: ContainerRef::Name("scratch".into()),
            canonical: "scratch".into(),
            id: "cid".into(),
            workdir: Some("/work".into()),
            user: user.map(str::to_owned),
            shell: ContainerShell::BashOrSh,
            ssh,
        })
    }

    fn ssh_launch(control_path: Option<&str>) -> SshLaunch {
        SshLaunch::new("deploy@devbox".into(), control_path.map(Into::into)).unwrap()
    }

    fn strings(argv: Vec<OsString>) -> Vec<String> {
        argv.into_iter()
            .map(|arg| arg.into_string().unwrap())
            .collect()
    }

    #[test]
    fn local_container_runs_the_script_through_docker_exec_with_positional_arguments() {
        let (program, argv) =
            command_argv(&container(Some("dev"), None), "echo \"$1\"", &["-x", "a b"]).unwrap();

        assert_eq!(program, "docker");
        assert_eq!(
            strings(argv),
            [
                "exec",
                "-i",
                "-u",
                "dev",
                "cid",
                "sh",
                "-c",
                "echo \"$1\"",
                "sh",
                "-x",
                "a b"
            ]
        );
    }

    #[test]
    fn local_container_without_a_user_passes_no_user_flag() {
        let (_, argv) = command_argv(&container(None, None), "true", &[]).unwrap();

        assert!(!strings(argv).iter().any(|arg| arg == "-u"));
    }

    #[test]
    fn container_on_an_ssh_host_wraps_docker_exec_in_batch_mode_ssh() {
        let env = container(Some("dev"), Some(ssh_launch(Some("/tmp/ctl-%C"))));

        let (program, argv) = command_argv(&env, "true", &["it's"]).unwrap();
        let argv = strings(argv);

        assert_eq!(program, "ssh");
        assert_eq!(
            argv[..6],
            [
                "-S",
                "/tmp/ctl-%C",
                "-o",
                "ControlMaster=auto",
                "-o",
                "ControlPersist=600"
            ]
        );
        for option in ["BatchMode=yes", "RemoteCommand=none", "RequestTTY=no"] {
            assert!(argv.iter().any(|arg| arg == option), "{option}");
        }
        assert_eq!(argv[argv.len() - 2], "deploy@devbox");
        assert_eq!(
            argv.last().unwrap(),
            r"docker 'exec' '-i' '-u' 'dev' 'cid' 'sh' '-c' 'true' 'sh' 'it'\''s'"
        );
    }

    #[test]
    fn container_on_an_ssh_host_still_runs_without_a_control_path() {
        let env = container(None, Some(ssh_launch(None)));

        let (program, argv) = command_argv(&env, "true", &[]).unwrap();

        assert_eq!(program, "ssh");
        assert!(!strings(argv).iter().any(|arg| arg == "-S"));
    }

    #[test]
    fn ssh_pane_runs_the_script_through_its_own_control_path_in_batch_mode() {
        let env = PaneEnvironment::Ssh(ssh_launch(Some("/tmp/hssh/abc-%C")));

        let (program, argv) = command_argv(&env, "echo \"$1\"", &["/srv/my dir"]).unwrap();
        let argv = strings(argv);

        assert_eq!(program, "ssh");
        let options = [
            "-S",
            "/tmp/hssh/abc-%C",
            "-o",
            "ControlMaster=auto",
            "-o",
            "ControlPersist=600",
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
            "deploy@devbox",
        ];
        assert_eq!(argv[..options.len()], options);
        assert_eq!(
            argv[options.len()..],
            [r#"'sh' '-c' 'echo "$1"' 'sh' '/srv/my dir'"#]
        );
    }

    #[test]
    fn ssh_pane_without_a_control_path_is_refused_instead_of_logging_in_again() {
        let env = PaneEnvironment::Ssh(ssh_launch(None));

        let error = command_argv(&env, "true", &[]).unwrap_err();

        assert_eq!(
            error,
            ExecError::SshControlUnavailable {
                host: "deploy@devbox".into()
            }
        );
        assert_eq!(error.failure().0, "ssh_unavailable");
    }

    #[test]
    fn remote_command_strings_read_back_as_the_original_arguments() {
        let env = PaneEnvironment::Ssh(ssh_launch(Some("/tmp/c")));
        let hostile = ["a b", "it's", "$(id)", "x;y", "-flag", "new\nline", ""];

        let (_, argv) = command_argv(&env, "script", &hostile).unwrap();
        let command = strings(argv).pop().unwrap();
        // What the remote login shell does with the string.
        let output = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("for a in {command}; do printf '%s|' \"$a\"; done"))
            .output()
            .unwrap();

        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "sh|-c|script|sh|a b|it's|$(id)|x;y|-flag|new\nline||"
        );
    }

    #[test]
    fn deadline_budget_is_five_seconds_locally_twenty_over_ssh_and_five_when_polling() {
        let local = container(None, None);
        let remote_container = container(None, Some(ssh_launch(None)));
        let ssh = PaneEnvironment::Ssh(ssh_launch(Some("/tmp/c")));

        assert_eq!(local.deadline_budget(false), Duration::from_secs(5));
        assert_eq!(
            remote_container.deadline_budget(false),
            Duration::from_secs(20)
        );
        assert_eq!(ssh.deadline_budget(false), Duration::from_secs(20));
        for env in [&local, &remote_container, &ssh] {
            assert_eq!(env.deadline_budget(true), Duration::from_secs(5));
        }
    }

    #[test]
    fn location_names_the_container_and_the_ssh_host_it_runs_on() {
        assert_eq!(
            container(None, None).location(),
            PaneLocation {
                kind: PaneLocationKind::Container,
                ssh_host: None,
                container: Some("scratch".into())
            }
        );
        assert_eq!(
            container(None, Some(ssh_launch(None))).location(),
            PaneLocation {
                kind: PaneLocationKind::Container,
                ssh_host: Some("devbox".into()),
                container: Some("scratch".into())
            }
        );
        assert_eq!(
            PaneEnvironment::Ssh(ssh_launch(None)).location(),
            PaneLocation {
                kind: PaneLocationKind::Ssh,
                ssh_host: Some("devbox".into()),
                container: None
            }
        );
    }

    fn run_with_fakes(
        env: &PaneEnvironment,
        name: &str,
        script: &str,
        budget: Duration,
    ) -> Result<Vec<u8>, ExecError> {
        let docker = FakeProgram::new("docker", name, script);
        let ssh = FakeProgram::new("ssh", name, script);
        let (docker_path, ssh_path) = (docker.program(), ssh.program());
        run_script_with(
            &Programs {
                docker: &docker_path,
                ssh: &ssh_path,
            },
            env,
            "true",
            &[],
            Instant::now() + budget,
        )
    }

    #[test]
    fn run_returns_raw_stdout_bytes_including_nul_and_invalid_utf8() {
        let result = run_with_fakes(
            &container(None, None),
            "raw",
            r"printf 'a\000\377b'",
            Duration::from_secs(5),
        );

        assert_eq!(result, Ok(b"a\0\xffb".to_vec()));
    }

    #[test]
    fn run_gives_the_program_the_argv_command_argv_built() {
        let result = run_with_fakes(
            &container(Some("dev"), None),
            "argv",
            r#"printf '%s|' "$@""#,
            Duration::from_secs(5),
        );

        assert_eq!(result, Ok(b"exec|-i|-u|dev|cid|sh|-c|true|sh|".to_vec()));
    }

    #[test]
    fn run_times_out_at_the_shared_deadline() {
        let started = Instant::now();

        let result = run_with_fakes(
            &container(None, None),
            "slow",
            "sleep 5",
            Duration::from_millis(150),
        );

        assert_eq!(result, Err(ExecError::Timeout { over_ssh: false }));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn ssh_login_failures_ask_the_user_to_open_an_ssh_pane() {
        let env = PaneEnvironment::Ssh(ssh_launch(Some("/tmp/c")));

        let result = run_with_fakes(
            &env,
            "login",
            "echo 'deploy@devbox: Permission denied (publickey).' >&2; exit 255",
            Duration::from_secs(5),
        );

        let error = result.unwrap_err();
        assert_eq!(
            error,
            ExecError::SshLoginRequired {
                host: "deploy@devbox".into()
            }
        );
        assert!(error
            .failure()
            .1
            .contains("open an ssh pane to deploy@devbox"));
    }

    #[test]
    fn other_ssh_failures_report_the_first_stderr_line() {
        let env = PaneEnvironment::Ssh(ssh_launch(Some("/tmp/c")));

        let result = run_with_fakes(
            &env,
            "refused",
            "echo 'ssh: connect to host devbox port 22: Connection refused' >&2; exit 255",
            Duration::from_secs(5),
        );

        assert_eq!(
            result,
            Err(ExecError::SshFailed {
                host: "deploy@devbox".into(),
                detail: "ssh: connect to host devbox port 22: Connection refused".into()
            })
        );
    }

    #[test]
    fn a_container_without_sh_reports_offhost_shell_unavailable() {
        let result = run_with_fakes(
            &container(None, None),
            "no-sh",
            r#"echo 'OCI runtime exec failed: exec: "sh": executable file not found in $PATH' >&2; exit 126"#,
            Duration::from_secs(5),
        );

        let error = result.unwrap_err();
        assert_eq!(error, ExecError::ShellUnavailable);
        assert_eq!(error.failure().0, "offhost_shell_unavailable");
    }

    #[test]
    fn a_remote_without_sh_reports_offhost_shell_unavailable() {
        let env = PaneEnvironment::Ssh(ssh_launch(Some("/tmp/c")));

        let result = run_with_fakes(
            &env,
            "remote-no-sh",
            "echo 'bash: sh: command not found' >&2; exit 127",
            Duration::from_secs(5),
        );

        assert_eq!(result, Err(ExecError::ShellUnavailable));
    }

    #[test]
    fn a_missing_container_maps_to_container_not_found() {
        let result = run_with_fakes(
            &container(None, None),
            "gone",
            "echo 'Error response from daemon: No such container: cid' >&2; exit 1",
            Duration::from_secs(5),
        );

        let error = result.unwrap_err();
        assert_eq!(error.failure().0, "container_not_found");
    }

    #[test]
    fn a_stopped_container_maps_to_container_unavailable() {
        let result = run_with_fakes(
            &container(None, None),
            "stopped",
            "echo 'Error response from daemon: container cid is not running' >&2; exit 1",
            Duration::from_secs(5),
        );

        assert_eq!(result.unwrap_err().failure().0, "container_unavailable");
    }

    #[test]
    fn a_missing_docker_or_ssh_executable_has_its_own_error() {
        let missing = Programs {
            docker: Path::new("/nonexistent/docker"),
            ssh: Path::new("/nonexistent/ssh"),
        };
        let deadline = Instant::now() + Duration::from_secs(5);

        let docker = run_script_with(&missing, &container(None, None), "true", &[], deadline);
        let ssh = run_script_with(
            &missing,
            &PaneEnvironment::Ssh(ssh_launch(Some("/tmp/c"))),
            "true",
            &[],
            deadline,
        );

        assert_eq!(docker, Err(ExecError::DockerNotFound));
        assert_eq!(ssh, Err(ExecError::SshNotFound));
    }
}
