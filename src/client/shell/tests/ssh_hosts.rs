use super::*;

fn ssh_hosts_result() -> crate::api::schema::ResponseResult {
    crate::api::schema::ResponseResult::SshHosts {
        hosts: vec![
            crate::api::schema::SshHostInfoDto {
                alias: "prod".into(),
                hostname: Some("prod.example.com".into()),
                user: Some("deploy".into()),
            },
            crate::api::schema::SshHostInfoDto {
                alias: "lab".into(),
                hostname: None,
                user: None,
            },
            crate::api::schema::SshHostInfoDto {
                alias: "staging".into(),
                hostname: Some("staging.example.com".into()),
                user: None,
            },
        ],
    }
}

fn workspace_created(label: &str) -> crate::api::schema::ResponseResult {
    serde_json::from_value(serde_json::json!({
        "type": "workspace_created",
        "workspace": {
            "workspace_id": "ws_9", "number": 9, "label": label, "focused": true,
            "pane_count": 1, "tab_count": 1, "active_tab_id": "ws_9:1", "agent_status": "unknown"
        },
        "tab": {
            "tab_id": "ws_9:1", "workspace_id": "ws_9", "number": 1, "label": "1",
            "focused": true, "pane_count": 1, "agent_status": "unknown"
        },
        "root_pane": {
            "pane_id": "ws_9:p1", "terminal_id": "t9", "workspace_id": "ws_9", "tab_id": "ws_9:1",
            "focused": true, "ssh_host": label, "agent_status": "unknown", "revision": 0
        }
    }))
    .expect("workspace_created result")
}

fn shell_state() -> ClientShellState {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state
}

/// Runs the binding and answers its `ssh.hosts` listing.
fn open_picker(state: &mut ClientShellState) {
    let mut prepare = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::OpenSshWorkspace),
        &mut prepare,
    );
    let [ClientShellAction::Endpoint { request, .. }] = &prepare.actions[..] else {
        panic!("open ssh host should list hosts through ssh.hosts");
    };
    assert!(matches!(
        request.method,
        crate::api::schema::Method::SshHosts(_)
    ));
    let request_id = request.id.clone();
    state.handle_endpoint_result("boot-1", &request_id, Ok(ssh_hosts_result()));
}

fn frame_text(state: &mut ClientShellState) -> String {
    let frame = state.compose(106, 30).expect("open ssh host modal");
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

fn assert_ssh_open_request(actions: &[ClientShellAction], alias: &str) {
    let [ClientShellAction::Endpoint { request, .. }] = actions else {
        panic!("choosing a host should open it through ssh.open: {actions:?}");
    };
    let crate::api::schema::Method::SshOpen(params) = &request.method else {
        panic!("expected ssh.open, got {:?}", request.method);
    };
    assert_eq!(params.host, alias);
    assert_eq!(
        params.target,
        crate::api::schema::SshOpenTarget::Workspace(crate::api::schema::WorkspaceCreateParams {
            source_workspace_id: None,
            cwd: None,
            focus: true,
            label: Some(alias.into()),
            env: Default::default(),
        })
    );
}

#[test]
fn ssh_picker_lists_configured_hosts_with_their_destinations() {
    let mut state = shell_state();

    open_picker(&mut state);

    let Some(ClientShellOverlay::OpenPicker(open)) = &state.overlay else {
        panic!("the listing should open the ssh host picker");
    };
    let shown: Vec<(&str, &str)> = open
        .entries
        .iter()
        .map(|entry| (entry.title.as_str(), entry.detail.as_str()))
        .collect();
    assert_eq!(
        shown,
        [
            ("prod", "deploy@prod.example.com"),
            ("lab", ""),
            ("staging", "staging.example.com")
        ]
    );
    let text = frame_text(&mut state);
    assert!(text.contains("open ssh host"), "{text}");
    assert!(text.contains("deploy@prod.example.com"), "{text}");
    assert!(text.contains("3 hosts"), "{text}");
}

#[test]
fn ssh_picker_enter_opens_the_selected_host_in_a_new_named_workspace() {
    let mut state = shell_state();
    open_picker(&mut state);

    state.handle_input_bytes(b"\x1b[B");
    let open = state.handle_input_bytes(b"\r");

    assert_ssh_open_request(&open.actions, "lab");
    assert!(matches!(
        &state.overlay,
        Some(ClientShellOverlay::OpenPicker(open)) if open.opening
    ));
    let [ClientShellAction::Endpoint { request, .. }] = &open.actions[..] else {
        unreachable!();
    };
    let request_id = request.id.clone();
    state.handle_endpoint_result("boot-1", &request_id, Ok(workspace_created("lab")));
    assert!(
        state.overlay.is_none(),
        "a successful open closes the picker"
    );
}

#[test]
fn ssh_picker_filter_narrows_hosts_and_enter_opens_the_match() {
    let mut state = shell_state();
    open_picker(&mut state);

    state.handle_input_bytes(b"stag");
    let open = state.handle_input_bytes(b"\r");

    assert_ssh_open_request(&open.actions, "staging");
}

#[test]
fn ssh_picker_moves_the_selection_with_ctrl_keys_while_typing() {
    let mut state = shell_state();
    open_picker(&mut state);

    state.handle_input_bytes(b"\x0e");
    let open = state.handle_input_bytes(b"\r");

    assert_ssh_open_request(&open.actions, "lab");
}

#[test]
fn ssh_picker_row_click_opens_that_host() {
    let mut state = shell_state();
    open_picker(&mut state);
    frame_text(&mut state);

    let (row, index) = state.hits.worktree_rows[1];
    assert_eq!(index, 1, "rows map to entries, not screen order");
    let open = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: row.x + 2,
        row: row.y,
        modifiers: KeyModifiers::empty(),
    })]);

    assert_ssh_open_request(&open.actions, "lab");
}

