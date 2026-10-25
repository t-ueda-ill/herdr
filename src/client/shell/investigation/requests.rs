use super::*;
use crate::api::schema::*;
use std::time::{Duration, Instant};

const HOST_POLL_INTERVAL: Duration = Duration::from_secs(1);
const OFFHOST_POLL_INTERVAL: Duration = Duration::from_secs(5);
/// Least time between two index refreshes started by polling. Why not on every
/// change: a busy tree would re-run the server's `git ls-files` back to back.
const HOST_INDEX_INTERVAL: Duration = Duration::from_secs(10);
const OFFHOST_INDEX_INTERVAL: Duration = Duration::from_secs(30);

/// Back-dates a tab's last poll so the next tick refreshes it whatever its interval.
pub(super) fn due_poll_time() -> Instant {
    Instant::now()
        .checked_sub(Duration::from_secs(10))
        .unwrap_or_else(Instant::now)
}

impl ClientShellState {
    pub(in crate::client::shell) fn request_investigation_preview(
        &mut self,
        pin: bool,
        focus: bool,
        outcome: &mut ClientShellInput,
    ) {
        let Some(tab) = self.investigation_mut() else {
            return;
        };
        let Some((path, offset)) = tab.target() else {
            return;
        };
        if let Some(index) = tab.documents.iter().position(|d| d.path == path) {
            tab.active_file = Some(index);
            if offset > 0 {
                tab.documents[index].cursor = offset.min(tab.documents[index].text.len());
                while !tab.documents[index]
                    .text
                    .is_char_boundary(tab.documents[index].cursor)
                {
                    tab.documents[index].cursor -= 1;
                }
                tab.documents[index].scroll = tab.documents[index].line().saturating_sub(3);
            }
        } else {
            if tab.preview.as_ref().is_none_or(|d| d.path != path) {
                tab.preview = Some(Document::new(path.clone()));
            }
            tab.active_file = None;
            if let Some(document) = tab.preview.as_mut() {
                document.clear_selection();
                document.cursor = if document.loaded {
                    offset.min(document.text.len())
                } else {
                    offset
                };
                if document.loaded {
                    while !document.text.is_char_boundary(document.cursor) {
                        document.cursor -= 1;
                    }
                }
            }
            if pin {
                if let Some(document) = tab.preview.take() {
                    tab.documents.push(document);
                    tab.active_file = Some(tab.documents.len() - 1);
                }
            }
        }
        if focus {
            tab.focus = Focus::Body;
        }
        tab.preview_generation = tab.preview_generation.wrapping_add(1);
        if tab.document().is_some_and(|d| d.loaded) {
            self.persist_chrome_preferences(outcome);
            return;
        }
        let (id, generation) = (tab.id, tab.preview_generation);
        let (workspace, location, preferred) = (
            tab.browser.workspace_id.clone(),
            tab.browser.location.clone(),
            tab.browser.location_pane.clone(),
        );
        if let Some(d) = tab.document_mut() {
            d.request_generation = generation;
        }
        tab.pending_reads.insert(path.clone());
        let method = self.file_read_method(
            &workspace,
            &location,
            path.clone(),
            None,
            false,
            preferred.as_deref(),
        );
        let sent = match method {
            Ok(method) => {
                let pane = Self::method_location_pane(&method);
                let sent = self.push_endpoint_method_with_kind(
                    method,
                    PendingEndpointKind::Investigation(FileRequest::Read {
                        id,
                        path,
                        generation,
                        prepare: false,
                    }),
                    outcome,
                );
                if sent {
                    self.remember_location_pane(pane);
                }
                sent
            }
            Err(notice) => {
                if let Some(tab) = self.investigation_mut() {
                    tab.notice = notice;
                }
                false
            }
        };
        if !sent {
            if let Some(tab) = self.investigation_mut() {
                tab.pending_reads.clear();
            }
        }
        self.persist_chrome_preferences(outcome);
    }
    pub(in crate::client::shell) fn handle_file_viewer_endpoint_result(
        &mut self,
        request: FileViewerRequest,
        result: Result<ResponseResult, ClientShellEndpointError>,
        outcome: &mut ClientShellInput,
    ) -> bool {
        let generation = match &request {
            FileViewerRequest::List { generation, .. }
            | FileViewerRequest::Refresh { generation, .. }
            | FileViewerRequest::IndexRefresh { generation }
            | FileViewerRequest::Index { generation } => Some(*generation),
            _ => None,
        };
        let index = self.workbench.tabs.iter().position(|t| {
            t.endpoint == self.active_endpoint_id && generation == Some(t.browser.generation)
        });
        let background = matches!(
            request,
            FileViewerRequest::Refresh { .. } | FileViewerRequest::IndexRefresh { .. }
        );
        let selected_path = |state: &Self| {
            index
                .and_then(|i| state.workbench.tabs.get(i))
                .and_then(|tab| tab.browser.selected_entry())
                .map(|entry| entry.path.clone())
        };
        let before = selected_path(self);
        self.workbench.dispatch = index;
        let repaint = self.handle_file_viewer_endpoint_result_inner(request, result, outcome);
        self.workbench.dispatch = None;
        // Why not for every answer: a background refresh would otherwise snap
        // a scrolled document back to its cursor, drop a text selection and
        // rewrite the preferences on each poll. Only a moved selection needs
        // the preview to follow.
        if background && selected_path(self) == before {
            return repaint;
        }
        if self.investigation_active() {
            self.request_investigation_preview(false, false, outcome);
            self.persist_chrome_preferences(outcome);
        }
        repaint
    }
    pub(in crate::client::shell) fn handle_investigation_result(
        &mut self,
        request: FileRequest,
        result: Result<ResponseResult, ClientShellEndpointError>,
        outcome: &mut ClientShellInput,
    ) {
        let result = result.map(super::super::file_viewer::offhost_result_as_host);
        let id = match &request {
            FileRequest::Read { id, .. }
            | FileRequest::Search { id, .. }
            | FileRequest::Replace { id, .. }
            | FileRequest::Plan { id, .. }
            | FileRequest::Operate { id }
            | FileRequest::Editor { id, .. }
            | FileRequest::EditorCheck { id, .. } => *id,
        };
        let Some(index) = self
            .workbench
            .tabs
            .iter()
            .position(|t| t.id == id && t.endpoint == self.active_endpoint_id)
        else {
            return;
        };
        let tab = &mut self.workbench.tabs[index];
        match request {
            FileRequest::Read {
                path,
                generation,
                prepare,
                ..
            } => {
                if prepare && generation != tab.prepare_generation {
                    return;
                }
                if !prepare {
                    tab.pending_reads.remove(&path);
                }
                if prepare {
                    tab.prepare_pending = tab.prepare_pending.saturating_sub(1);
                    match result {
                        Ok(ResponseResult::FileDocument { document }) => {
                            if let Err(error) = super::operations::prepare_document(tab, document) {
                                tab.notice = error;
                                tab.plan.clear();
                                tab.prepare_queue.clear();
                                tab.dialog = None;
                            }
                        }
                        Err(error) => {
                            tab.notice = error.message;
                            tab.plan.clear();
                            tab.prepare_queue.clear();
                            tab.dialog = None;
                        }
                        _ => {
                            tab.notice = "unexpected file response".into();
                            tab.dialog = None;
                        }
                    }
                    return;
                }
                let document = tab
                    .documents
                    .iter_mut()
                    .find(|d| d.path == path && d.request_generation == generation)
                    .or_else(|| {
                        tab.preview
                            .as_mut()
                            .filter(|d| d.path == path && generation == d.request_generation)
                    });
                if let Some(document) = document {
                    match result {
                        Ok(ResponseResult::FileDocument { document: loaded }) => {
                            if !loaded.unchanged {
                                let changed =
                                    document.loaded && document.revision != loaded.revision;
                                if changed && tab.content_search {
                                    tab.search_ready = false;
                                    tab.notice =
                                        "file changed; rerun the content search before replacing"
                                            .into();
                                }
                                let first_load = !document.loaded;
                                document.update(loaded.text, loaded.revision);
                                if first_load && !document.restored_scroll {
                                    document.scroll = document.line().saturating_sub(3);
                                }
                                document.restored_scroll = false;
                            }
                        }
                        Err(error) => tab.notice = error.message,
                        _ => tab.notice = "unexpected file response".into(),
                    }
                }
            }
            FileRequest::Search { generation, .. } => {
                if generation != tab.search_generation {
                    return;
                }
                tab.searching = false;
                match result {
                    Ok(ResponseResult::FileSearch {
                        matches,
                        truncated,
                        cancelled,
                        warnings,
                    }) => {
                        if cancelled {
                            return;
                        }
                        tab.matches = matches;
                        tab.selected_match = 0;
                        tab.omitted.clear();
                        tab.search_ready = true;
                        tab.notice = format!(
                            "{} matches{}{}",
                            tab.matches.len(),
                            if truncated {
                                " (limited; narrow the search)"
                            } else {
                                ""
                            },
                            if warnings.is_empty() {
                                String::new()
                            } else {
                                format!(" · {} skipped/errors: {}", warnings.len(), warnings[0])
                            }
                        );
                        if Some(index) == self.workbench.active && self.workbench.visible {
                            self.request_investigation_preview(false, false, outcome);
                        }
                    }
                    Err(error) => {
                        tab.notice = error.message;
                        tab.search_ready = false;
                    }
                    _ => tab.notice = "unexpected search response".into(),
                }
            }
            FileRequest::Replace {
                originals, undo, ..
            } => {
                tab.operation_pending = false;
                match result {
                    Ok(ResponseResult::FilesChanged { outcomes }) => {
                        let mut next_undo = Vec::new();
                        for item in &outcomes {
                            if let Some(version) = &item.revision {
                                if let Some(original) =
                                    originals.iter().find(|o| o.path == item.path)
                                {
                                    next_undo.push(FileChange {
                                        path: item.path.clone(),
                                        revision: version.clone(),
                                        text: original.text.clone(),
                                    });
                                }
                            }
                        }
                        if undo {
                            tab.undo.retain(|change| {
                                outcomes
                                    .iter()
                                    .any(|o| o.path == change.path && o.error.is_some())
                            });
                        } else {
                            tab.undo = next_undo;
                        }
                        let errors: Vec<_> = outcomes
                            .iter()
                            .filter_map(|o| o.error.as_ref().map(|e| format!("{}: {e}", o.path)))
                            .collect();
                        tab.notice = format!(
                            "{} files changed, {} skipped{}",
                            outcomes.len() - errors.len(),
                            errors.len(),
                            errors.first().map_or(String::new(), |e| format!(" · {e}"))
                        );
                        tab.search_ready = false;
                        tab.plan.clear();
                        tab.prepare_queue.clear();
                        tab.dialog = None;
                        tab.last_poll = due_poll_time();
                    }
                    Err(error) => tab.notice = error.message,
                    _ => tab.notice = "unexpected replacement response".into(),
                }
            }
            FileRequest::Plan { params, .. } => {
                tab.operation_pending = false;
                match result {
                    Ok(ResponseResult::FilesPlanned { outcomes }) => {
                        tab.dialog = Some(Dialog::Confirm {
                            selected: match params.collision {
                                FileCollision::Skip => 0,
                                FileCollision::Rename => 1,
                                FileCollision::Replace => 2,
                            },
                            params,
                            outcomes,
                        })
                    }
                    Err(error) => tab.notice = error.message,
                    _ => tab.notice = "unexpected operation plan response".into(),
                }
            }
            FileRequest::Operate { .. } => {
                tab.operation_pending = false;
                match result {
                    Ok(ResponseResult::FilesChanged { outcomes }) => {
                        let failures: Vec<_> =
                            outcomes.iter().filter(|o| o.error.is_some()).collect();
                        tab.notice = format!(
                            "{} completed, {} skipped{}",
                            outcomes.len() - failures.len(),
                            failures.len(),
                            failures.first().map_or(String::new(), |o| format!(
                                " · {}: {}",
                                o.path,
                                o.error.as_deref().unwrap_or_default()
                            ))
                        );
                        let unavailable: Vec<_> = outcomes
                            .iter()
                            .filter(|o| {
                                o.error
                                    .as_deref()
                                    .is_some_and(|e| e.starts_with("trash unavailable:"))
                            })
                            .map(|o| o.path.clone())
                            .collect();
                        if !unavailable.is_empty() {
                            tab.dialog = Some(Dialog::Confirm {
                                params: operation_params(FileOperation::Delete, unavailable, None),
                                selected: 0,
                                outcomes: outcomes
                                    .iter()
                                    .filter(|o| o.error.is_some())
                                    .cloned()
                                    .collect(),
                            });
                        }
                        tab.selected_files.clear();
                        if let Some(clipboard) = self.workbench.clipboard.as_mut().filter(|c| c.cut)
                        {
                            clipboard.paths.retain(|p| {
                                !outcomes.iter().any(|o| o.path == *p && o.error.is_none())
                            });
                        }
                        let paths: Vec<_> = tab.browser.directories.keys().cloned().collect();
                        if Some(index) == self.workbench.active && self.workbench.visible {
                            for path in paths {
                                self.request_file_viewer_listing(path, outcome);
                            }
                        }
                    }
                    Err(error) => tab.notice = error.message,
                    _ => tab.notice = "unexpected operation response".into(),
                }
            }
            FileRequest::EditorCheck { pane, .. } => {
                if self
                    .workbench
                    .editor
                    .as_ref()
                    .is_none_or(|e| e.id != id || e.pane != pane)
                {
                    return;
                }
                match result {
                    Err(error) if error.code.as_deref() == Some("pane_not_found") => {
                        self.workbench.editor = None;
                        tab.last_poll = due_poll_time();
                        self.show_files(index);
                    }
                    _ => {
                        if let Some(editor) = self.workbench.editor.as_mut() {
                            editor.awaiting = false;
                            editor.due = Instant::now() + Duration::from_millis(500);
                        }
                    }
                }
            }
            FileRequest::Editor { .. } => {
                self.workbench.editor_opening = false;
                match result {
                    Ok(ResponseResult::PaneInfo { pane }) => {
                        self.workbench.editor = Some(EditorReturn {
                            id,
                            pane: pane.pane_id,
                            due: Instant::now(),
                            awaiting: false,
                        });
                        // Why not hidden in split: the editor pane takes
                        // focus beside the files region.
                        self.show_terminal_side();
                        self.workbench.terminal_requested = false;
                    }
                    Err(error) => {
                        tab.notice = error.message;
                        self.workbench.terminal_requested = false;
                    }
                    _ => tab.notice = "unexpected editor response".into(),
                }
            }
        }
        self.persist_chrome_preferences(outcome);
    }
    pub(crate) fn tick_investigation(&mut self, now: Instant, outcome: &mut ClientShellInput) {
        self.reconcile_investigation_boot(outcome);
        self.refresh_after_upload(outcome);
        let editor_request = self
            .workbench
            .editor
            .as_mut()
            .filter(|e| !e.awaiting && e.due <= now)
            .and_then(|e| {
                self.workbench
                    .tabs
                    .iter()
                    .any(|t| t.id == e.id && t.endpoint == self.active_endpoint_id)
                    .then(|| {
                        e.awaiting = true;
                        (e.id, e.pane.clone())
                    })
            });
        if let Some((id, pane)) = editor_request {
            if !self.push_endpoint_method_with_kind(
                Method::PaneGet(PaneTarget {
                    pane_id: pane.clone(),
                }),
                PendingEndpointKind::Investigation(FileRequest::EditorCheck { id, pane }),
                outcome,
            ) {
                if let Some(editor) = self.workbench.editor.as_mut() {
                    editor.awaiting = false;
                    editor.due = now + Duration::from_secs(2);
                }
            }
        }
        self.restore_investigations(outcome);
        self.resume_stalled_investigation(outcome);
        if self.workbench.terminal_requested {
            return;
        }
        if let Some(tab) = self.investigation_mut() {
            if tab.content_search && tab.cancel_generation != tab.search_generation {
                if let Some(params) = tab.search_params() {
                    tab.cancel_generation = tab.search_generation;
                    outcome.actions.push(ClientShellAction::FileSearchCancel {
                        boot_id: tab.boot.clone(),
                        params: FilesCancelParams {
                            search_id: params.search_id,
                            generation: params.generation,
                        },
                    });
                }
            }
        }
        self.pump_investigation_prepare(outcome);
        let Some(tab) = self.investigation_mut() else {
            return;
        };
        if tab.content_search && tab.search_due.is_some_and(|due| due <= now) {
            tab.search_due = None;
            if let Some(params) = tab.search_params() {
                tab.searching = true;
                let id = tab.id;
                let generation = tab.search_generation;
                let sent = self.push_endpoint_method_with_kind(
                    Method::FilesSearch(params),
                    PendingEndpointKind::Investigation(FileRequest::Search { id, generation }),
                    outcome,
                );
                if !sent {
                    if let Some(tab) = self.investigation_mut() {
                        tab.searching = false;
                        tab.notice = "content search is unavailable on this server".into();
                    }
                }
                outcome.repaint = true;
            }
        }
        let document_sent = self.poll_investigation_document(now, outcome);
        self.poll_investigation_listing(now, document_sent, outcome);
    }
    /// Returns whether a read went out, so the listing poll does not start a
    /// second request in the same tick.
    fn poll_investigation_document(
        &mut self,
        now: Instant,
        outcome: &mut ClientShellInput,
    ) -> bool {
        let Some(tab) = self.investigation_mut() else {
            return false;
        };
        // Off-host reads cross ssh or docker, so they are rarer; and one is
        // never started behind another request, which the endpoint would
        // refuse as busy and starve the user's own actions.
        let offhost = tab.browser.location.is_some();
        let interval = if offhost {
            OFFHOST_POLL_INTERVAL
        } else {
            HOST_POLL_INTERVAL
        };
        if now.duration_since(tab.last_poll) < interval {
            return false;
        }
        if offhost && !self.pending_requests.is_empty() {
            return false;
        }
        let Some(tab) = self.investigation_mut() else {
            return false;
        };
        tab.last_poll = now;
        let (workspace, location, preferred) = (
            tab.browser.workspace_id.clone(),
            tab.browser.location.clone(),
            tab.browser.location_pane.clone(),
        );
        let Some(document) = tab
            .document()
            .filter(|d| !tab.pending_reads.contains(&d.path))
        else {
            return false;
        };
        {
            let path = document.path.clone();
            let revision = document.loaded.then(|| {
                document
                    .pending
                    .as_ref()
                    .map_or_else(|| document.revision.clone(), |(_, r)| r.clone())
            });
            let id = tab.id;
            tab.preview_generation = tab.preview_generation.wrapping_add(1);
            let generation = tab.preview_generation;
            if let Some(d) = tab.document_mut() {
                d.request_generation = generation;
            }
            let method = self.file_read_method(
                &workspace,
                &location,
                path.clone(),
                revision,
                true,
                preferred.as_deref(),
            );
            let method = match method {
                Ok(method) => method,
                Err(notice) => {
                    if let Some(tab) = self.investigation_mut() {
                        tab.notice = notice;
                    }
                    return false;
                }
            };
            if let Some(tab) = self.investigation_mut() {
                tab.pending_reads.insert(path.clone());
            }
            let pane = Self::method_location_pane(&method);
            let sent = self.push_endpoint_method_with_kind(
                method,
                PendingEndpointKind::Investigation(FileRequest::Read {
                    id,
                    path,
                    generation,
                    prepare: false,
                }),
                outcome,
            );
            if sent {
                self.remember_location_pane(pane);
            } else if let Some(tab) = self.investigation_mut() {
                tab.pending_reads.clear();
            }
            sent
        }
    }
    /// Re-lists the root and the shown expanded directories in sweeps: when
    /// the interval is due a sweep queues them all, and each is asked for as
    /// soon as the previous one is answered. Why not concurrently, even on the
    /// host: the server refuses reads beyond its slot pool as busy, and a deep
    /// tree would ask for all of them at once. Off-host it is also the
    /// endpoint's one-request rule. Why not one folder per interval: a tree of
    /// n folders would then take n intervals to catch up.
    fn poll_investigation_listing(
        &mut self,
        now: Instant,
        document_sent: bool,
        outcome: &mut ClientShellInput,
    ) {
        if document_sent {
            return;
        }
        let Some(tab) = self.investigation() else {
            return;
        };
        let offhost = tab.browser.location.is_some();
        let (interval, index_interval) = if offhost {
            (OFFHOST_POLL_INTERVAL, OFFHOST_INDEX_INTERVAL)
        } else {
            (HOST_POLL_INTERVAL, HOST_INDEX_INTERVAL)
        };
        let (queue_empty, index_due, busy) = (
            tab.browser.refresh.queue.is_empty(),
            tab.browser.refresh.index_due,
            tab.browser.refresh.busy(),
        );
        let timer_due = now.duration_since(tab.last_list_poll) >= interval;
        if !timer_due && queue_empty && !index_due {
            return;
        }
        if offhost && !self.pending_requests.is_empty() {
            return;
        }
        if busy {
            // A reconnect drops the pending requests without answering them.
            if self.pending_requests.values().any(|pending| {
                matches!(
                    pending.kind,
                    PendingEndpointKind::FileViewer(
                        FileViewerRequest::Refresh { .. } | FileViewerRequest::IndexRefresh { .. }
                    )
                )
            }) {
                return;
            }
            if let Some(tab) = self.investigation_mut() {
                tab.browser.refresh.in_flight.clear();
                tab.browser.refresh.index_in_flight = false;
            }
        }
        if timer_due && queue_empty {
            if let Some(tab) = self.investigation_mut() {
                tab.last_list_poll = now;
                tab.browser.refresh.queue = tab.browser.refresh_targets().into();
            }
        }
        while let Some(path) = self
            .investigation_mut()
            .and_then(|tab| tab.browser.refresh.queue.pop_front())
        {
            let shown = self
                .investigation()
                .is_some_and(|tab| tab.browser.refresh_targets().contains(&path));
            if shown {
                if !self.request_file_viewer_refresh(path, outcome) {
                    if let Some(tab) = self.investigation_mut() {
                        tab.browser.refresh.queue.clear();
                    }
                }
                return;
            }
        }
        let Some(tab) = self.investigation_mut() else {
            return;
        };
        let refresh = &mut tab.browser.refresh;
        if !refresh.index_due {
            return;
        }
        let allowed = refresh.index_forced
            || refresh
                .last_index_refresh
                .is_none_or(|last| now.duration_since(last) >= index_interval);
        if !allowed {
            return;
        }
        refresh.index_due = false;
        refresh.index_forced = false;
        // Dropped, not kept due, while the index is loading or absent: the
        // first answer is as new as a refresh would be.
        if matches!(
            tab.browser.index,
            crate::client::shell::file_viewer::model::FileIndex::Ready { .. }
                | crate::client::shell::file_viewer::model::FileIndex::Unavailable { .. }
        ) {
            tab.browser.refresh.last_index_refresh = Some(now);
            self.request_file_viewer_index_refresh(outcome);
        }
    }
    /// Starts a sweep now, asks for the index whatever its rate limit, and
    /// reads the open document at once. Why not `schedule_search`: it would
    /// discard the search results and replacement plan; `refresh content
    /// search` in the menu does that on purpose.
    pub(in crate::client::shell) fn refresh_investigation_files(&mut self) {
        let Some(tab) = self.investigation_mut() else {
            return;
        };
        let targets = tab.browser.refresh_targets();
        tab.browser.refresh.queue = targets.into();
        tab.browser.refresh.index_due = true;
        tab.browser.refresh.index_forced = true;
        tab.last_list_poll = Instant::now();
        tab.last_poll = due_poll_time();
    }
    /// Resumes a tab that stopped for lack of a pane in its location, once one
    /// is running again: the stopped listings are requested again, one per tick.
    fn resume_stalled_investigation(&mut self, outcome: &mut ClientShellInput) {
        let Some(tab) = self.investigation() else {
            return;
        };
        let Some(location) = tab.browser.location.clone() else {
            return;
        };
        if self
            .file_location_pane(
                tab.browser.workspace_id.as_deref(),
                &location,
                tab.browser.location_pane.as_deref(),
            )
            .is_none()
        {
            return;
        }
        let stalled_notice = Self::no_pane_notice(&location);
        // Sorted so one directory is asked per tick in a stable order.
        let stalled = tab
            .browser
            .listing_retry
            .iter()
            .filter(|path| {
                matches!(
                    tab.browser.directories.get(*path),
                    Some(crate::client::shell::file_viewer::model::DirectoryState::Failed(_))
                )
            })
            .min()
            .cloned();
        if tab.notice == stalled_notice {
            if let Some(tab) = self.investigation_mut() {
                tab.notice.clear();
            }
        }
        if !self.pending_requests.is_empty() {
            return;
        }
        match stalled {
            Some(path) => self.request_file_viewer_listing(path, outcome),
            None => self.retry_file_viewer_index(outcome),
        }
    }
    fn restore_investigations(&mut self, outcome: &mut ClientShellInput) {
        if self.workbench.saved.is_empty()
            || self.snapshot.is_none()
            || !self.endpoint_is_online(&self.active_endpoint_id)
        {
            return;
        }
        if !self.supports_endpoint_method(&Method::FileRead(FileReadParams {
            path: String::new(),
            revision: None,
        })) {
            return;
        }
        let endpoint = self.active_endpoint_id.storage_key();
        let workspace = self
            .snapshot
            .as_ref()
            .and_then(|s| s.focused_workspace_id.clone());
        // An off-host entry waits for a pane of its location; it must not
        // hold back the host entries saved after it.
        let pane_directory = self.supports_pane_directory();
        let Some(position) = self.workbench.saved.iter().position(|s| {
            s.endpoint == endpoint
                && s.workspace == workspace
                && s.location.as_ref().is_none_or(|location| {
                    pane_directory
                        && self
                            .file_location_pane(workspace.as_deref(), location, None)
                            .is_some()
                })
        }) else {
            return;
        };
        // Why not retry blindly: opening an endpoint without directory.list
        // would raise an "Action unavailable" notice on every tick.
        if !self.supports_endpoint_method(&Method::DirectoryList(DirectoryListParams {
            workspace_id: None,
            pane_id: None,
            path: None,
            trust_repository: false,
        })) {
            return;
        }
        let location = self.workbench.saved[position].location.clone();
        let previous = (
            self.workbench.active,
            self.workbench.visible,
            self.workbench.files_focused,
        );
        // Why kept too: showing the restored tab adopts its workspace's
        // placement, while the shown region may belong to another workspace.
        let previous_placement = self.workbench.placement.save();
        let tabs_before = self.workbench.tabs.len();
        // Why a live tab of the workspace wins over the entry: it carries
        // the placement chosen since the entry was saved.
        let workspace_has_tab =
            self.workbench.tabs.iter().any(|t| {
                t.endpoint == self.active_endpoint_id && t.browser.workspace_id == workspace
            });
        let saved_root = self.workbench.saved[position].root.clone();
        self.open_file_viewer_in(workspace, Some(location), Some(saved_root), outcome);
        // Consumed only once a tab exists; a failed open keeps the entry for the next tick.
        if self.workbench.tabs.len() == tabs_before {
            return;
        }
        let saved = self.workbench.saved.remove(position);
        let Some(tab) = self.workbench.tabs.last_mut() else {
            return;
        };
        tab.horizontal = saved.horizontal;
        if !workspace_has_tab {
            tab.placement = saved.placement;
        }
        tab.content_search = saved.content_search && tab.browser.location.is_none();
        tab.query = saved.query.as_str().into();
        tab.include = saved.include.as_str().into();
        tab.exclude = saved.exclude.as_str().into();
        tab.regex = saved.regex;
        tab.case_sensitive = saved.case_sensitive;
        tab.whole_word = saved.whole_word;
        tab.ignored = saved.ignored;
        tab.documents = saved
            .files
            .into_iter()
            .map(|s| {
                let mut d = Document::new(s.path);
                d.cursor = s.cursor;
                d.scroll = s.scroll;
                d.horizontal = s.horizontal;
                d.restored_scroll = true;
                d
            })
            .collect();
        tab.active_file = saved.active_file.filter(|i| *i < tab.documents.len());
        tab.schedule_search();
        self.workbench.active = previous.0;
        self.workbench.visible = previous.1;
        self.workbench.files_focused = previous.2;
        // Why compared first: restoring also forgets the narrow fallback of
        // the region still on screen.
        if self.workbench.placement.save() != previous_placement {
            self.workbench.placement =
                FilesPlacement::restore(previous_placement.unwrap_or_default());
        }
    }
}

