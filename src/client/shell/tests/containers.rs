use super::*;

fn container_dto(
    reference: &str,
    name: &str,
    project: Option<&str>,
    service: Option<&str>,
    working_dir: Option<&str>,
    is_default: bool,
) -> crate::api::schema::ContainerInfoDto {
    crate::api::schema::ContainerInfoDto {
        reference: reference.into(),
        id: "0123456789abcdef0123".into(),
        name: name.into(),
        project: project.map(str::to_owned),
        service: service.map(str::to_owned),
        working_dir: working_dir.map(str::to_owned),
        workspace_id: None,
        ssh_host: None,
        is_default,
    }
}

fn containers_result() -> crate::api::schema::ResponseResult {
    crate::api::schema::ResponseResult::ContainerList {
        containers: vec![
            container_dto("herdr-dev", "herdr-dev", None, None, Some("/work"), true),
            container_dto(
                "shop/web",
                "shop-web-1",
                Some("shop"),
                Some("web"),
                Some("/srv"),
                false,
            ),
            container_dto("redis-1", "redis-1", None, None, None, false),
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
            "focused": true, "container": label, "agent_status": "unknown", "revision": 0
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

/// Runs the binding on a server without `ssh.hosts` and answers its
/// local `container.list` listing.
fn open_picker(state: &mut ClientShellState) {
    state.set_endpoint_methods(Some(vec!["container.list".into(), "container.open".into()]));
    let mut prepare = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::OpenContainerWorkspace),
        &mut prepare,
    );
    let [ClientShellAction::Endpoint { request, .. }] = &prepare.actions[..] else {
        panic!("open container should list containers through container.list");
    };
    assert!(matches!(
        request.method,
        crate::api::schema::Method::ContainerList(_)
    ));
    let request_id = request.id.clone();
    state.handle_endpoint_result("boot-1", &request_id, Ok(containers_result()));
}

fn frame_text(state: &mut ClientShellState) -> String {
    let frame = state.compose(106, 30).expect("open container modal");
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
        ],
    }
}

fn endpoint_request(actions: &[ClientShellAction]) -> &crate::api::schema::Request {
    let [ClientShellAction::Endpoint { request, .. }] = actions else {
        panic!("expected exactly one endpoint request: {actions:?}");
    };
    request
}

/// Runs the binding and answers `ssh.hosts`, leaving the host stage open.
fn open_host_stage(state: &mut ClientShellState) {
    let mut prepare = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::OpenContainerWorkspace),
        &mut prepare,
    );
    let request = endpoint_request(&prepare.actions);
    assert!(matches!(
        request.method,
        crate::api::schema::Method::SshHosts(_)
    ));
    let request_id = request.id.clone();
    state.handle_endpoint_result("boot-1", &request_id, Ok(ssh_hosts_result()));
}

fn host_titles(state: &ClientShellState) -> Vec<String> {
    let Some(ClientShellOverlay::OpenPicker(open)) = &state.overlay else {
        panic!("expected the open picker");
    };
    open.entries
        .iter()
        .map(|entry| entry.title.clone())
        .collect()
}

/// Picks the host row at `index` and answers its `container.list`.
fn choose_host(state: &mut ClientShellState, downs: usize) -> Option<String> {
    for _ in 0..downs {
        state.handle_input_bytes(b"\x1b[B");
    }
    let pick = state.handle_input_bytes(b"\r");
    let request = endpoint_request(&pick.actions);
    let crate::api::schema::Method::ContainerList(params) = &request.method else {
        panic!(
            "choosing a host should list its containers: {:?}",
            request.method
        );
    };
    let ssh_host = params.ssh_host.clone();
    let request_id = request.id.clone();
    state.handle_endpoint_result("boot-1", &request_id, Ok(containers_result()));
    ssh_host
}

fn assert_container_open_request(actions: &[ClientShellAction], reference: &str, label: &str) {
    let [ClientShellAction::Endpoint { request, .. }] = actions else {
        panic!("choosing a container should open it through container.open: {actions:?}");
    };
    let crate::api::schema::Method::ContainerOpen(params) = &request.method else {
        panic!("expected container.open, got {:?}", request.method);
    };
    assert_eq!(params.container, reference);
    assert_eq!(
        params.target,
        crate::api::schema::ContainerOpenTarget::Workspace(
            crate::api::schema::WorkspaceCreateParams {
                source_workspace_id: None,
                cwd: None,
                focus: true,
                label: Some(label.into()),
                env: Default::default(),
            }
        )
    );
}

