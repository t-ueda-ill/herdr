use crossterm::event::KeyModifiers;

use super::*;
use crate::api::schema::{AssistInfo, AssistPlacement, AssistProduct, AssistProductInfo};
use crate::client::shell::tests::{snapshot, surface};
use crate::raw_input::RawInputEvent;

const ASSIST_METHODS: [&str; 4] = [
    "assist.products",
    "assist.list",
    "assist.start",
    "assist.refresh",
];

fn shell_with(methods: &[&str], configure: impl FnOnce(&mut Config)) -> ClientShellState {
    let mut config = Config::default();
    configure(&mut config);
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.set_endpoint_methods(Some(
        methods.iter().map(|name| (*name).to_owned()).collect(),
    ));
    state
}

fn shell() -> ClientShellState {
    shell_with(&ASSIST_METHODS, |_| {})
}

fn requests(outcome: &ClientShellInput) -> Vec<(String, Method)> {
    outcome
        .actions
        .iter()
        .filter_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => {
                Some((request.id.clone(), request.method.clone()))
            }
            _ => None,
        })
        .collect()
}

fn product(product: AssistProduct, path: &str) -> AssistProductInfo {
    AssistProductInfo {
        product,
        path: path.into(),
    }
}

fn all_products() -> Vec<AssistProductInfo> {
    vec![
        product(AssistProduct::Claude, "/bin/claude"),
        product(AssistProduct::Codex, "/bin/codex"),
        product(AssistProduct::Copilot, "/bin/copilot"),
    ]
}

fn assist(pane_id: &str, target: &str) -> AssistInfo {
    AssistInfo {
        pane_id: pane_id.into(),
        target_pane_id: Some(target.into()),
        product: AssistProduct::Claude,
        context_path: "/tmp/herdr-assist-x/context.md".into(),
    }
}

fn pane_menu_labels(state: &mut ClientShellState, pane_id: &str) -> Vec<&'static str> {
    state.open_pane_context_menu(pane_id.into(), 5, 3);
    let labels = match state.overlay.as_ref() {
        Some(ClientShellOverlay::ContextMenu(menu)) => {
            menu.items().iter().map(|item| item.label).collect()
        }
        _ => panic!("pane context menu"),
    };
    state.overlay = None;
    labels
}

fn choose_in_pane_menu(state: &mut ClientShellState, label: &str) -> ClientShellInput {
    state.open_pane_context_menu("pane_1".into(), 5, 3);
    let index = match state.overlay.as_ref() {
        Some(ClientShellOverlay::ContextMenu(menu)) => menu
            .items()
            .iter()
            .position(|item| item.label == label)
            .unwrap_or_else(|| panic!("no {label} item")),
        _ => panic!("pane context menu"),
    };
    let mut outcome = ClientShellInput::default();
    state.activate_context_menu_item(index, &mut outcome);
    outcome
}

/// Chooses Ask AI… on `pane_1` and answers the products question.
fn ask_ai(state: &mut ClientShellState, products: Vec<AssistProductInfo>) -> Vec<(String, Method)> {
    let outcome = choose_in_pane_menu(state, ASK_AI_LABEL);
    let sent = requests(&outcome);
    assert!(
        matches!(&sent[..], [(_, Method::AssistProducts(_))]),
        "{sent:?}"
    );
    let (_, actions) = state.handle_endpoint_result(
        "boot-1",
        &sent[0].0,
        Ok(ResponseResult::AssistProducts { products }),
    );
    actions
        .into_iter()
        .filter_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => Some((request.id, request.method)),
            _ => None,
        })
        .collect()
}

fn key(state: &mut ClientShellState, code: KeyCode) -> ClientShellInput {
    state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        code,
        KeyModifiers::NONE,
    ))])
}

fn picker(state: &ClientShellState) -> &ClientAssistOverlay {
    match state.overlay.as_ref() {
        Some(ClientShellOverlay::Assist(picker)) => picker,
        other => panic!("not the assist picker: {other:?}"),
    }
}

