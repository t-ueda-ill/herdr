use super::*;
use crate::api::schema::{
    Method, PanePromptDirection, PaneTextPoint, PaneTextRange, ResponseResult,
};

/// Copy mode on a pane with twenty rows of history above a two-row viewport.
fn state_in_copy_mode() -> (ClientShellState, PaneTextPoint) {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.config.copy_on_select = false;
    state.set_snapshot(Box::new(snapshot()));
    let mut pane_surface = surface();
    pane_surface.panes[0].scroll = Some(crate::protocol::PaneSurfaceScrollMetrics {
        offset_from_bottom: 0,
        max_offset_from_bottom: 20,
        viewport_rows: 2,
    });
    state.set_pane_surface(pane_surface);
    state.compose(106, 20).expect("composed frame");
    let mut enter = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::CopyMode),
        &mut enter,
    );
    let origin = state.copy_mode.as_ref().expect("copy mode").cursor;
    (state, origin)
}

fn sent_request(input: &ClientShellInput) -> (String, &Method) {
    let [ClientShellAction::Endpoint { request, .. }] = &input.actions[..] else {
        panic!("expected exactly one endpoint request");
    };
    (request.id.clone(), &request.method)
}

fn rejection(code: &str) -> ClientShellEndpointError {
    ClientShellEndpointError {
        code: Some(code.to_owned()),
        message: "rejected".to_owned(),
    }
}

#[test]
fn bracket_keys_ask_the_server_for_the_previous_and_next_prompt() {
    let (mut state, origin) = state_in_copy_mode();

    let previous = state.handle_input_bytes(b"[");
    let (previous_id, method) = sent_request(&previous);
    assert!(matches!(
        method,
        Method::PanePromptMotion(params)
            if params.cursor == origin && params.direction == PanePromptDirection::Previous
    ));
    let prompt = PaneTextPoint { row: 12, col: 0 };
    state.handle_endpoint_result(
        "boot-1",
        &previous_id,
        Ok(ResponseResult::PanePromptMotion {
            pane_id: "pane_1".into(),
            cursor: prompt,
            content_revision: 0,
        }),
    );
    assert_eq!(
        state.copy_mode.as_ref().map(|mode| mode.cursor),
        Some(prompt)
    );

    let next = state.handle_input_bytes(b"]");
    let (_, method) = sent_request(&next);
    assert!(matches!(
        method,
        Method::PanePromptMotion(params)
            if params.cursor == prompt && params.direction == PanePromptDirection::Next
    ));
}

#[test]
fn o_selects_the_command_output_the_server_reports_and_y_copies_it() {
    let (mut state, origin) = state_in_copy_mode();

    let select = state.handle_input_bytes(b"o");
    let (select_id, method) = sent_request(&select);
    assert!(matches!(
        method,
        Method::PaneCommandOutputRange(params) if params.cursor == origin
    ));
    let range = PaneTextRange {
        start: PaneTextPoint { row: 14, col: 0 },
        end: PaneTextPoint { row: 19, col: 7 },
    };
    state.handle_endpoint_result(
        "boot-1",
        &select_id,
        Ok(ResponseResult::PaneCommandOutputRange {
            pane_id: "pane_1".into(),
            range,
            content_revision: 0,
        }),
    );
    assert_eq!(state.mode, ClientShellMode::Copy);
    assert_eq!(
        state.copy_mode.as_ref().map(|mode| mode.cursor),
        Some(range.start)
    );

    let copy = state.handle_input_bytes(b"y");
    assert!(copy.actions.iter().any(|action| matches!(
        action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(
                &request.method,
                Method::PaneSelectionRead(params)
                    if params.anchor == range.start && params.cursor == range.end
            )
    )));
}

