use super::*;
use crate::api::schema::Method;
use crate::input::{KeybindAction, KeybindMatch};

const INHERITING_METHODS: [&str; 4] = [
    "tab.create_inherit",
    "pane.split_inherit",
    "tab.focus",
    "tab.create",
];

fn state_with_methods(
    methods: &[&str],
    off_host: bool,
    prompt_new_tab_name: bool,
) -> ClientShellState {
    let mut config = Config::default();
    config.ui.prompt_new_tab_name = prompt_new_tab_name;
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
    let mut snapshot = snapshot();
    if off_host {
        snapshot.panes[0].ssh_host = Some("prod".into());
    }
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(surface());
    state.set_endpoint_methods(Some(
        methods.iter().map(|name| (*name).to_owned()).collect(),
    ));
    state
}

fn requests(outcome: &ClientShellInput) -> Vec<&Method> {
    outcome
        .actions
        .iter()
        .filter_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => Some(&request.method),
            _ => None,
        })
        .collect()
}

fn press(state: &mut ClientShellState, action: KeybindAction) -> ClientShellInput {
    let mut outcome = ClientShellInput::default();
    state.record_binding(KeybindMatch::Action(action), &mut outcome);
    outcome
}

fn open_tab_menu_and_choose(
    state: &mut ClientShellState,
    action: ClientContextMenuAction,
) -> ClientShellInput {
    state.open_tab_context_menu("tab_1".into(), 5, 1);
    let index = match state.overlay.as_ref() {
        Some(ClientShellOverlay::ContextMenu(menu)) => menu
            .items()
            .iter()
            .position(|item| item.action == action)
            .expect("menu item"),
        _ => panic!("tab context menu"),
    };
    let mut outcome = ClientShellInput::default();
    state.activate_context_menu_item(index, &mut outcome);
    outcome
}

#[test]
fn new_tab_key_asks_the_server_to_inherit_the_focused_pane_location() {
    let mut state = state_with_methods(&INHERITING_METHODS, true, false);

    let outcome = press(&mut state, KeybindAction::NewTab);

    let [Method::TabCreateInherit(params)] = requests(&outcome)[..] else {
        panic!("expected tab.create_inherit: {:?}", requests(&outcome));
    };
    assert_eq!(params.create.workspace_id.as_deref(), Some("ws_1"));
    assert!(params.create.focus);
    assert_eq!(params.source_pane_id, None);
}

#[test]
fn new_tab_key_falls_back_to_tab_create_on_a_server_without_inheritance() {
    let mut state = state_with_methods(&["tab.create"], true, false);

    let outcome = press(&mut state, KeybindAction::NewTab);

    assert!(matches!(requests(&outcome)[..], [Method::TabCreate(_)]));
}

#[test]
fn new_tab_key_falls_back_to_tab_create_while_the_method_list_is_unknown() {
    let mut state = state_with_methods(&[], true, false);
    state.set_endpoint_methods(None);

    let outcome = press(&mut state, KeybindAction::NewTab);

    assert!(matches!(requests(&outcome)[..], [Method::TabCreate(_)]));
}

#[test]
fn split_key_asks_the_server_to_inherit_the_split_pane_location() {
    let mut state = state_with_methods(&INHERITING_METHODS, true, false);

    let outcome = press(&mut state, KeybindAction::SplitVertical);

    let [Method::PaneSplitInherit(params)] = requests(&outcome)[..] else {
        panic!("expected pane.split_inherit: {:?}", requests(&outcome));
    };
    assert_eq!(params.target_pane_id.as_deref(), Some("pane_1"));
}

#[test]
fn split_key_falls_back_to_pane_split_on_a_server_without_inheritance() {
    let mut state = state_with_methods(&["pane.split"], true, false);

    let outcome = press(&mut state, KeybindAction::SplitHorizontal);

    assert!(matches!(requests(&outcome)[..], [Method::PaneSplit(_)]));
}

#[test]
fn named_new_tab_asks_the_server_to_inherit_the_focused_pane_location() {
    let mut state = state_with_methods(&INHERITING_METHODS, true, true);
    press(&mut state, KeybindAction::NewTab);

    let outcome = state.handle_input_bytes(b"\r");

    assert!(matches!(
        requests(&outcome)[..],
        [Method::TabCreateInherit(_)]
    ));
}

#[test]
fn tab_menu_new_tab_asks_the_server_to_inherit() {
    let mut state = state_with_methods(&INHERITING_METHODS, true, false);

    let outcome = open_tab_menu_and_choose(&mut state, ClientContextMenuAction::NewTab);

    assert!(requests(&outcome)
        .iter()
        .any(|method| matches!(method, Method::TabCreateInherit(_))));
}