#[test]
fn container_picker_lists_running_containers_with_their_details() {
    let mut state = shell_state();

    open_picker(&mut state);

    let Some(ClientShellOverlay::OpenPicker(open)) = &state.overlay else {
        panic!("the listing should open the container picker");
    };
    let shown: Vec<(&str, &str, &str, &str)> = open
        .entries
        .iter()
        .map(|entry| {
            (
                entry.id.as_str(),
                entry.title.as_str(),
                entry.status,
                entry.detail.as_str(),
            )
        })
        .collect();
    assert_eq!(
        shown,
        [
            ("herdr-dev", "herdr-dev", "default", "/work"),
            ("shop/web", "shop-web-1", "", "shop/web"),
            ("redis-1", "redis-1", "", "0123456789ab"),
        ]
    );
    let text = frame_text(&mut state);
    assert!(text.contains("open container"), "{text}");
    assert!(text.contains("shop/web"), "{text}");
    assert!(text.contains("3 containers"), "{text}");
}

#[test]
fn container_picker_enter_opens_the_selected_container_in_a_new_named_workspace() {
    let mut state = shell_state();
    open_picker(&mut state);

    state.handle_input_bytes(b"\x1b[B");
    let open = state.handle_input_bytes(b"\r");

    assert_container_open_request(&open.actions, "shop/web", "shop-web-1");
    assert!(matches!(
        &state.overlay,
        Some(ClientShellOverlay::OpenPicker(open)) if open.opening
    ));
    let [ClientShellAction::Endpoint { request, .. }] = &open.actions[..] else {
        unreachable!();
    };
    let request_id = request.id.clone();
    state.handle_endpoint_result("boot-1", &request_id, Ok(workspace_created("shop-web-1")));
    assert!(
        state.overlay.is_none(),
        "a successful open closes the picker"
    );
}

#[test]
fn container_picker_filter_narrows_containers_and_enter_opens_the_match() {
    let mut state = shell_state();
    open_picker(&mut state);

    state.handle_input_bytes(b"redis");
    let open = state.handle_input_bytes(b"\r");

    assert_container_open_request(&open.actions, "redis-1", "redis-1");
}

#[test]
fn container_picker_row_click_opens_that_container() {
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

    assert_container_open_request(&open.actions, "shop/web", "shop-web-1");
}

#[test]
fn container_picker_escape_closes_without_a_request() {
    let mut state = shell_state();
    open_picker(&mut state);

    let closed = state.handle_input_bytes(b"\x1b");

    assert!(closed.actions.is_empty());
    assert!(state.overlay.is_none());
}

#[test]
fn container_picker_keeps_the_rejection_and_allows_another_choice() {
    let mut state = shell_state();
    open_picker(&mut state);
    let open = state.handle_input_bytes(b"\r");
    let [ClientShellAction::Endpoint { request, .. }] = &open.actions[..] else {
        panic!("enter should open the first container");
    };
    let request_id = request.id.clone();

    let (_, actions) = state.handle_endpoint_result(
        "boot-1",
        &request_id,
        Err(ClientShellEndpointError {
            code: Some("container_not_found".into()),
            message: "no running container matches herdr-dev".into(),
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
        Some("no running container matches herdr-dev")
    );
    state.handle_input_bytes(b"\x1b[B");
    let retry = state.handle_input_bytes(b"\r");
    assert_container_open_request(&retry.actions, "shop/web", "shop-web-1");
}

#[test]
fn container_picker_reports_no_containers_without_opening() {
    let mut state = shell_state();
    let mut prepare = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::OpenContainerWorkspace),
        &mut prepare,
    );
    let [ClientShellAction::Endpoint { request, .. }] = &prepare.actions[..] else {
        panic!("open container should list containers through container.list");
    };
    let request_id = request.id.clone();

    state.handle_endpoint_result(
        "boot-1",
        &request_id,
        Ok(crate::api::schema::ResponseResult::ContainerList {
            containers: Vec::new(),
        }),
    );

    assert!(state.overlay.is_none());
    assert_eq!(
        state.endpoint_error.as_deref(),
        Some("No running docker containers found.")
    );
}

#[test]
fn container_picker_on_a_server_without_container_list_only_disables_the_action() {
    let mut state = shell_state();
    state.set_endpoint_methods(Some(vec!["workspace.create".into()]));
    let mut prepare = ClientShellInput::default();

    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::OpenContainerWorkspace),
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
        .is_some_and(|notice| notice.key.code == "container.list"));
    assert!(state.snapshot.is_some(), "the connection stays up");
}

