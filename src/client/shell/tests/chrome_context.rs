use super::*;

#[test]
fn tab_overflow_controls_scroll_the_client_owned_tab_bar() {
    let mut snapshot = snapshot();
    snapshot.tabs.extend((2..=8).map(|number| ClientShellTab {
        tab_id: format!("tab_{number}"),
        workspace_id: "ws_1".into(),
        number,
        label: number.to_string(),
        custom_label: false,
        zoomed: false,
        focused: false,
        agent_status: AgentStatus::Idle,
    }));
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(surface());
    state.compose(80, 20).expect("overflow tab bar");

    assert!(state.hits.tab_scroll_right.width > 0);
    let scroll_right = state.hits.tab_scroll_right;
    let outcome =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: scroll_right.x + 1,
            row: scroll_right.y,
            modifiers: KeyModifiers::empty(),
        })]);
    assert!(outcome.repaint);
    assert_eq!(state.tab_scroll, 1);

    let mut update = state.snapshot.as_deref().expect("snapshot").clone();
    update.focused_tab_id = Some("tab_8".into());
    for tab in &mut update.tabs {
        tab.focused = tab.tab_id == "tab_8";
    }
    state.set_snapshot(Box::new(update));
    state.compose(80, 20).expect("focused overflow tab");
    assert!(state.hits.tabs.iter().any(|(_, tab_id)| tab_id == "tab_8"));

    state.compose(300, 20).expect("tabs without overflow");
    assert_eq!(state.tab_scroll, 0);
    assert_eq!(state.hits.tabs.len(), 8);
    state.compose(80, 20).expect("focused tab after narrowing");
    assert!(state.hits.tabs.iter().any(|(_, tab_id)| tab_id == "tab_8"));
}

#[test]
fn focused_last_overflow_tab_shows_its_full_label() {
    let mut projected = snapshot();
    let labels = [
        "1",
        "Laiza Portfolio Site",
        "linkedin posts",
        "update cv",
        "nvim",
        "brother",
        "day organiser",
        "nvim test",
    ];
    projected.tabs = labels
        .iter()
        .enumerate()
        .map(|(index, label)| ClientShellTab {
            tab_id: format!("tab_{}", index + 1),
            workspace_id: "ws_1".into(),
            number: index + 1,
            label: (*label).into(),
            custom_label: index > 0,
            zoomed: false,
            focused: index == 7,
            agent_status: AgentStatus::Idle,
        })
        .collect();
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    for number in [8, 7, 8] {
        let tab_id = format!("tab_{number}");
        projected.focused_tab_id = Some(tab_id.clone());
        projected.workspaces[0].active_tab_id = tab_id.clone();
        projected.panes[0].tab_id = tab_id.clone();
        for tab in &mut projected.tabs {
            tab.focused = tab.tab_id == tab_id;
        }
        state.set_snapshot(Box::new(projected.clone()));
        state.set_pane_surface(surface());
        let frame = state
            .compose(133, 20)
            .expect("reporter's overflowing strip");
        assert_eq!(
            state.hits.new_tab.right() - state.hits.tab_scroll_left.x,
            107
        );
        let rect = state
            .hits
            .tabs
            .iter()
            .find(|(_, id)| id == &tab_id)
            .expect("focused tab")
            .0;
        let text = (rect.x..rect.right())
            .map(|x| {
                frame.cells[(rect.y * frame.width + x) as usize]
                    .symbol
                    .as_str()
            })
            .collect::<String>();
        assert!(
            text.contains(labels[number - 1]),
            "focused tab rendered as {text:?}, rect={rect:?}"
        );
    }

    // Manual scrolling must be able to reveal the rest of a partially drawn last tab.
    let end_scroll = state.tab_scroll;
    let left = state.hits.tab_scroll_left;
    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: left.x + 1,
        row: left.y,
        modifiers: KeyModifiers::empty(),
    })]);
    let frame = state.compose(133, 20).expect("manual scroll left");
    assert_eq!(state.tab_scroll, end_scroll - 1);
    let right = state.hits.tab_scroll_right;
    assert_eq!(
        frame.cells[(right.y * frame.width + right.x + 1) as usize].fg,
        crate::protocol::color_to_u32(state.config.palette.overlay1),
        "right arrow stays enabled while the final tab is clipped"
    );
    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: right.x + 1,
        row: right.y,
        modifiers: KeyModifiers::empty(),
    })]);
    let frame = state.compose(133, 20).expect("manual scroll right");
    assert_eq!(state.tab_scroll, end_scroll);
    assert!(frame_rows(&frame)[0].contains("nvim test"));
    assert_eq!(
        frame.cells[(right.y * frame.width + right.x + 1) as usize].fg,
        crate::protocol::color_to_u32(state.config.palette.overlay0),
        "right arrow dims at the useful scroll limit"
    );
}

