use super::super::preferences::{
    self, ClientChromePreferences, ClientOpenPickerPreferences, ClientOpenPickerRecent,
};
use super::*;

/// Endpoint preferences file removed again when the test ends, pass or fail.
struct TempPreferences(std::path::PathBuf);

impl TempPreferences {
    fn new(test: &str, memory: Option<ClientOpenPickerPreferences>) -> Self {
        let path = std::env::temp_dir().join(format!(
            "herdr-open-picker-{test}-{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        if let Some(memory) = memory {
            preferences::store(
                &path,
                ClientChromePreferences {
                    open_picker: Some(memory),
                    ..ClientChromePreferences::default()
                },
            )
            .expect("seed open picker preferences");
        }
        Self(path)
    }

    fn state(&self) -> ClientShellState {
        let config = ClientShellConfig::from_config(&Config::default())
            .with_preferences_path(self.0.clone());
        let mut state = ClientShellState::new(config);
        state.set_snapshot(Box::new(snapshot()));
        state.set_pane_surface(surface());
        state
    }

    fn stored(&self) -> ClientOpenPickerPreferences {
        preferences::load(&self.0)
            .and_then(|saved| saved.open_picker)
            .unwrap_or_default()
    }
}

impl Drop for TempPreferences {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn memory(favorites: &[&str], recent: &[(&str, u64, u32)]) -> ClientOpenPickerPreferences {
    ClientOpenPickerPreferences {
        favorites: favorites.iter().map(|key| (*key).to_owned()).collect(),
        recent: recent
            .iter()
            .map(|(key, last_used_ms, count)| ClientOpenPickerRecent {
                key: (*key).to_owned(),
                last_used_ms: *last_used_ms,
                count: *count,
            })
            .collect(),
    }
}

fn ssh_hosts_result() -> crate::api::schema::ResponseResult {
    let host = |alias: &str, hostname: Option<&str>| crate::api::schema::SshHostInfoDto {
        alias: alias.into(),
        hostname: hostname.map(str::to_owned),
        user: None,
    };
    crate::api::schema::ResponseResult::SshHosts {
        hosts: vec![
            host("prod", Some("prod.example.com")),
            host("lab", None),
            host("staging", Some("staging.example.com")),
        ],
    }
}

fn container(name: &str, is_default: bool) -> crate::api::schema::ContainerInfoDto {
    crate::api::schema::ContainerInfoDto {
        reference: name.into(),
        id: format!("{name}-0123456789abcdef"),
        name: name.into(),
        project: None,
        service: None,
        working_dir: Some(format!("/srv/{name}")),
        workspace_id: None,
        ssh_host: None,
        is_default,
    }
}

fn containers_result() -> crate::api::schema::ResponseResult {
    crate::api::schema::ResponseResult::ContainerList {
        containers: vec![
            container("herdr-dev", true),
            container("shop-web-1", false),
            container("redis-1", false),
            container("db-1", false),
        ],
    }
}

fn workspace_created() -> crate::api::schema::ResponseResult {
    serde_json::from_value(serde_json::json!({
        "type": "workspace_created",
        "workspace": {
            "workspace_id": "ws_9", "number": 9, "label": "opened", "focused": true,
            "pane_count": 1, "tab_count": 1, "active_tab_id": "ws_9:1", "agent_status": "unknown"
        },
        "tab": {
            "tab_id": "ws_9:1", "workspace_id": "ws_9", "number": 1, "label": "1",
            "focused": true, "pane_count": 1, "agent_status": "unknown"
        },
        "root_pane": {
            "pane_id": "ws_9:p1", "terminal_id": "t9", "workspace_id": "ws_9", "tab_id": "ws_9:1",
            "focused": true, "agent_status": "unknown", "revision": 0
        }
    }))
    .expect("workspace_created result")
}

fn endpoint_request(actions: &[ClientShellAction]) -> &crate::api::schema::Request {
    let [ClientShellAction::Endpoint { request, .. }] = actions else {
        panic!("expected exactly one endpoint request: {actions:?}");
    };
    request
}

fn answer(
    state: &mut ClientShellState,
    actions: &[ClientShellAction],
    result: crate::api::schema::ResponseResult,
) {
    let request_id = endpoint_request(actions).id.clone();
    state.handle_endpoint_result("boot-1", &request_id, Ok(result));
}

fn open_ssh_picker(state: &mut ClientShellState) {
    let mut prepare = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::OpenSshWorkspace),
        &mut prepare,
    );
    answer(state, &prepare.actions, ssh_hosts_result());
}

