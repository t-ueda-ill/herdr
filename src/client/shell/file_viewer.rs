//! File viewer overlay: browse the focused pane's directory and act on a path.
//!
//! Listings and the repository index come from the endpoint
//! (`directory.list`, `directory.index`), because a remote client cannot read
//! the host filesystem. Expansion, toggles and search are client state only.

mod fuzzy;
pub(super) mod model;

use model::{
    git_error_notice, host_path_contains, indexed_paths, join_host_path, shell_quote_path,
    DirectoryListing, DirectoryState, FileIndex, FileViewerOptions, CONTROL_CHARACTERS_IN_PATH,
    EDIT_FILES_ONLY, NO_LIVE_PANE, PANE_NOT_FOCUSED,
};
pub(super) use model::{location_label, FileLocation};
pub(super) use model::{ClientFileViewerOverlay, FileViewerButton};

use super::*;
use crate::api::schema::{
    DirectoryIndexParams, DirectoryListParams, FileReadParams, Method, PaneDirectoryIndexParams,
    PaneDirectoryListParams, PaneDirectoryOpenEditorParams, PaneFileReadParams,
    PaneOpenEditorParams, ResponseResult, WorkspaceTarget,
};

const DOUBLE_CLICK: std::time::Duration = std::time::Duration::from_millis(350);
const WHEEL_ROWS: isize = 3;

/// What an endpoint request of the file viewer was for.
#[derive(Debug)]
pub(super) enum FileViewerRequest {
    /// `path` is `None` for the first listing, whose answer names the root.
    List {
        generation: u64,
        path: Option<String>,
    },
    Index {
        generation: u64,
    },
    OpenEditor {
        instance: u64,
    },
}

impl FileViewerRequest {
    /// Failures the viewer shows on its own lines instead of as endpoint
    /// toasts. Connection trouble and a root that cannot be listed at all
    /// still raise the usual notice.
    pub(super) fn reports_errors_inline(&self, code: &str) -> bool {
        !matches!(
            code,
            "endpoint_timeout" | "endpoint_cancelled" | "server_unavailable"
        ) && !matches!(self, Self::List { path: None, .. })
    }
}

#[derive(Clone, Copy, Debug)]
enum FileViewerAction {
    Enter,
    Keep,
    Edit,
    Copy,
}

impl ClientShellState {
    pub(super) fn open_file_viewer(&mut self, outcome: &mut ClientShellInput) {
        let workspace_id = self.workspace_action_id();
        if !self.toggle_investigation() {
            self.open_file_viewer_for(workspace_id, outcome);
        }
        outcome.repaint = true;
    }

    /// Focuses `workspace_id` first so the path goes to the pane whose
    /// directory is shown.
    pub(super) fn open_file_viewer_for(
        &mut self,
        workspace_id: Option<String>,
        outcome: &mut ClientShellInput,
    ) {
        self.open_file_viewer_in(workspace_id, None, None, outcome);
    }

    /// `forced_location` (`Some(None)` is the host) is for restoring a saved
    /// tab, whose environment is no longer the focused pane's.
    pub(super) fn open_file_viewer_in(
        &mut self,
        workspace_id: Option<String>,
        forced_location: Option<Option<FileLocation>>,
        initial_root: Option<String>,
        outcome: &mut ClientShellInput,
    ) {
        outcome.repaint = true;
        // Decided here, from the snapshot, so nothing waits for an answer to
        // know which filesystem the tab shows. An old server cannot list the
        // pane's own environment: it keeps listing the host's.
        let location = forced_location
            .unwrap_or_else(|| self.workspace_file_location(workspace_id.as_deref()));
        let host_pane = self
            .focused_pane_id()
            .filter(|_| workspace_id == self.workspace_action_id());
        let list = match self.file_list_method(
            &workspace_id,
            &location,
            host_pane,
            initial_root.clone(),
            None,
        ) {
            Ok(list) => list,
            Err(notice) => {
                self.set_endpoint_error(notice);
                return;
            }
        };
        // Checked before focusing: an endpoint without the method only gets
        // the "Action unavailable" notice from the push, and nothing moves.
        if !self.supports_endpoint_method(&list) {
            self.push_endpoint_method_with_kind(list, PendingEndpointKind::Generic, outcome);
            return;
        }
        let focused_workspace = self
            .snapshot
            .as_deref()
            .and_then(|snapshot| snapshot.focused_workspace_id.clone());
        if let Some(target) = workspace_id
            .as_ref()
            .filter(|target| focused_workspace.as_ref() != Some(*target))
        {
            self.push_endpoint_method(
                Method::WorkspaceFocus(WorkspaceTarget {
                    workspace_id: target.clone(),
                }),
                outcome,
            );
        }
        let location_pane = Self::method_location_pane(&list);
        self.file_viewer_generation = self.file_viewer_generation.wrapping_add(1);
        let generation = self.file_viewer_generation;
        if !self.push_endpoint_method_with_kind(
            list,
            PendingEndpointKind::FileViewer(FileViewerRequest::List {
                generation,
                path: initial_root.clone(),
            }),
            outcome,
        ) {
            return;
        }
        let index_supported = self.request_file_viewer_index(
            generation,
            workspace_id.clone(),
            &location,
            initial_root.clone(),
            location_pane.as_deref(),
            outcome,
        );
        let mut viewer = ClientFileViewerOverlay::new(FileViewerOptions {
            generation,
            location,
            workspace_id,
            show_hidden: self.config.file_viewer_show_hidden,
            show_ignored: self.config.file_viewer_show_ignored,
            index_supported,
        });
        viewer.location_pane = location_pane.filter(|_| viewer.location.is_some());
        // A restored tab already knows its root, so the one listing asked for
        // is the root's, as after a root change.
        if let Some(root) = initial_root {
            viewer
                .directories
                .insert(root.clone(), DirectoryState::Loading);
            viewer.expanded.insert(root.clone());
            viewer.root = Some(root);
            viewer.rebuild(false);
        }
        self.overlay = Some(ClientShellOverlay::FileViewer(Box::new(viewer)));
        self.promote_file_viewer();
    }