#[test]
fn focused_workspace_change_reveals_new_workspace_in_full_sidebar() {
    let mut initial = snapshot();
    let template = initial.workspaces[0].clone();
    initial.workspaces = (1..=12)
        .map(|number| ClientShellWorkspace {
            workspace_id: format!("ws_{number}"),
            number,
            label: format!("space-{number}"),
            branch: None,
            focused: number == 1,
            ..template.clone()
        })
        .collect();

    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(initial));
    state.set_pane_surface(surface());
    state.compose(106, 20).expect("full sidebar");
    assert!(state.hits.workspace_max_scroll > 0);
    assert!(state
        .hits
        .workspaces
        .iter()
        .all(|hit| hit.workspace_id != "ws_12"));

    let mut update = state.snapshot.as_deref().expect("snapshot").clone();
    update.revision = 2;
    update.focused_workspace_id = Some("ws_12".into());
    for workspace in &mut update.workspaces {
        workspace.focused = workspace.workspace_id == "ws_12";
    }
    let mut updated_surface = surface();
    updated_surface.projection_revision = 2;
    state.set_snapshot(Box::new(update));
    state.set_pane_surface(updated_surface);
    state.compose(106, 2).expect("zero-height workspace body");
    assert!(state.reveal_focused_workspace);
    state.compose(106, 20).expect("updated full sidebar");

    assert!(state
        .hits
        .workspaces
        .iter()
        .any(|hit| hit.workspace_id == "ws_12"));
}

#[test]
fn client_owned_sidebar_dividers_resize_live() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.compose(106, 30).expect("expanded sidebar");
    assert!(state.hits.machines.is_empty());
    let workspace_body = state.hits.workspace_body;
    let needless_scroll =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: workspace_body.x,
            row: workspace_body.y,
            modifiers: KeyModifiers::empty(),
        })]);
    assert_eq!(state.hits.workspace_max_scroll, 0);
    assert_eq!(state.workspace_scroll, 0);
    assert!(!needless_scroll.repaint);
    let width_divider = state.hits.sidebar_divider;
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: width_divider.x,
        row: width_divider.y + 2,
        modifiers: KeyModifiers::empty(),
    })]);
    let resize =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 31,
            row: width_divider.y + 2,
            modifiers: KeyModifiers::empty(),
        })]);
    assert_eq!(state.sidebar_width, 32);
    assert!(state.sidebar_width_manual);
    assert!(resize.repaint);
    assert!(resize.resize);
    let waiting_frame = state.compose(106, 30).expect("waiting for resized surface");
    let waiting_text: String = waiting_frame
        .cells
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect();
    assert!(
        waiting_text.contains(" spaces"),
        "local sidebar must keep spaces while resizing: {waiting_text}"
    );
    assert!(!waiting_text.contains(" machines"));
    assert!(!waiting_text.contains("Select a connected machine"));
    assert!(!waiting_text.contains("LIVE"));
    assert!(waiting_frame.cursor.is_none());
    assert!(state.pane_surface.is_none());
    assert!(state.hits.panes.is_empty());
    assert!(state.hits.pane_splits.is_empty());
    assert!(state.hits.machines.is_empty());
    assert_eq!(state.hits.sidebar_divider.x, 31);
    assert_eq!(state.hits.workspaces[0].workspace_id, "ws_1");

    let next_resize =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 32,
            row: width_divider.y + 2,
            modifiers: KeyModifiers::empty(),
        })]);
    assert!(next_resize.resize);
    state.compose(106, 30).expect("continued resize");
    assert_eq!(state.hits.sidebar_divider.x, 32);
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: 32,
        row: width_divider.y + 2,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(state.chrome_drag.is_none());

    state.set_pane_surface(surface());
    let recovered_frame = state.compose(106, 30).expect("resized sidebar");
    let recovered_text: String = recovered_frame
        .cells
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect();
    assert!(recovered_text.contains(" spaces"));
    assert!(recovered_text.contains("LIVE"));
    assert!(!state.hits.panes.is_empty());
    let section_divider = state.hits.sidebar_section_divider;
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: section_divider.x + 2,
        row: section_divider.y,
        modifiers: KeyModifiers::empty(),
    })]);
    let split = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: section_divider.x + 2,
        row: 20,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(state.sidebar_section_split > 0.6);
    assert!(split.repaint);
    assert!(!split.resize);
}

