//! The previews picker: lists the active server's running previews, opens
//! one in the browser, or stops it.

use crossterm::event::{MouseButton, MouseEventKind};

use super::*;
use crate::api::schema::{EmptyParams, Method, PreviewInfo, PreviewStopParams, ResponseResult};

impl ClientShellState {
    pub(super) fn open_previews(&mut self, outcome: &mut ClientShellInput) {
        self.push_endpoint_method_with_kind(
            Method::PreviewList(EmptyParams::default()),
            PendingEndpointKind::PreviewList,
            outcome,
        );
    }

    pub(super) fn handle_preview_endpoint_result(
        &mut self,
        kind: PendingEndpointKind,
        result: Result<ResponseResult, ClientShellEndpointError>,
    ) -> bool {
        match (kind, result) {
            (PendingEndpointKind::PreviewList, Ok(ResponseResult::PreviewList { previews })) => {
                self.overlay = Some(ClientShellOverlay::Previews(ClientPreviewsOverlay::new(
                    previews,
                )));
            }
            (
                PendingEndpointKind::PreviewStop { preview_id },
                Ok(ResponseResult::PreviewStopped { .. }),
            ) => self.forget_preview(&preview_id, None),
            // Someone else stopped it first: the row is stale either way.
            (PendingEndpointKind::PreviewStop { preview_id }, Err(error))
                if error.code.as_deref() == Some("preview_not_found") =>
            {
                self.forget_preview(&preview_id, None)
            }
            (PendingEndpointKind::PreviewStop { preview_id }, Err(error)) => {
                self.forget_preview(&preview_id, Some(error.message));
            }
            // The endpoint notice already says why the list failed.
            (_, _) => {}
        }
        true
    }

    /// Drops a stopped preview's row, or shows why stopping it failed.
    fn forget_preview(&mut self, preview_id: &str, error: Option<String>) {
        let Some(ClientShellOverlay::Previews(previews)) = self.overlay.as_mut() else {
            return;
        };
        previews.stopping = false;
        if let Some(error) = error {
            previews.error = Some(error);
            return;
        }
        previews.error = None;
        let Some(index) = previews
            .entries
            .iter()
            .position(|entry| entry.preview_id == preview_id)
        else {
            return;
        };
        previews.entries.remove(index);
        let filtered = previews.filtered_indices();
        previews.selected = filtered
            .iter()
            .copied()
            .find(|candidate| *candidate >= index)
            .or_else(|| filtered.last().copied())
            .unwrap_or(0);
    }

    pub(super) fn move_previews_selection(&mut self, delta: isize) {
        let Some(ClientShellOverlay::Previews(previews)) = self.overlay.as_mut() else {
            return;
        };
        let filtered = previews.filtered_indices();
        if filtered.is_empty() {
            previews.selected = 0;
            return;
        }
        let current = filtered
            .iter()
            .position(|index| *index == previews.selected)
            .unwrap_or(0);
        let next = (current as isize + delta).clamp(0, filtered.len() as isize - 1) as usize;
        previews.selected = filtered[next];
    }

    fn selected_preview(&self) -> Option<&PreviewInfo> {
        let Some(ClientShellOverlay::Previews(previews)) = self.overlay.as_ref() else {
            return None;
        };
        previews
            .selected_entry_index()
            .and_then(|index| previews.entries.get(index))
    }

    /// Opens the selected preview in this machine's browser and closes the
    /// picker. Refused when the server runs elsewhere: its loopback is not
    /// this machine's.
    pub(super) fn submit_preview_open(&mut self, outcome: &mut ClientShellInput) {
        outcome.repaint = true;
        let Some(url) = self.selected_preview().map(|preview| preview.url.clone()) else {
            return;
        };
        if !self.preview_opens_here() {
            if let Some(ClientShellOverlay::Previews(previews)) = self.overlay.as_mut() {
                previews.error = Some(super::investigation::PREVIEW_ELSEWHERE.into());
            }
            return;
        }
        self.overlay = None;
        outcome.actions.push(ClientShellAction::OpenSafeWebUrl(url));
    }