    /// Whether `pane_id` runs in a container or over ssh, as the snapshot says.
    /// Only the old-server fallback asks: it lists the host for such a pane.
    fn file_viewer_pane_off_host(&self, pane_id: &str) -> bool {
        self.snapshot
            .as_deref()
            .and_then(|snapshot| snapshot.panes.iter().find(|pane| pane.pane_id == pane_id))
            .is_some_and(|pane| FileLocation::of_pane(pane).is_some())
    }

    pub(super) fn supports_pane_directory(&self) -> bool {
        self.supports_endpoint_method(&Method::PaneDirectoryList(PaneDirectoryListParams {
            pane_id: String::new(),
            path: None,
            trust_repository: false,
        }))
    }

    /// Environment of `workspace_id`'s pane that the viewer opens for. Why not
    /// a focused pane per workspace: the snapshot only names the globally
    /// focused pane, so another workspace is judged by its active tab.
    pub(super) fn workspace_file_location(
        &self,
        workspace_id: Option<&str>,
    ) -> Option<FileLocation> {
        // Gated here so opening and toggling cannot disagree: an old server
        // lists the host, and its tabs have no location.
        if !self.supports_pane_directory() {
            return None;
        }
        let snapshot = self.snapshot.as_deref()?;
        let workspace_id = workspace_id.or(snapshot.focused_workspace_id.as_deref())?;
        let active_tab = snapshot
            .workspaces
            .iter()
            .find(|workspace| workspace.workspace_id == workspace_id)
            .map(|workspace| workspace.active_tab_id.as_str());
        let mut panes = snapshot.panes.iter().filter(|pane| {
            pane.workspace_id == workspace_id && active_tab.is_none_or(|tab| pane.tab_id == tab)
        });
        let first = panes.clone().find(|pane| pane.focused).or(panes.next())?;
        FileLocation::of_pane(first)
    }

    /// A live pane of `workspace_id` running in `location`. `preferred` (the pane
    /// the tab last used) wins while it still qualifies, then the focused one.
    /// Why not the focused pane first: Compose replicas share one location, so
    /// focusing replica 2 would move replica 1's investigation into it.
    pub(super) fn file_location_pane(
        &self,
        workspace_id: Option<&str>,
        location: &FileLocation,
        preferred: Option<&str>,
    ) -> Option<String> {
        let snapshot = self.snapshot.as_deref()?;
        let mut candidates = snapshot.panes.iter().filter(|pane| {
            workspace_id.is_none_or(|id| pane.workspace_id == id)
                && FileLocation::of_pane(pane).as_ref() == Some(location)
        });
        candidates
            .clone()
            .find(|pane| Some(pane.pane_id.as_str()) == preferred)
            .or_else(|| {
                candidates
                    .clone()
                    .find(|pane| snapshot.focused_pane_id.as_deref() == Some(pane.pane_id.as_str()))
            })
            .or(candidates.next())
            .map(|pane| pane.pane_id.clone())
    }

    /// The pane an off-host request is addressed to; `None` for host methods.
    pub(super) fn method_location_pane(method: &Method) -> Option<String> {
        match method {
            Method::PaneDirectoryList(params) => Some(params.pane_id.clone()),
            Method::PaneDirectoryIndex(params) => Some(params.pane_id.clone()),
            Method::PaneFileRead(params) => Some(params.pane_id.clone()),
            _ => None,
        }
    }

    /// Makes the pane a sent request went through the one the tab keeps using.
    pub(super) fn remember_location_pane(&mut self, pane_id: Option<String>) {
        let Some(pane_id) = pane_id else {
            return;
        };
        if let Some(viewer) = self.file_viewer().filter(|v| v.location.is_some()) {
            viewer.location_pane = Some(pane_id);
        }
    }

    /// The pane the browser last used, to prefer for its next request.
    pub(super) fn location_pane_hint(&mut self) -> Option<String> {
        self.file_viewer()?.location_pane.clone()
    }

    pub(super) fn no_pane_notice(location: &FileLocation) -> String {
        format!("no pane running in {}", location.label())
    }

    /// `host_pane` only matters for the host's `directory.list`, which names the
    /// pane whose directory is shown.
    fn file_list_method(
        &self,
        workspace_id: &Option<String>,
        location: &Option<FileLocation>,
        host_pane: Option<String>,
        path: Option<String>,
        preferred: Option<&str>,
    ) -> Result<Method, String> {
        let Some(location) = location else {
            return Ok(Method::DirectoryList(DirectoryListParams {
                workspace_id: workspace_id.clone(),
                pane_id: host_pane,
                path,
                trust_repository: false,
            }));
        };
        let pane_id = self
            .file_location_pane(workspace_id.as_deref(), location, preferred)
            .ok_or_else(|| Self::no_pane_notice(location))?;
        Ok(Method::PaneDirectoryList(PaneDirectoryListParams {
            pane_id,
            path,
            trust_repository: false,
        }))
    }

    fn file_index_method(
        &self,
        workspace_id: &Option<String>,
        location: &Option<FileLocation>,
        host_pane: Option<String>,
        path: Option<String>,
        preferred: Option<&str>,
    ) -> Result<Method, String> {
        let Some(location) = location else {
            return Ok(Method::DirectoryIndex(DirectoryIndexParams {
                workspace_id: workspace_id.clone(),
                pane_id: host_pane,
                path,
                trust_repository: false,
            }));
        };
        let pane_id = self
            .file_location_pane(workspace_id.as_deref(), location, preferred)
            .ok_or_else(|| Self::no_pane_notice(location))?;
        Ok(Method::PaneDirectoryIndex(PaneDirectoryIndexParams {
            pane_id,
            path,
            trust_repository: false,
        }))
    }