#[test]
fn context_menus_capture_stable_targets_and_route_actions() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.compose(106, 20).expect("composed frame");

    let workspace = state.hits.workspaces[0].rect;
    let open_workspace_menu =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: workspace.x + 2,
            row: workspace.y,
            modifiers: KeyModifiers::empty(),
        })]);
    assert!(open_workspace_menu.actions.is_empty());
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            target: ClientContextMenuTarget::Workspace { ref workspace_id, .. },
            ..
        })) if workspace_id == "ws_1"
    ));
    let workspace_items = match state.overlay.as_ref() {
        Some(ClientShellOverlay::ContextMenu(menu)) => menu.items(),
        _ => panic!("workspace context menu"),
    };
    assert!(workspace_items
        .iter()
        .any(|item| item.action == ClientContextMenuAction::NewWorktree));
    state.compose(106, 20).expect("workspace context menu");
    let rename = state.hits.context_menu_rows[0].0;
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: rename.x + 1,
        row: rename.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Rename(ClientRenameOverlay {
            target: ClientRenameTarget::Workspace { ref workspace_id },
            ..
        })) if workspace_id == "ws_1"
    ));

    state.overlay = None;
    state.compose(106, 20).expect("composed frame");
    let pane = state.hits.panes[0].rect;
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Right),
        column: pane.x + 1,
        row: pane.y,
        modifiers: KeyModifiers::empty(),
    })]);
    state.compose(106, 20).expect("pane context menu");
    let split_index = match state.overlay.as_ref() {
        Some(ClientShellOverlay::ContextMenu(menu)) => menu
            .items()
            .iter()
            .position(|item| item.action == ClientContextMenuAction::SplitRight)
            .expect("split right item"),
        _ => panic!("pane context menu"),
    };
    let split = state.hits.context_menu_rows[split_index].0;
    let outcome =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: split.x + 1,
            row: split.y,
            modifiers: KeyModifiers::empty(),
        })]);
    let [ClientShellAction::Endpoint { request, .. }] = &outcome.actions[..] else {
        panic!("pane split context action should use endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::PaneSplit(params)
            if params.target_pane_id.as_deref() == Some("pane_1")
                && params.direction == crate::api::schema::SplitDirection::Right
    ));
}

#[test]
fn global_menu_opens_from_sidebar_and_routes_client_actions() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.compose(106, 30).expect("shell frame");
    let launcher = state.hits.global_launcher;
    assert_ne!(launcher, Rect::default());

    let open = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: launcher.x,
        row: launcher.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(open.repaint);
    let menu = state.compose(106, 30).expect("global menu");
    let text = menu
        .cells
        .chunks(menu.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("settings"));
    assert!(text.contains("keybinds"));
    assert!(text.contains("reload config"));
    assert!(text.contains("detach"));

    let keybinds = state.hits.global_menu_rows[1].0;
    let help = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: keybinds.x,
        row: keybinds.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(help.actions.is_empty());
    assert!(matches!(state.overlay, Some(ClientShellOverlay::Help(_))));

    let detach_index = super::super::global_menu::global_menu_items(&snapshot())
        .iter()
        .position(|(label, _)| *label == "detach")
        .expect("detach item");
    state.overlay = Some(ClientShellOverlay::GlobalMenu(ClientGlobalMenuOverlay {
        highlighted: detach_index,
        scroll: 0,
    }));
    let detach = state.handle_input_bytes(b"\r");
    assert!(detach.detach);
    assert!(state.overlay.is_none());
}

#[test]
fn new_tab_overlay_owns_text_cursor_and_submits_public_api_request() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    let mut open = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::NewTab),
        &mut open,
    );
    assert!(open.actions.is_empty());
    let frame = state.compose(106, 20).expect("new tab overlay");
    let text = frame
        .cells
        .chunks(frame.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("new tab"));
    assert!(text.contains("save"));
    let restored = frame.to_ratatui_buffer().expect("overlay frame");
    assert!(!restored
        .cell((26, 7))
        .expect("overlay title cell")
        .modifier
        .contains(Modifier::DIM));
    assert!(frame.cursor.as_ref().is_some_and(|cursor| cursor.visible));

    assert!(state.handle_input_bytes(b"logs").actions.is_empty());
    let create = state.handle_input_bytes(b"\r");
    let [ClientShellAction::Endpoint { request, .. }] = &create.actions[..] else {
        panic!("new tab save should use endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::TabCreate(params)
            if params.workspace_id.as_deref() == Some("ws_1")
                && params.label.as_deref() == Some("logs")
    ));
    assert!(state.overlay.is_none());
}

