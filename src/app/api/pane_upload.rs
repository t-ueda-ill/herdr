//! `pane.upload.*`: copies client files into the environment a pane's shell
//! sees. The app thread only validates, resolves the target from what it
//! already holds, and queues jobs; each session's worker does the I/O.

use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::Arc;

use crate::api::schema::{
    Method, PaneLocation, PaneLocationKind, PaneUploadBeginParams, Request, ResponseResult,
};
use crate::app::App;
use crate::offhost::launcher::{EnvironmentLauncher, Transfer};
use crate::offhost::{ops, PaneEnvironment, PaneTarget};
use crate::server::client_shell::{
    running_off_host_shell, running_pane_off_host_shell, RunningOffHostShell,
};
use crate::terminal::TerminalId;
use crate::upload::path::Failure;
use crate::upload::receiver_script::RECEIVER_SCRIPT;
use crate::upload::registry::{UploadEnvironment, UploadHandle, UploadOwner, UploadTargetKey};
use crate::upload::session::{Declared, Session};
use crate::upload::sink::UploadSink;
use crate::upload::sink_host::HostSink;
use crate::upload::sink_remote::{RemoteSink, RemoteTimeouts};
use crate::upload::worker::{Control, Reply, Timeouts, UploadJob, Worker, WriteJob};
use crate::upload::UPLOAD_WINDOW;

use super::responses::{encode_error, encode_success};

/// Delivers one encoded response wherever the request came from.
pub(crate) type UploadResponder = Box<dyn FnOnce(String) + Send>;

/// Who sent a request, which decides what it may target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UploadOrigin {
    Api,
    /// A client-shell endpoint: workspace panes, its popup, and the host.
    ShellClient(u64),
    /// A direct terminal attach: only the terminal it is attached to.
    AttachedClient {
        client_id: u64,
        terminal_id: String,
    },
}

impl UploadOrigin {
    fn owner(&self) -> UploadOwner {
        match self {
            Self::Api => UploadOwner::Api,
            Self::ShellClient(client_id) | Self::AttachedClient { client_id, .. } => {
                UploadOwner::Client(*client_id)
            }
        }
    }
}

enum Destination {
    Host(PathBuf),
    /// Empty: the receiver starts in the login directory (spec Q6).
    Remote(String),
}

struct ResolvedTarget {
    key: UploadTargetKey,
    environment: UploadEnvironment,
    destination: Destination,
}

pub(super) fn invalid(message: impl Into<String>) -> Failure {
    ("invalid_params", message.into())
}

pub(super) fn reply_for(id: String, respond: UploadResponder) -> Reply {
    Box::new(move |result| {
        respond(match result {
            Ok(result) => encode_success(id, result),
            Err((code, message)) => encode_error(id, code, message),
        })
    })
}

pub(super) fn host_location() -> PaneLocation {
    PaneLocation {
        kind: PaneLocationKind::Host,
        ssh_host: None,
        container: None,
    }
}

fn validate_host_destination(destination: &str) -> Result<PathBuf, Failure> {
    let path = Path::new(destination);
    if destination.chars().any(char::is_control)
        || !path.is_absolute()
        || path.components().any(|part| part == Component::ParentDir)
    {
        return Err(invalid(format!(
            "destination must be an absolute path without `..`: {destination:?}"
        )));
    }
    Ok(path.to_path_buf())
}

fn validate_remote_destination(destination: &str) -> Result<String, Failure> {
    ops::validate_path(destination)?;
    if destination.chars().any(char::is_control) {
        return Err(invalid("destination must not contain control characters"));
    }
    Ok(destination.to_owned())
}

pub(super) fn ensure_offhost_supported() -> Result<(), Failure> {
    if !crate::platform::OFFHOST_FILE_VIEWER_SUPPORTED {
        return Err((
            "platform_unsupported",
            "ssh and container panes are not supported on this host".into(),
        ));
    }
    Ok(())
}

