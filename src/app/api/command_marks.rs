//! `pane.prompt_motion` and `pane.command_output_range`: shell command
//! boundaries of a pane, for clients that move or select by command.

use crate::api::schema::{
    PaneCommandOutputRangeParams, PanePromptDirection, PanePromptMotionParams, PaneTextPoint,
    PaneTextRange, ResponseResult,
};
use crate::app::App;
use crate::pane::{CommandMarkError, PromptMotion};

use super::responses::{encode_error, encode_success};

fn pane_not_found(id: String, pane_id: &str) -> String {
    encode_error(id, "pane_not_found", format!("pane {pane_id} not found"))
}

fn stale_content(id: String) -> String {
    encode_error(id, "stale_content", "pane content changed")
}

/// Why a rejection and not an unchanged cursor: a shell without marks makes
/// every command key a no-op, and the client has no other way to say why.
fn unmarked(id: String) -> String {
    encode_error(
        id,
        "prompt_marks_unavailable",
        "the shell in this pane does not mark its prompts (OSC 133)",
    )
}

/// An odd revision is a write in progress, as in `pane.copy_motion`.
fn is_stale(expected: Option<u64>, current: u64) -> bool {
    expected.is_some_and(|revision| revision != current || !current.is_multiple_of(2))
}

fn public_point(point: crate::pane::TerminalTextPoint) -> PaneTextPoint {
    PaneTextPoint {
        row: point.row,
        col: point.col,
    }
}

