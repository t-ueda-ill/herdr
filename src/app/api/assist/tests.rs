use std::path::PathBuf;

use crate::api::schema::{
    AssistPlacement, AssistProduct, AssistRefreshParams, AssistStartParams, ErrorResponse, Method,
    Request, ResponseResult, SuccessResponse,
};
// Why unix only: the tests that open an assist run a `/bin/sh` script as the CLI.
#[cfg(unix)]
use crate::api::schema::{
    AssistInfo, PaneLayoutParams, PaneTarget, SplitDirection, TaskSubmitParams,
};
use crate::app::App;
use crate::config::Config;
use crate::workspace::Workspace;

/// Two finished commands and a prompt with `git rebase main` typed at it.
const MARKED_SHELL: &[u8] = b"\x1b]133;A\x07$ \x1b]133;B\x07make\r\n\x1b]133;C\x07error: x\r\n\
\x1b]133;A\x07$ \x1b]133;B\x07ls\r\n\x1b]133;C\x07src\r\n\
\x1b]133;A\x07$ \x1b]133;B\x07git rebase main";

struct Fixture {
    app: App,
    target: String,
    bin: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for link in self.app.state.assists.iter() {
            super::remove_context(&link.context_path);
        }
        let _ = std::fs::remove_dir_all(&self.bin);
    }
}

/// An app with one pane of `cols` columns, and a `PATH` that holds a fake
/// `codex` which only waits.
fn fixture(cols: u16, bytes: &[u8]) -> Fixture {
    let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app = App::new(
        &Config::default(),
        crate::app::AppPolicy::TEST,
        None,
        api_rx,
        crate::api::EventHub::default(),
    );
    app.state.workspaces = vec![Workspace::test_new("assist")];
    app.state.ensure_test_terminals();
    let pane_id = app.state.workspaces[0].tabs[0].root_pane;
    let target = app.public_pane_id(0, pane_id).unwrap();
    app.state.insert_test_runtime(
        pane_id,
        crate::terminal::TerminalRuntime::test_with_scrollback_bytes(cols, 10, 10_000, bytes),
    );
    let bin = std::env::temp_dir().join(format!(
        "herdr-assist-test-{}",
        crate::tasks::unguessable_name()
    ));
    std::fs::create_dir_all(&bin).unwrap();
    let codex = bin.join(if cfg!(windows) { "codex.exe" } else { "codex" });
    std::fs::write(&codex, "#!/bin/sh\nsleep 30\n").unwrap();
    crate::platform::set_permission_bits(&codex, 0o755).unwrap();
    app.test_assist_path = Some(bin.clone().into_os_string());
    Fixture { app, target, bin }
}

fn call(app: &mut App, method: Method) -> String {
    app.handle_api_request(Request {
        id: "req".into(),
        method,
    })
}

fn success(response: &str) -> ResponseResult {
    serde_json::from_str::<SuccessResponse>(response)
        .unwrap_or_else(|_| panic!("not a success: {response}"))
        .result
}

fn error_code(response: &str) -> String {
    serde_json::from_str::<ErrorResponse>(response)
        .unwrap_or_else(|_| panic!("not an error: {response}"))
        .error
        .code
}

#[cfg(unix)]
fn start(fixture: &mut Fixture, placement: AssistPlacement) -> String {
    let target = fixture.target.clone();
    call(
        &mut fixture.app,
        Method::AssistStart(AssistStartParams {
            pane_id: target,
            product: None,
            placement,
            focus: true,
        }),
    )
}

#[cfg(unix)]
fn started(response: &str) -> (AssistInfo, bool) {
    match success(response) {
        ResponseResult::AssistStarted { assist, created } => (assist, created),
        other => panic!("unexpected result: {other:?}"),
    }
}

fn pane_count(fixture: &Fixture) -> usize {
    fixture.app.state.workspaces[0].tabs[0]
        .layout
        .pane_ids()
        .len()
}