    pub(super) fn submit_preview_stop(&mut self, outcome: &mut ClientShellInput) {
        outcome.repaint = true;
        let Some(preview_id) = self
            .selected_preview()
            .map(|preview| preview.preview_id.clone())
        else {
            return;
        };
        let Some(ClientShellOverlay::Previews(previews)) = self.overlay.as_mut() else {
            return;
        };
        if previews.stopping {
            return;
        }
        previews.stopping = true;
        previews.error = None;
        if !self.push_endpoint_method_with_kind(
            Method::PreviewStop(PreviewStopParams {
                preview_id: preview_id.clone(),
            }),
            PendingEndpointKind::PreviewStop { preview_id },
            outcome,
        ) {
            if let Some(ClientShellOverlay::Previews(previews)) = self.overlay.as_mut() {
                previews.stopping = false;
            }
        }
    }

    pub(super) fn insert_previews_overlay_text(&mut self, text: &str) -> bool {
        let Some(ClientShellOverlay::Previews(previews)) = self.overlay.as_mut() else {
            return false;
        };
        if !previews.search_focused {
            return false;
        }
        if previews.query.insert(text) {
            if let Some(first) = previews.filtered_indices().first().copied() {
                previews.selected = first;
            }
        }
        true
    }

    pub(super) fn route_previews_overlay_key(
        &mut self,
        key: &crate::input::TerminalKey,
        outcome: &mut ClientShellInput,
    ) -> bool {
        let Some(ClientShellOverlay::Previews(previews)) = self.overlay.as_mut() else {
            return false;
        };
        let search_focused = previews.search_focused;
        if search_focused {
            if let Some(content_changed) = previews.query.handle_key(key) {
                if content_changed {
                    if let Some(first) = previews.filtered_indices().first().copied() {
                        previews.selected = first;
                    }
                }
                outcome.repaint = true;
                return true;
            }
        }
        let (code, modifiers) = crate::config::normalize_key_combo((key.code, key.modifiers));
        match code {
            KeyCode::Esc => self.overlay = None,
            KeyCode::Enter => self.submit_preview_open(outcome),
            KeyCode::Delete => self.submit_preview_stop(outcome),
            KeyCode::Up => self.move_previews_selection(-1),
            KeyCode::Down => self.move_previews_selection(1),
            KeyCode::Char('n' | 'p') if modifiers == crossterm::event::KeyModifiers::CONTROL => {
                self.move_previews_selection(if code == KeyCode::Char('n') { 1 } else { -1 });
            }
            KeyCode::Char('/') if !search_focused => previews.search_focused = true,
            _ => {}
        }
        outcome.repaint = true;
        true
    }

    /// Mouse on the picker: rows open, the stop button stops the selected
    /// row, the wheel moves the selection.
    pub(super) fn route_previews_mouse(
        &mut self,
        kind: MouseEventKind,
        point: (u16, u16),
        outcome: &mut ClientShellInput,
    ) -> bool {
        if !matches!(self.overlay, Some(ClientShellOverlay::Previews(_))) {
            return false;
        }
        match kind {
            MouseEventKind::ScrollUp => self.move_previews_selection(-1),
            MouseEventKind::ScrollDown => self.move_previews_selection(1),
            MouseEventKind::Down(MouseButton::Left) => {
                if super::contains(self.hits.overlay_cancel, point) {
                    self.overlay = None;
                } else if super::contains(self.hits.overlay_secondary, point) {
                    self.submit_preview_stop(outcome);
                } else if super::contains(self.hits.overlay_primary, point) {
                    self.submit_preview_open(outcome);
                } else if super::contains(self.hits.worktree_search, point) {
                    if let Some(ClientShellOverlay::Previews(previews)) = self.overlay.as_mut() {
                        previews.search_focused = true;
                    }
                } else if let Some((_, index)) = self
                    .hits
                    .worktree_rows
                    .iter()
                    .find(|(rect, _)| super::contains(*rect, point))
                    .copied()
                {
                    if let Some(ClientShellOverlay::Previews(previews)) = self.overlay.as_mut() {
                        previews.selected = index;
                    }
                }
            }
            _ => return true,
        }
        outcome.repaint = true;
        true
    }
}

#[cfg(test)]
mod tests;