#[test]
fn tab_menu_offers_a_host_tab_only_while_the_focused_pane_runs_off_host() {
    let labels = |off_host| {
        let mut state = state_with_methods(&INHERITING_METHODS, off_host, false);
        state.open_tab_context_menu("tab_1".into(), 5, 1);
        match state.overlay.as_ref() {
            Some(ClientShellOverlay::ContextMenu(menu)) => menu
                .items()
                .iter()
                .map(|item| item.label)
                .collect::<Vec<_>>(),
            _ => panic!("tab context menu"),
        }
    };

    assert_eq!(labels(true), ["New tab", "New host tab", "Rename", "Close"]);
    assert_eq!(labels(false), ["New tab", "Rename", "Close"]);
}

#[test]
fn tab_menu_new_host_tab_sends_the_plain_tab_create() {
    let mut state = state_with_methods(&INHERITING_METHODS, true, false);

    let outcome = open_tab_menu_and_choose(&mut state, ClientContextMenuAction::NewHostTab);

    let methods = requests(&outcome);
    assert!(methods
        .iter()
        .any(|method| matches!(method, Method::TabCreate(_))));
    assert!(!methods
        .iter()
        .any(|method| matches!(method, Method::TabCreateInherit(_))));
}

#[test]
fn named_new_host_tab_sends_the_plain_tab_create() {
    let mut state = state_with_methods(&INHERITING_METHODS, true, true);
    open_tab_menu_and_choose(&mut state, ClientContextMenuAction::NewHostTab);

    let outcome = state.handle_input_bytes(b"\r");

    assert!(matches!(requests(&outcome)[..], [Method::TabCreate(_)]));
}

#[test]
fn pane_menu_split_asks_the_server_to_inherit() {
    let mut state = state_with_methods(&INHERITING_METHODS, true, false);
    state.open_pane_context_menu("pane_1".into(), 5, 3);
    let index = match state.overlay.as_ref() {
        Some(ClientShellOverlay::ContextMenu(menu)) => menu
            .items()
            .iter()
            .position(|item| item.action == ClientContextMenuAction::SplitDown)
            .expect("split item"),
        _ => panic!("pane context menu"),
    };

    let mut outcome = ClientShellInput::default();
    state.activate_context_menu_item(index, &mut outcome);

    assert!(matches!(
        requests(&outcome)[..],
        [Method::PaneSplitInherit(_)]
    ));
}

fn tab_menu_labels(state: &mut ClientShellState, tab_id: &str) -> Vec<&'static str> {
    state.open_tab_context_menu(tab_id.into(), 5, 1);
    match state.overlay.as_ref() {
        Some(ClientShellOverlay::ContextMenu(menu)) => {
            menu.items().iter().map(|item| item.label).collect()
        }
        _ => panic!("tab context menu"),
    }
}

/// Two tabs: `tab_1` is focused and runs on the host, `tab_2` has an ssh pane.
fn state_with_an_ssh_tab(methods: &[&str]) -> ClientShellState {
    let mut state = state_with_methods(methods, false, false);
    let mut snapshot = snapshot();
    let mut tab = snapshot.tabs[0].clone();
    tab.tab_id = "tab_2".into();
    tab.focused = false;
    snapshot.tabs.push(tab);
    let mut pane = snapshot.panes[0].clone();
    pane.pane_id = "pane_2".into();
    pane.tab_id = "tab_2".into();
    pane.focused = false;
    pane.ssh_host = Some("prod".into());
    snapshot.panes.push(pane);
    state.set_snapshot(Box::new(snapshot));
    state
}

#[test]
fn tab_menu_offers_a_host_tab_for_a_container_only_pane() {
    let mut state = state_with_methods(&INHERITING_METHODS, false, false);
    let mut snapshot = snapshot();
    snapshot.panes[0].container = Some("db".into());
    state.set_snapshot(Box::new(snapshot));

    assert!(tab_menu_labels(&mut state, "tab_1").contains(&"New host tab"));
}

#[test]
fn tab_menu_offers_a_host_tab_for_an_inactive_tab_that_runs_off_host() {
    let mut state = state_with_an_ssh_tab(&INHERITING_METHODS);

    assert!(tab_menu_labels(&mut state, "tab_2").contains(&"New host tab"));
    assert!(!tab_menu_labels(&mut state, "tab_1").contains(&"New host tab"));
}

#[test]
fn tab_menu_hides_the_host_tab_without_server_inheritance() {
    let mut state = state_with_methods(&["tab.create", "tab.focus"], true, false);

    assert_eq!(
        tab_menu_labels(&mut state, "tab_1"),
        ["New tab", "Rename", "Close"]
    );
}