#[test]
fn the_pane_menu_offers_ask_ai_after_the_splits_when_the_server_can_open_assists() {
    let mut state = shell();

    let labels = pane_menu_labels(&mut state, "pane_1");

    let split_down = labels
        .iter()
        .position(|label| *label == "Split down")
        .unwrap();
    assert_eq!(labels[split_down + 1], ASK_AI_LABEL);
    assert!(!labels.contains(&SEND_LATEST_LABEL));
}

#[test]
fn the_pane_menu_has_no_ask_ai_when_the_server_lacks_assists() {
    let mut state = shell_with(&["pane.split"], |_| {});

    assert!(!pane_menu_labels(&mut state, "pane_1").contains(&ASK_AI_LABEL));
}

#[test]
fn one_product_opens_without_a_picker() {
    let mut state = shell();

    let sent = ask_ai(
        &mut state,
        vec![product(AssistProduct::Codex, "/bin/codex")],
    );

    assert!(state.overlay.is_none());
    match &sent[..] {
        [(_, Method::AssistStart(params))] => {
            assert_eq!(params.pane_id, "pane_1");
            assert_eq!(params.product, Some(AssistProduct::Codex));
            assert_eq!(params.placement, AssistPlacement::Auto);
        }
        other => panic!("unexpected requests: {other:?}"),
    }
}

#[test]
fn several_products_open_the_picker_on_the_first_one() {
    let mut state = shell();

    let sent = ask_ai(&mut state, all_products());

    assert!(sent.is_empty(), "{sent:?}");
    let picker = picker(&state);
    assert_eq!(picker.selected, 0);
    assert_eq!(picker.placement, AssistPlacement::Auto);
}

#[test]
fn the_picker_starts_on_the_configured_product() {
    let mut state = shell_with(&ASSIST_METHODS, |config| {
        config.ui.assist_product = Some(AssistProduct::Copilot);
    });

    ask_ai(&mut state, all_products());

    assert_eq!(picker(&state).selected, 2);
}

#[test]
fn tab_cycles_the_placement_and_enter_opens_the_selected_product_there() {
    let mut state = shell();
    ask_ai(&mut state, all_products());

    key(&mut state, KeyCode::Down);
    key(&mut state, KeyCode::Tab);
    assert_eq!(picker(&state).placement, AssistPlacement::Right);
    key(&mut state, KeyCode::Tab);
    assert_eq!(picker(&state).placement, AssistPlacement::Below);
    let outcome = key(&mut state, KeyCode::Enter);

    match &requests(&outcome)[..] {
        [(_, Method::AssistStart(params))] => {
            assert_eq!(params.product, Some(AssistProduct::Codex));
            assert_eq!(params.placement, AssistPlacement::Below);
            assert!(params.focus);
        }
        other => panic!("unexpected requests: {other:?}"),
    }
}

#[test]
fn tab_after_below_goes_back_to_auto() {
    let mut state = shell();
    ask_ai(&mut state, all_products());

    for _ in 0..3 {
        key(&mut state, KeyCode::Tab);
    }

    assert_eq!(picker(&state).placement, AssistPlacement::Auto);
}

#[test]
fn an_opened_assist_closes_the_picker_and_offers_send_latest_on_both_panes() {
    let mut state = shell();
    let mut with_helper = snapshot();
    let mut helper = with_helper.panes[0].clone();
    helper.pane_id = "pane_9".into();
    with_helper.panes.push(helper);
    state.set_snapshot(Box::new(with_helper));
    ask_ai(&mut state, all_products());
    let outcome = key(&mut state, KeyCode::Enter);
    let sent = requests(&outcome);

    state.handle_endpoint_result(
        "boot-1",
        &sent[0].0,
        Ok(ResponseResult::AssistStarted {
            assist: assist("pane_9", "pane_1"),
            created: true,
        }),
    );

    assert!(state.overlay.is_none());
    assert!(pane_menu_labels(&mut state, "pane_1").contains(&SEND_LATEST_LABEL));
    let helper_labels = pane_menu_labels(&mut state, "pane_9");
    assert!(helper_labels.contains(&SEND_LATEST_LABEL));
    assert!(!helper_labels.contains(&ASK_AI_LABEL));
}