pub(super) fn environment_of(shell: Option<RunningOffHostShell<'_>>) -> UploadEnvironment {
    match shell {
        Some(RunningOffHostShell::Container(launch)) => UploadEnvironment::OffHost(Box::new(
            PaneEnvironment::Container(Box::new(launch.clone())),
        )),
        Some(RunningOffHostShell::Ssh(launch)) => {
            UploadEnvironment::OffHost(Box::new(PaneEnvironment::Ssh(launch.clone())))
        }
        None => UploadEnvironment::Host,
    }
}

impl App {
    /// Starts a `pane.upload.*` request from the API socket. Returns false for
    /// any other method so the caller can keep dispatching it.
    pub(crate) fn handle_deferred_pane_upload_api_request(
        &mut self,
        request: Request,
        respond_to: Sender<String>,
    ) -> bool {
        self.handle_pane_upload_request(
            UploadOrigin::Api,
            request,
            Box::new(move |response| {
                let _ = respond_to.send(response);
            }),
        )
    }

    pub(crate) fn handle_pane_upload_request(
        &mut self,
        origin: UploadOrigin,
        request: Request,
        respond: UploadResponder,
    ) -> bool {
        let Request { id, method } = request;
        let owner = origin.owner();
        match method {
            Method::PaneUploadBegin(params) => {
                self.begin_upload(&origin, params, reply_for(id, respond));
            }
            Method::PaneUploadWrite(params) => {
                let upload_id = params.upload_id.clone();
                let job = WriteJob {
                    path: params.path,
                    entry: params.entry,
                    offset: params.offset,
                    data: params.data,
                    last: params.last,
                };
                let reply = reply_for(id, respond);
                self.queue_upload_job(&upload_id, owner, UploadJob::Write(job, reply));
            }
            Method::PaneUploadFinish(params) => {
                let reply = reply_for(id, respond);
                self.queue_upload_job(
                    &params.upload_id,
                    owner,
                    UploadJob::Finish(params.collision, reply),
                );
            }
            Method::PaneUploadCancel(params) => {
                let result = self
                    .upload_sessions
                    .cancel(&params.upload_id, owner)
                    .map(|()| ResponseResult::Ok {});
                reply_for(id, respond)(result);
            }
            _ => return false,
        }
        true
    }

    fn queue_upload_job(&mut self, upload_id: &str, owner: UploadOwner, job: UploadJob) {
        let (target, environment) = match self.upload_sessions.get(upload_id, owner) {
            Ok(handle) => (handle.target.clone(), handle.environment.clone()),
            Err(failure) => {
                job.refuse(failure);
                return;
            }
        };
        let stale = self.upload_target_changed(&target, &environment);
        if stale {
            self.upload_sessions.abort_stale(upload_id);
            job.refuse((
                "stale_pane_target",
                "the upload's pane closed or now runs somewhere else".into(),
            ));
            return;
        }
        self.upload_sessions.send(upload_id, owner, job);
    }

    /// Whether the target is gone or its shell moved since begin.
    pub(super) fn upload_target_changed(
        &self,
        target: &UploadTargetKey,
        began: &UploadEnvironment,
    ) -> bool {
        let current = if let Some(public) = &target.public_pane {
            let Some((ws_idx, pane)) = self.parse_pane_id(public) else {
                return true;
            };
            if !self.pane_is_live(ws_idx, pane) {
                return true;
            }
            environment_of(running_pane_off_host_shell(self, ws_idx, pane))
        } else if let Some(terminal) = &target.terminal {
            let Some(runtime) = self.terminal_runtimes.get(terminal) else {
                return true;
            };
            environment_of(running_off_host_shell(self, runtime, terminal))
        } else {
            UploadEnvironment::Host
        };
        current != *began
    }