impl ClientShellState {
    pub(in crate::client::shell) fn pump_investigation_prepare(
        &mut self,
        outcome: &mut ClientShellInput,
    ) {
        let Some(tab) = self.investigation_mut() else {
            return;
        };
        if !matches!(tab.dialog, Some(Dialog::Replace { .. })) {
            tab.prepare_queue.clear();
            return;
        }
        let mut requests = Vec::new();
        while tab.prepare_pending < 2 {
            let Some(path) = tab.prepare_queue.pop_front() else {
                break;
            };
            tab.prepare_pending += 1;
            requests.push((tab.id, tab.prepare_generation, path));
        }
        let (workspace, location, preferred) = (
            tab.browser.workspace_id.clone(),
            tab.browser.location.clone(),
            tab.browser.location_pane.clone(),
        );
        for (id, generation, path) in requests {
            let method = self.file_read_method(
                &workspace,
                &location,
                path.clone(),
                None,
                false,
                preferred.as_deref(),
            );
            let sent = method.is_ok_and(|method| {
                self.push_endpoint_method_with_kind(
                    method,
                    PendingEndpointKind::Investigation(FileRequest::Read {
                        id,
                        path,
                        generation,
                        prepare: true,
                    }),
                    outcome,
                )
            });
            if !sent {
                if let Some(tab) = self.investigation_mut() {
                    tab.prepare_pending = tab.prepare_pending.saturating_sub(1);
                    tab.prepare_queue.clear();
                    tab.prepare_generation = tab.prepare_generation.wrapping_add(1);
                    tab.prepare_pending = 0;
                    tab.plan.clear();
                    tab.dialog = None;
                    tab.notice =
                        "could not prepare every selected file; no replacement was applied".into();
                }
            }
        }
    }
}

