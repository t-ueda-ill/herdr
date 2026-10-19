use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::api::schema::{ErrorResponse, PaneOpenEditorParams, ResponseResult, SuccessResponse};
use crate::app::App;
use crate::events::AppEvent;
use crate::workspace::Workspace;

static EDITED: Mutex<Option<PathBuf>> = Mutex::new(None);

fn recording_editor(path: &Path) -> std::io::Result<Vec<String>> {
    *EDITED.lock().unwrap() = Some(path.to_path_buf());
    Ok(vec!["/bin/sh".into(), "-c".into(), "sleep 5".into()])
}

fn unsupported_editor(_path: &Path) -> std::io::Result<Vec<String>> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "no editor here",
    ))
}

/// Two panes in one tab; the first is focused and the tab is not zoomed.
fn app_with_two_panes() -> (App, crate::layout::PaneId, String, String) {
    let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app = App::new(
        &crate::config::Config::default(),
        crate::app::AppPolicy::TEST,
        None,
        api_rx,
        crate::api::EventHub::default(),
    );
    let mut workspace = Workspace::test_new("editor");
    let focused = workspace.tabs[0].root_pane;
    let other = workspace.test_split(ratatui::layout::Direction::Horizontal);
    workspace.tabs[0].layout.focus_pane(focused);
    app.state.workspaces = vec![workspace];
    app.state.ensure_test_terminals();
    app.state.active = Some(0);
    app.state.selected = 0;
    app.state.mode = crate::app::Mode::Terminal;
    for pane in [focused, other] {
        app.state.insert_test_runtime(
            pane,
            crate::terminal::TerminalRuntime::test_with_scrollback_bytes(20, 5, 1000, b""),
        );
    }
    let focused_id = app.public_pane_id(0, focused).unwrap();
    let other_id = app.public_pane_id(0, other).unwrap();
    (app, focused, focused_id, other_id)
}

fn params(pane_id: Option<String>, path: &str) -> PaneOpenEditorParams {
    PaneOpenEditorParams {
        workspace_id: None,
        pane_id,
        path: path.into(),
    }
}

fn error_code(response: &str) -> String {
    serde_json::from_str::<ErrorResponse>(response)
        .unwrap_or_else(|err| panic!("expected error ({err}): {response}"))
        .error
        .code
}

fn file_in_temp_dir(name: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("herdr-open-editor-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("notes.md");
    std::fs::write(&file, "notes").unwrap();
    (dir, file)
}

#[tokio::test]
async fn open_editor_rejects_directories_relative_paths_and_stale_targets() {
    let (mut app, _, focused_id, other_id) = app_with_two_panes();
    let (dir, file) = file_in_temp_dir("reject");
    let file = file.display().to_string();

    for (params, code) in [
        (params(None, "notes.md"), "invalid_params"),
        (params(None, "../notes.md"), "invalid_params"),
        (
            params(Some(focused_id), &dir.display().to_string()),
            "invalid_params",
        ),
        (params(Some(other_id), &file), "stale_pane_target"),
        (params(Some("w404:p1".into()), &file), "pane_not_found"),
    ] {
        let response = app.handle_pane_open_editor_with("edit".into(), params, recording_editor);
        assert_eq!(error_code(&response), code);
    }
    assert!(app.overlay_panes.is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn open_editor_refuses_a_focused_pane_that_failed_to_restore() {
    let (mut app, focused, focused_id, _) = app_with_two_panes();
    let (dir, file) = file_in_temp_dir("not-live");
    let terminal_id = app.state.workspaces[0]
        .terminal_id(focused)
        .unwrap()
        .clone();
    app.state
        .terminals
        .get_mut(&terminal_id)
        .unwrap()
        .restore_error = Some("gone".into());

    let response = app.handle_pane_open_editor_with(
        "edit".into(),
        params(Some(focused_id), &file.display().to_string()),
        recording_editor,
    );

    assert_eq!(error_code(&response), "pane_not_live");
    assert!(app.overlay_panes.is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn open_editor_reports_an_unsupported_platform() {
    let (mut app, _, _, _) = app_with_two_panes();
    let (dir, file) = file_in_temp_dir("unsupported");

    let response = app.handle_pane_open_editor_with(
        "edit".into(),
        params(None, &file.display().to_string()),
        unsupported_editor,
    );

    assert_eq!(error_code(&response), "platform_unsupported");
    assert!(app.overlay_panes.is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn open_editor_zooms_an_overlay_pane_that_closes_back_to_the_previous_focus() {
    let (mut app, focused, _, _) = app_with_two_panes();
    let (dir, _) = file_in_temp_dir("overlay");
    // A file that does not exist yet is accepted: the editor creates it.
    let new_file = dir.join("new.md");

    let response = app.handle_pane_open_editor_with(
        "edit".into(),
        params(None, &new_file.display().to_string()),
        recording_editor,
    );

    let success: SuccessResponse = serde_json::from_str(&response)
        .unwrap_or_else(|err| panic!("expected success ({err}): {response}"));
    let ResponseResult::PaneInfo { pane } = success.result else {
        panic!("expected pane info: {response}");
    };
    assert!(pane.focused);
    assert_eq!(EDITED.lock().unwrap().as_deref(), Some(new_file.as_path()));
    let (_, editor_pane) = app.parse_pane_id(&pane.pane_id).unwrap();
    let overlay = app.overlay_panes.get(&editor_pane).expect("overlay pane");
    assert_eq!(overlay.previous_focus, focused);
    assert!(!overlay.previous_zoomed);
    assert!(overlay.temp_files.is_empty());
    let tab = &app.state.workspaces[0].tabs[0];
    assert!(tab.zoomed);
    assert_eq!(tab.layout.focused(), editor_pane);

    app.handle_internal_event(AppEvent::PaneDied {
        pane_id: editor_pane,
        exit_reason: crate::platform::ChildExitReason::Exited,
    });

    let tab = &app.state.workspaces[0].tabs[0];
    assert_eq!(tab.layout.focused(), focused);
    assert!(!tab.zoomed);
    assert!(app.overlay_panes.is_empty());
    for (_, runtime) in app.terminal_runtimes.drain() {
        runtime.shutdown();
    }
    let _ = std::fs::remove_dir_all(dir);
}