    fn begin_upload(&mut self, origin: &UploadOrigin, params: PaneUploadBeginParams, reply: Reply) {
        let owner = origin.owner();
        let prepared = Session::new(Declared {
            names: params.names.clone(),
            entries: params.entries,
            bytes: params.bytes,
        })
        .and_then(|session| self.upload_sessions.admit(owner).map(|()| session))
        .and_then(|session| {
            self.resolve_upload_target(origin, &params)
                .map(|target| (session, target))
        });
        let (session, target) = match prepared {
            Ok(prepared) => prepared,
            Err(failure) => return reply(Err(failure)),
        };
        let id = self.upload_sessions.next_id();
        let control = Arc::new(Control::default());
        let token = crate::upload::stage_token();
        let (location, sink): (PaneLocation, Box<dyn UploadSink>) =
            match (&target.environment, target.destination) {
                (UploadEnvironment::OffHost(env), Destination::Remote(destination)) => (
                    env.location(),
                    Box::new(RemoteSink::new(
                        Box::new(EnvironmentLauncher::system(
                            PaneEnvironment::clone(env),
                            RECEIVER_SCRIPT,
                            Transfer::UPLOAD,
                        )),
                        destination,
                        token,
                        control.clone(),
                        RemoteTimeouts::default(),
                    )),
                ),
                (_, Destination::Host(destination)) => {
                    (host_location(), Box::new(HostSink::new(destination, token)))
                }
                (UploadEnvironment::Host, Destination::Remote(_)) => {
                    return reply(Err(invalid("a host target needs a host destination")))
                }
            };
        let (jobs, rx) = std::sync::mpsc::sync_channel(UPLOAD_WINDOW as usize + 1);
        // The queue is empty, so the first job always fits.
        let _ = jobs.try_send(UploadJob::Begin(reply));
        let worker = Worker {
            id: id.clone(),
            session,
            sink,
            control: control.clone(),
            location,
            jobs: rx,
            timeouts: Timeouts::default(),
        };
        if let Err((worker, error)) =
            crate::thread_spawn::spawn_named_with("herdr-upload", worker, Worker::run)
        {
            tracing::warn!(%error, "failed to spawn an upload worker");
            if let Ok(job) = worker.jobs.try_recv() {
                job.refuse(crate::upload::sink::upload_failed(format!(
                    "could not start the upload: {error}"
                )));
            }
            return;
        }
        self.upload_sessions.insert(
            id,
            UploadHandle {
                owner,
                target: target.key,
                environment: target.environment,
                jobs,
                control,
            },
        );
    }

    fn resolve_upload_target(
        &self,
        origin: &UploadOrigin,
        params: &PaneUploadBeginParams,
    ) -> Result<ResolvedTarget, Failure> {
        if let UploadOrigin::AttachedClient { terminal_id, .. } = origin {
            // Why: a direct attach holds one terminal and nothing else.
            if params.pane_id.is_some() || params.terminal_id.as_ref() != Some(terminal_id) {
                return Err(invalid(
                    "a direct attach can only upload to its own terminal",
                ));
            }
        }
        match (&params.pane_id, &params.terminal_id) {
            (Some(_), Some(_)) => Err(invalid("pass pane_id or terminal_id, not both")),
            (Some(pane_id), None) => self.resolve_upload_pane(pane_id, params),
            (None, Some(terminal_id)) => self.resolve_upload_terminal(origin, terminal_id, params),
            (None, None) => {
                let destination = params
                    .destination
                    .as_deref()
                    .ok_or_else(|| invalid("destination is required without a pane or terminal"))?;
                Ok(ResolvedTarget {
                    key: UploadTargetKey::default(),
                    environment: UploadEnvironment::Host,
                    destination: Destination::Host(validate_host_destination(destination)?),
                })
            }
        }
    }