#[test]
fn global_menu_offers_open_container_before_open_ssh_host() {
    let snapshot = snapshot();
    let labels: Vec<&str> = super::super::global_menu::global_menu_items(&snapshot)
        .into_iter()
        .map(|(label, _)| label)
        .collect();

    let container = labels
        .iter()
        .position(|label| *label == "open container")
        .expect("global menu entry");
    assert_eq!(labels.last(), Some(&"detach"));
    assert_eq!(labels[labels.len() - 2], "open ssh host");
    assert_eq!(container, labels.len() - 3);

    let mut state = shell_state();
    state.overlay = Some(ClientShellOverlay::GlobalMenu(ClientGlobalMenuOverlay {
        highlighted: container,
    }));
    let prepare = state.handle_input_bytes(b"\r");
    assert!(matches!(
        &prepare.actions[..],
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(request.method, crate::api::schema::Method::SshHosts(_))
    ));
}

#[test]
fn host_stage_lists_local_first_then_ssh_hosts_with_destinations() {
    let mut state = shell_state();

    open_host_stage(&mut state);

    assert_eq!(host_titles(&state), ["local", "prod", "lab"]);
    let text = frame_text(&mut state);
    assert!(text.contains("open container"), "{text}");
    assert!(text.contains("deploy@prod.example.com"), "{text}");
    assert!(text.contains("3 hosts"), "{text}");
}

#[test]
fn choosing_local_lists_containers_without_an_ssh_host() {
    let mut state = shell_state();
    open_host_stage(&mut state);

    assert_eq!(choose_host(&mut state, 0), None);

    assert_eq!(host_titles(&state), ["herdr-dev", "shop-web-1", "redis-1"]);
    let text = frame_text(&mut state);
    assert!(text.contains("open container · local"), "{text}");
}

#[test]
fn choosing_a_host_lists_its_containers_and_opens_them_with_a_host_suffixed_label() {
    let mut state = shell_state();
    open_host_stage(&mut state);

    assert_eq!(choose_host(&mut state, 1), Some("prod".into()));

    let text = frame_text(&mut state);
    assert!(text.contains("open container · prod"), "{text}");
    state.handle_input_bytes(b"\x1b[B");
    let open = state.handle_input_bytes(b"\r");
    let request = endpoint_request(&open.actions);
    let crate::api::schema::Method::ContainerOpen(params) = &request.method else {
        panic!("expected container.open, got {:?}", request.method);
    };
    assert_eq!(params.container, "shop/web");
    assert_eq!(params.ssh_host.as_deref(), Some("prod"));
    assert_eq!(
        params.target,
        crate::api::schema::ContainerOpenTarget::Workspace(
            crate::api::schema::WorkspaceCreateParams {
                source_workspace_id: None,
                cwd: None,
                focus: true,
                label: Some("shop-web-1@prod".into()),
                env: Default::default(),
            }
        )
    );
}

#[test]
fn escape_in_the_container_stage_returns_to_the_retained_hosts_then_closes() {
    let mut state = shell_state();
    open_host_stage(&mut state);
    choose_host(&mut state, 1);

    let back = state.handle_input_bytes(b"\x1b");

    assert!(
        back.actions.is_empty(),
        "going back must not ask the server"
    );
    assert_eq!(host_titles(&state), ["local", "prod", "lab"]);
    let Some(ClientShellOverlay::OpenPicker(open)) = &state.overlay else {
        panic!("Esc keeps the picker on the host stage");
    };
    assert_eq!(open.selected, 1, "the chosen host stays selected");
    let closed = state.handle_input_bytes(b"\x1b");
    assert!(closed.actions.is_empty());
    assert!(state.overlay.is_none());
}

#[test]
fn an_unreachable_host_keeps_the_host_stage_with_the_error() {
    let mut state = shell_state();
    open_host_stage(&mut state);
    state.handle_input_bytes(b"\x1b[B");
    let pick = state.handle_input_bytes(b"\r");
    let request_id = endpoint_request(&pick.actions).id.clone();

    state.handle_endpoint_result(
        "boot-1",
        &request_id,
        Err(ClientShellEndpointError {
            code: Some("ssh_failed".into()),
            message: "ssh prod: connection refused".into(),
        }),
    );

    let Some(ClientShellOverlay::OpenPicker(open)) = &state.overlay else {
        panic!("a failed listing keeps the picker");
    };
    assert_eq!(open.source, OpenPickerSource::ContainerHost);
    assert!(!open.opening);
    assert_eq!(open.error.as_deref(), Some("ssh prod: connection refused"));
    assert_eq!(choose_host(&mut state, 1), Some("lab".into()));
}