/// Local listing only: a server without `ssh.hosts` skips the host stage.
fn open_local_container_picker(state: &mut ClientShellState) {
    state.set_endpoint_methods(Some(vec!["container.list".into(), "container.open".into()]));
    let mut prepare = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::OpenContainerWorkspace),
        &mut prepare,
    );
    answer(state, &prepare.actions, containers_result());
}

fn open_host_stage(state: &mut ClientShellState) {
    let mut prepare = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::OpenContainerWorkspace),
        &mut prepare,
    );
    answer(state, &prepare.actions, ssh_hosts_result());
}

fn picker(state: &ClientShellState) -> &ClientOpenPickerOverlay {
    let Some(ClientShellOverlay::OpenPicker(open)) = &state.overlay else {
        panic!("expected the open picker");
    };
    open
}

/// Titles in the order the picker shows them, after the current filter.
fn listed(state: &ClientShellState) -> Vec<String> {
    let open = picker(state);
    open.filtered_indices()
        .into_iter()
        .map(|index| open.entries[index].title.clone())
        .collect()
}

fn selected_title(state: &ClientShellState) -> String {
    let open = picker(state);
    let index = open.selected_entry_index().expect("a selected row");
    open.entries[index].title.clone()
}

fn frame_text(state: &mut ClientShellState) -> String {
    let frame = state.compose(106, 30).expect("open picker modal");
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

fn click(state: &mut ClientShellState, x: u16, y: u16) -> ClientShellInput {
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: x,
        row: y,
        modifiers: KeyModifiers::empty(),
    })])
}

#[test]
fn ssh_hosts_list_favorites_then_recent_opens_then_the_rest_in_config_order() {
    let prefs = TempPreferences::new(
        "ssh-order",
        Some(memory(&["ssh:staging"], &[("ssh:lab", 1_000, 1)])),
    );
    let mut state = prefs.state();

    open_ssh_picker(&mut state);

    assert_eq!(listed(&state), ["staging", "lab", "prod"]);
    assert_eq!(
        selected_title(&state),
        "staging",
        "the top row starts selected"
    );
}

#[test]
fn containers_list_favorites_then_the_default_then_recent_opens_then_the_rest() {
    let prefs = TempPreferences::new(
        "container-order",
        Some(memory(
            &["container:local:redis-1"],
            &[("container:local:db-1", 1_000, 1)],
        )),
    );
    let mut state = prefs.state();

    open_local_container_picker(&mut state);

    assert_eq!(
        listed(&state),
        ["redis-1", "herdr-dev", "db-1", "shop-web-1"]
    );
}

#[test]
fn recent_opens_list_the_latest_first_and_the_more_frequent_on_a_tie() {
    let prefs = TempPreferences::new(
        "recent-order",
        Some(memory(
            &[],
            &[
                ("ssh:prod", 1_000, 1),
                ("ssh:lab", 2_000, 1),
                ("ssh:staging", 1_000, 4),
            ],
        )),
    );
    let mut state = prefs.state();

    open_ssh_picker(&mut state);

    assert_eq!(listed(&state), ["lab", "staging", "prod"]);
}

#[test]
fn filtering_keeps_the_favorite_and_recent_order_among_matches() {
    let prefs = TempPreferences::new(
        "filter-order",
        Some(memory(
            &["container:local:redis-1"],
            &[("container:local:db-1", 1_000, 1)],
        )),
    );
    let mut state = prefs.state();
    open_local_container_picker(&mut state);

    state.handle_input_bytes(b"-1");

    assert_eq!(listed(&state), ["redis-1", "db-1", "shop-web-1"]);
    let open = state.handle_input_bytes(b"\r");
    let crate::api::schema::Method::ContainerOpen(params) = &endpoint_request(&open.actions).method
    else {
        panic!("enter should open the first match through container.open");
    };
    assert_eq!(params.container, "redis-1-0123456789abcdef");
}

#[test]
fn star_key_favorites_the_selected_row_and_saves_it() {
    let prefs = TempPreferences::new("star-key", None);
    let mut state = prefs.state();
    open_ssh_picker(&mut state);
    state.handle_input_bytes(b"\x1b[B");

    let starred = state.handle_input_bytes(b"*");

    assert!(starred.actions.is_empty(), "starring never asks the server");
    assert_eq!(listed(&state), ["lab", "prod", "staging"]);
    assert_eq!(
        selected_title(&state),
        "lab",
        "the selection follows the row"
    );
    assert!(picker(&state).entries[0].favorite);
    assert_eq!(prefs.stored().favorites, ["ssh:lab"]);
    let mut reopened = prefs.state();
    open_ssh_picker(&mut reopened);
    assert_eq!(listed(&reopened), ["lab", "prod", "staging"]);

    state.handle_input_bytes(b"*");

    assert_eq!(listed(&state), ["prod", "lab", "staging"]);
    assert!(prefs.stored().favorites.is_empty());
}

