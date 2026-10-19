use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::api::schema::{
    DirectoryIndexParams, DirectoryListParams, ErrorResponse, Method, Request, ResponseResult,
    SuccessResponse,
};
use crate::app::App;
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

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "herdr-directory-app-{name}-{}-{stamp}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(std::fs::canonicalize(path).unwrap())
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// One workspace with two panes whose terminals report `first` and `second`
/// as their working directories; the first pane is focused. Callers run on
/// tokio because test runtimes start their compression task there.
fn app_with_panes(first: &Path, second: &Path) -> (App, String, String) {
    let mut app = test_app();
    let mut workspace = Workspace::test_new("files");
    let first_pane = workspace.tabs[0].root_pane;
    let second_pane = workspace.test_split(ratatui::layout::Direction::Horizontal);
    workspace.tabs[0].layout.focus_pane(first_pane);
    app.state.workspaces = vec![workspace];
    app.state.ensure_test_terminals();
    app.state.active = Some(0);
    app.state.selected = 0;
    for (pane, cwd) in [(first_pane, first), (second_pane, second)] {
        let terminal_id = app.state.workspaces[0].terminal_id(pane).unwrap().clone();
        app.state.terminals.get_mut(&terminal_id).unwrap().cwd = cwd.to_path_buf();
        app.state.insert_test_runtime(
            pane,
            crate::terminal::TerminalRuntime::test_with_scrollback_bytes(20, 5, 1000, b""),
        );
    }
    let first_id = app.public_pane_id(0, first_pane).unwrap();
    let second_id = app.public_pane_id(0, second_pane).unwrap();
    (app, first_id, second_id)
}

fn list(params: DirectoryListParams) -> Request {
    Request {
        id: "list".into(),
        method: Method::DirectoryList(params),
    }
}

fn run(app: &mut App, request: Request) -> String {
    let (respond_to, response_rx) = std::sync::mpsc::channel();
    assert!(app.handle_deferred_directory_api_request(request, respond_to));
    response_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("directory read should answer")
}

fn success(response: &str) -> ResponseResult {
    serde_json::from_str::<SuccessResponse>(response)
        .unwrap_or_else(|err| panic!("expected success ({err}): {response}"))
        .result
}

fn error_code(response: &str) -> String {
    serde_json::from_str::<ErrorResponse>(response)
        .unwrap_or_else(|err| panic!("expected error ({err}): {response}"))
        .error
        .code
}

#[tokio::test]
async fn directory_list_defaults_to_the_focused_pane_working_directory() {
    let first = TempDir::new("focused");
    let second = TempDir::new("other");
    std::fs::write(first.0.join("here.txt"), "").unwrap();
    let (mut app, _, _) = app_with_panes(&first.0, &second.0);

    let ResponseResult::DirectoryList {
        root,
        path,
        canonical,
        entries,
        ..
    } = success(&run(&mut app, list(DirectoryListParams::default())))
    else {
        panic!("expected directory list");
    };

    assert_eq!(root, first.0.display().to_string());
    assert_eq!(path, root);
    assert_eq!(canonical, root);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "here.txt");
}

#[tokio::test]
async fn directory_list_uses_the_named_pane_and_an_explicit_path() {
    let first = TempDir::new("named-first");
    let second = TempDir::new("named-second");
    std::fs::create_dir(second.0.join("sub")).unwrap();
    let (mut app, _, second_id) = app_with_panes(&first.0, &second.0);

    let ResponseResult::DirectoryList { root, path, .. } = success(&run(
        &mut app,
        list(DirectoryListParams {
            pane_id: Some(second_id),
            path: Some(second.0.join("sub").display().to_string()),
            ..Default::default()
        }),
    )) else {
        panic!("expected directory list");
    };

    assert_eq!(root, second.0.display().to_string());
    assert_eq!(path, second.0.join("sub").display().to_string());
}

#[tokio::test]
async fn directory_root_falls_back_to_the_new_workspace_directory_without_a_live_pane() {
    let first = TempDir::new("dead-first");
    let second = TempDir::new("dead-second");
    let (mut app, _, second_id) = app_with_panes(&first.0, &second.0);
    let (_, second_pane) = app.parse_pane_id(&second_id).unwrap();
    let terminal_id = app.state.workspaces[0]
        .terminal_id(second_pane)
        .unwrap()
        .clone();
    app.state
        .terminals
        .get_mut(&terminal_id)
        .unwrap()
        .restore_error = Some("gone".into());

    let root = app.directory_root(None, Some(&second_id)).unwrap();

    assert_eq!(root.path, app.resolved_new_workspace_cwd_from(0));
    assert_eq!(root.path, first.0);
    assert_eq!(root.pane_id, None);
}

#[test]
fn directory_root_without_workspaces_uses_the_new_terminal_directory() {
    let app = test_app();

    let root = app.directory_root(None, None).unwrap();

    assert_eq!(root.path, app.resolve_new_terminal_cwd(None));
    assert_eq!(root.pane_id, None);
}