    fn resolve_upload_pane(
        &self,
        pane_id: &str,
        params: &PaneUploadBeginParams,
    ) -> Result<ResolvedTarget, Failure> {
        let (ws_idx, pane) = self
            .parse_pane_id(pane_id)
            .ok_or_else(|| ("pane_not_found", format!("pane not found: {pane_id}")))?;
        if !self.pane_is_live(ws_idx, pane) {
            return Err((
                "pane_not_live",
                format!("pane {pane_id} has no running shell"),
            ));
        }
        let environment = environment_of(running_pane_off_host_shell(self, ws_idx, pane));
        let workspace = self.state.workspaces.get(ws_idx);
        let terminal = workspace
            .and_then(|workspace| workspace.terminal_id(pane))
            .cloned();
        let key = UploadTargetKey {
            pane: Some(pane),
            terminal: terminal.clone(),
            public_pane: Some(pane_id.to_owned()),
        };
        let runtime_cwd = self
            .state
            .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane)
            .and_then(crate::terminal::TerminalRuntime::cwd);
        let destination = match &environment {
            UploadEnvironment::OffHost(env) => {
                ensure_offhost_supported()?;
                let reported = runtime_cwd.and_then(|cwd| cwd.to_str().map(str::to_owned));
                Destination::Remote(remote_destination(env, params, reported)?)
            }
            UploadEnvironment::Host => {
                let launch_cwd = terminal
                    .as_ref()
                    .and_then(|terminal| self.state.terminals.get(terminal))
                    .map(|state| state.cwd.clone());
                let cwd = crate::terminal::inherited_cwd(false, runtime_cwd, launch_cwd);
                Destination::Host(host_destination(params, cwd)?)
            }
        };
        Ok(ResolvedTarget {
            key,
            environment,
            destination,
        })
    }

    fn resolve_upload_terminal(
        &self,
        origin: &UploadOrigin,
        terminal_id: &str,
        params: &PaneUploadBeginParams,
    ) -> Result<ResolvedTarget, Failure> {
        let not_found = || {
            (
                "pane_not_found",
                format!("terminal not found: {terminal_id}"),
            )
        };
        let (terminal, state) = self
            .state
            .terminals
            .get_key_value(terminal_id)
            .ok_or_else(not_found)?;
        let popup = self
            .state
            .popup_pane
            .as_ref()
            .filter(|popup| popup.terminal_id == *terminal);
        if matches!(origin, UploadOrigin::ShellClient(_)) && popup.is_none() {
            // Why: workspace panes are addressed by pane_id, so a shell client
            // naming a terminal means its popup, and only the current one.
            return Err(("stale_pane_target", "the popup is no longer open".into()));
        }
        let runtime = self.terminal_runtimes.get(terminal).ok_or_else(|| {
            (
                "pane_not_live",
                format!("terminal {terminal_id} has no running shell"),
            )
        })?;
        let environment = environment_of(running_off_host_shell(self, runtime, terminal));
        let key = UploadTargetKey {
            pane: popup.map(|popup| popup.pane_id),
            terminal: Some(TerminalId::clone(terminal)),
            public_pane: None,
        };
        let reported = runtime
            .cwd()
            .and_then(|cwd| cwd.to_str().map(str::to_owned));
        let destination = match &environment {
            UploadEnvironment::OffHost(env) => {
                ensure_offhost_supported()?;
                Destination::Remote(remote_destination(env, params, reported)?)
            }
            UploadEnvironment::Host => {
                let cwd =
                    crate::terminal::inherited_cwd(false, runtime.cwd(), Some(state.cwd.clone()));
                Destination::Host(host_destination(params, cwd)?)
            }
        };
        Ok(ResolvedTarget {
            key,
            environment,
            destination,
        })
    }
}

/// Q6 off-host: the requested directory, else where the shell is (OSC 7, then
/// a container's working directory), else the login directory. Why not
/// `PaneTarget.root`'s `/` fallback: the spec says `$HOME`.
fn remote_destination(
    env: &PaneEnvironment,
    params: &PaneUploadBeginParams,
    reported: Option<String>,
) -> Result<String, Failure> {
    if let Some(destination) = &params.destination {
        return validate_remote_destination(destination);
    }
    let cwd = PaneTarget::resolve(env.clone(), reported)
        .cwd
        .unwrap_or_default();
    crate::upload::sink::check_resolved_destination(&cwd)?;
    Ok(cwd)
}

/// Q6 on this host: the requested directory, else the shell's, else home.
fn host_destination(
    params: &PaneUploadBeginParams,
    cwd: Option<PathBuf>,
) -> Result<PathBuf, Failure> {
    if let Some(destination) = &params.destination {
        return validate_host_destination(destination);
    }
    let cwd = cwd.or_else(std::env::home_dir).ok_or_else(|| {
        invalid("no destination: the pane has no working directory and there is no home directory")
    })?;
    crate::upload::sink::check_resolved_destination(&cwd.to_string_lossy())?;
    Ok(cwd)
}

#[cfg(all(test, unix))]
mod tests;
