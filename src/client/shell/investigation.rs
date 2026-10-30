//! Client-owned investigation tabs. Server terminal identities are never repurposed.
mod details;
mod drops;
mod input;
mod keys;
mod model;
mod operations;
mod placement;
mod regions;
mod render;
mod requests;
mod surface;
#[cfg(test)]
mod tests;
use super::*;
pub(crate) use drops::{ClientUploadQuestion, WorkbenchDropPlace, WorkbenchDropTarget};
pub(super) use model::*;
pub(super) use placement::{
    lenient, FilesDock, FilesPlacement, FilesPresentation, SavedFilesPlacement,
};

impl ClientShellState {
    pub(crate) fn investigation_active(&self) -> bool {
        // Why `as_deref` rather than cloning the id: `layout()` calls this
        // for every frame and surface-size query, which must not allocate.
        let focused_workspace = self
            .snapshot
            .as_deref()
            .and_then(|s| s.focused_workspace_id.as_deref());
        self.workbench.visible
            && self
                .workbench
                .active
                .and_then(|i| self.workbench.tabs.get(i))
                .is_some_and(|t| {
                    self.endpoint_boot_id(&self.active_endpoint_id) == Some(t.boot.as_str())
                        && t.endpoint == self.active_endpoint_id
                        && t.browser.workspace_id.as_deref() == focused_workspace
                })
    }
    pub(super) fn investigation(&self) -> Option<&Investigation> {
        self.investigation_active()
            .then(|| self.workbench.tabs.get(self.workbench.active?))
            .flatten()
    }
    pub(super) fn investigation_mut(&mut self) -> Option<&mut Investigation> {
        if !self.investigation_active() {
            return None;
        }
        self.workbench.tabs.get_mut(self.workbench.active?)
    }
    pub(super) fn promote_file_viewer(&mut self) {
        if !self.supports_endpoint_method(&crate::api::schema::Method::FileRead(
            crate::api::schema::FileReadParams {
                path: String::new(),
                revision: None,
            },
        )) {
            return;
        }
        let Some(ClientShellOverlay::FileViewer(viewer)) = self.overlay.take() else {
            return;
        };
        let boot = self
            .endpoint_boot_id(&self.active_endpoint_id)
            .unwrap_or_default()
            .to_owned();
        let mut tab = Investigation::new(*viewer, self.active_endpoint_id.clone(), boot);
        tab.connection_generation = self.active_snapshot_generation;
        // Why the shown placement when the workspace has no tab yet: it is
        // the reader's latest choice, and the saved one for this workspace
        // went with its last tab.
        tab.placement = self
            .workbench
            .tabs
            .iter()
            .find(|t| same_workspace(t, &tab))
            .map_or_else(|| self.workbench.placement.save(), |t| t.placement);
        self.mode = ClientShellMode::Terminal;
        self.workbench.tabs.push(tab);
        self.show_files(self.workbench.tabs.len() - 1);
    }
    /// Shows tab `index` and brings the files region forward, in the
    /// placement of its workspace.
    pub(super) fn show_files(&mut self, index: usize) {
        if let Some(saved) = self.workbench.tabs.get(index).map(|t| t.placement) {
            // Why compared first: restoring also forgets the narrow
            // fallback, which must survive reshowing the same workspace.
            if saved != self.workbench.placement.save() {
                self.workbench.placement = FilesPlacement::restore(saved.unwrap_or_default());
            }
        }
        self.workbench.active = Some(index);
        self.workbench.visible = true;
        self.workbench.files_focused = true;
    }
    /// Keeps the shown placement with every tab of the shown workspace, so
    /// its saved entries and a later reopen use it (Q8).
    pub(super) fn remember_files_placement(&mut self) {
        let Some(shown) = self
            .workbench
            .active
            .and_then(|i| self.workbench.tabs.get(i))
        else {
            return;
        };
        let (endpoint, workspace) = (shown.endpoint.clone(), shown.browser.workspace_id.clone());
        let saved = self.workbench.placement.save();
        for tab in &mut self.workbench.tabs {
            if tab.endpoint == endpoint && tab.browser.workspace_id == workspace {
                tab.placement = saved;
            }
        }
        // Why pending entries too: an off-host entry waiting for its pane is
        // restored after the live tabs may have closed, and would otherwise
        // bring back the placement from before this choice.
        let endpoint = endpoint.storage_key();
        for entry in &mut self.workbench.saved {
            if entry.endpoint == endpoint && entry.workspace == workspace {
                entry.placement = saved;
            }
        }
    }
    /// Hides the workbench of the focused workspace.
    pub(super) fn hide_files(&mut self) {
        self.workbench.visible = false;
        self.workbench.files_focused = false;
    }
    /// Brings the terminals forward. Full screen hides the workbench to do
    /// so; split keeps the files region beside them (Q6a), since it belongs
    /// to the workspace rather than to one terminal tab.
    pub(super) fn show_terminal_side(&mut self) {
        if !self.investigation_active() || self.investigation_full_screen() {
            self.workbench.visible = false;
        }
        self.workbench.files_focused = false;
    }
    /// Keys, pastes and drops go to the files workbench rather than the
    /// focused pane. Full screen always has focus; split has it once the
    /// files region was brought forward and until a terminal is.
    pub(super) fn files_has_focus(&self) -> bool {
        self.investigation_active()
            && (self.workbench.files_focused || self.investigation_full_screen())
    }
    pub(super) fn remove_investigation(&mut self, index: usize) {
        if index >= self.workbench.tabs.len() {
            return;
        }
        self.workbench.tabs.remove(index);
        match self.workbench.active {
            Some(active) if active == index => {
                self.workbench.active = None;
                self.hide_files();
            }
            Some(active) if active > index => self.workbench.active = Some(active - 1),
            _ => {}
        }
    }
    /// Steps open_files through hidden, shown with focus, and hidden again.
    /// A split region shown beside a focused terminal takes focus first
    /// (Q18a), so the key never hides what the reader was about to use.
    pub(super) fn toggle_investigation(&mut self, outcome: &mut ClientShellInput) -> bool {
        if self.files_has_focus() {
            self.hide_files();
            return true;
        }
        if self.investigation_active() {
            self.mode = ClientShellMode::Terminal;
            self.workbench.files_focused = true;
            outcome.repaint = true;
            return true;
        }
        let workspace = self
            .snapshot
            .as_ref()
            .and_then(|s| s.focused_workspace_id.clone());
        // The tab whose location is the focused pane's: a host shell and an
        // ssh shell of one workspace each have their own tab.
        let location = self.workspace_file_location(workspace.as_deref());
        let wanted = |t: &Investigation| {
            t.endpoint == self.active_endpoint_id
                && t.browser.workspace_id == workspace
                && t.browser.location == location
        };
        let recent = self
            .workbench
            .active
            .filter(|i| self.workbench.tabs.get(*i).is_some_and(wanted))
            .or_else(|| self.workbench.tabs.iter().rposition(wanted));
        if let Some(index) = recent {
            self.mode = ClientShellMode::Terminal;
            self.show_files(index);
            self.reroot_file_viewer_to_pane(workspace.as_deref(), outcome);
            return true;
        }
        false
    }
    pub(super) fn investigation_terminal_action(
        &mut self,
        action: crate::input::KeybindAction,
        outcome: &mut ClientShellInput,
    ) -> bool {
        use crate::input::KeybindAction as A;
        if self.files_region_action(action, outcome) {
            return true;
        }
        let Some(snapshot) = self.snapshot.as_ref() else {
            return false;
        };
        let workspace = snapshot.focused_workspace_id.clone();
        let terminals: Vec<_> = snapshot
            .tabs
            .iter()
            .filter(|t| Some(&t.workspace_id) == workspace.as_ref())
            .map(|t| t.tab_id.clone())
            .collect();
        let investigations: Vec<_> = self
            .workbench
            .tabs
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                t.endpoint == self.active_endpoint_id && t.browser.workspace_id == workspace
            })
            .map(|(i, _)| i)
            .collect();
        if investigations.is_empty() {
            return false;
        }
        // Why focus rather than visibility: beside a focused terminal the
        // key closes that terminal's tab, as it would with no files shown.
        if action == A::CloseTab && self.files_has_focus() {
            if let Some(index) = self.workbench.active {
                self.workbench.tabs.remove(index);
                self.workbench.active = None;
                self.hide_files();
                self.persist_chrome_preferences(outcome);
            }
            return true;
        }
        let index = match action {
            A::SwitchTab(index) => index,
            A::PreviousTab | A::NextTab => {
                let current = if self.files_has_focus() {
                    terminals.len()
                        + investigations
                            .iter()
                            .position(|i| Some(*i) == self.workbench.active)
                            .unwrap_or(0)
                } else {
                    terminals
                        .iter()
                        .position(|id| Some(id) == snapshot.focused_tab_id.as_ref())
                        .unwrap_or(0)
                };
                (current as isize + if action == A::NextTab { 1 } else { -1 })
                    .rem_euclid((terminals.len() + investigations.len()) as isize)
                    as usize
            }
            _ => return false,
        };
        if let Some(tab_id) = terminals.get(index) {
            self.show_terminal_side();
            self.push_endpoint_method(
                crate::api::schema::Method::TabFocus(crate::api::schema::TabTarget {
                    tab_id: tab_id.clone(),
                }),
                outcome,
            );
        } else if let Some(&index) = investigations.get(index.saturating_sub(terminals.len())) {
            self.show_files(index);
        }
        outcome.repaint = true;
        true
    }
}

/// Both tabs browse the same workspace of the same endpoint.
fn same_workspace(left: &Investigation, right: &Investigation) -> bool {
    left.endpoint == right.endpoint && left.browser.workspace_id == right.browser.workspace_id
}