#[test]
fn close_confirmation_error_becomes_client_owned_overlay_and_stable_workspace_close() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    let mut close = ClientShellInput::default();
    // Exercise the server error fallback directly: last-pane keybindings now
    // confirm locally before issuing a request.
    state.push_endpoint_method(
        crate::api::schema::Method::PaneClose(crate::api::schema::PaneTarget {
            pane_id: "pane_1".into(),
        }),
        &mut close,
    );
    let [ClientShellAction::Endpoint { request, .. }] = &close.actions[..] else {
        panic!("pane close should use endpoint API");
    };
    let request_id = request.id.clone();
    assert!(
        state
            .handle_endpoint_result(
                "boot-1",
                &request_id,
                Err(ClientShellEndpointError {
                    code: Some("confirmation_required".into()),
                    message: "confirmation required".into(),
                }),
            )
            .0
    );
    let frame = state.compose(106, 20).expect("confirmation overlay");
    let text = frame
        .cells
        .chunks(frame.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("Close workspace?"));
    assert!(text.contains("1 pane"));

    let confirm = state.handle_input_bytes(b"\r");
    let [ClientShellAction::Endpoint { request, .. }] = &confirm.actions[..] else {
        panic!("confirmation should use endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::WorkspaceClose(params)
            if params.workspace_id == "ws_1" && !params.close_group
    ));
}

mod popup_menus {
    use super::*;
    use crate::client::shell::global_menu::{global_menu_item_has_badge, global_menu_items};
    use crossterm::event::KeyCode;

