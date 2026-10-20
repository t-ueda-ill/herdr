//! `directory.list` and `directory.index`: the file viewer's host filesystem reads.
//!
//! The app thread only resolves the root from pane state; the filesystem and Git
//! work runs on a worker that answers the request itself, so neither blocks the
//! event loop and no state has to be reconciled afterwards.

mod git;
mod listing;

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use crate::api::schema::{Method, Request, ResponseResult};
use crate::app::App;

use super::responses::{encode_error, encode_success};
use git::GitCli;
pub(super) use listing::requested_path;
use listing::Failure;

/// Shared by every Git call of one request.
const GIT_DEADLINE: Duration = Duration::from_secs(5);
/// Concurrent directory workers. Separate from the worktree pool so a slow
/// index cannot starve worktree discovery, or the reverse.
pub(crate) const DIRECTORY_READ_SLOTS: usize = 8;

enum DirectoryRead {
    List,
    Index,
}

/// Where a directory request starts, resolved on the app thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DirectoryRoot {
    pub(super) path: PathBuf,
    /// Public id of the live pane whose directory `path` is; `None` after
    /// the fallback to the new-workspace directory.
    pub(super) pane_id: Option<String>,
}

/// Captured on the app thread; the worker needs nothing else.
struct DirectoryJob {
    id: String,
    read: DirectoryRead,
    root: DirectoryRoot,
    path: Option<PathBuf>,
    trust_repository: bool,
    respond_to: Sender<String>,
    _permit: tokio::sync::OwnedSemaphorePermit,
}

impl DirectoryJob {
    fn run(&self) -> String {
        let git = GitCli {
            program: Path::new("git"),
            trust_repository: self.trust_repository,
            deadline: Instant::now() + GIT_DEADLINE,
        };
        let path = self.path.as_deref().unwrap_or(&self.root.path);
        let result = match self.read {
            DirectoryRead::List => self.list(path, &git),
            DirectoryRead::Index => Self::index(path, &git),
        };
        match result {
            Ok(result) => encode_success(self.id.clone(), result),
            Err((code, message)) => encode_error(self.id.clone(), code, message),
        }
    }

    fn list(&self, path: &Path, git: &GitCli<'_>) -> Result<ResponseResult, Failure> {
        let listing = listing::list_directory(path, git, listing::MAX_LIST_ENTRIES)?;
        let nonzero = |count: usize| u64::try_from(count).ok().filter(|count| *count > 0);
        Ok(ResponseResult::DirectoryList {
            root: crate::platform::file_path_display(&self.root.path),
            path: crate::platform::file_path_display(path),
            canonical: crate::platform::file_path_display(&listing.canonical),
            entries: listing.entries,
            truncated: listing.truncated,
            git_error: listing.git_error,
            pane_id: self.root.pane_id.clone(),
            omitted: nonzero(listing.omitted),
            unlistable: nonzero(listing.unlistable),
        })
    }

    fn index(path: &Path, git: &GitCli<'_>) -> Result<ResponseResult, Failure> {
        let index = listing::index_repository(path, git, listing::MAX_INDEX_ENTRIES)?;
        Ok(ResponseResult::DirectoryIndex {
            repo_root: index
                .repo_root
                .map(|root| crate::platform::file_path_display(&root)),
            entries: index.entries,
            truncated: index.truncated,
            git_error: index.git_error,
        })
    }
}

fn ensure_directory_platform_supported() -> Result<(), Failure> {
    if !crate::platform::FILE_VIEWER_SUPPORTED {
        return Err((
            "platform_unsupported",
            "the file viewer is not supported on Windows hosts yet".into(),
        ));
    }
    Ok(())
}

