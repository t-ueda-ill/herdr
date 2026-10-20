//! `pane.directory.list`, `pane.directory.index`, `pane.file.read` and
//! `pane.directory.open_editor`: the file viewer for panes whose shell runs in
//! a container or over ssh.
//!
//! The app thread only resolves the target pane to a captured
//! [`PaneEnvironment`]; docker and ssh run on a worker that answers the request
//! itself, so a slow remote never blocks the event loop.

use std::sync::mpsc::Sender;
use std::time::Instant;

use crate::api::schema::{Method, PaneDirectoryOpenEditorParams, Request, ResponseResult};
use crate::app::App;
use crate::events::{AppEvent, PaneEditorProbeResult};
use crate::offhost::exec::{EnvironmentRunner, Programs};
use crate::offhost::{ops, PaneEnvironment, PaneTarget};
use crate::server::client_shell::{running_pane_off_host_shell, RunningOffHostShell};

use super::responses::{encode_error, encode_success};

type Failure = (&'static str, String);

enum PaneRead {
    List {
        path: Option<String>,
        trust_repository: bool,
    },
    Index {
        path: Option<String>,
        trust_repository: bool,
    },
    File {
        path: String,
        revision: Option<String>,
        poll: bool,
    },
}

impl PaneRead {
    fn poll(&self) -> bool {
        matches!(self, Self::File { poll: true, .. })
    }
}

struct PaneReadJob {
    id: String,
    read: PaneRead,
    target: PaneTarget,
    respond_to: Sender<String>,
    _permit: tokio::sync::OwnedSemaphorePermit,
}

impl PaneReadJob {
    fn run(&self) -> String {
        let deadline = Instant::now() + self.target.environment.deadline_budget(self.read.poll());
        let runner = EnvironmentRunner {
            programs: Programs::system(),
            env: &self.target.environment,
            deadline,
        };
        let location = self.target.environment.location();
        let root = self.target.root.as_deref();
        let result = match &self.read {
            PaneRead::List {
                path,
                trust_repository,
            } => ops::list(&runner, location, root, path.as_deref(), *trust_repository)
                .map(ResponseResult::PaneDirectoryList),
            PaneRead::Index {
                path,
                trust_repository,
            } => ops::index(&runner, location, root, path.as_deref(), *trust_repository)
                .map(ResponseResult::PaneDirectoryIndex),
            PaneRead::File { path, revision, .. } => {
                ops::read(&runner, location, path, revision.as_deref())
                    .map(ResponseResult::PaneFileDocument)
            }
        };
        match result {
            Ok(result) => encode_success(self.id.clone(), result),
            Err((code, message)) => encode_error(self.id.clone(), code, message),
        }
    }
}

fn ensure_platform_supported() -> Result<(), Failure> {
    if !crate::platform::OFFHOST_FILE_VIEWER_SUPPORTED {
        return Err((
            "platform_unsupported",
            "ssh and container panes are not supported on this host".into(),
        ));
    }
    Ok(())
}

impl App {
    /// Starts a `pane.*` off-host read on a worker. Returns false for any other
    /// method so the caller can keep dispatching it.
    pub(crate) fn handle_deferred_pane_directory_api_request(
        &mut self,
        request: Request,
        respond_to: Sender<String>,
    ) -> bool {
        let Request { id, method } = request;
        match method {
            Method::PaneDirectoryList(params) => {
                self.start_pane_read(
                    id,
                    &params.pane_id,
                    PaneRead::List {
                        path: params.path,
                        trust_repository: params.trust_repository,
                    },
                    respond_to,
                );
            }
            Method::PaneDirectoryIndex(params) => {
                self.start_pane_read(
                    id,
                    &params.pane_id,
                    PaneRead::Index {
                        path: params.path,
                        trust_repository: params.trust_repository,
                    },
                    respond_to,
                );
            }
            Method::PaneFileRead(params) => {
                self.start_pane_read(
                    id,
                    &params.pane_id,
                    PaneRead::File {
                        path: params.path,
                        revision: params.revision,
                        poll: params.poll,
                    },
                    respond_to,
                );
            }
            Method::PaneDirectoryOpenEditor(params) => {
                self.start_pane_editor_probe(id, params, respond_to);
            }
            _ => return false,
        }
        true
    }

    fn start_pane_read(
        &mut self,
        id: String,
        pane_id: &str,
        read: PaneRead,
        respond_to: Sender<String>,
    ) {
        let captured = ensure_platform_supported()
            .and_then(|()| self.pane_target(pane_id))
            .and_then(|target| {
                // Rejected here as well as in the worker so a bad path never
                // takes a slot or a round trip.
                match &read {
                    PaneRead::List {
                        path: Some(path), ..
                    }
                    | PaneRead::Index {
                        path: Some(path), ..
                    }
                    | PaneRead::File { path, .. } => ops::validate_path(path)?,
                    _ => {}
                }
                Ok(target)
            });
        let target = match captured {
            Ok(target) => target,
            Err((code, message)) => {
                let _ = respond_to.send(encode_error(id, code, message));
                return;
            }
        };
        let Some(permit) = self.try_acquire_directory_slot(&id, &respond_to) else {
            return;
        };
        let job = PaneReadJob {
            id,
            read,
            target,
            respond_to,
            _permit: permit,
        };
        let spawned =
            crate::thread_spawn::spawn_named_with("herdr-pane-directory-read", job, |job| {
                let response = job.run();
                let _ = job.respond_to.send(response);
            });
        if let Err((job, err)) = spawned {
            tracing::warn!(err = %err, "failed to spawn pane directory read thread");
            let _ = job.respond_to.send(encode_error(
                job.id.clone(),
                "directory_read_failed",
                format!("could not start the directory read: {err}"),
            ));
        }
    }

    fn try_acquire_directory_slot(
        &self,
        id: &str,
        respond_to: &Sender<String>,
    ) -> Option<tokio::sync::OwnedSemaphorePermit> {
        match self.directory_read_slots.clone().try_acquire_owned() {
            Ok(permit) => Some(permit),
            Err(_) => {
                let _ = respond_to.send(encode_error(
                    id.to_owned(),
                    "directory_busy",
                    "too many directory reads are pending; retry shortly",
                ));
                None
            }
        }
    }

    /// The environment, root, and directory of a live off-host pane. Host panes
    /// (cold-restored ones included, since their shell runs on this host) are
    /// rejected: their files are `directory.*`'s.
    pub(crate) fn pane_target(&self, pane_id: &str) -> Result<PaneTarget, Failure> {
        let (ws_idx, pane) = self
            .parse_pane_id(pane_id)
            .ok_or_else(|| ("pane_not_found", format!("pane not found: {pane_id}")))?;
        if !self.pane_is_live(ws_idx, pane) {
            return Err((
                "pane_not_live",
                format!("pane {pane_id} has no running shell"),
            ));
        }
        let environment = match running_pane_off_host_shell(self, ws_idx, pane) {
            Some(RunningOffHostShell::Container(launch)) => {
                PaneEnvironment::Container(launch.clone())
            }
            Some(RunningOffHostShell::Ssh(launch)) => PaneEnvironment::Ssh(launch.clone()),
            None => {
                return Err((
                    "invalid_params",
                    format!("pane {pane_id} runs on this host; use directory.list and file.read"),
                ))
            }
        };
        let reported = self
            .state
            .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane)
            .and_then(crate::terminal::TerminalRuntime::cwd)
            .and_then(|cwd| cwd.to_str().map(str::to_owned));
        Ok(PaneTarget::resolve(environment, reported))
    }

    fn start_pane_editor_probe(
        &mut self,
        id: String,
        params: PaneDirectoryOpenEditorParams,
        respond_to: Sender<String>,
    ) {
        let captured = ensure_platform_supported()
            .and_then(|()| ops::validate_path(&params.path))
            .and_then(|()| self.editor_target(&params.pane_id));
        let target = match captured {
            Ok(target) => target,
            Err((code, message)) => {
                let _ = respond_to.send(encode_error(id, code, message));
                return;
            }
        };
        let Some(permit) = self.try_acquire_directory_slot(&id, &respond_to) else {
            return;
        };
        let event_tx = self.event_tx.clone();
        let job = EditorProbeJob {
            id,
            pane_id: params.pane_id,
            path: params.path,
            target,
            respond_to,
            _permit: permit,
        };
        let spawned =
            crate::thread_spawn::spawn_named_with("herdr-pane-editor-probe", job, move |job| {
                let deadline = Instant::now() + job.target.environment.deadline_budget(false);
                let runner = EnvironmentRunner {
                    programs: Programs::system(),
                    env: &job.target.environment,
                    deadline,
                };
                let probe = ops::probe_editable(&runner, &job.path);
                let _ = event_tx.blocking_send(AppEvent::PaneEditorProbed(Box::new(
                    PaneEditorProbeResult {
                        id: job.id,
                        pane_id: job.pane_id,
                        path: job.path,
                        environment: job.target.environment,
                        cwd: job.target.cwd,
                        probe,
                        respond_to: job.respond_to,
                    },
                )));
            });
        if let Err((job, err)) = spawned {
            tracing::warn!(err = %err, "failed to spawn pane editor probe thread");
            let _ = job.respond_to.send(encode_error(
                job.id.clone(),
                "editor_failed",
                format!("could not start the editor: {err}"),
            ));
        }
    }

    /// Like [`Self::pane_target`], and the pane must be the focused one: the
    /// overlay opens over the active workspace's focused pane, as for
    /// `pane.open_editor`.
    fn editor_target(&self, pane_id: &str) -> Result<PaneTarget, Failure> {
        let target = self.pane_target(pane_id)?;
        let (ws_idx, pane) = self
            .parse_pane_id(pane_id)
            .ok_or_else(|| ("pane_not_found", format!("pane not found: {pane_id}")))?;
        let is_focused = self.state.active == Some(ws_idx)
            && self
                .state
                .workspaces
                .get(ws_idx)
                .and_then(crate::workspace::Workspace::focused_pane_id)
                == Some(pane);
        if !is_focused {
            return Err(("stale_pane_target", "pane is no longer focused".to_owned()));
        }
        Ok(target)
    }

    /// Where the overlay's host process (docker or ssh) starts.
    /// Why not `cwd_for_pane`, the overlay default: for an off-host pane it is a
    /// path in the container or on the remote, which does not exist here.
    fn editor_host_cwd(&self, pane_id: &str) -> Option<std::path::PathBuf> {
        let (ws_idx, pane) = self.parse_pane_id(pane_id)?;
        self.state
            .workspaces
            .get(ws_idx)?
            .active_tab()?
            .host_cwd_for_pane(pane, &self.state.terminals, &self.terminal_runtimes)
    }

    /// Finishes `pane.directory.open_editor` once the probe is back.
    pub(crate) fn handle_pane_editor_probed(&mut self, result: PaneEditorProbeResult) -> bool {
        self.handle_pane_editor_probed_with(result, &Programs::system())
    }

    /// `programs` names the docker and ssh executables the overlay runs, so
    /// tests can substitute fakes without touching the process environment.
    fn handle_pane_editor_probed_with(
        &mut self,
        result: PaneEditorProbeResult,
        programs: &Programs<'_>,
    ) -> bool {
        let PaneEditorProbeResult {
            id,
            pane_id,
            path,
            environment,
            cwd,
            probe,
            respond_to,
        } = result;
        let response = probe.and_then(|()| {
            let target = self.editor_target(&pane_id)?;
            // The pane may have been replaced by one elsewhere while probing.
            if target.environment != environment {
                return Err((
                    "stale_pane_target",
                    "pane no longer runs in the probed environment".to_owned(),
                ));
            }
            let login = crate::pane::current_target_uses_login_shell(self.state.shell_mode);
            let mut argv =
                crate::offhost::editor::editor_argv(&environment, cwd.as_deref(), login, &path);
            // `editor_argv` always starts with the bare program name.
            if let Some(program) = argv.first_mut() {
                let replacement = match program.as_str() {
                    "docker" => Some(programs.docker),
                    "ssh" => Some(programs.ssh),
                    _ => None,
                };
                if let Some(path) = replacement.and_then(|path| path.to_str()) {
                    *program = path.to_owned();
                }
            }
            let host_cwd = self.editor_host_cwd(&pane_id);
            self.spawn_editor_overlay(&argv, host_cwd)
        });
        let spawned = response.is_ok();
        let _ = respond_to.send(match response {
            Ok(pane) => encode_success(id, ResponseResult::PaneInfo { pane }),
            Err((code, message)) => encode_error(id, code, message),
        });
        spawned
    }
}

struct EditorProbeJob {
    id: String,
    pane_id: String,
    path: String,
    target: PaneTarget,
    respond_to: Sender<String>,
    _permit: tokio::sync::OwnedSemaphorePermit,
}

// The tests start workers that spawn `/bin/sh`-style fakes and use Unix paths.
#[cfg(all(test, unix))]
mod tests;
