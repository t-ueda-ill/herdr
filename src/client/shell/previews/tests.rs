use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use super::*;
use crate::api::schema::{PaneLocation, PaneLocationKind};
use crate::raw_input::RawInputEvent;

fn preview(id: &str, root: &str, port: u16, location: Option<PaneLocation>) -> PreviewInfo {
    PreviewInfo {
        preview_id: id.into(),
        root: root.into(),
        location,
        port,
        url: format!("http://127.0.0.1:{port}/secret{port}/"),
    }
}

fn running() -> Vec<PreviewInfo> {
    vec![
        preview("preview-1", "/home/me/site", 40001, None),
        preview(
            "preview-2",
            "/srv/docs",
            40002,
            Some(PaneLocation {
                kind: PaneLocationKind::Container,
                ssh_host: Some("coding".into()),
                container: Some("web".into()),
            }),
        ),
    ]
}

fn shell() -> ClientShellState {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(crate::client::shell::tests::snapshot()));
    state.set_pane_surface(crate::client::shell::tests::surface());
    state
}

fn requests(outcome: &ClientShellInput) -> Vec<(String, Method)> {
    outcome
        .actions
        .iter()
        .filter_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => {
                Some((request.id.clone(), request.method.clone()))
            }
            _ => None,
        })
        .collect()
}

/// The picker as the global menu item opens it, answered with `previews`.
fn picker(previews: Vec<PreviewInfo>) -> ClientShellState {
    let mut state = shell();
    let mut outcome = ClientShellInput::default();
    state.open_previews(&mut outcome);
    let sent = requests(&outcome);
    assert!(
        matches!(&sent[..], [(_, Method::PreviewList(_))]),
        "{sent:?}"
    );
    state.handle_endpoint_result(
        "boot-1",
        &sent[0].0,
        Ok(ResponseResult::PreviewList { previews }),
    );
    state
}

fn key(state: &mut ClientShellState, code: KeyCode) -> ClientShellInput {
    state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        code,
        KeyModifiers::NONE,
    ))])
}

fn click(state: &mut ClientShellState, rect: Rect) -> ClientShellInput {
    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: rect.x + 1,
        row: rect.y,
        modifiers: KeyModifiers::empty(),
    })])
}

fn shown(state: &ClientShellState) -> Vec<String> {
    match state.overlay.as_ref() {
        Some(ClientShellOverlay::Previews(previews)) => previews
            .filtered_indices()
            .into_iter()
            .map(|index| previews.entries[index].preview_id.clone())
            .collect(),
        other => panic!("previews picker is not open: {other:?}"),
    }
}