#[cfg(unix)]
/// The direction and ratio of the one split in the read pane's tab.
fn split_of(fixture: &mut Fixture) -> (SplitDirection, f32) {
    let target = fixture.target.clone();
    match success(&call(
        &mut fixture.app,
        Method::PaneLayout(PaneLayoutParams {
            pane_id: Some(target),
        }),
    )) {
        ResponseResult::PaneLayout { layout } => {
            assert_eq!(layout.splits.len(), 1);
            (layout.splits[0].direction.clone(), layout.splits[0].ratio)
        }
        other => panic!("unexpected result: {other:?}"),
    }
}

#[tokio::test]
async fn products_lists_what_the_path_holds() {
    let mut fixture = fixture(80, MARKED_SHELL);

    let response = call(&mut fixture.app, Method::AssistProducts(Default::default()));

    match success(&response) {
        ResponseResult::AssistProducts { products } => {
            assert_eq!(products.len(), 1);
            assert_eq!(products[0].product, AssistProduct::Codex);
        }
        other => panic!("unexpected result: {other:?}"),
    }
}

#[cfg(unix)]
#[tokio::test]
async fn start_opens_the_assist_beside_the_pane_and_links_them() {
    let mut fixture = fixture(80, MARKED_SHELL);

    let (assist, created) = started(&start(&mut fixture, AssistPlacement::Auto));

    assert!(created);
    assert_eq!(
        assist.target_pane_id.as_deref(),
        Some(fixture.target.as_str())
    );
    assert_eq!(assist.product, AssistProduct::Codex);
    assert_ne!(assist.pane_id, fixture.target);
    assert_eq!(pane_count(&fixture), 2);
}

#[cfg(unix)]
#[tokio::test]
async fn a_narrow_pane_gets_the_assist_in_its_lower_half() {
    let mut fixture = fixture(80, MARKED_SHELL);

    started(&start(&mut fixture, AssistPlacement::Auto));

    assert_eq!(split_of(&mut fixture), (SplitDirection::Down, 0.5));
}

#[cfg(unix)]
#[tokio::test]
async fn a_wide_pane_gets_the_assist_on_its_right() {
    let mut fixture = fixture(160, MARKED_SHELL);

    started(&start(&mut fixture, AssistPlacement::Auto));

    assert_eq!(split_of(&mut fixture), (SplitDirection::Right, 0.6));
}

#[cfg(unix)]
#[tokio::test]
async fn the_context_file_holds_the_commands_and_only_its_owner_reads_it() {
    use std::os::unix::fs::PermissionsExt;
    let mut fixture = fixture(80, MARKED_SHELL);

    let (assist, _) = started(&start(&mut fixture, AssistPlacement::Auto));

    let text = std::fs::read_to_string(&assist.context_path).unwrap();
    assert!(text.contains("```command\n$ make\n```"), "{text}");
    assert!(text.contains("```output\nerror: x\n```"), "{text}");
    assert!(text.contains("```prompt\n$ git rebase main\n```"), "{text}");
    let mode =
        |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    let path = PathBuf::from(&assist.context_path);
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(path.parent().unwrap()), 0o700);
}

#[cfg(unix)]
#[tokio::test]
async fn a_shell_without_marks_gives_the_bottom_of_its_screen() {
    let mut fixture = fixture(80, b"$ make\r\nerror: x\r\n$ ");

    let (assist, _) = started(&start(&mut fixture, AssistPlacement::Auto));

    let text = std::fs::read_to_string(&assist.context_path).unwrap();
    assert!(text.contains("## Screen (bottom)"), "{text}");
    assert!(text.contains("error: x"), "{text}");
}

#[cfg(unix)]
#[tokio::test]
async fn a_second_start_for_the_same_pane_returns_the_open_assist() {
    let mut fixture = fixture(80, MARKED_SHELL);
    let (first, _) = started(&start(&mut fixture, AssistPlacement::Auto));

    let (second, created) = started(&start(&mut fixture, AssistPlacement::Auto));

    assert!(!created);
    assert_eq!(second.pane_id, first.pane_id);
    assert_eq!(pane_count(&fixture), 2);
}