impl App {
    pub(super) fn handle_pane_prompt_motion(
        &mut self,
        id: String,
        params: PanePromptMotionParams,
    ) -> String {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(runtime) =
            self.state
                .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)
        else {
            return pane_not_found(id, &params.pane_id);
        };
        let before = runtime.content_seq();
        if is_stale(params.content_revision, before) {
            return stale_content(id);
        }
        let motion = match params.direction {
            PanePromptDirection::Previous => PromptMotion::Previous,
            PanePromptDirection::Next => PromptMotion::Next,
        };
        let target = runtime.prompt_motion_target(params.cursor.row, motion);
        let after = runtime.content_seq();
        if params.content_revision.is_some() && after != before {
            return stale_content(id);
        }
        let cursor = match target {
            Ok(target) => public_point(target),
            // The first and the last prompt are where the motion stops.
            Err(CommandMarkError::NotFound) => params.cursor,
            Err(CommandMarkError::Unmarked) => return unmarked(id),
        };
        encode_success(
            id,
            ResponseResult::PanePromptMotion {
                pane_id: params.pane_id,
                cursor,
                content_revision: after,
            },
        )
    }

    pub(super) fn handle_pane_command_output_range(
        &mut self,
        id: String,
        params: PaneCommandOutputRangeParams,
    ) -> String {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(runtime) =
            self.state
                .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)
        else {
            return pane_not_found(id, &params.pane_id);
        };
        let before = runtime.content_seq();
        if is_stale(params.content_revision, before) {
            return stale_content(id);
        }
        let range = runtime.command_output_range(params.cursor.row);
        let after = runtime.content_seq();
        if params.content_revision.is_some() && after != before {
            return stale_content(id);
        }
        match range {
            Ok((start, end)) => encode_success(
                id,
                ResponseResult::PaneCommandOutputRange {
                    pane_id: params.pane_id,
                    range: PaneTextRange {
                        start: public_point(start),
                        end: public_point(end),
                    },
                    content_revision: after,
                },
            ),
            Err(CommandMarkError::NotFound) => encode_error(
                id,
                "command_output_not_found",
                "the command at the cursor has no output",
            ),
            Err(CommandMarkError::Unmarked) => unmarked(id),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{ErrorResponse, SuccessResponse};
    use crate::config::Config;
    use crate::workspace::Workspace;

    /// Two finished commands and the prompt waiting for input:
    /// rows 0-2 `first`, rows 3-5 `second`, row 6 the waiting prompt.
    const MARKED_SHELL: &[u8] =
        b"\x1b]133;A\x07$ \x1b]133;B\x07first\r\n\x1b]133;C\x07a1\r\na2\r\n\
\x1b]133;A\x07$ \x1b]133;B\x07second\r\n\x1b]133;C\x07b1\r\nb2 tail\r\n\
\x1b]133;A\x07$ \x1b]133;B\x07";
    const UNMARKED_SHELL: &[u8] = b"$ first\r\na1\r\n$ ";

    fn app_with_pane_output(bytes: &[u8]) -> (App, String) {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("commands")];
        app.state.ensure_test_terminals();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let public_pane_id = app.public_pane_id(0, pane_id).unwrap();
        app.state.insert_test_runtime(
            pane_id,
            crate::terminal::TerminalRuntime::test_with_scrollback_bytes(20, 10, 1000, bytes),
        );
        (app, public_pane_id)
    }

    fn prompt_motion(
        app: &mut App,
        pane_id: &str,
        row: u32,
        col: u16,
        direction: PanePromptDirection,
    ) -> String {
        app.handle_pane_prompt_motion(
            "req".into(),
            PanePromptMotionParams {
                pane_id: pane_id.to_owned(),
                cursor: PaneTextPoint { row, col },
                direction,
                content_revision: None,
            },
        )
    }

    fn command_output_range(app: &mut App, pane_id: &str, row: u32) -> String {
        app.handle_pane_command_output_range(
            "req".into(),
            PaneCommandOutputRangeParams {
                pane_id: pane_id.to_owned(),
                cursor: PaneTextPoint { row, col: 0 },
                content_revision: None,
            },
        )
    }

    fn success(response: &str) -> ResponseResult {
        serde_json::from_str::<SuccessResponse>(response)
            .unwrap()
            .result
    }

    fn error_code(response: &str) -> String {
        serde_json::from_str::<ErrorResponse>(response)
            .unwrap()
            .error
            .code
    }

    #[tokio::test]
    async fn prompt_motion_moves_to_the_first_cell_of_the_previous_prompt() {
        let (mut app, pane_id) = app_with_pane_output(MARKED_SHELL);

        let response = prompt_motion(&mut app, &pane_id, 5, 4, PanePromptDirection::Previous);

        assert_eq!(
            success(&response),
            ResponseResult::PanePromptMotion {
                pane_id,
                cursor: PaneTextPoint { row: 3, col: 0 },
                content_revision: 0,
            }
        );
    }

    #[tokio::test]
    async fn prompt_motion_moves_to_the_next_prompt() {
        let (mut app, pane_id) = app_with_pane_output(MARKED_SHELL);

        let response = prompt_motion(&mut app, &pane_id, 1, 0, PanePromptDirection::Next);

        assert_eq!(
            success(&response),
            ResponseResult::PanePromptMotion {
                pane_id,
                cursor: PaneTextPoint { row: 3, col: 0 },
                content_revision: 0,
            }
        );
    }

    #[tokio::test]
    async fn prompt_motion_keeps_the_cursor_at_the_first_prompt() {
        let (mut app, pane_id) = app_with_pane_output(MARKED_SHELL);

        let response = prompt_motion(&mut app, &pane_id, 0, 3, PanePromptDirection::Previous);

        assert_eq!(
            success(&response),
            ResponseResult::PanePromptMotion {
                pane_id,
                cursor: PaneTextPoint { row: 0, col: 3 },
                content_revision: 0,
            }
        );
    }

    #[tokio::test]
    async fn command_output_range_returns_the_output_of_the_command_at_the_cursor() {
        let (mut app, pane_id) = app_with_pane_output(MARKED_SHELL);

        let response = command_output_range(&mut app, &pane_id, 1);

        assert_eq!(
            success(&response),
            ResponseResult::PaneCommandOutputRange {
                pane_id,
                range: PaneTextRange {
                    start: PaneTextPoint { row: 1, col: 0 },
                    end: PaneTextPoint { row: 2, col: 1 },
                },
                content_revision: 0,
            }
        );
    }

    #[tokio::test]
    async fn command_output_range_at_the_waiting_prompt_returns_the_output_directly_above_it() {
        let (mut app, pane_id) = app_with_pane_output(MARKED_SHELL);

        let response = command_output_range(&mut app, &pane_id, 6);

        assert_eq!(
            success(&response),
            ResponseResult::PaneCommandOutputRange {
                pane_id,
                range: PaneTextRange {
                    start: PaneTextPoint { row: 4, col: 0 },
                    end: PaneTextPoint { row: 5, col: 6 },
                },
                content_revision: 0,
            }
        );
    }

    #[tokio::test]
    async fn a_shell_without_prompt_marks_rejects_both_methods_with_the_reason() {
        let (mut app, pane_id) = app_with_pane_output(UNMARKED_SHELL);

        assert_eq!(
            error_code(&prompt_motion(
                &mut app,
                &pane_id,
                2,
                0,
                PanePromptDirection::Previous
            )),
            "prompt_marks_unavailable"
        );
        assert_eq!(
            error_code(&command_output_range(&mut app, &pane_id, 2)),
            "prompt_marks_unavailable"
        );
    }

    #[tokio::test]
    async fn a_command_without_output_is_reported_as_not_found() {
        let (mut app, pane_id) = app_with_pane_output(
            b"\x1b]133;A\x07$ \x1b]133;B\x07quiet\r\n\x1b]133;C\x07\
\x1b]133;A\x07$ \x1b]133;B\x07second\r\n\x1b]133;C\x07b1\r\n\x1b]133;A\x07$ \x1b]133;B\x07",
        );

        assert_eq!(
            error_code(&command_output_range(&mut app, &pane_id, 0)),
            "command_output_not_found"
        );
    }

    #[tokio::test]
    async fn both_methods_reject_a_content_revision_the_pane_has_moved_past() {
        let (mut app, pane_id) = app_with_pane_output(MARKED_SHELL);
        let internal_pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let runtime = app
            .state
            .runtime_for_pane_in_workspace(&app.terminal_runtimes, 0, internal_pane_id)
            .unwrap();
        let revision = runtime.content_seq();
        runtime.test_process_pty_bytes(b"ls\r\n");

        let motion = app.handle_pane_prompt_motion(
            "req".into(),
            PanePromptMotionParams {
                pane_id: pane_id.clone(),
                cursor: PaneTextPoint { row: 5, col: 0 },
                direction: PanePromptDirection::Previous,
                content_revision: Some(revision),
            },
        );
        let range = app.handle_pane_command_output_range(
            "req".into(),
            PaneCommandOutputRangeParams {
                pane_id,
                cursor: PaneTextPoint { row: 5, col: 0 },
                content_revision: Some(revision),
            },
        );

        assert_eq!(error_code(&motion), "stale_content");
        assert_eq!(error_code(&range), "stale_content");
    }

    #[tokio::test]
    async fn an_unknown_pane_is_reported_as_not_found() {
        let (mut app, _pane_id) = app_with_pane_output(MARKED_SHELL);

        assert_eq!(
            error_code(&command_output_range(&mut app, "w9:p9", 0)),
            "pane_not_found"
        );
    }
}