fn frame_text(state: &mut ClientShellState) -> String {
    let frame = state.compose(106, 30).expect("frame");
    frame
        .cells
        .chunks(frame.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_global_menu_lists_previews_before_detach() {
    let items =
        super::super::global_menu::global_menu_items(&crate::client::shell::tests::snapshot());
    let labels: Vec<_> = items.iter().map(|(label, _)| *label).collect();
    let previews = labels
        .iter()
        .position(|label| *label == "previews")
        .unwrap();
    assert_eq!(labels[previews + 1], "detach");
}

#[test]
fn the_picker_lists_each_preview_with_its_place_and_address() {
    let mut state = picker(running());
    let text = frame_text(&mut state);
    for expected in [
        "previews",
        "/home/me/site",
        "this host",
        "http://127.0.0.1:40001",
        "/srv/docs",
        "web on coding",
        "del stop",
    ] {
        assert!(text.contains(expected), "missing {expected:?}\n{text}");
    }
    assert!(!text.contains("secret"), "the token stays off screen");
}

#[test]
fn an_empty_list_says_so_and_offers_no_stop() {
    let mut state = picker(Vec::new());
    let text = frame_text(&mut state);
    assert!(text.contains("no previews running"));
    assert!(!text.contains("del stop"));
}

#[test]
fn enter_opens_the_selected_preview_in_the_browser_and_closes() {
    let mut state = picker(running());
    key(&mut state, KeyCode::Down);
    let outcome = key(&mut state, KeyCode::Enter);
    assert!(outcome.actions.iter().any(|action| matches!(
        action,
        ClientShellAction::OpenPreviewUrl(url) if url == "http://127.0.0.1:40002/secret40002/"
    )));
    assert!(state.overlay.is_none());
}

#[test]
fn delete_stops_the_selected_preview_and_drops_its_row() {
    let mut state = picker(running());
    let outcome = key(&mut state, KeyCode::Delete);
    let sent = requests(&outcome);
    assert!(matches!(
        &sent[..],
        [(_, Method::PreviewStop(params))] if params.preview_id == "preview-1"
    ));
    state.handle_endpoint_result(
        "boot-1",
        &sent[0].0,
        Ok(ResponseResult::PreviewStopped {
            preview_id: "preview-1".into(),
        }),
    );
    assert_eq!(shown(&state), vec!["preview-2"]);
    let Some(ClientShellOverlay::Previews(previews)) = state.overlay.as_ref() else {
        unreachable!();
    };
    assert_eq!(previews.selected, 0);
    assert!(!previews.stopping);
}

#[test]
fn a_preview_already_gone_is_dropped_and_other_failures_say_why() {
    let mut state = picker(running());
    let gone = requests(&key(&mut state, KeyCode::Delete));
    state.handle_endpoint_result(
        "boot-1",
        &gone[0].0,
        Err(ClientShellEndpointError {
            code: Some("preview_not_found".into()),
            message: "no running preview preview-1".into(),
        }),
    );
    assert_eq!(shown(&state), vec!["preview-2"]);

    let failed = requests(&key(&mut state, KeyCode::Delete));
    state.handle_endpoint_result(
        "boot-1",
        &failed[0].0,
        Err(ClientShellEndpointError {
            code: Some("invalid_params".into()),
            message: "stop refused for this test".into(),
        }),
    );
    assert_eq!(shown(&state), vec!["preview-2"]);
    assert!(frame_text(&mut state).contains("stop refused for this test"));
}

#[test]
fn a_second_stop_waits_for_the_first_answer() {
    let mut state = picker(running());
    assert_eq!(requests(&key(&mut state, KeyCode::Delete)).len(), 1);
    assert!(requests(&key(&mut state, KeyCode::Delete)).is_empty());
}

#[test]
fn the_filter_narrows_by_root_place_or_port() {
    let mut state = picker(running());
    key(&mut state, KeyCode::Char('/'));
    for ch in "coding".chars() {
        key(&mut state, KeyCode::Char(ch));
    }
    assert_eq!(shown(&state), vec!["preview-2"]);
    let outcome = key(&mut state, KeyCode::Enter);
    assert!(outcome.actions.iter().any(|action| matches!(
        action,
        ClientShellAction::OpenPreviewUrl(url) if url.contains(":40002/")
    )));
}

#[test]
fn escape_closes_the_picker() {
    let mut state = picker(running());
    key(&mut state, KeyCode::Esc);
    assert!(state.overlay.is_none());
}

#[test]
fn the_stop_button_acts_on_the_clicked_row() {
    let mut state = picker(running());
    frame_text(&mut state);
    let second = state.hits.worktree_rows[1].0;
    click(&mut state, second);
    frame_text(&mut state);
    let stop = state.hits.overlay_secondary;
    assert_ne!(stop, Rect::default());
    let sent = requests(&click(&mut state, stop));
    assert!(matches!(
        &sent[..],
        [(_, Method::PreviewStop(params))] if params.preview_id == "preview-2"
    ));
}

#[test]
fn a_remote_server_keeps_the_picker_open_and_says_why() {
    let mut state = picker(running());
    state.active_endpoint_id =
        ClientEndpointId::Ssh(crate::client::endpoint::ProfileId::generate());
    let outcome = key(&mut state, KeyCode::Enter);
    assert!(!outcome
        .actions
        .iter()
        .any(|action| matches!(action, ClientShellAction::OpenPreviewUrl(_))));
    let Some(ClientShellOverlay::Previews(previews)) = state.overlay.as_ref() else {
        panic!("the picker stays open");
    };
    assert_eq!(
        previews.error.as_deref(),
        Some(super::super::investigation::PREVIEW_ELSEWHERE)
    );
}

#[test]
fn three_previews_are_all_shown_without_scrolling() {
    let mut previews = running();
    previews.push(preview("preview-3", "/srv/blog", 40003, None));
    let mut state = picker(previews);

    let text = frame_text(&mut state);

    assert_eq!(state.hits.worktree_rows.len(), 3);
    assert!(text.contains("/srv/blog"), "{text}");
}
