//! `preview.start`, `preview.list`, `preview.stop`: serve a folder from this
//! host or an ssh/container pane to a browser on loopback.
//!
//! The app thread only captures where the folder lives and binds the port;
//! reads, renders and change checks run on the preview's own threads.

use std::path::Path;
use std::time::{Duration, Instant};

use crate::api::schema::{PreviewInfo, PreviewStartParams, PreviewStopParams, ResponseResult};
use crate::app::App;
use crate::offhost::exec::{run_script_with, Programs};
use crate::offhost::PaneEnvironment;
use crate::preview::registry::{entry_relative, normalize_root};
use crate::preview::server::PreviewServer;
use crate::preview::source::{FileSource, LocalSource, OffHostSource};
use crate::server::client_shell::running_pane_off_host_shell;

use super::responses::{encode_error, encode_success};

type Failure = (&'static str, String);

/// How often an open page's folder is checked for changes. Off-host checks
/// cost a process in the environment, so they run less often.
const LOCAL_WATCH: Duration = Duration::from_millis(500);
const OFFHOST_WATCH: Duration = Duration::from_secs(1);

fn offhost_source(root: String, environment: PaneEnvironment) -> OffHostSource {
    let run = move |script: &str, args: &[&str]| {
        let deadline = Instant::now() + environment.deadline_budget(false);
        run_script_with(&Programs::system(), &environment, script, args, deadline)
            .map_err(|err| err.failure().1)
    };
    OffHostSource::new(root, Box::new(run))
}

impl App {
    pub(crate) fn handle_preview_start(
        &mut self,
        id: String,
        params: PreviewStartParams,
    ) -> String {
        match self.start_preview(params) {
            Ok(preview) => encode_success(id, ResponseResult::PreviewStarted { preview }),
            Err((code, message)) => encode_error(id, code, message),
        }
    }

    pub(crate) fn handle_preview_list(&mut self, id: String) -> String {
        let previews = self.previews.list();
        encode_success(id, ResponseResult::PreviewList { previews })
    }

    pub(crate) fn handle_preview_stop(&mut self, id: String, params: PreviewStopParams) -> String {
        if self.previews.stop(&params.preview_id) {
            encode_success(
                id,
                ResponseResult::PreviewStopped {
                    preview_id: params.preview_id,
                },
            )
        } else {
            encode_error(
                id,
                "preview_not_found",
                format!("no running preview {}", params.preview_id),
            )
        }
    }

    /// `None` for this host: no pane, or a pane whose shell runs here.
    fn preview_environment(
        &self,
        pane_id: Option<&str>,
    ) -> Result<Option<PaneEnvironment>, Failure> {
        let Some(pane_id) = pane_id else {
            return Ok(None);
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
        let environment = match running_pane_off_host_shell(self, ws_idx, pane) {
            Some(crate::server::client_shell::RunningOffHostShell::Container(launch)) => {
                PaneEnvironment::Container(Box::new(launch.clone()))
            }
            Some(crate::server::client_shell::RunningOffHostShell::Ssh(launch)) => {
                PaneEnvironment::Ssh(launch.clone())
            }
            None => return Ok(None),
        };
        if !crate::platform::OFFHOST_FILE_VIEWER_SUPPORTED {
            return Err((
                "platform_unsupported",
                "ssh and container panes are not supported on this host".into(),
            ));
        }
        Ok(Some(environment))
    }

    fn start_preview(&mut self, params: PreviewStartParams) -> Result<PreviewInfo, Failure> {
        let environment = self.preview_environment(params.pane_id.as_deref())?;
        let invalid = |message: String| ("invalid_params", message);
        let root = match &environment {
            None => {
                if !Path::new(&params.root).is_absolute() {
                    return Err(invalid(format!("root must be absolute: {:?}", params.root)));
                }
                std::fs::canonicalize(&params.root)
                    .map_err(|err| ("not_found", format!("{}: {err}", params.root)))?
                    .to_string_lossy()
                    .into_owned()
            }
            Some(_) => {
                crate::offhost::ops::validate_path(&params.root)?;
                normalize_root(&params.root)
            }
        };
        let entry = entry_relative(
            &root,
            params
                .entry
                .as_deref()
                .map(|entry| match &environment {
                    // The host root is canonical, so the entry must be too.
                    None => std::fs::canonicalize(entry)
                        .map(|path| path.to_string_lossy().into_owned())
                        .unwrap_or_else(|_| entry.to_owned()),
                    Some(_) => entry.to_owned(),
                })
                .as_deref(),
        )
        .map_err(invalid)?;
        let location = environment.as_ref().map(PaneEnvironment::location);
        if let Some(running) = self.previews.find(&root, location.as_ref(), &entry) {
            return Ok(running);
        }
        let (source, watch_every): (Box<dyn FileSource>, Duration) = match environment {
            None => {
                let source = LocalSource::new(Path::new(&root))
                    .map_err(|err| ("invalid_params", format!("{root}: {err}")))?;
                (Box::new(source), LOCAL_WATCH)
            }
            Some(environment) => (
                Box::new(offhost_source(root.clone(), environment)),
                OFFHOST_WATCH,
            ),
        };
        let server = PreviewServer::start(source, watch_every).map_err(|err| {
            (
                "preview_failed",
                format!("could not start the preview: {err}"),
            )
        })?;
        Ok(self.previews.insert(&root, location, server, &entry))
    }
}

#[cfg(all(test, unix))]
mod tests;
