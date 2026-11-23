//! The full view of a pane's request and result, opened from Go to.

use super::goto_preview::{GotoPreviewRead, GotoTasks, PreviewBody, VIEWER_LINES};
use super::*;
use crate::api::schema::TaskInfo;

#[derive(Debug)]
pub(crate) struct ClientGotoResultOverlay {
    /// The picker as it was, so esc returns to the same row, query, and filter.
    pub(super) navigator: ClientNavigatorOverlay,
    pub(super) endpoint_id: ClientEndpointId,
    pub(super) pane_id: String,
    /// The picker row it opened from, for its name and state.
    pub(super) row: ClientNavigatorRow,
    /// The task shown, fixed when the view chose what to read. Why not the
    /// picker's list: a refresh could pair a newer request with this result.
    pub(super) task: Option<TaskInfo>,
    /// `None` until the server's task list arrives, which decides whether
    /// the view reads the result or the screen.
    pub(super) read: Option<GotoPreviewRead>,
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
        let listed = navigator.tasks.listed(&endpoint_id)
            || !self.endpoint_advertises(&endpoint_id, "task.list");
        let (task, read, body) = if listed {
            let (task, read, body) = choose_full_view(&navigator.tasks, &endpoint_id, &pane_id);
            (task, Some(read), body)
        } else {
            (None, None, PreviewBody::Loading)
        };
        self.overlay = Some(ClientShellOverlay::GotoResult(Box::new(
            ClientGotoResultOverlay {
                navigator,
                endpoint_id,
                pane_id,
                row,
                task,
                read,
                pending: false,
                body,
                scroll: 0,
            },
        )));
        true
    }

    /// Chooses what the view reads once the task list is known.
    pub(super) fn choose_goto_result_read(&mut self) -> bool {
        let Some(ClientShellOverlay::GotoResult(viewer)) = self.overlay.as_mut() else {
            return false;
        };
        let (task, read, body) = choose_full_view(
            &viewer.navigator.tasks,
            &viewer.endpoint_id,
            &viewer.pane_id,
        );
        viewer.task = task;
        viewer.read = Some(read);
        viewer.body = body;
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
            // Why clamp first: a larger window lowers the limit, and the
            // first key would otherwise only undo the overshoot.
            viewer.scroll = viewer.scroll.min(max).saturating_add_signed(delta).min(max);
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
                // Why the stored pane: the picker's filter may no longer
                // list it.
                let Some(ClientShellOverlay::GotoResult(viewer)) = self.overlay.as_ref() else {
                    return true;
                };
                let endpoint_id = viewer.endpoint_id.clone();
                let pane_id = viewer.pane_id.clone();
                if self.focus_or_activate(
                    endpoint_id,
                    ClientEndpointFocusTarget::Pane(pane_id),
                    outcome,
                ) {
                    self.overlay = None;
                }
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

/// The task to show, what to read for it, and the text already at hand.
fn choose_full_view(
    tasks: &GotoTasks,
    endpoint_id: &ClientEndpointId,
    pane_id: &str,
) -> (Option<TaskInfo>, GotoPreviewRead, PreviewBody) {
    let task = tasks.task(endpoint_id, pane_id).cloned();
    let read = match &task {
        Some(task) if task.state.is_finished() => GotoPreviewRead::Result {
            pane_id: pane_id.to_owned(),
            task_id: task.task_id.clone(),
        },
        _ => GotoPreviewRead::Screen {
            pane_id: pane_id.to_owned(),
            lines: VIEWER_LINES,
        },
    };
    // Why reuse: a result does not change, and the preview read it whole.
    let body = match (&read, tasks.preview(endpoint_id, pane_id)) {
        (GotoPreviewRead::Result { task_id, .. }, Some(preview))
            if preview.task_id() == Some(task_id.as_str())
                && matches!(preview.body, PreviewBody::Text { .. }) =>
        {
            preview.body.clone()
        }
        _ => PreviewBody::Loading,
    };
    (task, read, body)
}