#[cfg(unix)]
#[tokio::test]
async fn an_assist_cannot_get_an_assist_of_its_own() {
    let mut fixture = fixture(80, MARKED_SHELL);
    let (assist, _) = started(&start(&mut fixture, AssistPlacement::Auto));

    let response = call(
        &mut fixture.app,
        Method::AssistStart(AssistStartParams {
            pane_id: assist.pane_id,
            product: None,
            placement: AssistPlacement::Auto,
            focus: true,
        }),
    );

    assert_eq!(error_code(&response), "pane_is_assist");
}

#[tokio::test]
async fn a_product_that_is_not_on_the_path_is_refused_with_what_was_found() {
    let mut fixture = fixture(80, MARKED_SHELL);
    let target = fixture.target.clone();

    let response = call(
        &mut fixture.app,
        Method::AssistStart(AssistStartParams {
            pane_id: target,
            product: Some(AssistProduct::Claude),
            placement: AssistPlacement::Auto,
            focus: true,
        }),
    );

    assert_eq!(error_code(&response), "assist_product_not_found");
    assert!(response.contains("found: codex"), "{response}");
    assert_eq!(pane_count(&fixture), 1);
}

#[cfg(unix)]
#[tokio::test]
async fn closing_the_assist_removes_its_context_file() {
    let mut fixture = fixture(80, MARKED_SHELL);
    let (assist, _) = started(&start(&mut fixture, AssistPlacement::Auto));

    success(&call(
        &mut fixture.app,
        Method::PaneClose(PaneTarget {
            pane_id: assist.pane_id,
        }),
    ));

    let path = PathBuf::from(&assist.context_path);
    assert!(!path.exists());
    assert!(!path.parent().unwrap().exists());
    assert_eq!(fixture.app.state.assists.iter().count(), 0);
}

#[cfg(unix)]
#[tokio::test]
async fn closing_the_read_pane_keeps_the_assist_without_a_target() {
    let mut fixture = fixture(80, MARKED_SHELL);
    let (assist, _) = started(&start(&mut fixture, AssistPlacement::Auto));
    let target = fixture.target.clone();

    success(&call(
        &mut fixture.app,
        Method::PaneClose(PaneTarget { pane_id: target }),
    ));

    match success(&call(
        &mut fixture.app,
        Method::AssistList(Default::default()),
    )) {
        ResponseResult::AssistList { assists } => {
            assert_eq!(assists.len(), 1);
            assert_eq!(assists[0].target_pane_id, None);
        }
        other => panic!("unexpected result: {other:?}"),
    }
    let refresh = call(
        &mut fixture.app,
        Method::AssistRefresh(AssistRefreshParams {
            pane_id: assist.pane_id,
        }),
    );
    assert_eq!(error_code(&refresh), "assist_target_closed");
}

#[cfg(unix)]
#[tokio::test]
async fn refresh_rewrites_the_context_file_before_it_tells_the_assist() {
    let mut fixture = fixture(80, MARKED_SHELL);
    let (assist, _) = started(&start(&mut fixture, AssistPlacement::Auto));
    std::fs::write(&assist.context_path, "old").unwrap();
    let target = fixture.target.clone();

    // Why an error is fine here: the fake product is not a detected agent,
    // so the line cannot be sent; the file must be new anyway.
    let response = call(
        &mut fixture.app,
        Method::AssistRefresh(AssistRefreshParams { pane_id: target }),
    );

    assert!(error_code(&response).starts_with("agent_"), "{response}");
    let text = std::fs::read_to_string(&assist.context_path).unwrap();
    assert!(text.contains("$ git rebase main"), "{text}");
}

#[tokio::test]
async fn refresh_of_a_pane_without_an_assist_is_refused() {
    let mut fixture = fixture(80, MARKED_SHELL);
    let target = fixture.target.clone();

    let response = call(
        &mut fixture.app,
        Method::AssistRefresh(AssistRefreshParams { pane_id: target }),
    );

    assert_eq!(error_code(&response), "assist_not_found");
}