impl ClientShellState {
    fn reconcile_investigation_boot(&mut self, outcome: &mut ClientShellInput) {
        let Some(boot) = self
            .endpoint_boot_id(&self.active_endpoint_id)
            .map(str::to_owned)
        else {
            return;
        };
        for tab in self.workbench.tabs.iter_mut().filter(|t| {
            t.endpoint == self.active_endpoint_id
                && (t.boot != boot || t.connection_generation != self.active_snapshot_generation)
        }) {
            tab.boot = boot.clone();
            tab.connection_generation = self.active_snapshot_generation;
            tab.dialog = None;
            tab.plan.clear();
            tab.undo.clear();
            tab.prepare_queue.clear();
            tab.prepare_pending = 0;
            tab.prepare_generation = tab.prepare_generation.wrapping_add(1);
            tab.pending_reads.clear();
            tab.operation_pending = false;
            tab.searching = false;
            tab.schedule_search();
            for document in tab.documents.iter_mut().chain(tab.preview.iter_mut()) {
                document.revision.clear();
            }
            tab.last_poll = due_poll_time();
            tab.notice = "server reconnected; file state reloading".into();
            outcome.repaint = true;
        }
        if self
            .workbench
            .clipboard
            .as_ref()
            .is_some_and(|c| c.endpoint == self.active_endpoint_id && c.boot != boot)
        {
            self.workbench.clipboard = None;
        }
    }
}