#[test]
fn previous_prompt_after_selecting_output_extends_the_selection_over_the_command_line() {
    let (mut state, _origin) = state_in_copy_mode();
    let select = state.handle_input_bytes(b"o");
    let (select_id, _) = sent_request(&select);
    let range = PaneTextRange {
        start: PaneTextPoint { row: 14, col: 0 },
        end: PaneTextPoint { row: 19, col: 7 },
    };
    state.handle_endpoint_result(
        "boot-1",
        &select_id,
        Ok(ResponseResult::PaneCommandOutputRange {
            pane_id: "pane_1".into(),
            range,
            content_revision: 0,
        }),
    );

    let extend = state.handle_input_bytes(b"[");
    let (extend_id, _) = sent_request(&extend);
    let prompt = PaneTextPoint { row: 13, col: 0 };
    state.handle_endpoint_result(
        "boot-1",
        &extend_id,
        Ok(ResponseResult::PanePromptMotion {
            pane_id: "pane_1".into(),
            cursor: prompt,
            content_revision: 0,
        }),
    );

    let copy = state.handle_input_bytes(b"y");
    assert!(copy.actions.iter().any(|action| matches!(
        action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(
                &request.method,
                Method::PaneSelectionRead(params)
                    if params.anchor == prompt && params.cursor == range.end
            )
    )));
}

#[test]
fn a_copy_key_typed_before_the_output_range_arrives_copies_that_range() {
    let (mut state, _origin) = state_in_copy_mode();
    let select = state.handle_input_bytes(b"o");
    let (select_id, _) = sent_request(&select);
    assert!(state.handle_input_bytes(b"y").actions.is_empty());

    let range = PaneTextRange {
        start: PaneTextPoint { row: 14, col: 0 },
        end: PaneTextPoint { row: 19, col: 7 },
    };
    let (_, actions) = state.handle_endpoint_result(
        "boot-1",
        &select_id,
        Ok(ResponseResult::PaneCommandOutputRange {
            pane_id: "pane_1".into(),
            range,
            content_revision: 0,
        }),
    );

    assert_eq!(state.mode, ClientShellMode::Terminal);
    assert!(actions.iter().any(|action| matches!(
        action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(
                &request.method,
                Method::PaneSelectionRead(params)
                    if params.anchor == range.start && params.cursor == range.end
            )
    )));
}

#[test]
fn a_pane_without_prompt_marks_keeps_copy_mode_unselected_and_drops_queued_keys() {
    let (mut state, origin) = state_in_copy_mode();
    let select = state.handle_input_bytes(b"o");
    let (select_id, _) = sent_request(&select);
    state.handle_input_bytes(b"y");

    let (repaint, actions) = state.handle_endpoint_result(
        "boot-1",
        &select_id,
        Err(rejection("prompt_marks_unavailable")),
    );

    assert!(repaint);
    assert!(actions.is_empty());
    assert_eq!(state.mode, ClientShellMode::Copy);
    let copy_mode = state.copy_mode.as_ref().expect("copy mode stays open");
    assert_eq!(copy_mode.cursor, origin);
    assert!(copy_mode.selection.is_none());
    assert!(state.visible_endpoint_notice.is_some());
}

#[test]
fn an_output_range_for_an_outdated_cursor_is_ignored() {
    let (mut state, origin) = state_in_copy_mode();
    let select = state.handle_input_bytes(b"o");
    let (select_id, _) = sent_request(&select);
    state
        .copy_mode
        .as_mut()
        .expect("copy mode")
        .content_revision = 2;

    state.handle_endpoint_result(
        "boot-1",
        &select_id,
        Ok(ResponseResult::PaneCommandOutputRange {
            pane_id: "pane_1".into(),
            range: PaneTextRange {
                start: PaneTextPoint { row: 14, col: 0 },
                end: PaneTextPoint { row: 19, col: 7 },
            },
            content_revision: 0,
        }),
    );

    let copy_mode = state.copy_mode.as_ref().expect("copy mode");
    assert_eq!(copy_mode.cursor, origin);
    assert!(copy_mode.selection.is_none());
}

#[test]
fn a_server_without_the_command_methods_gets_no_request() {
    let (mut state, origin) = state_in_copy_mode();
    state.set_endpoint_methods(Some(
        ["pane.copy_motion", "pane.selection.read"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
    ));

    let select = state.handle_input_bytes(b"o");
    let previous = state.handle_input_bytes(b"[");

    assert!(select.actions.is_empty());
    assert!(previous.actions.is_empty());
    assert_eq!(state.mode, ClientShellMode::Copy);
    assert_eq!(
        state.copy_mode.as_ref().map(|mode| mode.cursor),
        Some(origin)
    );
    // The pipeline is free again: a motion the server does support still goes out.
    assert_eq!(state.handle_input_bytes(b"w").actions.len(), 1);
}
