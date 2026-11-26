use crate::api::schema::{
    EmptyParams, Method, PaneListParams, PaneReadParams, PaneSendKeysParams, PaneSplitParams,
    Request, SshOpenParams, WorkspaceCloseParams,
};
use crate::app::App;
use crate::ssh_host::SshLaunch;
use crate::workspace::Workspace;

fn test_app() -> App {
    let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
    App::new(
        &crate::config::Config::default(),
        crate::app::AppPolicy::TEST,
        None,
        api_rx,
        crate::api::EventHub::default(),
    )
}

enum Kind {
    Local,
    Ssh(&'static str),
    ContainerOn(&'static str),
}

/// One workspace with two panes: the first is focused.
fn app_with(first: Kind, second: Kind) -> (App, String, String) {
    let mut app = test_app();
    let mut workspace = Workspace::test_new("prod");
    let first_pane = workspace.tabs[0].root_pane;
    let second_pane = workspace.test_split(ratatui::layout::Direction::Horizontal);
    workspace.tabs[0].layout.focus_pane(first_pane);
    app.state.workspaces = vec![workspace];
    app.state.ensure_test_terminals();
    app.state.active = Some(0);
    app.state.selected = 0;
    for (pane, kind) in [(first_pane, first), (second_pane, second)] {
        let terminal_id = app.state.workspaces[0].terminal_id(pane).unwrap().clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        match kind {
            Kind::Local => {}
            Kind::Ssh(host) => terminal.ssh = SshLaunch::new(host.into(), None),
            Kind::ContainerOn(host) => {
                terminal.container = Some(crate::container::ContainerLaunch {
                    reference: crate::container::ContainerRef::Name("web".into()),
                    canonical: "web".into(),
                    id: "cid".into(),
                    workdir: None,
                    start_dir: None,
                    user: None,
                    shell: crate::container::ContainerShell::BashOrSh,
                    ssh: SshLaunch::new(host.into(), None),
                });
            }
        }
    }
    let first_id = app.public_pane_id(0, first_pane).unwrap();
    let second_id = app.public_pane_id(0, second_pane).unwrap();
    (app, first_id, second_id)
}

fn request(method: Method) -> Request {
    Request {
        id: "req".into(),
        method,
    }
}

fn send_keys(pane_id: &str) -> Request {
    request(Method::PaneSendKeys(PaneSendKeysParams {
        pane_id: pane_id.into(),
        keys: vec!["Enter".into()],
    }))
}

fn error_code(response: &str) -> String {
    let value: serde_json::Value = serde_json::from_str(response).unwrap();
    value["error"]["code"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

#[test]
fn keys_for_a_pane_on_a_protected_host_are_refused() {
    let (app, prod, _) = app_with(Kind::Ssh("noref_prod"), Kind::Local);

    let denial = app
        .protected_host_denial(&send_keys(&prod))
        .expect("refused");

    assert_eq!(error_code(&denial), "protected_host");
}

#[test]
fn a_local_pane_beside_a_protected_one_stays_reachable() {
    let (app, _, local) = app_with(Kind::Ssh("noref_prod"), Kind::Local);

    assert_eq!(app.protected_host_denial(&send_keys(&local)), None);
}

#[test]
fn a_pane_on_an_unprotected_host_stays_reachable() {
    let (app, staging, _) = app_with(Kind::Ssh("staging"), Kind::Local);

    assert_eq!(app.protected_host_denial(&send_keys(&staging)), None);
}

#[test]
fn a_container_on_a_protected_host_is_protected() {
    let (app, web, _) = app_with(Kind::ContainerOn("deploy@noref_prod"), Kind::Local);

    assert!(app.protected_host_denial(&send_keys(&web)).is_some());
}

#[test]
fn the_screen_of_a_protected_pane_cannot_be_read() {
    let (app, prod, _) = app_with(Kind::Ssh("noref_prod"), Kind::Local);
    let read = request(Method::PaneRead(PaneReadParams {
        pane_id: prod,
        source: crate::api::schema::ReadSource::Recent,
        lines: None,
        format: crate::api::schema::ReadFormat::Text,
        strip_ansi: true,
        intent: crate::api::schema::ReadIntent::Passive,
    }));

    assert!(app.protected_host_denial(&read).is_some());
}

#[test]
fn listings_still_show_protected_panes() {
    let (app, _, _) = app_with(Kind::Ssh("noref_prod"), Kind::Local);

    for method in [
        Method::PaneList(PaneListParams::default()),
        Method::WorkspaceList(EmptyParams::default()),
        Method::SessionSnapshot(EmptyParams::default()),
    ] {
        assert_eq!(app.protected_host_denial(&request(method)), None);
    }
}

#[test]
fn a_workspace_holding_a_protected_pane_cannot_be_closed() {
    let (app, _, _) = app_with(Kind::Local, Kind::Ssh("noref_prod"));
    let workspace_id = app.public_workspace_id(0);
    let close = request(Method::WorkspaceClose(WorkspaceCloseParams {
        workspace_id,
        close_group: false,
    }));

    assert!(app.protected_host_denial(&close).is_some());
}

fn split_params(target_pane_id: Option<&str>) -> PaneSplitParams {
    serde_json::from_value(serde_json::json!({
        "direction": "right",
        "target_pane_id": target_pane_id,
    }))
    .unwrap()
}

fn split_focused() -> Request {
    request(Method::PaneSplit(split_params(None)))
}

#[test]
fn a_request_without_a_pane_acts_on_the_focused_pane() {
    let (protected_focus, _, _) = app_with(Kind::Ssh("noref_prod"), Kind::Local);
    assert!(protected_focus
        .protected_host_denial(&split_focused())
        .is_some());

    let (local_focus, _, _) = app_with(Kind::Local, Kind::Ssh("noref_prod"));
    assert_eq!(local_focus.protected_host_denial(&split_focused()), None);
}

#[test]
fn an_agent_name_on_a_protected_pane_is_refused() {
    let (mut app, prod, _) = app_with(Kind::Ssh("noref_prod"), Kind::Local);
    let (ws_idx, pane_id) = app.parse_pane_id(&prod).unwrap();
    let terminal_id = app.state.terminal_id_for_pane(ws_idx, pane_id).unwrap();
    app.state
        .terminals
        .get_mut(&terminal_id)
        .unwrap()
        .agent_name = Some("ops".into());
    let prompt = request(Method::AgentSendKeys(
        crate::api::schema::AgentSendKeysParams {
            target: "ops".into(),
            keys: vec!["Enter".into()],
        },
    ));

    assert!(app.protected_host_denial(&prompt).is_some());
}

fn ssh_open(host: &str) -> Request {
    ssh_open_into(host, None)
}

fn ssh_open_into(host: &str, split_of: Option<&str>) -> Request {
    let target = match split_of {
        Some(pane_id) => crate::api::schema::SshOpenTarget::Split(split_params(Some(pane_id))),
        None => crate::api::schema::SshOpenTarget::Workspace(
            serde_json::from_value(serde_json::json!({})).unwrap(),
        ),
    };
    request(Method::SshOpen(SshOpenParams {
        host: host.into(),
        target,
    }))
}

#[test]
fn a_new_pane_cannot_be_opened_beside_a_protected_one() {
    let (app, prod, local) = app_with(Kind::Ssh("noref_prod"), Kind::Local);

    assert!(app
        .protected_host_denial(&ssh_open_into("staging", Some(&prod)))
        .is_some());
    assert_eq!(
        app.protected_host_denial(&ssh_open_into("staging", Some(&local))),
        None
    );
}

#[test]
fn a_protected_host_cannot_be_opened_through_the_api() {
    let (app, _, _) = app_with(Kind::Local, Kind::Local);

    assert!(app.protected_host_denial(&ssh_open("noref_prod")).is_some());
    assert!(app
        .protected_host_denial(&ssh_open("deploy@NOREF_prod"))
        .is_some());
    assert_eq!(app.protected_host_denial(&ssh_open("staging")), None);
}

#[test]
fn the_refusal_names_what_was_refused() {
    let (app, prod, _) = app_with(Kind::Ssh("noref_prod"), Kind::Local);

    let denial = app.protected_host_denial(&send_keys(&prod)).unwrap();

    let value: serde_json::Value = serde_json::from_str(&denial).unwrap();
    assert_eq!(value["id"], "req");
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains(&format!("pane {prod}")),
        "{denial}"
    );
}

#[test]
fn a_directory_listing_from_a_protected_pane_is_refused() {
    let (app, prod, local) = app_with(Kind::Ssh("noref_prod"), Kind::Local);
    let list = |pane_id: Option<&str>| {
        request(Method::DirectoryList(
            serde_json::from_value(serde_json::json!({ "pane_id": pane_id })).unwrap(),
        ))
    };

    assert!(app.protected_host_denial(&list(Some(&prod))).is_some());
    assert!(
        app.protected_host_denial(&list(None)).is_some(),
        "the focused pane is protected"
    );
    assert_eq!(app.protected_host_denial(&list(Some(&local))), None);
}

#[test]
fn explaining_an_agent_on_a_protected_pane_is_refused() {
    let (app, prod, _) = app_with(Kind::Ssh("noref_prod"), Kind::Local);
    let explain = request(Method::AgentExplain(crate::api::schema::AgentTarget {
        target: prod,
    }));

    assert!(app.protected_host_denial(&explain).is_some());
}

#[test]
fn a_terminal_id_in_place_of_a_pane_id_is_refused() {
    let (app, prod, _) = app_with(Kind::Ssh("noref_prod"), Kind::Local);
    let (ws_idx, pane_id) = app.parse_pane_id(&prod).unwrap();
    let terminal_id = app.state.terminal_id_for_pane(ws_idx, pane_id).unwrap();

    assert!(app
        .protected_host_denial(&send_keys(terminal_id.as_str()))
        .is_some());
}

/// A second workspace with one local pane, after the protected-or-not pair
/// of `app_with`. The first workspace stays active.
fn add_local_workspace(app: &mut App) -> (String, String) {
    app.state.workspaces.push(Workspace::test_new("local"));
    app.state.ensure_test_terminals();
    let pane = app.state.workspaces[1].tabs[0].root_pane;
    (
        app.public_workspace_id(1),
        app.public_pane_id(1, pane).unwrap(),
    )
}

#[test]
fn a_split_in_a_named_local_workspace_ignores_the_global_focus() {
    let (mut app, _, _) = app_with(Kind::Ssh("noref_prod"), Kind::Local);
    let (local_workspace, _) = add_local_workspace(&mut app);
    let mut params = split_params(None);
    params.workspace_id = Some(local_workspace);

    assert_eq!(
        app.protected_host_denial(&request(Method::PaneSplit(params))),
        None
    );
}

#[test]
fn a_tab_without_a_workspace_goes_to_the_active_one() {
    let (app, _, _) = app_with(Kind::Local, Kind::Ssh("noref_prod"));
    let create = request(Method::TabCreate(
        serde_json::from_value(serde_json::json!({})).unwrap(),
    ));

    assert!(app.protected_host_denial(&create).is_some());
}

#[test]
fn a_command_for_a_named_local_tab_ignores_the_global_focus() {
    let (mut app, _, _) = app_with(Kind::Ssh("noref_prod"), Kind::Local);
    add_local_workspace(&mut app);
    let local_tab = app.public_tab_id(1, 0).unwrap();
    let invoke = |tab_id: Option<String>| {
        request(Method::CommandInvoke(
            serde_json::from_value(serde_json::json!({
                "command_id": "c",
                "tab_id": tab_id,
            }))
            .unwrap(),
        ))
    };

    assert_eq!(app.protected_host_denial(&invoke(Some(local_tab))), None);
    assert!(app.protected_host_denial(&invoke(None)).is_some());
}

#[test]
fn a_directional_request_from_a_local_pane_cannot_reach_a_protected_neighbour() {
    let (app, _, local) = app_with(Kind::Ssh("noref_prod"), Kind::Local);
    let swap = request(Method::PaneSwap(
        serde_json::from_value(serde_json::json!({
            "pane_id": local,
            "direction": "left",
        }))
        .unwrap(),
    ));

    assert!(app.protected_host_denial(&swap).is_some());
}

#[test]
fn a_plugin_pane_without_a_placement_opens_in_the_active_workspace() {
    let (app, _, _) = app_with(Kind::Local, Kind::Ssh("noref_prod"));
    let open = request(Method::PluginPaneOpen(
        serde_json::from_value(serde_json::json!({
            "plugin_id": "p",
            "entrypoint": "e",
        }))
        .unwrap(),
    ));

    assert!(app.protected_host_denial(&open).is_some());
}

/// A local parent workspace and a linked worktree workspace whose one pane
/// is on a protected host.
fn app_with_protected_linked_worktree() -> (App, String, String) {
    let (mut app, _, _) = app_with(Kind::Ssh("noref_prod"), Kind::Ssh("noref_prod"));
    app.state.workspaces[0].worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
        key: "repo-key".into(),
        label: "herdr".into(),
        repo_root: "/repo/herdr".into(),
        checkout_path: "/repo/herdr-issue".into(),
        is_linked_worktree: true,
    });
    let mut parent = Workspace::test_new("parent");
    parent.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
        key: "repo-key".into(),
        label: "herdr".into(),
        repo_root: "/repo/herdr".into(),
        checkout_path: "/repo/herdr".into(),
        is_linked_worktree: false,
    });
    app.state.workspaces.insert(0, parent);
    app.state.active = Some(0);
    app.state.ensure_test_terminals();
    let parent_pane = app.state.workspaces[0].tabs[0].root_pane;
    let workspace = app.public_workspace_id(0);
    let pane = app.public_pane_id(0, parent_pane).unwrap();
    (app, workspace, pane)
}