#[test]
fn a_host_without_running_containers_keeps_the_host_stage() {
    let mut state = shell_state();
    open_host_stage(&mut state);
    state.handle_input_bytes(b"\x1b[B");
    let pick = state.handle_input_bytes(b"\r");
    let request_id = endpoint_request(&pick.actions).id.clone();

    state.handle_endpoint_result(
        "boot-1",
        &request_id,
        Ok(crate::api::schema::ResponseResult::ContainerList {
            containers: Vec::new(),
        }),
    );

    let Some(ClientShellOverlay::OpenPicker(open)) = &state.overlay else {
        panic!("an empty listing keeps the picker");
    };
    assert_eq!(open.source, OpenPickerSource::ContainerHost);
    assert_eq!(
        open.error.as_deref(),
        Some("No running docker containers found on prod.")
    );
}

#[test]
fn a_server_without_ssh_hosts_lists_local_containers_without_a_notice() {
    let mut state = shell_state();
    state.set_endpoint_methods(Some(vec!["container.list".into()]));
    let mut prepare = ClientShellInput::default();

    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::OpenContainerWorkspace),
        &mut prepare,
    );

    let request = endpoint_request(&prepare.actions);
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::ContainerList(params) if params.ssh_host.is_none()
    ));
    assert!(state.visible_endpoint_notice.is_none());
}

#[test]
fn an_empty_ssh_host_list_skips_the_host_stage() {
    let mut state = shell_state();
    let mut prepare = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::OpenContainerWorkspace),
        &mut prepare,
    );
    let request_id = endpoint_request(&prepare.actions).id.clone();

    let (_, actions) = state.handle_endpoint_result(
        "boot-1",
        &request_id,
        Ok(crate::api::schema::ResponseResult::SshHosts { hosts: Vec::new() }),
    );

    let request = endpoint_request(&actions);
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::ContainerList(params) if params.ssh_host.is_none()
    ));
    let request_id = request.id.clone();
    state.handle_endpoint_result("boot-1", &request_id, Ok(containers_result()));
    assert_eq!(host_titles(&state), ["herdr-dev", "shop-web-1", "redis-1"]);
}

#[test]
fn a_failed_ssh_host_listing_falls_back_to_local_containers() {
    let mut state = shell_state();
    let mut prepare = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::OpenContainerWorkspace),
        &mut prepare,
    );
    let request_id = endpoint_request(&prepare.actions).id.clone();

    // An unreadable ~/.ssh/config must not make local containers unreachable.
    let (_, actions) = state.handle_endpoint_result(
        "boot-1",
        &request_id,
        Err(ClientShellEndpointError {
            code: Some("ssh_unavailable".into()),
            message: "could not read ~/.ssh/config".into(),
        }),
    );

    let request = endpoint_request(&actions);
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::ContainerList(params) if params.ssh_host.is_none()
    ));
    let request_id = request.id.clone();
    state.handle_endpoint_result("boot-1", &request_id, Ok(containers_result()));
    assert_eq!(host_titles(&state), ["herdr-dev", "shop-web-1", "redis-1"]);
}

#[test]
fn typing_filters_the_host_stage_without_slash() {
    let mut state = shell_state();
    open_host_stage(&mut state);

    state.handle_input_bytes(b"lab");

    assert_eq!(choose_host(&mut state, 0), Some("lab".into()));
}

#[test]
fn ctrl_n_moves_the_selection_while_typing_in_the_container_stage() {
    let mut state = shell_state();
    open_picker(&mut state);

    state.handle_input_bytes(b"\x0e");
    let open = state.handle_input_bytes(b"\r");

    assert_container_open_request(&open.actions, "shop/web", "shop-web-1");
}

#[test]
fn host_row_click_lists_that_hosts_containers() {
    let mut state = shell_state();
    open_host_stage(&mut state);
    frame_text(&mut state);

    let (row, _) = state.hits.worktree_rows[1];
    let pick = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: row.x + 2,
        row: row.y,
        modifiers: KeyModifiers::empty(),
    })]);

    assert!(matches!(
        &endpoint_request(&pick.actions).method,
        crate::api::schema::Method::ContainerList(params) if params.ssh_host.as_deref() == Some("prod")
    ));
}
