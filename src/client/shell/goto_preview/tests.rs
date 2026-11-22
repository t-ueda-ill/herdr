use std::time::{Duration, Instant};

use super::*;
use crate::api::schema::{Request, TaskInfo};
use crate::client::shell::tests::snapshot;

const READS: [&str; 3] = ["pane.read", "task.list", "task.result"];

fn picker(methods: &[&str]) -> ClientShellState {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(
        &crate::config::Config::default(),
    ));
    state.set_snapshot(Box::new(snapshot()));
    state.set_endpoint_methods(Some(methods.iter().map(|m| (*m).to_owned()).collect()));
    state.open_navigator_overlay();
    state
}

fn select_pane(state: &mut ClientShellState) {
    let Some(ClientShellOverlay::Navigator(navigator)) = state.overlay.as_mut() else {
        panic!("picker is open");
    };
    navigator.selected = Some(ClientNavigatorTarget::Pane {
        endpoint_id: ClientEndpointId::Local,
        pane_id: "pane_1".into(),
    });
}

fn sync(state: &mut ClientShellState, now: Instant) -> Vec<Request> {
    let mut outcome = ClientShellInput::default();
    state.sync_goto_preview(now, &mut outcome);
    outcome
        .actions
        .into_iter()
        .map(|action| match action {
            ClientShellAction::EndpointQuery { request, .. } => *request,
            other => panic!("expected a query, got {other:?}"),
        })
        .collect()
}

fn answer(state: &mut ClientShellState, request: &Request, result: ResponseResult) {
    state.handle_endpoint_query_result(&ClientEndpointId::Local, "boot-1", &request.id, Ok(result));
}

fn task(id: &str, state: &str, finished_at_ms: Option<u64>) -> TaskInfo {
    serde_json::from_value(serde_json::json!({
        "task_id": id,
        "state": state,
        "target": "reviewer",
        "terminal_id": "term_1",
        "pane_id": "pane_1",
        "location": { "id": "local", "kind": "host" },
        "text": "review the diff",
        "created_at_ms": 1,
        "finished_at_ms": finished_at_ms,
    }))
    .expect("task info")
}

fn screen(text: &str) -> ResponseResult {
    serde_json::from_value(serde_json::json!({
        "type": "pane_read",
        "read": {
            "pane_id": "pane_1", "workspace_id": "ws_1", "tab_id": "tab_1",
            "source": "recent", "format": "text", "text": text,
            "revision": 1, "truncated": false
        }
    }))
    .expect("pane read")
}

fn body(state: &ClientShellState) -> Option<PreviewBody> {
    let Some(ClientShellOverlay::Navigator(navigator)) = state.overlay.as_ref() else {
        return None;
    };
    navigator
        .tasks
        .preview(&ClientEndpointId::Local, "pane_1")
        .map(|preview| preview.body.clone())
}

fn method_names(requests: &[Request]) -> Vec<&'static str> {
    requests
        .iter()
        .map(|request| crate::api::api_method_name(&request.method))
        .collect()
}

#[test]
fn the_picker_asks_each_server_for_its_tasks_once() {
    let mut state = picker(&READS);
    let now = Instant::now();

    let first = sync(&mut state, now);
    let second = sync(&mut state, now);

    assert_eq!(method_names(&first), ["task.list"]);
    assert!(second.is_empty());
}

#[test]
fn a_pane_with_a_finished_task_previews_its_result() {
    let mut state = picker(&READS);
    let now = Instant::now();
    let list = sync(&mut state, now);
    answer(
        &mut state,
        &list[0],
        ResponseResult::TaskList {
            tasks: vec![task("t_1", "completed", Some(5))],
        },
    );
    select_pane(&mut state);

    let read = sync(&mut state, now);

    let [Request {
        method: Method::TaskResult(target),
        ..
    }] = &read[..]
    else {
        panic!("expected task.result, got {read:?}");
    };
    assert_eq!(target.task_id, "t_1");
    assert_eq!(body(&state), Some(PreviewBody::Loading));
}

#[test]
fn a_pane_with_a_running_task_previews_its_screen() {
    let mut state = picker(&READS);
    let now = Instant::now();
    let list = sync(&mut state, now);
    answer(
        &mut state,
        &list[0],
        ResponseResult::TaskList {
            tasks: vec![
                task("t_1", "completed", Some(5)),
                task("t_2", "running", None),
            ],
        },
    );
    select_pane(&mut state);

    let read = sync(&mut state, now);
    answer(&mut state, &read[0], screen("$ cargo test"));

    assert_eq!(method_names(&read), ["pane.read"]);
    assert_eq!(
        body(&state),
        Some(PreviewBody::Text {
            source: PreviewSource::ScreenTail,
            text: "$ cargo test".into(),
            note: None,
        })
    );
}