#[test]
fn closing_a_local_parent_with_its_group_cannot_close_a_protected_worktree() {
    let (app, parent, parent_pane) = app_with_protected_linked_worktree();
    assert!(
        !app.state.workspace_group_close_indices(0).is_empty(),
        "the fixture links the workspaces"
    );
    let close_group = request(Method::WorkspaceClose(WorkspaceCloseParams {
        workspace_id: parent,
        close_group: true,
    }));
    let close_last_pane = request(Method::PaneClose(crate::api::schema::PaneTarget {
        pane_id: parent_pane,
    }));

    assert!(app.protected_host_denial(&close_group).is_some());
    assert!(app.protected_host_denial(&close_last_pane).is_some());
}

/// Marks the second pane as the assist of the first, as `assist.start` links them.
fn link_assist(app: &mut App, target: &str, helper: &str, protected: bool) {
    let terminal = |app: &App, id: &str| {
        let (ws_idx, pane_id) = app.parse_pane_id(id).unwrap();
        app.state.terminal_id_for_pane(ws_idx, pane_id).unwrap()
    };
    let link = crate::assist::AssistLink {
        helper: terminal(app, helper),
        target: Some(terminal(app, target)),
        product: crate::api::schema::AssistProduct::Claude,
        context_path: "/tmp/herdr-assist-test/context.md".into(),
        protected,
    };
    app.state.assists.insert(link);
}

