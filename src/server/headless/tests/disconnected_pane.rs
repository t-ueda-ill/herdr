//! Input to a container or ssh pane whose connection ended with an error.

use super::*;

fn disconnected_pane(server: &mut HeadlessServer) -> (tokio::sync::mpsc::Receiver<Bytes>, String) {
    let mut screen = (0..60)
        .map(|line| format!("debug1: line {line}\r\n"))
        .collect::<String>();
    screen.push_str("Permission denied (publickey).\r\n");
    // Modes the exited process left on: mouse reporting and application cursor keys.
    screen.push_str("\x1b[?1000h\x1b[?1006h\x1b[?1h");
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

fn scroll_of(server: &HeadlessServer) -> Option<crate::pane::ScrollMetrics> {
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
}

#[tokio::test]
async fn the_wheel_still_scrolls_a_disconnected_pane() {
    let mut server = test_headless_server();
    let (mut input_rx, pane_id) = disconnected_pane(&mut server);
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

#[tokio::test]
async fn page_up_scrolls_a_disconnected_pane_and_its_release_does_nothing() {
    let mut server = test_headless_server();
    let (mut input_rx, pane_id) = disconnected_pane(&mut server);
    let before = scroll_of(&server);
    let page_up = |kind| crate::protocol::ClientPaneInputEvent::Key {
        code: crate::protocol::ClientKeyCode::PageUp,
        modifiers: 0,
        kind,
        repeat_count: 1,
        shifted_codepoint: None,
        generated_text: None,
        tracks_release: true,
        physical_key_id: None,
        windows_record: None,
    };

    input(
        &mut server,
        &pane_id,
        page_up(crate::protocol::ClientKeyKind::Press),
    );
    let scrolled = scroll_of(&server);
    input(
        &mut server,
        &pane_id,
        page_up(crate::protocol::ClientKeyKind::Release),
    );

    assert_ne!(scrolled, before);
    assert_eq!(scroll_of(&server), scrolled);
    assert!(input_rx.try_recv().is_err());
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn closing_a_disconnected_split_gives_its_space_back_like_pane_close() {
    let mut server = test_headless_server();
    let mut workspace = crate::workspace::Workspace::test_new("disconnected-split");
    let first_pane = workspace.tabs[0].root_pane;
    let dead_pane = workspace.test_split(ratatui::layout::Direction::Vertical);
    for pane in [first_pane, dead_pane] {
        workspace.insert_test_runtime(
            pane,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b""),
        );
    }
    server.app.state.workspaces = vec![workspace];
    server.app.state.active = Some(0);
    server.app.state.selected = 0;
    server.app.state.mode = crate::app::Mode::Terminal;
    server.app.state.ensure_test_terminals();
    let terminal_id = server.app.state.workspaces[0]
        .terminal_id(dead_pane)
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
    let (control, _) = connect_test_shell(&mut server, 73, 185, 46);
    let _ = control.recv().expect("snapshot");
    let shrunk = server.app.state.workspaces[0].test_runtimes[&first_pane].current_size();
    let (rows, cols) = server.app.state.workspaces[0].test_runtimes[&dead_pane].current_size();
    let close_column = (0..cols)
        .find(|&column| {
            crate::ui::disconnect_bar_button_at(cols, Some(255), column)
                == Some(crate::ui::DisconnectBarButton::Close)
        })
        .unwrap();
    let pane_id = server.app.public_pane_id(0, dead_pane).unwrap();

    server.handle_server_event(ServerEvent::ClientShellPaneInput {
        client_id: 73,
        pane_id,
        events: vec![crate::protocol::ClientPaneInputEvent::Mouse {
            kind: crate::protocol::ClientMouseKind::Down(crate::protocol::ClientMouseButton::Left),
            position: crate::protocol::ClientMousePosition::Cell {
                column: close_column,
                row: rows - 1,
            },
            geometry: None,
            modifiers: 0,
            lines: 1,
        }],
    });

    assert!(server.app.find_pane(dead_pane).is_none());
    let grown = server.app.state.workspaces[0].test_runtimes[&first_pane].current_size();
    assert!(grown.0 > shrunk.0, "{shrunk:?} -> {grown:?}");
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn a_disconnected_shell_asks_clients_to_forward_its_clicks() {
    let mut server = test_headless_server();
    let mut workspace = crate::workspace::Workspace::test_new("failed-ssh");
    let pane = workspace.tabs[0].root_pane;
    workspace.insert_test_runtime(
        pane,
        crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 23, b"Permission denied\r\n"),
    );
    server.app.state.workspaces = vec![workspace];
    server.app.state.active = Some(0);
    server.app.state.selected = 0;
    server.app.state.mode = crate::app::Mode::Terminal;
    let (_control, render) = connect_matching_test_shell(&mut server, 7);
    let mouse_reporting = |server: &mut HeadlessServer| {
        server.render_and_stream();
        let mut surface = None;
        while let Ok(bytes) = render.try_recv() {
            if let ServerMessage::PaneSurface(next) = read_server_message(bytes) {
                surface = Some(next);
            }
        }
        surface.expect("a new surface").panes[0].mouse_reporting
    };
    assert!(!mouse_reporting(&mut server));

    server
        .app
        .state
        .runtime_for_pane_in_workspace(&server.app.terminal_runtimes, 0, pane)
        .unwrap()
        .retire_exited_process();

    assert!(
        mouse_reporting(&mut server),
        "a plain shell would otherwise turn a click on the bar into a selection"
    );
    shutdown_test_runtimes(&mut server);
}

#[test]
fn pane_reconnect_reapplies_the_controlled_geometry_like_other_layout_requests() {
    let method = crate::api::schema::Method::PaneReconnect(crate::api::schema::PaneTarget {
        pane_id: "w1:p1".into(),
    });
    assert!(HeadlessServer::public_request_may_change_geometry(&method));
    assert!(HeadlessServer::shell_endpoint_claims_geometry(&method));
}

#[tokio::test]
async fn a_resolved_container_lookup_sizes_the_tab_from_its_controller() {
    let mut server = test_headless_server();
    let mut workspace = crate::workspace::Workspace::test_new("reconnect-geometry");
    let first_pane = workspace.tabs[0].root_pane;
    let other_pane = workspace.test_split(ratatui::layout::Direction::Vertical);
    for pane in [first_pane, other_pane] {
        workspace.insert_test_runtime(
            pane,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b""),
        );
    }
    server.app.state.workspaces = vec![workspace];
    server.app.state.active = Some(0);
    server.app.state.selected = 0;
    server.app.state.mode = crate::app::Mode::Terminal;
    server.app.state.ensure_test_terminals();
    let (control, _) = connect_test_shell(&mut server, 73, 185, 46);
    let _ = control.recv().expect("snapshot");
    let controlled = server.app.state.workspaces[0].test_runtimes[&first_pane].current_size();
    // A runtime spawned from the global view size instead of the controller's.
    server.app.state.workspaces[0].test_runtimes[&first_pane].resize(10, 30, 0, 0);
    let terminal_id = server.app.state.workspaces[0]
        .terminal_id(first_pane)
        .unwrap()
        .clone();

    server.handle_internal_event_with_forwarding(AppEvent::OffHostReconnectResolved {
        terminal_id,
        container_id: None,
    });

    assert_eq!(
        server.app.state.workspaces[0].test_runtimes[&first_pane].current_size(),
        controlled
    );
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn late_output_of_a_disconnected_pane_redraws_its_bar_instead_of_patching_rows() {
    let mut server = test_headless_server();
    let pane_id = install_shared_view_test_runtime(&mut server);
    server.app.state.ensure_test_terminals();
    let (control, render) = connect_matching_test_shell(&mut server, 7);
    let _ = control.recv().expect("snapshot");
    server.render_and_stream();
    let _ = recv_pane_surface(&render, "initial surface");
    let terminal_id = server.app.state.workspaces[0]
        .terminal_id(pane_id)
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
    server.render_and_stream();
    let _ = recv_pane_surface(&render, "surface with the bar");

    write_shared_test_pane(&mut server, pane_id, b"\r\nConnection closed.");

    assert!(
        !server.render_retained_pane_surface_and_stream(&HashSet::from([pane_id])),
        "a row patch would paint over the bar"
    );
    shutdown_test_runtimes(&mut server);
}