#[test]
fn ssh_picker_escape_closes_without_a_request() {
    let mut state = shell_state();
    open_picker(&mut state);

    let closed = state.handle_input_bytes(b"\x1b");

    assert!(closed.actions.is_empty());
    assert!(state.overlay.is_none());
}

#[test]
fn ssh_picker_keeps_the_rejection_and_allows_another_choice() {
    let mut state = shell_state();
    open_picker(&mut state);
    let open = state.handle_input_bytes(b"\r");
    let [ClientShellAction::Endpoint { request, .. }] = &open.actions[..] else {
        panic!("enter should open the first host");
    };
    let request_id = request.id.clone();

    let (_, actions) = state.handle_endpoint_result(
        "boot-1",
        &request_id,
        Err(ClientShellEndpointError {
            code: Some("ssh_host_not_found".into()),
            message: "no Host alias in ~/.ssh/config matches prod".into(),
        }),
    );

    assert!(
        actions.is_empty(),
        "a rejection must not retry automatically"
    );
    let Some(ClientShellOverlay::OpenPicker(picker)) = &state.overlay else {
        panic!("a rejected open keeps the picker");
    };
    assert!(!picker.opening);
    assert_eq!(
        picker.error.as_deref(),
        Some("no Host alias in ~/.ssh/config matches prod")
    );
    state.handle_input_bytes(b"\x1b[B");
    let retry = state.handle_input_bytes(b"\r");
    assert_ssh_open_request(&retry.actions, "lab");
}

#[test]
fn ssh_picker_reports_an_empty_config_without_opening() {
    let mut state = shell_state();
    let mut prepare = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::OpenSshWorkspace),
        &mut prepare,
    );
    let [ClientShellAction::Endpoint { request, .. }] = &prepare.actions[..] else {
        panic!("open ssh host should list hosts through ssh.hosts");
    };
    let request_id = request.id.clone();

    state.handle_endpoint_result(
        "boot-1",
        &request_id,
        Ok(crate::api::schema::ResponseResult::SshHosts { hosts: Vec::new() }),
    );

    assert!(state.overlay.is_none());
    assert_eq!(
        state.endpoint_error.as_deref(),
        Some("No ssh hosts found in ~/.ssh/config.")
    );
}

#[test]
fn ssh_picker_on_a_server_without_ssh_hosts_only_disables_the_action() {
    let mut state = shell_state();
    state.set_endpoint_methods(Some(vec!["workspace.create".into()]));
    let mut prepare = ClientShellInput::default();

    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::OpenSshWorkspace),
        &mut prepare,
    );

    assert!(
        prepare.actions.is_empty(),
        "nothing is sent to an old server"
    );
    assert!(state.overlay.is_none());
    assert!(state
        .visible_endpoint_notice
        .as_ref()
        .is_some_and(|notice| notice.key.code == "ssh.hosts"));
    assert!(state.snapshot.is_some(), "the connection stays up");
}