#[test]
fn the_assist_of_a_protected_pane_is_protected_too() {
    let (mut app, prod, helper) = app_with(Kind::Ssh("noref_prod"), Kind::Local);
    link_assist(&mut app, &prod, &helper, true);

    assert!(app.protected_host_denial(&send_keys(&helper)).is_some());
}

#[test]
fn the_assist_stays_protected_after_the_pane_it_read_closes() {
    let (mut app, prod, helper) = app_with(Kind::Ssh("noref_prod"), Kind::Local);
    link_assist(&mut app, &prod, &helper, true);
    let (ws_idx, pane_id) = app.parse_pane_id(&prod).unwrap();
    let target = app.state.terminal_id_for_pane(ws_idx, pane_id).unwrap();
    app.state.assists.forget_terminal(&target);

    assert!(app.protected_host_denial(&send_keys(&helper)).is_some());
}

#[test]
fn the_assist_of_a_protected_pane_still_reports_its_own_agent() {
    let (mut app, prod, helper) = app_with(Kind::Ssh("noref_prod"), Kind::Local);
    link_assist(&mut app, &prod, &helper, true);
    let report = request(Method::PaneReportAgent(
        serde_json::from_value(serde_json::json!({
            "pane_id": helper,
            "source": "claude-hook",
            "agent": "claude",
            "state": "working",
        }))
        .unwrap(),
    ));

    assert_eq!(app.protected_host_denial(&report), None);
}

