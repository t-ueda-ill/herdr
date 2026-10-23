//! `pane.download.*`: streams files out of the environment a pane's shell sees
//! (or the server's own filesystem) to a client that pulls them with ordered
//! reads. The app thread only validates, resolves the source from what it
//! already holds, and queues jobs; each session's worker does the waiting.

use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::Arc;

use crate::api::schema::{Method, PaneDownloadBeginParams, PaneLocation, Request, ResponseResult};
use crate::app::App;
use crate::download::frame::Limits;
use crate::download::source::DownloadSource;
use crate::download::source_host::{HostOptions, HostSource};
use crate::download::source_remote::{RemoteOptions, RemoteSource};
use crate::download::worker::{DownloadJob, Setup, Timeouts, Worker};
use crate::download::{sender_script::SENDER_SCRIPT, DownloadHandle, DOWNLOAD_CHUNK_BYTES};
use crate::download::{DOWNLOAD_WINDOW, FRAME_QUEUE};
use crate::offhost::launcher::{EnvironmentLauncher, Transfer};
use crate::offhost::ops;
use crate::server::client_shell::running_pane_off_host_shell;
use crate::upload::path::{
    validate_component, validate_names, Failure, MAX_NAMES, MAX_NAMES_TOTAL_BYTES, MAX_PATH_BYTES,
};
use crate::upload::registry::{UploadEnvironment, UploadOwner, UploadTargetKey};
use crate::upload::worker::{Control, Reply};

use super::pane_upload::{
    ensure_offhost_supported, environment_of, host_location, invalid, reply_for, UploadResponder,
};

/// Who sent a request, which decides who owns the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DownloadOrigin {
    Api,
    /// A client-shell endpoint, over its control messages or its command lane.
    ShellClient(u64),
}

impl DownloadOrigin {
    fn owner(&self) -> UploadOwner {
        match self {
            Self::Api => UploadOwner::Api,
            Self::ShellClient(client_id) => UploadOwner::Client(*client_id),
        }
    }
}

/// Where the selected paths live.
struct ResolvedSource {
    key: UploadTargetKey,
    environment: UploadEnvironment,
}

/// The selection after validation: what to read and what each item is called.
struct Selection {
    /// Last components, in the order of the paths.
    names: Vec<String>,
}

fn check_common(params: &PaneDownloadBeginParams) -> Result<(), Failure> {
    if params.paths.is_empty() || params.paths.len() > MAX_NAMES {
        return Err(invalid(format!(
            "paths must hold 1 to {MAX_NAMES} entries, not {}",
            params.paths.len()
        )));
    }
    let mut total = 0usize;
    for path in &params.paths {
        if path.len() > MAX_PATH_BYTES {
            return Err(invalid(format!(
                "a path is longer than {MAX_PATH_BYTES} bytes"
            )));
        }
        if path.chars().any(char::is_control) {
            return Err(invalid("paths must not contain control characters"));
        }
        total += path.len();
    }
    if total > MAX_NAMES_TOTAL_BYTES {
        return Err(invalid(format!(
            "paths must total at most {MAX_NAMES_TOTAL_BYTES} bytes"
        )));
    }
    if params.max_bytes == Some(0) {
        return Err(invalid("max_bytes must be at least 1"));
    }
    Ok(())
}

/// The names the items will have, refusing two items that would share one.
fn names_of(
    paths: &[String],
    name_of: impl Fn(&str) -> Option<String>,
) -> Result<Vec<String>, Failure> {
    let mut names = Vec::with_capacity(paths.len());
    for path in paths {
        let name = name_of(path).ok_or_else(|| invalid(format!("{path:?} has no usable name")))?;
        validate_component(&name, "name")?;
        if names.contains(&name) {
            return Err(invalid(format!("two selected items are both named {name}")));
        }
        names.push(name);
    }
    validate_names(&names)?;
    Ok(names)
}