    fn shell(rows: u16) -> ClientShellState {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(snapshot()));
        state.set_pane_surface(surface());
        state.compose(106, rows).expect("composed frame");
        state
    }

    fn mouse(
        state: &mut ClientShellState,
        kind: MouseEventKind,
        x: u16,
        y: u16,
    ) -> ClientShellInput {
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::empty(),
        })])
    }

    fn key(state: &mut ClientShellState, code: KeyCode) -> ClientShellInput {
        state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
            code,
            KeyModifiers::empty(),
        ))])
    }

    fn symbol_at(frame: &FrameData, x: u16, y: u16) -> String {
        frame.cells[y as usize * frame.width as usize + x as usize]
            .symbol
            .clone()
    }

    /// Right-clicks the first workspace row and returns the clicked cell.
    fn open_workspace_menu(state: &mut ClientShellState) -> (u16, u16) {
        let workspace = state.hits.workspaces[0].rect;
        let click = (workspace.x + 2, workspace.y);
        mouse(
            state,
            MouseEventKind::Down(MouseButton::Right),
            click.0,
            click.1,
        );
        click
    }

    fn context_items(state: &ClientShellState) -> Vec<ClientContextMenuItem> {
        match state.overlay.as_ref() {
            Some(ClientShellOverlay::ContextMenu(menu)) => menu.items(),
            _ => panic!("context menu is open"),
        }
    }

    fn context_highlight(state: &ClientShellState) -> usize {
        match state.overlay.as_ref() {
            Some(ClientShellOverlay::ContextMenu(menu)) => menu.highlighted,
            _ => panic!("context menu is open"),
        }
    }

    fn global_highlight(state: &ClientShellState) -> usize {
        match state.overlay.as_ref() {
            Some(ClientShellOverlay::GlobalMenu(menu)) => menu.highlighted,
            _ => panic!("global menu is open"),
        }
    }

    fn open_global_menu(state: &mut ClientShellState) -> Rect {
        let launcher = state.hits.global_launcher;
        mouse(
            state,
            MouseEventKind::Down(MouseButton::Left),
            launcher.x,
            launcher.y,
        );
        launcher
    }

    #[test]
    fn spaces_context_menu_opens_as_a_framed_panel_whose_corner_is_the_clicked_cell() {
        let mut state = shell(20);
        let (x, y) = open_workspace_menu(&mut state);
        let items = context_items(&state);
        let frame = state.compose(106, 20).expect("context menu frame");

        let width = items
            .iter()
            .map(|item| crate::client::shell::render::display_width(item.label))
            .max()
            .unwrap_or(0)
            .saturating_add(4)
            .max(14);
        let height = items.len() as u16 + 2;
        let rows = &state.hits.context_menu_rows;
        assert_eq!(rows.len(), items.len());
        for (index, (rect, item)) in rows.iter().enumerate() {
            assert_eq!(*item, index);
            assert_eq!(*rect, Rect::new(x + 1, y + 1 + index as u16, width - 2, 1));
        }
        assert_eq!(symbol_at(&frame, x, y), "┌");
        assert_eq!(symbol_at(&frame, x + width - 1, y), "┐");
        assert_eq!(symbol_at(&frame, x, y + height - 1), "└");
        assert_eq!(symbol_at(&frame, x + width - 1, y + height - 1), "┘");
        assert_eq!(symbol_at(&frame, x, y + 1), "│");
        assert_eq!(context_highlight(&state), 0);
    }

    #[test]
    fn moving_over_a_spaces_context_menu_row_highlights_it_and_moving_off_rows_keeps_it() {
        let mut state = shell(20);
        open_workspace_menu(&mut state);
        state.compose(106, 20).expect("context menu frame");
        let row = state.hits.context_menu_rows[2].0;

        let outcome = mouse(&mut state, MouseEventKind::Moved, row.x + 1, row.y);
        assert_eq!(context_highlight(&state), 2);
        assert!(outcome.repaint);

        let outcome = mouse(&mut state, MouseEventKind::Moved, 105, 19);
        assert_eq!(context_highlight(&state), 2);
        assert!(!outcome.repaint);
    }

    #[test]
    fn clicking_outside_a_spaces_context_menu_closes_it_and_the_click_reaches_nothing_else() {
        let mut state = shell(20);
        open_workspace_menu(&mut state);
        state.compose(106, 20).expect("context menu frame");
        let pane = state.hits.panes[0].rect;
        let point = (pane.right() - 2, pane.bottom() - 2);
        assert!(!state
            .hits
            .context_menu_rows
            .iter()
            .any(|(rect, _)| contains(*rect, point)));

        let outcome = mouse(
            &mut state,
            MouseEventKind::Down(MouseButton::Left),
            point.0,
            point.1,
        );

        assert!(state.overlay.is_none());
        assert!(outcome.actions.is_empty());
        assert!(outcome.repaint);
    }

    #[test]
    fn spaces_context_menu_keys_move_within_bounds_activate_with_enter_and_close_with_escape() {
        let mut state = shell(20);
        open_workspace_menu(&mut state);
        let last = context_items(&state).len() - 1;

        key(&mut state, KeyCode::Up);
        assert_eq!(context_highlight(&state), 0);
        key(&mut state, KeyCode::Down);
        key(&mut state, KeyCode::Down);
        key(&mut state, KeyCode::Up);
        assert_eq!(context_highlight(&state), 1);
        for _ in 0..last + 3 {
            key(&mut state, KeyCode::Down);
        }
        assert_eq!(context_highlight(&state), last);

        key(&mut state, KeyCode::Esc);
        assert!(state.overlay.is_none());

        open_workspace_menu(&mut state);
        assert_eq!(
            context_items(&state)[0].action,
            ClientContextMenuAction::Rename
        );
        let outcome = key(&mut state, KeyCode::Enter);
        assert!(outcome.actions.is_empty());
        assert!(matches!(
            state.overlay,
            Some(ClientShellOverlay::Rename(ClientRenameOverlay {
                target: ClientRenameTarget::Workspace { ref workspace_id },
                ..
            })) if workspace_id == "ws_1"
        ));
    }

    #[test]
    fn global_menu_opens_as_a_framed_panel_right_aligned_directly_above_the_launcher() {
        let mut state = shell(30);
        let launcher = open_global_menu(&mut state);
        let frame = state.compose(106, 30).expect("global menu frame");

        let items = global_menu_items(&snapshot());
        let width = items
            .iter()
            .map(|(label, action)| {
                crate::client::shell::render::display_width(label)
                    + u16::from(global_menu_item_has_badge(&snapshot(), *action)) * 2
            })
            .max()
            .unwrap_or(8)
            + 4;
        let height = items.len() as u16 + 2;
        let x = launcher.right() - width;
        let y = launcher.y - height;
        let rows = &state.hits.global_menu_rows;
        assert_eq!(rows.len(), items.len());
        for (index, (rect, item)) in rows.iter().enumerate() {
            assert_eq!(*item, index);
            assert_eq!(*rect, Rect::new(x + 1, y + 1 + index as u16, width - 2, 1));
        }
        assert_eq!(symbol_at(&frame, x, y), "┌");
        assert_eq!(symbol_at(&frame, x + width - 1, y), "┐");
        assert_eq!(symbol_at(&frame, x, y + height - 1), "└");
        assert_eq!(symbol_at(&frame, x + width - 1, y + height - 1), "┘");
        assert_eq!(global_highlight(&state), 0);
    }

    #[test]
    fn moving_over_a_global_menu_row_highlights_it() {
        let mut state = shell(30);
        open_global_menu(&mut state);
        state.compose(106, 30).expect("global menu frame");
        let row = state.hits.global_menu_rows[2].0;

        let outcome = mouse(&mut state, MouseEventKind::Moved, row.x + 1, row.y);

        assert_eq!(global_highlight(&state), 2);
        assert!(outcome.repaint);
    }

    #[test]
    fn clicking_outside_the_global_menu_or_on_its_launcher_closes_it() {
        let mut state = shell(30);
        open_global_menu(&mut state);
        state.compose(106, 30).expect("global menu frame");
        let pane = state.hits.panes[0].rect;
        let outcome = mouse(
            &mut state,
            MouseEventKind::Down(MouseButton::Left),
            pane.right() - 2,
            pane.y + 1,
        );
        assert!(state.overlay.is_none());
        assert!(outcome.actions.is_empty());

        let launcher = open_global_menu(&mut state);
        assert!(matches!(
            state.overlay,
            Some(ClientShellOverlay::GlobalMenu(_))
        ));
        state.compose(106, 30).expect("global menu frame");
        mouse(
            &mut state,
            MouseEventKind::Down(MouseButton::Left),
            launcher.x,
            launcher.y,
        );
        assert!(state.overlay.is_none());
    }

    #[test]
    fn global_menu_keys_move_within_bounds_with_arrows_or_j_k_and_close_with_escape() {
        let mut state = shell(30);
        open_global_menu(&mut state);
        let last = global_menu_items(&snapshot()).len() - 1;

        key(&mut state, KeyCode::Up);
        assert_eq!(global_highlight(&state), 0);
        key(&mut state, KeyCode::Down);
        key(&mut state, KeyCode::Char('j'));
        key(&mut state, KeyCode::Char('k'));
        assert_eq!(global_highlight(&state), 1);
        key(&mut state, KeyCode::Up);
        for _ in 0..last + 3 {
            key(&mut state, KeyCode::Down);
        }
        assert_eq!(global_highlight(&state), last);

        key(&mut state, KeyCode::Esc);
        assert!(state.overlay.is_none());
    }

    fn new_workspace_shell() -> ClientShellState {
        let mut config = Config::default();
        config.ui.prompt_new_workspace_name = false;
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
        state.set_snapshot(Box::new(snapshot()));
        state.set_pane_surface(surface());
        state.compose(106, 20).expect("composed frame");
        state
    }

    /// Clicks the spaces " new" footer and renders the chooser it opens.
    fn open_new_workspace_chooser(state: &mut ClientShellState) -> ClientShellInput {
        let footer = state.hits.new_workspace;
        let outcome = mouse(
            state,
            MouseEventKind::Down(MouseButton::Left),
            footer.x + 1,
            footer.y,
        );
        state.compose(106, 20).expect("chooser frame");
        outcome
    }

    fn click_chooser_row(state: &mut ClientShellState, index: usize) -> ClientShellInput {
        let (row, _) = state
            .hits
            .new_workspace_menu_rows
            .iter()
            .copied()
            .find(|(_, item)| *item == index)
            .expect("chooser row drawn");
        mouse(
            state,
            MouseEventKind::Down(MouseButton::Left),
            row.x + 1,
            row.y,
        )
    }

    fn method_names(methods: &[crate::api::schema::Method]) -> Option<Vec<String>> {
        Some(
            methods
                .iter()
                .map(|method| crate::api::api_method_name(method).to_owned())
                .collect(),
        )
    }

    fn single_request(outcome: &ClientShellInput) -> &crate::api::schema::Method {
        let [ClientShellAction::Endpoint { request, .. }] = &outcome.actions[..] else {
            panic!("one endpoint request");
        };
        &request.method
    }

    #[test]
    fn clicking_new_workspace_creates_one_from_the_current_workspace_without_cwd_or_label() {
        let mut state = new_workspace_shell();

        let opened = open_new_workspace_chooser(&mut state);
        assert!(opened.actions.is_empty());
        assert!(matches!(
            state.overlay,
            Some(ClientShellOverlay::NewWorkspaceMenu(_))
        ));

        let outcome = click_chooser_row(&mut state, 0);

        let crate::api::schema::Method::WorkspaceCreate(params) = single_request(&outcome) else {
            panic!("workspace.create");
        };
        assert_eq!(params.source_workspace_id.as_deref(), Some("ws_1"));
        assert_eq!(params.cwd, None);
        assert_eq!(params.label, None);
        assert!(params.focus);
        assert!(params.env.is_empty());
        assert!(state.overlay.is_none());
    }

    #[test]
    fn new_workspace_chooser_lists_local_ssh_and_container_directly_above_the_footer() {
        let mut state = new_workspace_shell();
        let footer = state.hits.new_workspace;
        open_new_workspace_chooser(&mut state);

        let labels: Vec<&str> = state
            .new_workspace_menu_items()
            .iter()
            .map(|item| item.label)
            .collect();
        assert_eq!(labels, ["local", "ssh host…", "container…"]);
        let rows = &state.hits.new_workspace_menu_rows;
        let indices: Vec<usize> = rows.iter().map(|(_, index)| *index).collect();
        assert_eq!(indices, [0, 1, 2]);
        let last = rows[2].0;
        assert_eq!(
            rows[0].0.x,
            footer.x + 1,
            "left border on the footer column"
        );
        assert_eq!(
            last.y + 2,
            footer.y,
            "bottom border on the row above the footer"
        );
    }

    #[test]
    fn new_workspace_chooser_disables_ssh_host_when_the_server_lacks_ssh_hosts() {
        use crate::api::schema::{ContainerListParams, Method};
        let mut state = new_workspace_shell();
        state.set_endpoint_methods(method_names(&[Method::ContainerList(
            ContainerListParams::default(),
        )]));
        open_new_workspace_chooser(&mut state);

        let enabled: Vec<bool> = state
            .new_workspace_menu_items()
            .iter()
            .map(|item| item.enabled)
            .collect();
        assert_eq!(enabled, [true, false, true]);
        let click = click_chooser_row(&mut state, 1);
        assert!(click.actions.is_empty());
        key(&mut state, KeyCode::Down);
        key(&mut state, KeyCode::Down);
        let enter = key(&mut state, KeyCode::Enter);
        assert!(enter.actions.is_empty());
        assert!(matches!(
            state.overlay,
            Some(ClientShellOverlay::NewWorkspaceMenu(_))
        ));
    }

    #[test]
    fn new_workspace_chooser_disables_container_when_the_server_cannot_list_containers() {
        use crate::api::schema::{Method, SshHostsParams};
        let mut state = new_workspace_shell();
        state.set_endpoint_methods(method_names(&[Method::SshHosts(SshHostsParams::default())]));

        let enabled: Vec<bool> = state
            .new_workspace_menu_items()
            .iter()
            .map(|item| item.enabled)
            .collect();
        assert_eq!(enabled, [true, true, false]);
    }

    #[test]
    fn choosing_ssh_host_in_the_new_workspace_chooser_lists_ssh_hosts_for_the_picker() {
        let mut state = new_workspace_shell();
        open_new_workspace_chooser(&mut state);

        let outcome = click_chooser_row(&mut state, 1);

        assert!(matches!(
            single_request(&outcome),
            crate::api::schema::Method::SshHosts(_)
        ));
        assert!(state.overlay.is_none());
    }

    #[test]
    fn enter_activates_only_the_highlighted_chooser_row() {
        let mut state = new_workspace_shell();
        open_new_workspace_chooser(&mut state);

        key(&mut state, KeyCode::Down);
        key(&mut state, KeyCode::Down);
        let outcome = key(&mut state, KeyCode::Enter);

        assert!(matches!(
            single_request(&outcome),
            crate::api::schema::Method::SshHosts(_)
        ));
    }

    fn chooser_highlight(state: &ClientShellState) -> Option<usize> {
        match state.overlay.as_ref() {
            Some(ClientShellOverlay::NewWorkspaceMenu(menu)) => menu.highlighted,
            _ => panic!("new workspace chooser is open"),
        }
    }

    #[test]
    fn the_new_workspace_chooser_opens_with_no_row_highlighted_and_enter_does_nothing() {
        let mut state = new_workspace_shell();
        open_new_workspace_chooser(&mut state);
        assert_eq!(chooser_highlight(&state), None);

        let outcome = key(&mut state, KeyCode::Enter);

        assert!(outcome.actions.is_empty());
        assert_eq!(chooser_highlight(&state), None);
    }

    #[test]
    fn down_or_hovering_highlights_a_chooser_row() {
        let mut state = new_workspace_shell();
        open_new_workspace_chooser(&mut state);
        key(&mut state, KeyCode::Down);
        assert_eq!(chooser_highlight(&state), Some(0));

        key(&mut state, KeyCode::Esc);
        open_new_workspace_chooser(&mut state);
        key(&mut state, KeyCode::Up);
        assert_eq!(
            chooser_highlight(&state),
            Some(2),
            "up starts at the last row"
        );

        key(&mut state, KeyCode::Esc);
        open_new_workspace_chooser(&mut state);
        assert_eq!(chooser_highlight(&state), None);
        let (row, _) = state.hits.new_workspace_menu_rows[2];
        mouse(&mut state, MouseEventKind::Moved, row.x + 1, row.y);
        assert_eq!(chooser_highlight(&state), Some(2));
    }

    #[test]
    fn the_wheel_over_an_overflowing_popup_menu_scrolls_it_by_one_and_keeps_the_highlight_inside() {
        let mut state = shell(6);
        open_global_menu(&mut state);
        state.compose(106, 6).expect("global menu frame");
        let item_count = global_menu_items(&snapshot()).len();
        let visible = state.hits.global_menu_rows.len();
        assert!(visible + 1 < item_count);
        let (top, _) = state.hits.global_menu_rows[0];

        mouse(&mut state, MouseEventKind::ScrollDown, top.x + 1, top.y);
        state.compose(106, 6).expect("scrolled menu frame");

        assert_eq!(state.hits.global_menu_rows[0].1, 1);
        assert_eq!(global_highlight(&state), 1);

        let (top, _) = state.hits.global_menu_rows[0];
        mouse(&mut state, MouseEventKind::ScrollUp, top.x + 1, top.y);
        state.compose(106, 6).expect("scrolled back frame");

        assert_eq!(state.hits.global_menu_rows[0].1, 0);
        assert_eq!(global_highlight(&state), 1);
    }

    #[test]
    fn esc_or_a_click_outside_closes_the_new_workspace_chooser_without_a_request() {
        let mut state = new_workspace_shell();
        open_new_workspace_chooser(&mut state);
        let esc = key(&mut state, KeyCode::Esc);
        assert!(esc.actions.is_empty());
        assert!(state.overlay.is_none());

        open_new_workspace_chooser(&mut state);
        let outside = mouse(&mut state, MouseEventKind::Down(MouseButton::Left), 100, 2);
        assert!(outside.actions.is_empty());
        assert!(state.overlay.is_none());
    }

    #[test]
    fn multi_endpoint_new_footer_opens_the_same_chooser() {
        let mut state = new_workspace_shell();
        let profile = crate::client::endpoint::SavedSshEndpoint {
            id: crate::client::endpoint::ProfileId::parse("0123456789abcdef0123456789abcdef")
                .expect("profile id"),
            label: "Build".into(),
            target: "dev@build.example".into(),
            session: "agents".into(),
            enabled: true,
        };
        let remote = ClientEndpointId::Ssh(profile.id.clone());
        state.set_endpoint_catalog(&[profile]);
        state.set_endpoint_status(&remote, ClientEndpointStatus::Online);
        state.compose(106, 20).expect("endpoint sidebar frame");
        let footer = state.hits.new_workspace;
        assert!(footer.width > " new".len() as u16, "endpoint footer drawn");

        open_new_workspace_chooser(&mut state);
        let outcome = click_chooser_row(&mut state, 0);

        assert!(matches!(
            single_request(&outcome),
            crate::api::schema::Method::WorkspaceCreate(_)
        ));
    }

    #[test]
    fn an_overflowing_menu_keeps_its_scrolled_window_when_a_visible_row_is_hovered() {
        let mut state = shell(6);
        open_global_menu(&mut state);
        state.compose(106, 6).expect("global menu frame");
        let item_count = global_menu_items(&snapshot()).len();
        assert!(state.hits.global_menu_rows.len() < item_count);
        for _ in 0..item_count {
            key(&mut state, KeyCode::Down);
        }
        state.compose(106, 6).expect("scrolled menu frame");
        let scrolled: Vec<usize> = state
            .hits
            .global_menu_rows
            .iter()
            .map(|(_, index)| *index)
            .collect();
        assert_eq!(scrolled.last(), Some(&(item_count - 1)));

        let (top, top_index) = state.hits.global_menu_rows[0];
        mouse(&mut state, MouseEventKind::Moved, top.x + 1, top.y);
        state.compose(106, 6).expect("hovered menu frame");

        assert_eq!(global_highlight(&state), top_index);
        let after: Vec<usize> = state
            .hits
            .global_menu_rows
            .iter()
            .map(|(_, index)| *index)
            .collect();
        assert_eq!(after, scrolled);
    }
}