#[test]
fn the_assist_of_a_local_pane_stays_reachable() {
    let (mut app, local, helper) = app_with(Kind::Local, Kind::Local);
    link_assist(&mut app, &local, &helper, false);

    assert_eq!(app.protected_host_denial(&send_keys(&helper)), None);
}

#[test]
fn a_plugin_action_without_context_runs_at_the_global_focus() {
    let (app, _, _) = app_with(Kind::Ssh("noref_prod"), Kind::Local);
    let invoke = request(Method::PluginActionInvoke(
        serde_json::from_value(serde_json::json!({ "action_id": "a" })).unwrap(),
    ));

    assert!(app.protected_host_denial(&invoke).is_some());
}

#[test]
fn a_split_ratio_named_by_a_local_pane_cannot_resize_a_protected_neighbour() {
    let (app, _, local) = app_with(Kind::Ssh("noref_prod"), Kind::Local);
    let ratio = request(Method::LayoutSetSplitRatio(
        serde_json::from_value(serde_json::json!({
            "pane_id": local,
            "path": [],
            "ratio": 0.5,
        }))
        .unwrap(),
    ));

    assert!(app.protected_host_denial(&ratio).is_some());
}

#[test]
fn moving_a_local_pane_to_a_new_tab_lands_in_its_own_workspace() {
    let (mut app, _, local) = app_with(Kind::Local, Kind::Ssh("noref_prod"));
    let (_, other_local) = add_local_workspace(&mut app);
    let move_to_new_tab = |pane_id: &str| {
        request(Method::PaneMove(
            serde_json::from_value(serde_json::json!({
                "pane_id": pane_id,
                "destination": { "type": "new_tab" },
            }))
            .unwrap(),
        ))
    };

    assert!(app
        .protected_host_denial(&move_to_new_tab(&local))
        .is_some());
    assert_eq!(
        app.protected_host_denial(&move_to_new_tab(&other_local)),
        None
    );
}