#[tokio::test]
async fn directory_list_names_the_live_pane_that_supplied_the_root() {
    let first = TempDir::new("pane-first");
    let second = TempDir::new("pane-second");
    let (mut app, first_id, second_id) = app_with_panes(&first.0, &second.0);

    let ResponseResult::DirectoryList { pane_id, .. } =
        success(&run(&mut app, list(DirectoryListParams::default())))
    else {
        panic!("expected directory list");
    };
    let ResponseResult::DirectoryList { pane_id: named, .. } = success(&run(
        &mut app,
        list(DirectoryListParams {
            pane_id: Some(second_id.clone()),
            path: Some(first.0.display().to_string()),
            ..Default::default()
        }),
    )) else {
        panic!("expected directory list");
    };

    assert_eq!(pane_id, Some(first_id));
    assert_eq!(
        named,
        Some(second_id),
        "an explicit path still names the pane the root came from"
    );
}

#[tokio::test]
async fn directory_list_names_no_pane_when_the_root_fell_back() {
    let first = TempDir::new("fallback-first");
    let second = TempDir::new("fallback-second");
    let (mut app, first_id, _) = app_with_panes(&first.0, &second.0);
    let (_, first_pane) = app.parse_pane_id(&first_id).unwrap();
    let terminal_id = app.state.workspaces[0]
        .terminal_id(first_pane)
        .unwrap()
        .clone();
    app.state
        .terminals
        .get_mut(&terminal_id)
        .unwrap()
        .restore_error = Some("gone".into());

    let ResponseResult::DirectoryList { root, pane_id, .. } =
        success(&run(&mut app, list(DirectoryListParams::default())))
    else {
        panic!("expected directory list");
    };

    assert_eq!(pane_id, None);
    assert_eq!(
        root,
        app.resolved_new_workspace_cwd_from(0).display().to_string()
    );
}

#[tokio::test]
async fn directory_requests_reject_bad_targets_and_relative_paths_without_a_worker() {
    let first = TempDir::new("reject-first");
    let second = TempDir::new("reject-second");
    let (mut app, first_id, _) = app_with_panes(&first.0, &second.0);
    let available = app.directory_read_slots.available_permits();

    for (params, code) in [
        (
            DirectoryListParams {
                pane_id: Some("w404:p1".into()),
                ..Default::default()
            },
            "pane_not_found",
        ),
        (
            DirectoryListParams {
                workspace_id: Some("w404".into()),
                ..Default::default()
            },
            "workspace_not_found",
        ),
        (
            DirectoryListParams {
                path: Some("../etc".into()),
                pane_id: Some(first_id.clone()),
                ..Default::default()
            },
            "invalid_params",
        ),
        (
            DirectoryListParams {
                path: Some("src".into()),
                ..Default::default()
            },
            "invalid_params",
        ),
    ] {
        let (respond_to, response_rx) = std::sync::mpsc::channel();
        assert!(app.handle_deferred_directory_api_request(list(params), respond_to));
        let response = response_rx
            .try_recv()
            .expect("rejected before a worker starts");
        assert_eq!(error_code(&response), code);
    }
    assert_eq!(app.directory_read_slots.available_permits(), available);
}

#[tokio::test]
async fn directory_reads_answer_busy_when_every_slot_is_taken_and_release_slots() {
    let first = TempDir::new("busy");
    let (mut app, _, _) = app_with_panes(&first.0, &first.0);
    app.directory_read_slots = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
    let held = app
        .directory_read_slots
        .clone()
        .try_acquire_owned()
        .unwrap();

    let busy = run(&mut app, list(DirectoryListParams::default()));
    assert_eq!(error_code(&busy), "directory_busy");

    drop(held);
    let listed = run(&mut app, list(DirectoryListParams::default()));
    assert!(matches!(
        success(&listed),
        ResponseResult::DirectoryList { .. }
    ));
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while app.directory_read_slots.available_permits() != 1 {
        assert!(
            std::time::Instant::now() < deadline,
            "slot was not released"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[tokio::test]
async fn directory_index_reports_a_non_git_root_as_empty() {
    let first = TempDir::new("index-plain");
    std::fs::write(first.0.join("notes.md"), "").unwrap();
    let (mut app, _, _) = app_with_panes(&first.0, &first.0);

    let response = run(
        &mut app,
        Request {
            id: "index".into(),
            method: Method::DirectoryIndex(DirectoryIndexParams::default()),
        },
    );

    assert_eq!(
        success(&response),
        ResponseResult::DirectoryIndex {
            repo_root: None,
            entries: Vec::new(),
            truncated: false,
            git_error: None,
        }
    );
}

#[test]
fn other_methods_are_left_to_the_caller() {
    let mut app = test_app();
    let (respond_to, response_rx) = std::sync::mpsc::channel();

    assert!(!app.handle_deferred_directory_api_request(
        Request {
            id: "ping".into(),
            method: Method::Ping(Default::default()),
        },
        respond_to,
    ));
    assert!(response_rx.try_recv().is_err());
}

#[test]
fn synchronous_dispatch_refuses_directory_reads() {
    let mut app = test_app();

    let response = app.handle_api_request(list(DirectoryListParams::default()));

    assert_eq!(error_code(&response), "invalid_request");
}

#[test]
fn directory_limits_match_the_file_viewer_contract() {
    assert_eq!(super::listing::MAX_LIST_ENTRIES, 2_000);
    assert_eq!(super::listing::MAX_INDEX_ENTRIES, 50_000);
    assert_eq!(super::GIT_DEADLINE, Duration::from_secs(5));
}