fn host_selection(paths: &[String]) -> Result<Selection, Failure> {
    for path in paths {
        let parsed = Path::new(path);
        if !parsed.is_absolute() || parsed.components().any(|part| part == Component::ParentDir) {
            return Err(invalid(format!(
                "paths must be absolute and without `..`: {path:?}"
            )));
        }
    }
    let names = names_of(paths, |path| {
        Path::new(path)
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .map(str::to_owned)
    })?;
    Ok(Selection { names })
}

/// Why the shape is strict: the sender script splits each path at its last
/// `/`, which a trailing slash or the root would leave without a name.
fn remote_selection(paths: &[String]) -> Result<Selection, Failure> {
    for path in paths {
        ops::validate_path(path)?;
        if path.len() < 2 || path.ends_with('/') {
            return Err(invalid(format!(
                "a path must name an item, not end in `/`: {path:?}"
            )));
        }
    }
    let names = names_of(paths, |path| path.rsplit('/').next().map(str::to_owned))?;
    Ok(Selection { names })
}

/// The trailing number of a session id, for thread names.
fn session_number(id: &str) -> &str {
    id.rsplit('-').next().unwrap_or(id)
}

impl App {
    /// Starts a `pane.download.*` request from the API socket. Returns false
    /// for any other method so the caller can keep dispatching it.
    pub(crate) fn handle_deferred_pane_download_api_request(
        &mut self,
        request: Request,
        respond_to: Sender<String>,
    ) -> bool {
        self.handle_pane_download_request(
            DownloadOrigin::Api,
            request,
            Box::new(move |response| {
                let _ = respond_to.send(response);
            }),
        )
    }

    pub(crate) fn handle_pane_download_request(
        &mut self,
        origin: DownloadOrigin,
        request: Request,
        respond: UploadResponder,
    ) -> bool {
        let Request { id, method } = request;
        let owner = origin.owner();
        match method {
            Method::PaneDownloadBegin(params) => {
                self.begin_download(owner, params, reply_for(id, respond));
            }
            Method::PaneDownloadRead(params) => {
                let reply = reply_for(id, respond);
                self.queue_download_job(
                    &params.download_id,
                    owner,
                    DownloadJob::Read {
                        seq: params.seq,
                        reply,
                    },
                );
            }
            Method::PaneDownloadCancel(params) => {
                let result = self
                    .download_sessions
                    .cancel(&params.download_id, owner)
                    .map(|()| ResponseResult::Ok {});
                reply_for(id, respond)(result);
            }
            _ => return false,
        }
        true
    }

    fn queue_download_job(&mut self, download_id: &str, owner: UploadOwner, job: DownloadJob) {
        let (target, environment) = match self.download_sessions.get(download_id, owner) {
            Ok(handle) => (handle.target.clone(), handle.environment.clone()),
            Err(failure) => {
                job.refuse(failure);
                return;
            }
        };
        if self.upload_target_changed(&target, &environment) {
            self.download_sessions.abort_stale(download_id);
            job.refuse((
                "stale_pane_target",
                "the download's pane closed or now runs somewhere else".into(),
            ));
            return;
        }
        self.download_sessions.send(download_id, owner, job);
    }