#[test]
fn a_rejected_start_keeps_the_picker_with_the_reason() {
    let mut state = shell();
    ask_ai(&mut state, all_products());
    let outcome = key(&mut state, KeyCode::Enter);
    let sent = requests(&outcome);

    state.handle_endpoint_result(
        "boot-1",
        &sent[0].0,
        Err(ClientShellEndpointError {
            code: Some("assist_start_failed".into()),
            message: "no such file".into(),
        }),
    );

    assert_eq!(picker(&state).error.as_deref(), Some("no such file"));
    assert!(!picker(&state).starting);
}

#[test]
fn send_latest_asks_the_server_to_refresh_the_assist() {
    let mut state = shell();
    state.assist_links = vec![assist("pane_9", "pane_1")];

    let outcome = choose_in_pane_menu(&mut state, SEND_LATEST_LABEL);

    match &requests(&outcome)[..] {
        [(_, Method::AssistRefresh(params))] => assert_eq!(params.pane_id, "pane_1"),
        other => panic!("unexpected requests: {other:?}"),
    }
}

#[test]
fn the_assist_list_answer_replaces_what_the_client_knows() {
    let mut state = shell();
    state.assist_links = vec![assist("pane_8", "pane_7")];
    let mut outcome = ClientShellInput::default();
    state.refresh_assist_links(&mut outcome);
    let sent = requests(&outcome);

    state.handle_endpoint_result(
        "boot-1",
        &sent[0].0,
        Ok(ResponseResult::AssistList {
            assists: vec![assist("pane_9", "pane_1")],
        }),
    );

    assert_eq!(state.assist_links, vec![assist("pane_9", "pane_1")]);
}

#[test]
fn no_product_on_the_server_says_what_to_install() {
    let mut state = shell();

    let sent = ask_ai(&mut state, Vec::new());

    assert!(sent.is_empty());
    assert!(state.overlay.is_none());
    let (title, body) = state.endpoint_notice_for_test().expect("notice");
    assert_eq!(title, "No AI CLI found");
    assert!(body.contains("claude, codex or copilot"), "{body}");
}

#[test]
fn escape_closes_the_picker() {
    let mut state = shell();
    ask_ai(&mut state, all_products());

    key(&mut state, KeyCode::Esc);

    assert!(state.overlay.is_none());
}

#[test]
fn the_ask_ai_key_asks_for_the_products_of_the_focused_pane() {
    let config: Config = toml::from_str("[keys]\nask_ai = \"f9\"\n").expect("ask_ai parses");
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.set_endpoint_methods(Some(
        ASSIST_METHODS
            .iter()
            .map(|name| (*name).to_owned())
            .collect(),
    ));

    let outcome = key(&mut state, KeyCode::F(9));
    let sent = requests(&outcome);
    let (_, actions) = state.handle_endpoint_result(
        "boot-1",
        &sent[0].0,
        Ok(ResponseResult::AssistProducts {
            products: vec![product(AssistProduct::Codex, "/bin/codex")],
        }),
    );

    let started: Vec<_> = actions
        .into_iter()
        .filter_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => Some(request.method),
            _ => None,
        })
        .collect();
    match &started[..] {
        [Method::AssistStart(params)] => assert_eq!(params.pane_id, "pane_1"),
        other => panic!("unexpected requests: {other:?}"),
    }
}

#[test]
fn the_picker_shows_every_product_without_scrolling() {
    let mut state = shell();
    ask_ai(&mut state, all_products());

    let frame = state.compose(106, 30).expect("frame");
    let text: String = frame
        .cells
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect();

    assert_eq!(state.hits.worktree_rows.len(), 3);
    for path in ["/bin/claude", "/bin/codex", "/bin/copilot"] {
        assert!(text.contains(path), "{path} is not shown");
    }
}