#[test]
fn star_while_typing_a_filter_toggles_the_selected_row_instead_of_filtering() {
    let prefs = TempPreferences::new("star-typed", None);
    let mut state = prefs.state();
    open_ssh_picker(&mut state);
    state.handle_input_bytes(b"st");
    assert!(picker(&state).search_focused);

    state.handle_input_bytes(b"*");

    assert_eq!(picker(&state).query.to_string(), "st");
    assert!(picker(&state).search_focused, "the filter keeps the focus");
    assert_eq!(listed(&state), ["staging"]);
    assert_eq!(state.open_picker_memory.favorites, ["ssh:staging"]);
    assert_eq!(prefs.stored().favorites, ["ssh:staging"]);
}

#[test]
fn clicking_a_rows_star_column_toggles_it_without_opening() {
    let prefs = TempPreferences::new("star-click", None);
    let mut state = prefs.state();
    open_ssh_picker(&mut state);
    frame_text(&mut state);

    let (star, index) = state.hits.open_picker_favorites[1];
    assert_eq!(picker(&state).entries[index].title, "lab");
    let clicked = click(&mut state, star.x + 1, star.y);

    assert!(
        clicked.actions.is_empty(),
        "a star click must not open the row"
    );
    assert_eq!(listed(&state), ["lab", "prod", "staging"]);
    assert_eq!(selected_title(&state), "prod", "the selection stays put");
    assert_eq!(prefs.stored().favorites, ["ssh:lab"]);
}

#[test]
fn favorite_button_toggles_the_selected_row() {
    let prefs = TempPreferences::new("star-button", None);
    let mut state = prefs.state();
    open_ssh_picker(&mut state);
    state.handle_input_bytes(b"\x1b[B");
    frame_text(&mut state);

    let button = state.hits.overlay_favorite;
    let clicked = click(&mut state, button.x + 1, button.y);

    assert!(clicked.actions.is_empty());
    assert_eq!(listed(&state), ["lab", "prod", "staging"]);
    assert_eq!(prefs.stored().favorites, ["ssh:lab"]);
}

#[test]
fn favorites_show_a_star_before_the_title_and_the_buttons_offer_favorite() {
    let prefs = TempPreferences::new("star-render", Some(memory(&["ssh:staging"], &[])));
    let mut state = prefs.state();
    open_ssh_picker(&mut state);

    let text = frame_text(&mut state);

    assert!(text.contains("★ staging"), "{text}");
    assert!(!text.contains("★ prod"), "{text}");
    assert!(text.contains("↵ open"), "{text}");
    assert!(text.contains("* favorite"), "{text}");
    assert!(text.contains("esc cancel"), "{text}");
}

#[test]
fn a_successful_open_is_remembered_and_lists_that_host_first_next_time() {
    let prefs = TempPreferences::new("record-success", None);
    let mut state = prefs.state();
    open_ssh_picker(&mut state);
    state.handle_input_bytes(b"\x1b[B\x1b[B");

    let open = state.handle_input_bytes(b"\r");
    answer(&mut state, &open.actions, workspace_created());

    assert!(state.overlay.is_none());
    let stored = prefs.stored();
    assert_eq!(stored.recent.len(), 1);
    assert_eq!(stored.recent[0].key, "ssh:staging");
    assert_eq!(stored.recent[0].count, 1);
    assert!(stored.recent[0].last_used_ms > 0);
    open_ssh_picker(&mut state);
    assert_eq!(listed(&state), ["staging", "prod", "lab"]);
}

#[test]
fn the_latest_successful_open_lists_before_an_earlier_one() {
    let prefs = TempPreferences::new("record-latest", None);
    let mut state = prefs.state();
    for downs in [b"\x1b[B".as_slice(), b"\x1b[B\x1b[B".as_slice()] {
        open_ssh_picker(&mut state);
        state.handle_input_bytes(downs);
        let open = state.handle_input_bytes(b"\r");
        answer(&mut state, &open.actions, workspace_created());
    }

    open_ssh_picker(&mut state);

    // lab was opened first; staging was then the third row and opened last.
    assert_eq!(listed(&state), ["staging", "lab", "prod"]);
}