    /// `poll` marks a background refresh, which the server gives a short deadline.
    pub(super) fn file_read_method(
        &self,
        workspace_id: &Option<String>,
        location: &Option<FileLocation>,
        path: String,
        revision: Option<String>,
        poll: bool,
        preferred: Option<&str>,
    ) -> Result<Method, String> {
        let Some(location) = location else {
            return Ok(Method::FileRead(FileReadParams { path, revision }));
        };
        let pane_id = self
            .file_location_pane(workspace_id.as_deref(), location, preferred)
            .ok_or_else(|| Self::no_pane_notice(location))?;
        Ok(Method::PaneFileRead(PaneFileReadParams {
            pane_id,
            path,
            revision,
            poll,
        }))
    }

    /// The focused pane when paths may go there; otherwise the notice that
    /// explains why nothing is sent. Why not the snapshot's focused pane
    /// alone: right after opening for another workspace it still names the
    /// old workspace's pane until the focus change comes back, and a restored
    /// pane without a shell is still listed. Why not any pane of the tab's
    /// location: the path would land in a terminal the user is not looking at.
    pub(super) fn file_viewer_send_target(&self) -> Result<String, String> {
        let (listed, location) = match self.overlay.as_ref() {
            Some(ClientShellOverlay::FileViewer(viewer)) => {
                (Some(viewer.pane_id.as_deref()), viewer.location.clone())
            }
            _ => match self.investigation() {
                // A tab sends to whatever pane is focused in its location, not
                // to the pane its first listing happened to name.
                Some(tab) => (None, tab.browser.location.clone()),
                None => return Err(NO_LIVE_PANE.to_owned()),
            },
        };
        let snapshot = self.snapshot.as_deref().ok_or(NO_LIVE_PANE)?;
        let focused = snapshot.focused_pane_id.as_deref().ok_or(NO_LIVE_PANE)?;
        if let Some(listed) = listed {
            let listed = listed.ok_or(NO_LIVE_PANE)?;
            if !snapshot.panes.iter().any(|pane| pane.pane_id == listed) {
                return Err(NO_LIVE_PANE.to_owned());
            }
            if focused != listed {
                return Err(PANE_NOT_FOCUSED.to_owned());
            }
        }
        let pane = snapshot
            .panes
            .iter()
            .find(|pane| pane.pane_id == focused)
            .ok_or(NO_LIVE_PANE)?;
        if FileLocation::of_pane(pane) != location {
            return Err(format!(
                "terminal destination is not in {}",
                location_label(location.as_ref())
            ));
        }
        Ok(focused.to_owned())
    }

    /// Asks again for an index that failed for lack of a pane.
    pub(super) fn retry_file_viewer_index(&mut self, outcome: &mut ClientShellInput) {
        let Some((generation, workspace_id, location, root)) = self.file_viewer().and_then(|v| {
            v.index_retry.then(|| {
                (
                    v.generation,
                    v.workspace_id.clone(),
                    v.location.clone(),
                    v.root.clone(),
                )
            })
        }) else {
            return;
        };
        let preferred = self.location_pane_hint();
        let pane = location.as_ref().and_then(|location| {
            self.file_location_pane(workspace_id.as_deref(), location, preferred.as_deref())
        });
        if self.request_file_viewer_index(
            generation,
            workspace_id,
            &location,
            root,
            preferred.as_deref(),
            outcome,
        ) {
            self.remember_location_pane(pane);
            if let Some(viewer) = self.file_viewer() {
                viewer.index_retry = false;
                viewer.index = FileIndex::Loading;
                viewer.rebuild(true);
            }
        }
    }

    /// Returns false when the endpoint cannot index, so search falls back to
    /// the visible rows.
    fn request_file_viewer_index(
        &mut self,
        generation: u64,
        workspace_id: Option<String>,
        location: &Option<FileLocation>,
        path: Option<String>,
        preferred: Option<&str>,
        outcome: &mut ClientShellInput,
    ) -> bool {
        let host_pane = self
            .focused_pane_id()
            .filter(|_| workspace_id == self.workspace_action_id());
        let Ok(method) =
            self.file_index_method(&workspace_id, location, host_pane, path, preferred)
        else {
            return false;
        };
        // Checked up front, not through the push: a missing index must not
        // raise "Action unavailable" while the tree itself works.
        self.supports_endpoint_method(&method)
            && self.push_endpoint_method_with_kind(
                method,
                PendingEndpointKind::FileViewer(FileViewerRequest::Index { generation }),
                outcome,
            )
    }

    pub(super) fn file_viewer(&mut self) -> Option<&mut ClientFileViewerOverlay> {
        if let Some(index) = self.workbench.dispatch {
            return self.workbench.tabs.get_mut(index).map(|t| &mut t.browser);
        }
        if self.investigation_active() {
            return self.investigation_mut().map(|t| &mut t.browser);
        }
        match self.overlay.as_mut() {
            Some(ClientShellOverlay::FileViewer(viewer)) => Some(viewer),
            _ => None,
        }
    }