impl App {
    /// Starts `directory.list` or `directory.index` on a worker. Returns false
    /// for any other method so the caller can keep dispatching it.
    pub(crate) fn handle_deferred_directory_api_request(
        &mut self,
        request: Request,
        respond_to: Sender<String>,
    ) -> bool {
        let Request { id, method } = request;
        let (read, workspace_id, pane_id, path, trust_repository) = match method {
            Method::DirectoryList(params) => (
                DirectoryRead::List,
                params.workspace_id,
                params.pane_id,
                params.path,
                params.trust_repository,
            ),
            Method::DirectoryIndex(params) => (
                DirectoryRead::Index,
                params.workspace_id,
                params.pane_id,
                params.path,
                params.trust_repository,
            ),
            _ => return false,
        };
        let captured = ensure_directory_platform_supported()
            .and_then(|()| self.directory_root(workspace_id.as_deref(), pane_id.as_deref()))
            .and_then(|root| {
                let path = path.as_deref().map(listing::requested_path).transpose()?;
                Ok((root, path))
            });
        let (root, path) = match captured {
            Ok(captured) => captured,
            Err((code, message)) => {
                let _ = respond_to.send(encode_error(id, code, message));
                return true;
            }
        };
        let Ok(permit) = self.directory_read_slots.clone().try_acquire_owned() else {
            let _ = respond_to.send(encode_error(
                id,
                "directory_busy",
                "too many directory reads are pending; retry shortly",
            ));
            return true;
        };
        let job = DirectoryJob {
            id,
            read,
            root,
            path,
            trust_repository,
            respond_to,
            _permit: permit,
        };
        let spawned = crate::thread_spawn::spawn_named_with("herdr-directory-read", job, |job| {
            let response = job.run();
            let _ = job.respond_to.send(response);
        });
        if let Err((job, err)) = spawned {
            tracing::warn!(err = %err, "failed to spawn directory read thread");
            let _ = job.respond_to.send(encode_error(
                job.id.clone(),
                "directory_read_failed",
                format!("could not start the directory read: {err}"),
            ));
        }
        true
    }

    /// The directory a new tab would inherit from the target pane, which is
    /// where the user expects "here" to be. Falls back to the new-workspace
    /// directory when there is no live pane to ask.
    fn directory_root(
        &self,
        workspace_id: Option<&str>,
        pane_id: Option<&str>,
    ) -> Result<DirectoryRoot, Failure> {
        let (ws_idx, pane) = self.directory_target(workspace_id, pane_id)?;
        let Some(ws_idx) = ws_idx else {
            return Ok(DirectoryRoot {
                path: self.resolve_new_terminal_cwd(None),
                pane_id: None,
            });
        };
        let live = pane
            .filter(|&pane| self.pane_is_live(ws_idx, pane))
            .and_then(|pane| {
                Some(DirectoryRoot {
                    path: self.launch_cwd_for_pane_in_workspace(ws_idx, pane)?,
                    pane_id: Some(self.public_pane_id(ws_idx, pane)?),
                })
            });
        Ok(live.unwrap_or_else(|| DirectoryRoot {
            path: self.resolved_new_workspace_cwd_from(ws_idx),
            pane_id: None,
        }))
    }

    /// Resolves the optional `workspace_id`/`pane_id` pair the file viewer
    /// methods share: an explicit pane wins, otherwise the focused pane of the
    /// named or focused workspace. `(None, None)` means there is no workspace.
    pub(super) fn directory_target(
        &self,
        workspace_id: Option<&str>,
        pane_id: Option<&str>,
    ) -> Result<(Option<usize>, Option<crate::layout::PaneId>), Failure> {
        let workspace = workspace_id
            .map(|workspace_id| {
                self.parse_workspace_id(workspace_id).ok_or_else(|| {
                    (
                        "workspace_not_found",
                        format!("workspace {workspace_id} not found"),
                    )
                })
            })
            .transpose()?;
        if let Some(pane_id) = pane_id {
            let (ws_idx, pane) = self
                .parse_pane_id(pane_id)
                .ok_or_else(|| ("pane_not_found", format!("pane not found: {pane_id}")))?;
            if workspace.is_some_and(|workspace| workspace != ws_idx) {
                return Err((
                    "invalid_params",
                    format!(
                        "pane {pane_id} is not in workspace {}",
                        workspace_id.unwrap_or("")
                    ),
                ));
            }
            return Ok((Some(ws_idx), Some(pane)));
        }
        let ws_idx = workspace.or_else(|| {
            self.state.active.or_else(|| {
                self.state
                    .workspaces
                    .get(self.state.selected)
                    .map(|_| self.state.selected)
            })
        });
        let pane = ws_idx
            .and_then(|ws_idx| self.state.workspaces.get(ws_idx))
            .and_then(crate::workspace::Workspace::focused_pane_id);
        Ok((ws_idx, pane))
    }

    /// A pane whose shell exited or failed to restore has no meaningful
    /// working directory of its own and cannot take input.
    pub(super) fn pane_is_live(&self, ws_idx: usize, pane_id: crate::layout::PaneId) -> bool {
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.terminal_id(pane_id))
        else {
            return false;
        };
        self.state
            .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)
            .is_some()
            && self
                .state
                .terminals
                .get(terminal_id)
                .is_some_and(|terminal| terminal.restore_error.is_none())
    }
}

// The tests drive `/bin/sh`, Git, and Unix paths.
#[cfg(all(test, unix))]
mod tests;