#[test]
fn a_layout_without_a_target_opens_a_tab_in_the_active_workspace() {
    let (mut app, _, _) = app_with(Kind::Local, Kind::Local);
    // The protected pane sits in another tab of the active workspace.
    let tab_idx = app.state.workspaces[0].test_add_tab(Some("prod"));
    app.state.ensure_test_terminals();
    let tab = &app.state.workspaces[0].tabs[tab_idx];
    let terminal_id = tab.terminal_id(tab.root_pane).unwrap().clone();
    app.state.terminals.get_mut(&terminal_id).unwrap().ssh =
        SshLaunch::new("noref_prod".into(), None);
    app.state.workspaces[0].switch_tab(0);
    let apply = request(Method::LayoutApply(
        serde_json::from_value(serde_json::json!({ "root": { "type": "pane" } })).unwrap(),
    ));

    assert!(app.protected_host_denial(&apply).is_some());
}

#[test]
fn an_inherited_tab_without_a_workspace_lands_in_the_source_panes_workspace() {
    let (mut app, _, _) = app_with(Kind::Local, Kind::Ssh("noref_prod"));
    let (_, other_local) = add_local_workspace(&mut app);
    let inherit = request(Method::TabCreateInherit(
        serde_json::from_value(serde_json::json!({ "source_pane_id": other_local })).unwrap(),
    ));

    assert_eq!(app.protected_host_denial(&inherit), None);
}

#[test]
fn a_live_handoff_waits_until_no_protected_assist_is_open() {
    let (mut app, prod, helper) = app_with(Kind::Ssh("noref_prod"), Kind::Local);
    let handoff = request(Method::ServerLiveHandoff(
        serde_json::from_value(serde_json::json!({})).unwrap(),
    ));
    assert_eq!(app.protected_host_denial(&handoff), None);

    link_assist(&mut app, &prod, &helper, true);

    assert!(app.protected_host_denial(&handoff).is_some());
}