    pub(super) fn request_file_viewer_listing(
        &mut self,
        path: String,
        outcome: &mut ClientShellInput,
    ) {
        let Some(viewer) = self.file_viewer() else {
            return;
        };
        viewer.listing_retry.remove(&path);
        let generation = viewer.generation;
        let (workspace_id, location) = (viewer.workspace_id.clone(), viewer.location.clone());
        let preferred = self.location_pane_hint();
        let method = self.file_list_method(
            &workspace_id,
            &location,
            None,
            Some(path.clone()),
            preferred.as_deref(),
        );
        let sent = match method {
            Ok(method) => {
                let pane = Self::method_location_pane(&method);
                let sent = self.push_endpoint_method_with_kind(
                    method,
                    PendingEndpointKind::FileViewer(FileViewerRequest::List {
                        generation,
                        path: Some(path.clone()),
                    }),
                    outcome,
                );
                if sent {
                    self.remember_location_pane(pane);
                }
                sent
            }
            Err(notice) => {
                if let Some(viewer) = self.file_viewer() {
                    viewer.listing_retry.insert(path.clone());
                    viewer
                        .directories
                        .insert(path, DirectoryState::Failed(notice));
                    viewer.rebuild(true);
                }
                return;
            }
        };
        if let Some(viewer) = self.file_viewer() {
            let state = if sent {
                DirectoryState::Loading
            } else {
                DirectoryState::Failed("not listed; expand again to retry".to_owned())
            };
            viewer.directories.insert(path, state);
            viewer.rebuild(true);
        }
    }

    /// Lists `root` and indexes it again; the old tree is dropped.
    pub(super) fn change_file_viewer_root(
        &mut self,
        root: String,
        select: Option<String>,
        outcome: &mut ClientShellInput,
    ) -> bool {
        let Some((workspace_id, location)) = self
            .file_viewer()
            .map(|viewer| (viewer.workspace_id.clone(), viewer.location.clone()))
        else {
            return false;
        };
        let preferred = self.location_pane_hint();
        let list = match self.file_list_method(
            &workspace_id,
            &location,
            None,
            Some(root.clone()),
            preferred.as_deref(),
        ) {
            Ok(list) => list,
            Err(notice) => {
                self.set_file_viewer_notice(notice);
                return false;
            }
        };
        let pane = Self::method_location_pane(&list);
        self.file_viewer_generation = self.file_viewer_generation.wrapping_add(1);
        let generation = self.file_viewer_generation;
        if !self.push_endpoint_method_with_kind(
            list,
            PendingEndpointKind::FileViewer(FileViewerRequest::List {
                generation,
                path: Some(root.clone()),
            }),
            outcome,
        ) {
            return false;
        }
        self.remember_location_pane(pane.clone());
        let index_supported = self.request_file_viewer_index(
            generation,
            workspace_id,
            &location,
            Some(root.clone()),
            pane.as_deref(),
            outcome,
        );
        let Some(viewer) = self.file_viewer() else {
            return false;
        };
        viewer.generation = generation;
        viewer.directories.clear();
        viewer.listing_retry.clear();
        viewer
            .directories
            .insert(root.clone(), DirectoryState::Loading);
        viewer.root = Some(root);
        viewer.expanded.clear();
        viewer.index = if index_supported {
            FileIndex::Loading
        } else {
            FileIndex::Unsupported
        };
        viewer.pending_select = select;
        viewer.revealing = None;
        viewer.scroll = 0;
        viewer.rebuild(false);
        if let Some(tab) = self.investigation_mut() {
            tab.matches.clear();
            tab.selected_match = 0;
            tab.schedule_search();
        }
        true
    }

    pub(super) fn raise_file_viewer_root(&mut self, outcome: &mut ClientShellInput) {
        let Some(root) = self.file_viewer().and_then(|viewer| viewer.root.clone()) else {
            return;
        };
        let Some(parent) = self
            .file_viewer()
            .and_then(|viewer| viewer.parent_path(&root).map(str::to_owned))
        else {
            return;
        };
        self.change_file_viewer_root(parent, Some(root), outcome);
    }

    /// `expand: None` toggles. A listing is fetched only the first time.
    pub(super) fn set_file_viewer_expanded(
        &mut self,
        path: String,
        expand: Option<bool>,
        outcome: &mut ClientShellInput,
    ) {
        let Some(viewer) = self.file_viewer() else {
            return;
        };
        let expanded = viewer.expanded.contains(&path);
        if !expand.unwrap_or(!expanded) {
            viewer.expanded.remove(&path);
            viewer.rebuild(true);
            return;
        }
        if expanded {
            return;
        }
        viewer.expanded.insert(path.clone());
        let listed = matches!(
            viewer.directories.get(&path),
            Some(DirectoryState::Loaded(_) | DirectoryState::Loading | DirectoryState::Loop)
        );
        viewer.rebuild(true);
        if !listed {
            self.request_file_viewer_listing(path, outcome);
        }
    }

    /// Leaves search and shows `path` expanded and selected in the tree,
    /// widening the root to the repository when the path lies outside it.
    pub(super) fn reveal_in_file_viewer_tree(
        &mut self,
        path: String,
        outcome: &mut ClientShellInput,
    ) {
        let Some(viewer) = self.file_viewer() else {
            return;
        };
        viewer.query.clear();
        viewer.search_focused = false;
        let inside = viewer
            .root
            .as_deref()
            .is_some_and(|root| host_path_contains(root, &path));
        let repo_root = match &viewer.index {
            FileIndex::Ready { repo_root, .. } => Some(repo_root.clone()),
            _ => None,
        };
        if !inside {
            let Some(repo_root) =
                repo_root.filter(|repo_root| host_path_contains(repo_root, &path))
            else {
                viewer.rebuild(true);
                return;
            };
            if !self.change_file_viewer_root(repo_root, None, outcome) {
                return;
            }
        }
        let Some(viewer) = self.file_viewer() else {
            return;
        };
        let Some(root) = viewer.root.clone() else {
            return;
        };
        for directory in viewer.path_chain(&root, &path).into_iter().skip(1) {
            // An earlier failure (a busy server) is retried, as expanding would.
            if matches!(
                viewer.directories.get(&directory),
                Some(DirectoryState::Failed(_))
            ) {
                viewer.directories.remove(&directory);
            }
            viewer.expanded.insert(directory);
        }
        viewer.pending_select = Some(path.clone());
        viewer.revealing = Some(path);
        self.continue_file_viewer_reveal(outcome);
    }

