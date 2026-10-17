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

/// Runs the binding and answers its `container.list` listing.
fn open_picker(state: &mut ClientShellState) {
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

    state.handle_input_bytes(b"/");
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
            if matches!(request.method, crate::api::schema::Method::ContainerList(_))
    ));
}