#[test]
fn a_screen_is_read_again_after_two_seconds_and_keeps_its_text_meanwhile() {
    let mut state = picker(&["pane.read"]);
    select_pane(&mut state);
    let now = Instant::now();
    let read = sync(&mut state, now);
    answer(&mut state, &read[0], screen("old"));

    let soon = sync(&mut state, now + Duration::from_millis(500));
    let later = sync(&mut state, now + PREVIEW_TTL);

    assert!(soon.is_empty());
    assert_eq!(method_names(&later), ["pane.read"]);
    assert!(matches!(body(&state), Some(PreviewBody::Text { text, .. }) if text == "old"));
}

#[test]
fn a_result_is_not_read_again() {
    let mut state = picker(&["task.result"]);
    let now = Instant::now();
    let Some(ClientShellOverlay::Navigator(navigator)) = state.overlay.as_mut() else {
        panic!("picker is open");
    };
    navigator.tasks.list_sent(&ClientEndpointId::Local, now);
    navigator.tasks.receive_list(
        &ClientEndpointId::Local,
        Some(vec![task("t_1", "completed", Some(5))]),
    );
    select_pane(&mut state);
    let read = sync(&mut state, now);
    let result = serde_json::from_value(serde_json::json!({
        "type": "task_result",
        "result": { "task_id": "t_1", "source": "screen", "text": "tail",
                    "file_error": "not found" }
    }))
    .expect("task result");
    answer(&mut state, &read[0], result);

    let again = sync(&mut state, now + PREVIEW_TTL * 5);

    assert!(again.is_empty());
    assert_eq!(
        body(&state),
        Some(PreviewBody::Text {
            source: PreviewSource::ScreenTail,
            text: "tail".into(),
            note: Some("not found".into()),
        })
    );
}

#[test]
fn a_server_without_screen_reads_shows_the_preview_as_not_available() {
    let mut state = picker(&[]);
    select_pane(&mut state);

    let reads = sync(&mut state, Instant::now());

    assert!(reads.is_empty());
    assert!(matches!(body(&state), Some(PreviewBody::Unavailable(_))));
}

#[test]
fn a_server_gets_one_query_at_a_time() {
    let mut state = picker(&READS);
    select_pane(&mut state);
    let now = Instant::now();

    let first = sync(&mut state, now);
    let while_listing = sync(&mut state, now);

    assert_eq!(method_names(&first), ["task.list"]);
    assert!(while_listing.is_empty());
}

#[test]
fn a_lost_query_leaves_the_loading_state() {
    let mut state = picker(&["pane.read"]);
    select_pane(&mut state);
    let read = sync(&mut state, Instant::now());

    state.cancel_endpoint_request(&read[0].id);

    assert!(matches!(body(&state), Some(PreviewBody::Unavailable(_))));
    assert!(!state.is_endpoint_query(&read[0].id));
}

#[test]
fn a_pane_shows_its_task_in_progress_before_the_last_finished_one() {
    let tasks = newest_task_by_pane(vec![
        task("t_3", "completed", Some(9)),
        task("t_2", "running", None),
        task("t_1", "completed", Some(4)),
    ]);

    assert_eq!(tasks["pane_1"].task_id, "t_2");
}

#[test]
fn a_pane_without_work_in_progress_shows_the_task_that_finished_last() {
    let tasks = newest_task_by_pane(vec![
        task("t_1", "completed", Some(9)),
        task("t_2", "lost", Some(4)),
    ]);

    assert_eq!(tasks["pane_1"].task_id, "t_1");
}

#[test]
fn search_finds_a_pane_by_the_text_of_its_task() {
    let mut state = picker(&READS);
    let list = sync(&mut state, Instant::now());
    answer(
        &mut state,
        &list[0],
        ResponseResult::TaskList {
            tasks: vec![task("t_1", "running", None)],
        },
    );
    let Some(ClientShellOverlay::Navigator(navigator)) = state.overlay.as_mut() else {
        panic!("picker is open");
    };
    navigator.query = crate::client::shell::TextEditor::from("the diff");
    let Some(ClientShellOverlay::Navigator(navigator)) = state.overlay.as_ref() else {
        panic!("picker is open");
    };

    let rows = super::super::aggregate_navigation::navigator_rows(
        &state.endpoints,
        &state.active_endpoint_id,
        navigator,
    );

    assert!(rows.iter().any(|row| matches!(
        &row.target,
        ClientNavigatorTarget::Pane { pane_id, .. } if pane_id == "pane_1"
    )));
}

fn finished_picker() -> (ClientShellState, Instant) {
    let mut state = picker(&READS);
    let now = Instant::now();
    let list = sync(&mut state, now);
    answer(
        &mut state,
        &list[0],
        ResponseResult::TaskList {
            tasks: vec![task("t_1", "completed", Some(5))],
        },
    );
    select_pane(&mut state);
    let read = sync(&mut state, now);
    let result = serde_json::from_value(serde_json::json!({
        "type": "task_result",
        "result": { "task_id": "t_1", "source": "file", "text": "VERDICT: approve" }
    }))
    .expect("task result");
    answer(&mut state, &read[0], result);
    (state, now)
}

