//! Starting a long-lived helper script in a pane's environment (a container, or
//! an ssh host) with its stdio piped back to us. Uploads and downloads each run
//! one such helper per session, for the same reasons: `docker exec` costs
//! 100-300 ms per start and an ssh channel at least a round trip, and sshd's
//! `MaxSessions` caps parallelism.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use crate::noninteractive_process::{spawn_streaming, RunError, StreamingChild};
use crate::offhost::exec::{classify_failure, command_argv, ExecError};
use crate::offhost::PaneEnvironment;
use crate::upload::path::Failure;

/// Starts the helper somewhere. The seam lets tests run it with local shells.
pub(crate) trait OffhostLauncher: Send + Sync {
    fn launch(&self, args: &[&str]) -> Result<StreamingChild, Failure>;
    /// Why the helper stopped, from its exit status and stderr.
    fn classify(&self, status: Option<i32>, stderr: &str) -> Failure;
    fn timed_out(&self) -> Failure;
    /// Time to start and answer the begin queries.
    fn begin_budget(&self) -> Duration;
}

/// Words that differ between the transfers when a connection cannot be shared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Transfer {
    /// "upload to" / "download from": what the message says it cannot do.
    pub(crate) cannot: &'static str,
    /// What to do after reopening the pane.
    pub(crate) then: &'static str,
}

impl Transfer {
    pub(crate) const UPLOAD: Self = Self {
        cannot: "upload to",
        then: "drop again",
    };
    pub(crate) const DOWNLOAD: Self = Self {
        cannot: "download from",
        then: "try again",
    };
}

/// Q14: the message names the cause and what to do.
pub(crate) fn connection_not_shared(transfer: Transfer, host: &str) -> Failure {
    (
        "ssh_unavailable",
        format!(
            "cannot {} {host}: the pane's ssh connection is not shared \
             (ControlMaster unavailable). Reopen the ssh pane from Herdr so its \
             connection can be shared, then {}.",
            transfer.cannot, transfer.then
        ),
    )
}

/// The helper's argv in `env`. Why refuse a container on an ssh host
/// without a control path, unlike directory reads: a transfer must never open
/// a second login of its own (Q14).
pub(crate) fn script_argv(
    env: &PaneEnvironment,
    transfer: Transfer,
    script: &str,
    args: &[&str],
) -> Result<(&'static str, Vec<OsString>), Failure> {
    if let PaneEnvironment::Container(launch) = env {
        if let Some(ssh) = launch.ssh.as_ref().filter(|ssh| ssh.control_path.is_none()) {
            return Err(connection_not_shared(transfer, &ssh.host));
        }
    }
    command_argv(env, script, args).map_err(|error| match error {
        ExecError::SshControlUnavailable { host } => connection_not_shared(transfer, &host),
        other => other.failure(),
    })
}

/// Runs the helper in a pane's environment with the docker or ssh CLI.
pub(crate) struct EnvironmentLauncher {
    pub(crate) env: PaneEnvironment,
    pub(crate) docker: PathBuf,
    pub(crate) ssh: PathBuf,
    pub(crate) script: &'static str,
    pub(crate) transfer: Transfer,
}

impl EnvironmentLauncher {
    pub(crate) fn system(env: PaneEnvironment, script: &'static str, transfer: Transfer) -> Self {
        Self {
            env,
            docker: PathBuf::from("docker"),
            ssh: PathBuf::from("ssh"),
            script,
            transfer,
        }
    }

    fn over_ssh(&self) -> bool {
        !matches!(
            &self.env,
            PaneEnvironment::Container(launch) if launch.ssh.is_none()
        )
    }
}

impl OffhostLauncher for EnvironmentLauncher {
    fn launch(&self, args: &[&str]) -> Result<StreamingChild, Failure> {
        let (program, argv) = script_argv(&self.env, self.transfer, self.script, args)?;
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

/// Runs the helper with a local shell instead of docker or ssh.
#[cfg(all(test, unix))]
pub(crate) struct LocalShellLauncher {
    /// Program first, then any arguments that go before `-c`.
    pub(crate) shell: Vec<String>,
    pub(crate) script: &'static str,
    /// The `code` of failures it reports, so each transfer's tests see its own.
    pub(crate) code: &'static str,
    pub(crate) budget: Duration,
}

#[cfg(all(test, unix))]
impl LocalShellLauncher {
    pub(crate) fn new(shell: Vec<String>, script: &'static str, code: &'static str) -> Self {
        Self {
            shell,
            script,
            code,
            budget: Duration::from_secs(10),
        }
    }

    pub(crate) fn with_budget(mut self, budget: Duration) -> Self {
        self.budget = budget;
        self
    }
}

#[cfg(all(test, unix))]
impl OffhostLauncher for LocalShellLauncher {
    fn launch(&self, args: &[&str]) -> Result<StreamingChild, Failure> {
        let mut argv: Vec<String> = self.shell[1..].to_vec();
        argv.extend(["-c".to_owned(), self.script.to_owned(), "sh".to_owned()]);
        argv.extend(args.iter().map(|arg| (*arg).to_owned()));
        spawn_streaming(std::path::Path::new(&self.shell[0]), &argv)
            .map_err(|error| (self.code, format!("{error:?}")))
    }

    fn classify(&self, status: Option<i32>, stderr: &str) -> Failure {
        (
            self.code,
            format!("helper exited with {status:?}: {stderr}"),
        )
    }

    fn timed_out(&self) -> Failure {
        (self.code, "timed out".into())
    }

    fn begin_budget(&self) -> Duration {
        self.budget
    }
}