#[cfg(unix)]
#[tokio::test]
async fn a_task_for_an_assist_is_refused() {
    let mut fixture = fixture(80, MARKED_SHELL);
    let (assist, _) = started(&start(&mut fixture, AssistPlacement::Auto));

    let response = call(
        &mut fixture.app,
        Method::TaskSubmit(TaskSubmitParams {
            target: assist.pane_id,
            text: "review".into(),
            key: None,
            result_file: false,
            requested_by: None,
        }),
    );

    assert_eq!(error_code(&response), "assist_not_task_target");
}

#[cfg(unix)]
#[tokio::test]
async fn stopping_the_server_removes_the_context_folders_of_open_assists() {
    let mut fixture = fixture(80, MARKED_SHELL);
    let (assist, _) = started(&start(&mut fixture, AssistPlacement::Auto));

    fixture.app.remove_assist_contexts();

    assert!(!PathBuf::from(&assist.context_path)
        .parent()
        .unwrap()
        .exists());
}

#[cfg(unix)]
#[tokio::test]
async fn the_cli_argv_names_the_pane_by_id_and_not_by_its_label() {
    let mut fixture = fixture(80, MARKED_SHELL);
    let target_pane = fixture.app.state.workspaces[0].tabs[0].root_pane;
    let target = fixture
        .app
        .state
        .terminal_id_for_pane(0, target_pane)
        .unwrap();
    fixture
        .app
        .state
        .terminals
        .get_mut(&target)
        .unwrap()
        .set_manual_label("deploy --token s3cret".into());

    started(&start(&mut fixture, AssistPlacement::Auto));

    let helper = fixture
        .app
        .state
        .assists
        .iter()
        .next()
        .unwrap()
        .helper
        .clone();
    let argv = fixture.app.state.terminals[&helper]
        .launch_argv
        .clone()
        .expect("argv");
    let joined = argv.join(" ");
    assert!(joined.contains(&fixture.target), "{joined}");
    assert!(!joined.contains("s3cret"), "{joined}");
}

#[cfg(unix)]
#[tokio::test]
async fn a_session_save_keeps_neither_the_assist_pane_nor_its_screen_history() {
    let mut fixture = fixture(80, MARKED_SHELL);
    started(&start(&mut fixture, AssistPlacement::Auto));
    let helper = fixture
        .app
        .state
        .assists
        .iter()
        .next()
        .unwrap()
        .helper
        .clone();
    let (_, helper_pane) = fixture.app.state.pane_of_terminal(&helper).unwrap();
    fixture.app.persist_pane_history = true;

    let crate::app::session::SessionSaveJob::Save { snapshot, history } =
        fixture.app.capture_session_save_job()
    else {
        panic!("expected a save");
    };

    let tab = &snapshot.workspaces[0].tabs[0];
    assert!(!tab.panes.contains_key(&helper_pane.raw()));
    let history = history.expect("history");
    assert!(history.workspaces[0].tabs[0]
        .panes
        .keys()
        .all(|pane| *pane != helper_pane.raw()));
}

#[cfg(unix)]
#[tokio::test]
async fn an_assist_whose_cli_exits_closes_instead_of_getting_a_new_shell() {
    let mut fixture = fixture(80, MARKED_SHELL);
    started(&start(&mut fixture, AssistPlacement::Auto));
    let helper = fixture
        .app
        .state
        .assists
        .iter()
        .next()
        .unwrap()
        .helper
        .clone();
    let (_, helper_pane) = fixture.app.state.pane_of_terminal(&helper).unwrap();
    // Why set by hand: Windows sets it when a PowerShell agent exits, which
    // this test cannot start.
    fixture
        .app
        .state
        .terminals
        .get_mut(&helper)
        .unwrap()
        .respawn_shell_on_exit = true;

    assert!(matches!(
        fixture.app.runtime_exit_action(helper_pane),
        crate::app::api::RuntimeExitAction::ClosePane
    ));
}