    /// Moves a reveal forward: once a directory on the way is listed, the
    /// filter that hides its next step is turned off and that step is
    /// listed. Why not list every step at once: the server refuses reads
    /// beyond its slot pool, and whether a step is hidden or ignored is only
    /// known from its parent's listing.
    fn continue_file_viewer_reveal(&mut self, outcome: &mut ClientShellInput) {
        let Some(viewer) = self.file_viewer() else {
            return;
        };
        let (Some(target), Some(root)) = (viewer.revealing.clone(), viewer.root.clone()) else {
            return;
        };
        let chain = viewer.path_chain(&root, &target);
        let mut next = None;
        let mut reachable = true;
        for (index, directory) in chain.iter().enumerate() {
            let child = chain.get(index + 1);
            match viewer.directories.get(directory) {
                None => {
                    next = Some(directory.clone());
                    break;
                }
                Some(DirectoryState::Loading) => break,
                // The target's own row lives in its parent's listing.
                Some(DirectoryState::Failed(_) | DirectoryState::Loop) => {
                    reachable = child.is_none();
                    viewer.revealing = None;
                    break;
                }
                Some(DirectoryState::Loaded(listing)) => {
                    let Some(child) = child else {
                        viewer.revealing = None;
                        break;
                    };
                    let Some(entry) = listing
                        .entries
                        .iter()
                        .find(|entry| join_host_path(directory, &entry.name) == *child)
                    else {
                        reachable = false;
                        viewer.revealing = None;
                        break;
                    };
                    viewer.show_hidden |= entry.hidden;
                    viewer.show_ignored |= entry.ignored == Some(true);
                }
            }
        }
        if !reachable {
            viewer.notice = Some(format!("cannot reveal {target}"));
        }
        viewer.rebuild(true);
        // A finished reveal whose row is still not drawn (a step was
        // collapsed meanwhile) must not grab the selection later.
        if viewer.revealing.is_none() && viewer.pending_select.as_deref() == Some(target.as_str()) {
            viewer.pending_select = None;
        }
        if let Some(directory) = next {
            self.request_file_viewer_listing(directory, outcome);
        }
    }

    fn file_viewer_activate(&mut self, action: FileViewerAction, outcome: &mut ClientShellInput) {
        outcome.repaint = true;
        let Some(viewer) = self.file_viewer() else {
            return;
        };
        viewer.last_click = None;
        let searching = viewer.searching();
        let Some(entry) = viewer.selected_entry().cloned() else {
            return;
        };
        match action {
            FileViewerAction::Enter if entry.is_dir && searching => {
                self.reveal_in_file_viewer_tree(entry.path, outcome);
            }
            FileViewerAction::Enter if entry.is_dir => {
                self.set_file_viewer_expanded(entry.path, None, outcome);
            }
            FileViewerAction::Enter => self.send_file_viewer_path(&entry.path, true, outcome),
            FileViewerAction::Keep => self.send_file_viewer_path(&entry.path, false, outcome),
            FileViewerAction::Edit if entry.is_dir => {
                self.set_file_viewer_notice(EDIT_FILES_ONLY);
            }
            FileViewerAction::Edit => self.open_file_viewer_editor(entry.path, outcome),
            FileViewerAction::Copy => {
                outcome.actions.push(ClientShellAction::ClipboardWrite(
                    entry.path.as_bytes().to_vec(),
                ));
                self.show_copy_feedback(std::time::Instant::now());
                self.set_file_viewer_notice(format!("copied {}", entry.path));
            }
        }
    }

    fn set_file_viewer_notice(&mut self, notice: impl Into<String>) {
        if let Some(viewer) = self.file_viewer() {
            viewer.notice = Some(notice.into());
        }
    }

    pub(super) fn send_file_viewer_path(
        &mut self,
        path: &str,
        close: bool,
        outcome: &mut ClientShellInput,
    ) {
        // Why not check after resolving the pane: that leaves copy mode, and a
        // refused path should leave copy mode and the viewer as they were.
        let Some(word) = shell_quote_path(path) else {
            self.set_file_viewer_notice(CONTROL_CHARACTERS_IN_PATH);
            return;
        };
        let pane_id = match self.file_viewer_send_target() {
            Ok(pane_id) => pane_id,
            Err(notice) => {
                self.set_file_viewer_notice(notice);
                return;
            }
        };
        if self
            .copy_mode
            .as_ref()
            .is_some_and(|copy_mode| copy_mode.pane_id == pane_id)
        {
            self.exit_copy_mode(false, outcome);
        }
        let text = format!("{word} ");
        let alternate_screen = self
            .pane_surface
            .as_ref()
            .and_then(|surface| surface.panes.iter().find(|pane| pane.pane_id == pane_id))
            .is_some_and(|pane| pane.alternate_screen_active);
        // Why not a paste everywhere: a shell prompt takes typed text as the
        // user would type it, while full-screen apps need paste framing to
        // read a path as text. The server only adds the bracketed-paste
        // markers when the app enabled them.
        let event = if alternate_screen {
            ClientPaneInputEvent::Paste(text)
        } else {
            ClientPaneInputEvent::TextCommit(text)
        };
        push_target_event(ClientInputTarget::Pane(pane_id), event, outcome);
        if close {
            self.overlay = None;
        } else {
            self.set_file_viewer_notice(format!("sent {path}"));
        }
    }