#[test]
fn space_opens_the_full_result_without_reading_it_again() {
    let (mut state, now) = finished_picker();

    state.handle_input_bytes(b" ");
    let reads = sync(&mut state, now);

    let Some(ClientShellOverlay::GotoResult(viewer)) = state.overlay.as_ref() else {
        panic!("expected the result view");
    };
    assert!(matches!(&viewer.body, PreviewBody::Text { text, .. } if text == "VERDICT: approve"));
    assert!(reads.is_empty());
}

#[test]
fn esc_returns_to_the_picker_on_the_same_row() {
    let (mut state, _) = finished_picker();
    state.handle_input_bytes(b" ");

    state.handle_input_bytes(b"\x1b");

    let Some(ClientShellOverlay::Navigator(navigator)) = state.overlay.as_ref() else {
        panic!("expected the picker");
    };
    assert_eq!(
        navigator.selected,
        Some(ClientNavigatorTarget::Pane {
            endpoint_id: ClientEndpointId::Local,
            pane_id: "pane_1".into(),
        })
    );
}

#[test]
fn y_copies_the_result_text() {
    let (mut state, _) = finished_picker();
    state.handle_input_bytes(b"r");

    let outcome = state.handle_input_bytes(b"y");

    assert!(outcome.actions.iter().any(|action| matches!(
        action,
        ClientShellAction::ClipboardWrite(bytes) if bytes == b"VERDICT: approve"
    )));
}

#[test]
fn the_full_view_of_a_pane_without_a_result_reads_more_of_its_screen() {
    let mut state = picker(&["pane.read"]);
    select_pane(&mut state);
    let now = Instant::now();
    let preview = sync(&mut state, now);
    answer(&mut state, &preview[0], screen("$ cargo test"));
    state.handle_input_bytes(b" ");

    let reads = sync(&mut state, now);

    let [Request {
        method: Method::PaneRead(params),
        ..
    }] = &reads[..]
    else {
        panic!("expected pane.read, got {reads:?}");
    };
    assert_eq!(params.lines, Some(VIEWER_LINES));
    assert_eq!(params.source, crate::api::schema::ReadSource::Visible);
}

fn screen_text(state: &mut ClientShellState, cols: u16, rows: u16) -> String {
    state.set_pane_surface(crate::client::shell::tests::surface());
    let frame = state.compose(cols, rows).expect("frame");
    crate::client::shell::tests::frame_rows(&frame).join("\n")
}

#[test]
fn a_wide_picker_shows_the_request_and_the_result_beside_the_list() {
    let (mut state, _) = finished_picker();

    let text = screen_text(&mut state, 120, 30);

    assert!(text.contains("asked · t_1"), "{text}");
    assert!(text.contains("review the diff"), "{text}");
    assert!(text.contains("result file"), "{text}");
    assert!(text.contains("VERDICT: approve"), "{text}");
    assert!(text.contains("open result"), "{text}");
}

#[test]
fn a_narrow_picker_keeps_the_list_alone() {
    let (mut state, _) = finished_picker();

    let text = screen_text(&mut state, 80, 30);

    assert!(!text.contains("VERDICT: approve"), "{text}");
}

#[test]
fn a_workspace_row_counts_its_panes_by_state() {
    let mut state = picker(&[]);
    let mut projection = snapshot();
    projection.agents = vec![crate::protocol::ClientShellAgent {
        pane_id: "pane_1".into(),
        workspace_id: "ws_1".into(),
        tab_id: "tab_1".into(),
        name: None,
        display_agent: None,
        agent: Some("claude".into()),
        title: None,
        terminal_title: None,
        terminal_title_stripped: None,
        agent_status: crate::api::schema::AgentStatus::Blocked,
        state_change_seq: 1,
        state_labels: Vec::new(),
        tokens: Vec::new(),
        focused: true,
    }];
    state.set_snapshot(Box::new(projection));
    let Some(ClientShellOverlay::Navigator(navigator)) = state.overlay.as_mut() else {
        panic!("picker is open");
    };
    navigator.selected = Some(ClientNavigatorTarget::Workspace {
        endpoint_id: ClientEndpointId::Local,
        workspace_id: "ws_1".into(),
    });

    let text = screen_text(&mut state, 120, 30);

    assert!(text.contains("1 needs input"), "{text}");
}

#[test]
fn the_full_view_shows_the_request_and_the_whole_result() {
    let (mut state, _) = finished_picker();
    state.handle_input_bytes(b" ");

    let text = screen_text(&mut state, 120, 30);

    assert!(text.contains("Result"), "{text}");
    assert!(text.contains("review the diff"), "{text}");
    assert!(text.contains("VERDICT: approve"), "{text}");
    assert!(text.contains("esc back"), "{text}");
}