    fn begin_download(
        &mut self,
        owner: UploadOwner,
        params: PaneDownloadBeginParams,
        reply: Reply,
    ) {
        let prepared = check_common(&params)
            .and_then(|()| self.download_sessions.admit(owner))
            .and_then(|()| self.resolve_download_source(&params));
        let source = match prepared {
            Ok(source) => source,
            Err(failure) => return reply(Err(failure)),
        };
        let chunk = params.max_bytes.map_or(DOWNLOAD_CHUNK_BYTES, |bytes| {
            usize::try_from(bytes).map_or(DOWNLOAD_CHUNK_BYTES, |bytes| {
                bytes.min(DOWNLOAD_CHUNK_BYTES)
            })
        });
        let selection = match &source.environment {
            UploadEnvironment::Host => host_selection(&params.paths),
            UploadEnvironment::OffHost(_) => {
                ensure_offhost_supported().and_then(|()| remote_selection(&params.paths))
            }
        };
        let selection = match selection {
            Ok(selection) => selection,
            Err(failure) => return reply(Err(failure)),
        };
        let timeouts = Timeouts::default();
        let id = self.download_sessions.next_id();
        let number = session_number(&id).to_owned();
        let (location, source_impl): (PaneLocation, Box<dyn DownloadSource>) =
            match &source.environment {
                UploadEnvironment::Host => (
                    host_location(),
                    Box::new(HostSource::new(
                        params.paths.iter().map(PathBuf::from).collect(),
                        HostOptions {
                            stall: timeouts.stall,
                            tick: timeouts.tick,
                            walk: timeouts.walk,
                            queue: FRAME_QUEUE,
                            thread_name: format!("herdr-download-{number}-host"),
                            ..HostOptions::default()
                        },
                    )),
                ),
                UploadEnvironment::OffHost(env) => (
                    env.location(),
                    Box::new(RemoteSource::new(
                        Box::new(EnvironmentLauncher::system(
                            crate::offhost::PaneEnvironment::clone(env),
                            SENDER_SCRIPT,
                            Transfer::DOWNLOAD,
                        )),
                        params.paths.clone(),
                        RemoteOptions {
                            stall: timeouts.stall,
                            tick: timeouts.tick,
                            walk: timeouts.walk,
                            grace: timeouts.grace,
                            queue: FRAME_QUEUE,
                            thread_name: format!("herdr-download-{number}-out"),
                            ..RemoteOptions::default()
                        },
                    )),
                ),
            };
        let control = Arc::new(Control::default());
        let (jobs, rx) = std::sync::mpsc::sync_channel(DOWNLOAD_WINDOW as usize + 2);
        // The queue is empty, so the first job always fits.
        let _ = jobs.try_send(DownloadJob::Begin(reply));
        let worker = Worker::new(Setup {
            id: id.clone(),
            source: source_impl,
            names: selection.names,
            limits: Limits::new(chunk),
            control: control.clone(),
            location,
            jobs: rx,
            timeouts,
        });
        if let Err((worker, error)) = crate::thread_spawn::spawn_named_with(
            &format!("herdr-download-{number}"),
            worker,
            Worker::run,
        ) {
            tracing::warn!(%error, "failed to spawn a download worker");
            worker.refuse_begin((
                "download_failed",
                format!("could not start the download: {error}"),
            ));
            return;
        }
        self.download_sessions.insert(
            id,
            DownloadHandle {
                owner,
                target: source.key,
                environment: source.environment,
                jobs,
                control,
            },
        );
    }

    /// Where the paths live: the server itself, or a pane's shell environment.
    fn resolve_download_source(
        &self,
        params: &PaneDownloadBeginParams,
    ) -> Result<ResolvedSource, Failure> {
        let Some(pane_id) = &params.pane_id else {
            return Ok(ResolvedSource {
                key: UploadTargetKey::default(),
                environment: UploadEnvironment::Host,
            });
        };
        let (ws_idx, pane) = self
            .parse_pane_id(pane_id)
            .ok_or_else(|| ("pane_not_found", format!("pane not found: {pane_id}")))?;
        if !self.pane_is_live(ws_idx, pane) {
            return Err((
                "pane_not_live",
                format!("pane {pane_id} has no running shell"),
            ));
        }
        Ok(ResolvedSource {
            key: UploadTargetKey {
                pane: Some(pane),
                terminal: None,
                public_pane: Some(pane_id.clone()),
            },
            environment: environment_of(running_pane_off_host_shell(self, ws_idx, pane)),
        })
    }
}

#[cfg(all(test, unix))]
mod tests;
