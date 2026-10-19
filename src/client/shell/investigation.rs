//! Client-owned investigation tabs. Server terminal identities are never repurposed.
mod input;
mod model;
mod operations;
mod render;
mod requests;
mod surface;
#[cfg(test)]
mod tests;
use super::*;
pub(super) use model::*;

impl ClientShellState {
    pub(crate) fn investigation_active(&self) -> bool {
        self.workbench.visible
            && self
                .workbench
                .active
                .and_then(|i| self.workbench.tabs.get(i))
                .is_some_and(|t| {
                    self.endpoint_boot_id(&self.active_endpoint_id) == Some(t.boot.as_str())
                        && t.endpoint == self.active_endpoint_id
                        && t.browser.workspace_id
                            == self
                                .snapshot
                                .as_ref()
                                .and_then(|s| s.focused_workspace_id.clone())
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
        self.mode = ClientShellMode::Terminal;
        self.workbench.tabs.push(tab);
        self.workbench.active = Some(self.workbench.tabs.len() - 1);
        self.workbench.visible = true;
    }
    pub(super) fn toggle_investigation(&mut self) -> bool {
        if self.investigation_active() {
            self.workbench.visible = false;
            return true;
        }
        let workspace = self
            .snapshot
            .as_ref()
            .and_then(|s| s.focused_workspace_id.as_ref());
        let recent = self
            .workbench
            .active
            .filter(|i| {
                self.workbench.tabs.get(*i).is_some_and(|t| {
                    t.endpoint == self.active_endpoint_id
                        && t.browser.workspace_id.as_ref() == workspace
                })
            })
            .or_else(|| {
                self.workbench.tabs.iter().rposition(|t| {
                    t.endpoint == self.active_endpoint_id
                        && t.browser.workspace_id.as_ref() == workspace
                })
            });
        if let Some(index) = recent {
            let pane = self.focused_pane_id();
            self.workbench.tabs[index].browser.pane_id = pane;
            self.mode = ClientShellMode::Terminal;
            self.workbench.active = Some(index);
            self.workbench.visible = true;
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
        if action == A::CloseTab && self.investigation_active() {
            if let Some(index) = self.workbench.active {
                self.workbench.tabs.remove(index);
                self.workbench.active = None;
                self.workbench.visible = false;
                self.persist_chrome_preferences(outcome);
            }
            return true;
        }
        let index = match action {
            A::SwitchTab(index) => index,
            A::PreviousTab | A::NextTab => {
                let current = if self.investigation_active() {
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
            self.workbench.visible = false;
            self.push_endpoint_method(
                crate::api::schema::Method::TabFocus(crate::api::schema::TabTarget {
                    tab_id: tab_id.clone(),
                }),
                outcome,
            );
        } else if let Some(&index) = investigations.get(index.saturating_sub(terminals.len())) {
            self.workbench.tabs[index].browser.pane_id = self.focused_pane_id();
            self.workbench.active = Some(index);
            self.workbench.visible = true;
        }
        outcome.repaint = true;
        true
    }
}