    fn open_file_viewer_editor(&mut self, path: String, outcome: &mut ClientShellInput) {
        let pane_id = match self.file_viewer_send_target() {
            Ok(pane_id) => pane_id,
            Err(notice) => {
                self.set_file_viewer_notice(notice);
                return;
            }
        };
        let Some((workspace_id, instance, offhost)) = self.file_viewer().map(|viewer| {
            (
                viewer.workspace_id.clone(),
                viewer.instance,
                viewer.location.is_some(),
            )
        }) else {
            return;
        };
        let method = if offhost {
            Method::PaneDirectoryOpenEditor(PaneDirectoryOpenEditorParams { pane_id, path })
        } else {
            Method::PaneOpenEditor(PaneOpenEditorParams {
                workspace_id,
                pane_id: Some(pane_id),
                path,
            })
        };
        let sent = self.push_endpoint_method_with_kind(
            method,
            PendingEndpointKind::FileViewer(FileViewerRequest::OpenEditor { instance }),
            outcome,
        );
        if sent {
            self.set_file_viewer_notice("opening editor…");
        }
    }

    pub(super) fn insert_file_viewer_overlay_text(&mut self, text: &str) -> bool {
        let Some(viewer) = self.file_viewer() else {
            return false;
        };
        if !viewer.search_focused {
            return false;
        }
        // Space is the keep-open action, so the query never holds whitespace.
        let text: String = text.chars().filter(|ch| !ch.is_whitespace()).collect();
        if viewer.query.insert(&text) {
            viewer.rebuild(false);
        }
        true
    }

    pub(super) fn route_file_viewer_overlay_key(
        &mut self,
        key: &crate::input::TerminalKey,
        outcome: &mut ClientShellInput,
    ) -> bool {
        let Some(viewer) = self.file_viewer() else {
            return false;
        };
        if key.kind == crossterm::event::KeyEventKind::Release {
            return true;
        }
        viewer.last_click = None;
        outcome.repaint = true;
        let (code, modifiers) = crate::config::normalize_key_combo((key.code, key.modifiers));
        let ctrl = modifiers == crossterm::event::KeyModifiers::CONTROL;
        let plain = modifiers.is_empty();
        // Text the host reports with the key (an IME commit, kitty associated
        // text) is what was typed, even when it arrives on the space key.
        // Why not control text: a chord such as Ctrl+e stays an action.
        let host_text = key.generated_text.as_deref().filter(|text| {
            text.chars()
                .any(|ch| !ch.is_whitespace() && !ch.is_control())
        });
        if let Some(text) = host_text.filter(|_| viewer.search_focused) {
            let text = text.to_owned();
            self.insert_file_viewer_overlay_text(&text);
            return true;
        }
        // Ahead of the text editor, which would type Space, move to the end
        // on Ctrl+e and yank on Ctrl+y. Shift+Space counts as Space so it
        // never types a space into the query.
        let action = match code {
            KeyCode::Enter if plain => Some(FileViewerAction::Enter),
            KeyCode::Char(' ')
                if modifiers
                    .difference(crossterm::event::KeyModifiers::SHIFT)
                    .is_empty()
                    && host_text.is_none() =>
            {
                Some(FileViewerAction::Keep)
            }
            KeyCode::Char('e') if ctrl => Some(FileViewerAction::Edit),
            KeyCode::Char('y') if ctrl => Some(FileViewerAction::Copy),
            _ => None,
        };
        if let Some(action) = action {
            self.file_viewer_activate(action, outcome);
            return true;
        }
        let movement = match code {
            KeyCode::Up if plain => Some(-1),
            KeyCode::Down if plain => Some(1),
            KeyCode::Char('p') if ctrl => Some(-1),
            KeyCode::Char('n') if ctrl => Some(1),
            _ => None,
        };
        if let Some(delta) = movement {
            viewer.move_selection(delta);
            return true;
        }
        if viewer.search_focused {
            if code == KeyCode::Esc {
                viewer.query.clear();
                viewer.search_focused = false;
                viewer.rebuild(true);
            } else if matches!(code, KeyCode::Char(ch) if ch.is_whitespace()) {
                // An ideographic space and the like: the query holds no whitespace.
            } else if viewer.query.handle_key(key) == Some(true) {
                viewer.rebuild(false);
            }
            return true;
        }
        match code {
            KeyCode::Esc => self.overlay = None,
            KeyCode::Char('/') if plain => viewer.search_focused = true,
            KeyCode::Char('.') if plain => {
                viewer.show_hidden = !viewer.show_hidden;
                viewer.rebuild(true);
            }
            KeyCode::Char('i') if plain => {
                viewer.show_ignored = !viewer.show_ignored;
                viewer.rebuild(true);
            }
            KeyCode::Backspace if plain => self.raise_file_viewer_root(outcome),
            KeyCode::Right if plain => {
                if let Some(entry) = viewer.selected_entry().filter(|entry| entry.is_dir) {
                    let path = entry.path.clone();
                    self.set_file_viewer_expanded(path, Some(true), outcome);
                }
            }
            KeyCode::Left if plain => self.collapse_or_climb_file_viewer(outcome),
            _ => {}
        }
        true
    }

    pub(super) fn collapse_or_climb_file_viewer(&mut self, outcome: &mut ClientShellInput) {
        let Some(viewer) = self.file_viewer() else {
            return;
        };
        let Some(entry) = viewer.selected_entry().cloned() else {
            return;
        };
        if entry.expanded {
            self.set_file_viewer_expanded(entry.path, Some(false), outcome);
            return;
        }
        let Some(parent) = viewer.parent_path(&entry.path).map(str::to_owned) else {
            return;
        };
        if let Some(index) = viewer.rows.iter().position(|row| {
            row.entry
                .as_ref()
                .is_some_and(|candidate| candidate.path == parent)
        }) {
            viewer.selected = index;
        }
    }

