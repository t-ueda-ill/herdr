//! Input to a container or ssh pane whose connection ended with an error.

use super::*;

fn disconnected_pane(server: &mut HeadlessServer) -> (tokio::sync::mpsc::Receiver<Bytes>, String) {
    let mut screen = (0..60)
        .map(|line| format!("debug1: line {line}\r\n"))
        .collect::<String>();
    screen.push_str("Permission denied (publickey).\r\n");
    let mut workspace = crate::workspace::Workspace::test_new("disconnected");
    let root = workspace.tabs[0].root_pane;
    let (runtime, input_rx) =
        crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
            80,
            24,
            64 * 1024,
            screen.as_bytes(),
            4,
        );
    workspace.insert_test_runtime(root, runtime);
    server.app.state.workspaces = vec![workspace];
    server.app.state.active = Some(0);
    server.app.state.selected = 0;
    server.app.state.mode = crate::app::Mode::Terminal;
    server.app.state.ensure_test_terminals();
    let pane = server.app.state.workspaces[0].tabs[0].root_pane;
    let terminal_id = server.app.state.workspaces[0]
        .terminal_id(pane)
        .unwrap()
        .clone();
    server
        .app
        .state
        .terminals
        .get_mut(&terminal_id)
        .unwrap()
        .disconnected = Some(crate::terminal::Disconnected {
        exit_code: Some(255),
    });
    server.clients.insert(
        11,
        ClientConnection::new_with_mode(
            ClientConnectionMode::ClientShell,
            (80, 24),
            crate::kitty_graphics::HostCellSize::default(),
            1,
            RenderEncoding::SemanticFrame,
            None,
        ),
    );
    let pane_id = server.app.session_snapshot().focused_pane_id.unwrap();
    (input_rx, pane_id)
}

fn input(server: &mut HeadlessServer, pane_id: &str, event: crate::protocol::ClientPaneInputEvent) {
    server.handle_server_event(ServerEvent::ClientShellPaneInput {
        client_id: 11,
        pane_id: pane_id.to_owned(),
        events: vec![event],
    });
}

#[tokio::test]
async fn typing_into_a_disconnected_pane_reaches_nothing() {
    let mut server = test_headless_server();
    let (mut input_rx, pane_id) = disconnected_pane(&mut server);

    input(
        &mut server,
        &pane_id,
        crate::protocol::ClientPaneInputEvent::Key {
            code: crate::protocol::ClientKeyCode::Char('q'),
            modifiers: 0,
            kind: crate::protocol::ClientKeyKind::Press,
            repeat_count: 1,
            shifted_codepoint: None,
            generated_text: None,
            tracks_release: false,
            physical_key_id: None,
            windows_record: None,
        },
    );

    assert!(input_rx.try_recv().is_err());
    assert_eq!(server.app.state.workspaces.len(), 1);
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn clicking_close_on_the_disconnect_bar_closes_the_pane() {
    let mut server = test_headless_server();
    let (mut input_rx, pane_id) = disconnected_pane(&mut server);
    let close_column = (0..80)
        .find(|&column| {
            crate::ui::disconnect_bar_button_at(80, Some(255), column)
                == Some(crate::ui::DisconnectBarButton::Close)
        })
        .unwrap();

    input(
        &mut server,
        &pane_id,
        crate::protocol::ClientPaneInputEvent::Mouse {
            kind: crate::protocol::ClientMouseKind::Down(crate::protocol::ClientMouseButton::Left),
            position: crate::protocol::ClientMousePosition::Cell {
                column: close_column,
                row: 23,
            },
            geometry: None,
            modifiers: 0,
            lines: 1,
        },
    );

    assert!(input_rx.try_recv().is_err(), "the click is not mouse input");
    assert!(server.app.state.workspaces.is_empty());
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn the_wheel_still_scrolls_a_disconnected_pane() {
    let mut server = test_headless_server();
    let (mut input_rx, pane_id) = disconnected_pane(&mut server);
    let scroll_of = |server: &HeadlessServer| {
        server
            .app
            .state
            .runtime_for_pane_in_workspace(
                &server.app.terminal_runtimes,
                0,
                server.app.state.workspaces[0].tabs[0].root_pane,
            )
            .unwrap()
            .scroll_metrics()
    };
    let before = scroll_of(&server);

    input(
        &mut server,
        &pane_id,
        crate::protocol::ClientPaneInputEvent::Mouse {
            kind: crate::protocol::ClientMouseKind::ScrollUp,
            position: crate::protocol::ClientMousePosition::Cell { column: 1, row: 1 },
            geometry: None,
            modifiers: 0,
            lines: 3,
        },
    );

    assert_ne!(
        scroll_of(&server),
        before,
        "the error above the screen stays readable"
    );
    assert!(input_rx.try_recv().is_err());
    shutdown_test_runtimes(&mut server);
}
