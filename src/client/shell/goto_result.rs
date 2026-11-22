//! The full view of a pane's request and result, opened from Go to.

use super::goto_preview::{GotoPreviewRead, PreviewBody, VIEWER_LINES};
use super::*;

#[derive(Debug)]
pub(crate) struct ClientGotoResultOverlay {
    /// The picker as it was, so esc returns to the same row, query, and filter.
    pub(super) navigator: ClientNavigatorOverlay,
    pub(super) endpoint_id: ClientEndpointId,
    pub(super) pane_id: String,
    /// The picker row it opened from, for its name and state.
    pub(super) row: ClientNavigatorRow,
    pub(super) read: GotoPreviewRead,
    pub(super) pending: bool,
    pub(super) body: PreviewBody,
    pub(super) scroll: usize,
}

impl ClientShellState {
    /// Opens the full view of the selected pane. Other rows have no result.
    pub(super) fn open_goto_result(&mut self) -> bool {
        let row = match self.overlay.as_ref() {
            Some(ClientShellOverlay::Navigator(navigator)) => {
                let rows = render::client_navigator_rows(
                    &self.endpoints,
                    &self.active_endpoint_id,
                    navigator,
                );
                super::aggregate_navigation::navigator_selected_index(&rows, navigator)
                    .and_then(|index| rows.into_iter().nth(index))
            }
            _ => None,
        };
        let Some(row) = row.filter(|row| !row.stale) else {
            return false;
        };
        let ClientNavigatorTarget::Pane {
            endpoint_id,
            pane_id,
        } = row.target.clone()
        else {
            return false;
        };
        let Some(ClientShellOverlay::Navigator(navigator)) = self.overlay.take() else {
            return false;
        };
        let task = navigator.tasks.task(&endpoint_id, &pane_id);
        let read = match task {
            Some(task) if task.state.is_finished() => GotoPreviewRead::Result {
                pane_id: pane_id.clone(),
                task_id: task.task_id.clone(),
            },
            _ => GotoPreviewRead::Screen {
                pane_id: pane_id.clone(),
                lines: VIEWER_LINES,
            },
        };
        // Why reuse: a result does not change, and the preview read it whole.
        let body = match (&read, navigator.tasks.preview(&endpoint_id, &pane_id)) {
            (GotoPreviewRead::Result { task_id, .. }, Some(preview))
                if preview.task_id() == Some(task_id.as_str())
                    && matches!(preview.body, PreviewBody::Text { .. }) =>
            {
                preview.body.clone()
            }
            _ => PreviewBody::Loading,
        };
        self.overlay = Some(ClientShellOverlay::GotoResult(Box::new(
            ClientGotoResultOverlay {
                navigator,
                endpoint_id,
                pane_id,
                row,
                read,
                pending: false,
                body,
                scroll: 0,
            },
        )));
        true
    }

    /// Back to the picker, on the row the view opened from.
    pub(super) fn close_goto_result(&mut self) {
        if let Some(ClientShellOverlay::GotoResult(viewer)) = self.overlay.take() {
            self.overlay = Some(ClientShellOverlay::Navigator(viewer.navigator));
        }
    }

    pub(super) fn scroll_goto_result(&mut self, delta: isize) {
        let max = self.hits.goto_result_max_scroll;
        if let Some(ClientShellOverlay::GotoResult(viewer)) = self.overlay.as_mut() {
            viewer.scroll = viewer.scroll.saturating_add_signed(delta).min(max);
        }
    }

    pub(super) fn route_goto_result_key(
        &mut self,
        key: &crate::input::TerminalKey,
        outcome: &mut ClientShellInput,
    ) -> bool {
        if !matches!(self.overlay, Some(ClientShellOverlay::GotoResult(_))) {
            return false;
        }
        let (code, modifiers) = crate::config::normalize_key_combo((key.code, key.modifiers));
        let page = self.hits.goto_result_viewport.max(1) as isize;
        let control = modifiers == crossterm::event::KeyModifiers::CONTROL;
        match code {
            KeyCode::Esc => self.close_goto_result(),
            KeyCode::Enter => {
                self.close_goto_result();
                self.accept_navigator_selection(outcome);
            }
            KeyCode::Down | KeyCode::Char('j') if !control => self.scroll_goto_result(1),
            KeyCode::Up | KeyCode::Char('k') if !control => self.scroll_goto_result(-1),
            KeyCode::PageDown => self.scroll_goto_result(page),
            KeyCode::PageUp => self.scroll_goto_result(-page),
            KeyCode::Char('d') if control => self.scroll_goto_result(page / 2),
            KeyCode::Char('u') if control => self.scroll_goto_result(-page / 2),
            KeyCode::Home | KeyCode::Char('g') if !control => self.scroll_goto_result(isize::MIN),
            KeyCode::End | KeyCode::Char('G') if !control => self.scroll_goto_result(isize::MAX),
            KeyCode::Char('y') if !control => {
                if let Some(ClientShellOverlay::GotoResult(viewer)) = self.overlay.as_ref() {
                    if let PreviewBody::Text { text, .. } = &viewer.body {
                        outcome
                            .actions
                            .push(ClientShellAction::ClipboardWrite(text.clone().into_bytes()));
                        self.show_copy_feedback(std::time::Instant::now());
                    }
                }
            }
            _ => return true,
        }
        outcome.repaint = true;
        true
    }
}