    pub(super) fn handle_file_viewer_mouse(
        &mut self,
        mouse: crossterm::event::MouseEvent,
        outcome: &mut ClientShellInput,
    ) -> bool {
        self.handle_file_viewer_mouse_at(mouse, std::time::Instant::now(), outcome)
    }

    /// `now` is when the event arrived; double clicks are timed against it.
    pub(super) fn handle_file_viewer_mouse_at(
        &mut self,
        mouse: crossterm::event::MouseEvent,
        now: std::time::Instant,
        outcome: &mut ClientShellInput,
    ) -> bool {
        use crossterm::event::{MouseButton, MouseEventKind};

        if !matches!(self.overlay, Some(ClientShellOverlay::FileViewer(_))) {
            return false;
        }
        let point = (mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let delta = if mouse.kind == MouseEventKind::ScrollUp {
                    -WHEEL_ROWS
                } else {
                    WHEEL_ROWS
                };
                if let Some(viewer) = self.file_viewer() {
                    viewer.move_selection(delta);
                }
                outcome.repaint = true;
            }
            MouseEventKind::Down(MouseButton::Left) => {
                outcome.repaint = true;
                let button = self
                    .hits
                    .file_viewer_buttons
                    .iter()
                    .find(|(rect, _)| contains(*rect, point))
                    .map(|(_, button)| *button);
                let row = self
                    .hits
                    .file_viewer_rows
                    .iter()
                    .find(|(rect, _)| contains(*rect, point))
                    .map(|(_, index)| *index);
                if let Some(button) = button {
                    self.press_file_viewer_button(button, outcome);
                } else if contains(self.hits.file_viewer_filter, point) {
                    if let Some(viewer) = self.file_viewer() {
                        viewer.search_focused = true;
                        viewer.last_click = None;
                    }
                } else if let Some(index) = row {
                    self.click_file_viewer_row(index, now, outcome);
                } else if !contains(self.hits.file_viewer_popup, point) {
                    self.overlay = None;
                } else if let Some(viewer) = self.file_viewer() {
                    viewer.last_click = None;
                }
            }
            _ => {}
        }
        true
    }

    fn press_file_viewer_button(
        &mut self,
        button: FileViewerButton,
        outcome: &mut ClientShellInput,
    ) {
        let action = match button {
            FileViewerButton::Send => FileViewerAction::Enter,
            FileViewerButton::Keep => FileViewerAction::Keep,
            FileViewerButton::Edit => FileViewerAction::Edit,
            FileViewerButton::Copy => FileViewerAction::Copy,
            FileViewerButton::Close => {
                self.overlay = None;
                return;
            }
        };
        self.file_viewer_activate(action, outcome);
    }

    /// Click selects; on a tree directory it also toggles. A double click
    /// sends a file and closes, or reveals a directory search result.
    fn click_file_viewer_row(
        &mut self,
        index: usize,
        now: std::time::Instant,
        outcome: &mut ClientShellInput,
    ) {
        let Some(viewer) = self.file_viewer() else {
            return;
        };
        let Some(entry) = viewer.rows.get(index).and_then(|row| row.entry.clone()) else {
            return;
        };
        let double = viewer.last_click.is_some_and(|(row, at)| {
            row == index && now.saturating_duration_since(at) <= DOUBLE_CLICK
        });
        viewer.selected = index;
        viewer.last_click = (!double).then_some((index, now));
        let searching = viewer.searching();
        match (entry.is_dir, searching, double) {
            (true, false, _) => self.set_file_viewer_expanded(entry.path, None, outcome),
            (true, true, true) => self.reveal_in_file_viewer_tree(entry.path, outcome),
            (false, _, true) => self.send_file_viewer_path(&entry.path, true, outcome),
            _ => {}
        }
    }

    /// `outcome` collects the next listing a reveal asks for.
    pub(super) fn handle_file_viewer_endpoint_result_inner(
        &mut self,
        request: FileViewerRequest,
        result: Result<ResponseResult, ClientShellEndpointError>,
        outcome: &mut ClientShellInput,
    ) -> bool {
        let result = result.map(offhost_result_as_host);
        match request {
            FileViewerRequest::List { generation, path } => {
                self.complete_file_viewer_listing(generation, path, result, outcome)
            }
            FileViewerRequest::Index { generation } => {
                self.complete_file_viewer_index(generation, result)
            }
            FileViewerRequest::OpenEditor { instance } => {
                let Some(viewer) = self
                    .file_viewer()
                    .filter(|viewer| viewer.instance == instance)
                else {
                    return false;
                };
                match result {
                    Ok(ResponseResult::PaneInfo { .. } | ResponseResult::Ok {}) => {
                        self.overlay = None
                    }
                    Ok(_) => {
                        viewer.notice =
                            Some("endpoint returned an unexpected editor result".into());
                    }
                    Err(error) => viewer.notice = Some(error.message),
                }
                true
            }
        }
    }

    fn complete_file_viewer_listing(
        &mut self,
        generation: u64,
        requested: Option<String>,
        result: Result<ResponseResult, ClientShellEndpointError>,
        outcome: &mut ClientShellInput,
    ) -> bool {
        if self
            .file_viewer()
            .is_none_or(|viewer| viewer.generation != generation)
        {
            return false;
        }
        let rejected = result.is_err();
        let pane_gone = matches!(&result, Err(error) if pane_unavailable(error));
        let listing = match result {
            Ok(ResponseResult::DirectoryList {
                path,
                canonical,
                entries,
                truncated,
                git_error,
                pane_id,
                omitted,
                unlistable,
                ..
            }) => Ok((
                path,
                pane_id,
                DirectoryListing {
                    canonical,
                    entries,
                    truncated,
                    omitted,
                    unlistable,
                    git_error,
                },
            )),
            Ok(_) => Err("endpoint returned an unexpected listing".to_owned()),
            Err(error) => Err(listing_error_notice(&error)),
        };
        let off_host = match &listing {
            Ok((_, Some(pane_id), _)) if requested.is_none() => {
                self.file_viewer_pane_off_host(pane_id)
            }
            _ => false,
        };
        let Some(viewer) = self.file_viewer() else {
            return false;
        };
        match (requested, listing) {
            (None, Ok((root, pane_id, listing))) => {
                viewer.root = Some(root.clone());
                viewer.host_fallback = off_host && viewer.location.is_none();
                // An off-host tab sends to the focused pane of its location,
                // so the answer naming no pane says nothing about sending.
                if pane_id.is_none() && viewer.location.is_none() {
                    viewer.notice = Some(NO_LIVE_PANE.to_owned());
                }
                viewer.pane_id = pane_id;
                viewer
                    .directories
                    .insert(root, DirectoryState::Loaded(listing));
                viewer.rebuild(false);
            }
            (None, Err(notice)) => {
                // A promoted tab has no root to show, so it closes like the
                // overlay did instead of staying on "loading…".
                match self.workbench.dispatch {
                    Some(index) => self.remove_investigation(index),
                    None => self.overlay = None,
                }
                // A rejection already raised the endpoint notice.
                if !rejected {
                    self.set_endpoint_error(notice);
                }
            }
            (Some(path), Ok((_, _, listing))) => {
                let is_root = viewer.root.as_deref() == Some(path.as_str());
                let state = if !is_root && viewer.loops_back(&path, &listing.canonical) {
                    DirectoryState::Loop
                } else {
                    DirectoryState::Loaded(listing)
                };
                viewer.listing_retry.remove(&path);
                viewer.directories.insert(path, state);
                viewer.rebuild(true);
                self.continue_file_viewer_reveal(outcome);
            }
            // Why not go through `continue_file_viewer_reveal`: a failed or
            // cancelled answer must never start another request.
            (Some(path), Err(notice)) => {
                if pane_gone {
                    viewer.listing_retry.insert(path.clone());
                }
                viewer
                    .directories
                    .insert(path.clone(), DirectoryState::Failed(notice));
                if let Some(target) = viewer
                    .revealing
                    .clone()
                    .filter(|target| host_path_contains(&path, target))
                {
                    viewer.revealing = None;
                    if path != target {
                        viewer.pending_select = None;
                        viewer.notice = Some(format!("cannot reveal {target}"));
                    }
                }
                viewer.rebuild(true);
            }
        }
        true
    }

    fn complete_file_viewer_index(
        &mut self,
        generation: u64,
        result: Result<ResponseResult, ClientShellEndpointError>,
    ) -> bool {
        let Some(viewer) = self
            .file_viewer()
            .filter(|viewer| viewer.generation == generation)
        else {
            return false;
        };
        viewer.index_retry = matches!(&result, Err(error) if pane_unavailable(error));
        viewer.index = match result {
            Ok(ResponseResult::DirectoryIndex {
                repo_root: Some(repo_root),
                entries,
                truncated,
                ..
            }) => FileIndex::Ready {
                repo_root,
                paths: indexed_paths(entries),
                truncated,
            },
            Ok(ResponseResult::DirectoryIndex { git_error, .. }) => FileIndex::Unavailable {
                notice: git_error.as_deref().map(|error| {
                    if error == "git timed out" {
                        "index timed out".to_owned()
                    } else {
                        git_error_notice(error)
                    }
                }),
            },
            Ok(_) => FileIndex::Unavailable {
                notice: Some("index unavailable".to_owned()),
            },
            Err(error) => FileIndex::Unavailable {
                notice: Some(format!("index unavailable: {}", error.message)),
            },
        };
        let searching = viewer.searching();
        viewer.rebuild(!searching);
        true
    }
}