#[test]
fn a_rejected_open_is_not_remembered() {
    let prefs = TempPreferences::new("record-failure", None);
    let mut state = prefs.state();
    open_ssh_picker(&mut state);
    let open = state.handle_input_bytes(b"\r");
    let request_id = endpoint_request(&open.actions).id.clone();

    state.handle_endpoint_result(
        "boot-1",
        &request_id,
        Err(ClientShellEndpointError {
            code: Some("ssh_failed".into()),
            message: "ssh prod: connection refused".into(),
        }),
    );

    assert!(state.open_picker_memory.recent.is_empty());
    assert!(prefs.stored().recent.is_empty());
}

#[test]
fn a_remote_container_open_is_remembered_for_the_container_and_its_host() {
    let prefs = TempPreferences::new("record-remote", None);
    let mut state = prefs.state();
    open_host_stage(&mut state);
    assert_eq!(listed(&state), ["local", "prod", "lab", "staging"]);
    state.handle_input_bytes(b"\x1b[B\x1b[B");
    let pick = state.handle_input_bytes(b"\r");
    answer(&mut state, &pick.actions, containers_result());
    state.handle_input_bytes(b"\x1b[B\x1b[B");

    let open = state.handle_input_bytes(b"\r");
    assert!(matches!(
        &endpoint_request(&open.actions).method,
        crate::api::schema::Method::SshContainerOpen(params)
            if params.host == "lab" && params.container == "redis-1-0123456789abcdef"
    ));
    answer(&mut state, &open.actions, workspace_created());

    let keys: Vec<String> = prefs
        .stored()
        .recent
        .into_iter()
        .map(|recent| recent.key)
        .collect();
    assert!(
        keys.contains(&"container:lab:redis-1".to_owned()),
        "{keys:?}"
    );
    assert!(keys.contains(&"host:lab".to_owned()), "{keys:?}");
    open_host_stage(&mut state);
    assert_eq!(listed(&state), ["lab", "local", "prod", "staging"]);
}

#[test]
fn equally_named_containers_on_two_hosts_are_remembered_apart() {
    let prefs = TempPreferences::new("remote-scope", Some(memory(&["container:prod:db-1"], &[])));
    let mut state = prefs.state();
    open_local_container_picker(&mut state);

    assert_eq!(
        listed(&state),
        ["herdr-dev", "shop-web-1", "redis-1", "db-1"]
    );
    assert!(!picker(&state).entries.iter().any(|entry| entry.favorite));
}

#[test]
fn favorite_hosts_list_first_in_the_container_host_stage() {
    let prefs = TempPreferences::new("host-favorite", Some(memory(&["host:lab"], &[])));
    let mut state = prefs.state();

    open_host_stage(&mut state);

    assert_eq!(listed(&state), ["lab", "local", "prod", "staging"]);
}

#[test]
fn returning_to_the_host_stage_keeps_its_order_and_selection() {
    let prefs = TempPreferences::new("host-return", Some(memory(&["host:lab"], &[])));
    let mut state = prefs.state();
    open_host_stage(&mut state);
    state.handle_input_bytes(b"\x1b[B\x1b[B");
    let pick = state.handle_input_bytes(b"\r");
    answer(&mut state, &pick.actions, containers_result());

    state.handle_input_bytes(b"\x1b");

    assert_eq!(listed(&state), ["lab", "local", "prod", "staging"]);
    assert_eq!(selected_title(&state), "prod");
}

#[test]
fn favorites_missing_from_the_listing_are_hidden_but_kept() {
    let prefs = TempPreferences::new(
        "missing-favorite",
        Some(memory(&["ssh:retired", "ssh:lab"], &[])),
    );
    let mut state = prefs.state();
    open_ssh_picker(&mut state);

    assert_eq!(listed(&state), ["lab", "prod", "staging"]);
    state.handle_input_bytes(b"\x1b[B*");

    // Two favorites without history keep the order ssh config lists them in.
    assert_eq!(listed(&state), ["prod", "lab", "staging"]);
    let favorites = prefs.stored().favorites;
    assert!(
        favorites.contains(&"ssh:retired".to_owned()),
        "{favorites:?}"
    );
    assert!(favorites.contains(&"ssh:prod".to_owned()), "{favorites:?}");
    assert!(favorites.contains(&"ssh:lab".to_owned()), "{favorites:?}");
}