#[test]
fn opening_an_assist_brings_the_terminal_side_forward() {
    let mut state = shell();
    ask_ai(&mut state, all_products());
    state.workbench.files_focused = true;

    key(&mut state, KeyCode::Enter);

    assert!(!state.workbench.files_focused);
}

#[test]
fn a_later_assist_list_keeps_the_highlighted_item_by_its_action() {
    let mut state = shell();
    state.assist_links = vec![assist("pane_9", "pane_1")];
    state.open_pane_context_menu("pane_1".into(), 5, 3);
    let mut outcome = ClientShellInput::default();
    state.refresh_assist_links(&mut outcome);
    let sent = requests(&outcome);
    let highlight = |state: &ClientShellState| match state.overlay.as_ref() {
        Some(ClientShellOverlay::ContextMenu(menu)) => menu.items()[menu.highlighted].action,
        _ => panic!("pane context menu"),
    };
    if let Some(ClientShellOverlay::ContextMenu(menu)) = state.overlay.as_mut() {
        menu.highlighted = menu
            .items()
            .iter()
            .position(|item| item.action == ClientContextMenuAction::ToggleRightClickPassthrough)
            .unwrap();
    }
    state.hits.context_menu_rows = vec![(Rect::new(5, 3, 20, 1), 0)];

    state.handle_endpoint_result(
        "boot-1",
        &sent[0].0,
        Ok(ResponseResult::AssistList {
            assists: Vec::new(),
        }),
    );

    assert_eq!(
        highlight(&state),
        ClientContextMenuAction::ToggleRightClickPassthrough
    );
    assert!(state.hits.context_menu_rows.is_empty());
}

#[test]
fn switching_servers_forgets_the_assists_of_the_old_one() {
    let mut state = shell();
    state.assist_links = vec![assist("pane_9", "pane_1")];

    state.reset_endpoint_projection();

    assert!(state.assist_links.is_empty());
}

#[test]
fn a_late_products_answer_does_not_replace_a_dialog_opened_meanwhile() {
    let mut state = shell();
    let outcome = choose_in_pane_menu(&mut state, ASK_AI_LABEL);
    let sent = requests(&outcome);
    choose_in_pane_menu(&mut state, "Rename pane");
    assert!(matches!(state.overlay, Some(ClientShellOverlay::Rename(_))));

    state.handle_endpoint_result(
        "boot-1",
        &sent[0].0,
        Ok(ResponseResult::AssistProducts {
            products: all_products(),
        }),
    );

    assert!(matches!(state.overlay, Some(ClientShellOverlay::Rename(_))));
}

#[test]
fn asking_again_while_an_ask_is_pending_sends_nothing_more() {
    let mut state = shell();
    choose_in_pane_menu(&mut state, ASK_AI_LABEL);

    let again = choose_in_pane_menu(&mut state, ASK_AI_LABEL);

    assert!(requests(&again).is_empty());
}

#[test]
fn asking_again_works_once_the_assist_has_opened() {
    let mut state = shell();
    let sent = ask_ai(
        &mut state,
        vec![product(AssistProduct::Codex, "/bin/codex")],
    );
    state.handle_endpoint_result(
        "boot-1",
        &sent[0].0,
        Ok(ResponseResult::AssistStarted {
            assist: assist("pane_9", "pane_1"),
            created: true,
        }),
    );

    let again = choose_in_pane_menu(&mut state, ASK_AI_LABEL);

    assert!(matches!(
        &requests(&again)[..],
        [(_, Method::AssistProducts(_))]
    ));
}

#[test]
fn a_server_that_lists_no_methods_gets_no_assist_items_or_queries() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.set_endpoint_methods(None);

    let labels = pane_menu_labels(&mut state, "pane_1");
    let mut outcome = ClientShellInput::default();
    state.refresh_assist_links(&mut outcome);

    assert!(!labels.contains(&ASK_AI_LABEL));
    assert!(requests(&outcome).is_empty());
}