/// Answers of `pane.directory.*` and `pane.file.read` carry the same data as the
/// host methods' with a `location` added; the handlers read one shape.
/// Why not a pane id: off-host tabs choose their pane per request.
pub(super) fn offhost_result_as_host(result: ResponseResult) -> ResponseResult {
    match result {
        ResponseResult::PaneDirectoryList(list) => ResponseResult::DirectoryList {
            root: list.root,
            path: list.path,
            canonical: list.canonical,
            entries: list.entries,
            truncated: list.truncated,
            git_error: list.git_error,
            pane_id: None,
            omitted: list.omitted,
            unlistable: list.unlistable,
        },
        ResponseResult::PaneDirectoryIndex(index) => ResponseResult::DirectoryIndex {
            repo_root: index.repo_root,
            entries: index.entries,
            truncated: index.truncated,
            git_error: index.git_error,
        },
        ResponseResult::PaneFileDocument(content) => ResponseResult::FileDocument {
            document: content.document,
        },
        other => other,
    }
}

/// The server could not use the pane the request named (closed while queued).
fn pane_unavailable(error: &ClientShellEndpointError) -> bool {
    matches!(
        error.code.as_deref(),
        Some("pane_not_found" | "pane_not_live" | "stale_pane_target")
    )
}

fn listing_error_notice(error: &ClientShellEndpointError) -> String {
    match error.code.as_deref() {
        Some("permission_denied") => "permission denied".to_owned(),
        Some("not_found") => "not found".to_owned(),
        Some("not_a_directory") => "not a directory".to_owned(),
        Some("directory_busy") => "server busy; expand again to retry".to_owned(),
        _ => error.message.clone(),
    }
}
