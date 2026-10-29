use super::*;
use crate::client::upload::transfer::UploadProgressView;
use crossterm::event::{MouseButton, MouseEventKind};

fn ready_state() -> ClientShellState {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.compose(106, 24).unwrap();
    state
}

fn view(cancellable: bool) -> UploadProgressView {
    UploadProgressView {
        job: 4,
        title: if cancellable {
            "Uploading"
        } else {
            "Publishing…"
        }
        .into(),
        body: "1/3 · 0.1/0.2 MB — click to cancel".into(),
        cancellable,
    }
}

fn click(state: &mut ClientShellState, rect: Rect) -> ClientShellInput {
    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: rect.x + 1,
        row: rect.y,
        modifiers: KeyModifiers::empty(),
    })])
}

fn press(state: &mut ClientShellState, code: KeyCode) -> ClientShellInput {
    state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        code,
        KeyModifiers::empty(),
    ))])
}

#[test]
fn the_progress_card_is_drawn_with_its_counts_and_reports_its_area() {
    let mut state = ready_state();
    assert!(state.set_upload_progress(Some(view(true))));
    assert!(!state.set_upload_progress(Some(view(true))));

    let frame = state.compose(106, 24).unwrap();

    assert!(!state.hits.upload_progress.is_empty());
    let text = frame_rows(&frame.frame).join("\n");
    assert!(text.contains("Uploading"), "{text}");
    assert!(text.contains("1/3"), "{text}");
}

#[test]
fn clicking_the_card_queues_a_cancel_but_not_while_publishing() {
    let mut state = ready_state();
    state.set_upload_progress(Some(view(true)));
    state.compose(106, 24).unwrap();
    let card = state.hits.upload_progress;

    click(&mut state, card);

    assert_eq!(
        state.take_upload_decisions(),
        [ClientUploadDecision::Cancel(4)]
    );

    state.set_upload_progress(Some(view(false)));
    state.compose(106, 24).unwrap();
    let card = state.hits.upload_progress;
    let outcome = click(&mut state, card);
    assert!(state.take_upload_decisions().is_empty());
    assert!(
        outcome.requests.is_empty(),
        "the click must not reach the pane"
    );
}

#[test]
fn the_confirm_dialog_answers_enter_and_esc() {
    for (code, accept) in [(KeyCode::Enter, true), (KeyCode::Esc, false)] {
        let mut state = ready_state();
        assert!(state.open_upload_confirm(4, "Upload dropped files?".into(), "11 items".into()));

        press(&mut state, code);

        assert!(state.overlay.is_none());
        assert_eq!(
            state.take_upload_decisions(),
            [ClientUploadDecision::Confirm { job: 4, accept }]
        );
    }
}

#[test]
fn the_confirm_dialog_accepts_its_button_and_declines_a_click_elsewhere() {
    let mut state = ready_state();
    state.open_upload_confirm(4, "Upload dropped files?".into(), "11 items".into());
    state.compose(106, 24).unwrap();
    let button = state.hits.overlay_primary;
    click(&mut state, button);
    assert_eq!(
        state.take_upload_decisions(),
        [ClientUploadDecision::Confirm {
            job: 4,
            accept: true
        }]
    );

    state.open_upload_confirm(5, "Upload dropped files?".into(), "11 items".into());
    state.compose(106, 24).unwrap();
    click(&mut state, Rect::new(0, 22, 1, 1));
    assert_eq!(
        state.take_upload_decisions(),
        [ClientUploadDecision::Confirm {
            job: 5,
            accept: false
        }]
    );
}

#[test]
fn the_confirm_dialog_never_replaces_an_open_overlay() {
    let mut state = ready_state();
    state.overlay = Some(ClientShellOverlay::Help(ClientHelpOverlay {
        query: TextEditor::default(),
        search_focused: false,
        scroll: 0,
        ..Default::default()
    }));

    assert!(!state.open_upload_confirm(4, "t".into(), "d".into()));
    assert!(matches!(state.overlay, Some(ClientShellOverlay::Help(_))));
    assert!(!state.close_upload_confirm(4));
}

#[test]
fn a_confirm_dialog_replaced_without_an_answer_is_declined_once() {
    let mut state = ready_state();
    assert!(state.open_upload_confirm(4, "Upload dropped files?".into(), "11 items".into()));
    assert!(state.take_upload_decisions().is_empty(), "still open");

    // What a popup opening or an endpoint reset does to the overlay.
    state.overlay = Some(ClientShellOverlay::Help(ClientHelpOverlay {
        query: TextEditor::default(),
        search_focused: false,
        scroll: 0,
        ..Default::default()
    }));

    assert_eq!(
        state.take_upload_decisions(),
        [ClientUploadDecision::Confirm {
            job: 4,
            accept: false
        }]
    );
    assert!(state.take_upload_decisions().is_empty());
}

#[test]
fn an_answered_or_closed_confirm_dialog_is_not_declined_again() {
    let mut state = ready_state();
    state.open_upload_confirm(4, "t".into(), "d".into());
    press(&mut state, KeyCode::Enter);
    assert_eq!(
        state.take_upload_decisions(),
        [ClientUploadDecision::Confirm {
            job: 4,
            accept: true
        }]
    );

    state.open_upload_confirm(5, "t".into(), "d".into());
    assert!(state.close_upload_confirm(5));
    assert!(state.take_upload_decisions().is_empty());
}

#[test]
fn clicks_inside_an_overlay_never_reach_the_progress_card_beneath_it() {
    let mut state = ready_state();
    state.set_upload_progress(Some(view(true)));
    state.compose(106, 24).unwrap();
    let card = state.hits.upload_progress;
    state.overlay = Some(ClientShellOverlay::Help(ClientHelpOverlay {
        query: TextEditor::default(),
        search_focused: false,
        scroll: 0,
        ..Default::default()
    }));
    state.compose(106, 24).unwrap();

    click(&mut state, card);

    assert!(state.take_upload_decisions().is_empty());
}

#[test]
fn upload_notices_repeat_for_each_upload() {
    let mut state = ready_state();

    assert!(state.push_upload_notice(false, "Uploaded".into(), "1 item to /x".into()));
    state.visible_endpoint_notice = None;

    assert!(state.push_upload_notice(false, "Uploaded".into(), "1 item to /x".into()));
}
