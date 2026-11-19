use super::*;
use crate::api::schema::{ErrorResponse, PaneLocationKind, SuccessResponse};
use crate::container::{ContainerLaunch, ContainerRef, ContainerShell};
use crate::ssh_host::SshLaunch;
use crate::terminal::TerminalRuntime;
use crate::upload::test_support::TempDir;
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
    Host,
    Container,
    Ssh,
}

fn app_with(kind: Kind) -> (App, String) {
    let mut app = test_app();
    let workspace = Workspace::test_new("previews");
    let pane = workspace.tabs[0].root_pane;
    app.state.workspaces = vec![workspace];
    app.state.ensure_test_terminals();
    app.state.active = Some(0);
    let mut runtime = TerminalRuntime::test_with_scrollback_bytes(20, 5, 1000, b"");
    match kind {
        Kind::Container => runtime.test_set_in_container(true),
        Kind::Ssh => runtime.test_set_in_ssh(true),
        Kind::Host => {}
    }
    let terminal_id = app.state.workspaces[0].terminal_id(pane).unwrap().clone();
    let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
    match kind {
        Kind::Container => {
            terminal.container = Some(ContainerLaunch {
                reference: ContainerRef::Name("web".into()),
                canonical: "web".into(),
                id: "cid".into(),
                workdir: None,
                start_dir: None,
                user: None,
                shell: ContainerShell::BashOrSh,
                ssh: None,
            });
        }
        Kind::Ssh => {
            terminal.ssh = Some(SshLaunch::new("devbox".into(), Some("/tmp/c".into())).unwrap());
        }
        Kind::Host => {}
    }
    app.state.insert_test_runtime(pane, runtime);
    let pane_id = app.public_pane_id(0, pane).unwrap();
    (app, pane_id)
}

fn start(app: &mut App, pane_id: Option<&str>, root: &str, entry: Option<&str>) -> String {
    app.handle_preview_start(
        "req".into(),
        PreviewStartParams {
            pane_id: pane_id.map(Into::into),
            root: root.into(),
            entry: entry.map(Into::into),
        },
    )
}

fn started(response: &str) -> PreviewInfo {
    match serde_json::from_str::<SuccessResponse>(response)
        .unwrap_or_else(|err| panic!("expected success ({err}): {response}"))
        .result
    {
        ResponseResult::PreviewStarted { preview } => preview,
        other => panic!("unexpected result: {other:?}"),
    }
}

fn error_code(response: &str) -> String {
    serde_json::from_str::<ErrorResponse>(response)
        .unwrap_or_else(|err| panic!("expected error ({err}): {response}"))
        .error
        .code
}

fn listed(app: &mut App) -> Vec<PreviewInfo> {
    let response = app.handle_preview_list("req".into());
    match serde_json::from_str::<SuccessResponse>(&response)
        .unwrap()
        .result
    {
        ResponseResult::PreviewList { previews } => previews,
        other => panic!("unexpected result: {other:?}"),
    }
}

#[test]
fn a_host_folder_is_served_on_loopback_and_reused_when_started_again() {
    let root = TempDir::new("preview-host");
    std::fs::write(root.join("index.html"), "<p>hi</p>").unwrap();
    std::fs::create_dir_all(root.join("docs")).unwrap();
    std::fs::write(root.join("docs/a.md"), "# a").unwrap();
    let mut app = test_app();

    let first = started(&start(&mut app, None, root.str(), None));
    assert_eq!(
        first.root,
        std::fs::canonicalize(root.path())
            .unwrap()
            .to_string_lossy()
    );
    assert!(first.location.is_none());
    assert!(first
        .url
        .starts_with(&format!("http://127.0.0.1:{}/?herdr-token=", first.port)));

    let entry = root.join("docs/a.md");
    let again = started(&start(&mut app, None, root.str(), entry.to_str()));
    assert_eq!(again.preview_id, first.preview_id);
    assert!(again.url.contains("/docs/a.md?herdr-token="));
    assert_eq!(listed(&mut app).len(), 1);
}

#[test]
fn stopping_ends_the_preview_and_an_unknown_id_is_reported() {
    let root = TempDir::new("preview-stop");
    let mut app = test_app();
    let preview = started(&start(&mut app, None, root.str(), None));
    let stopped = app.handle_preview_stop(
        "req".into(),
        PreviewStopParams {
            preview_id: preview.preview_id.clone(),
        },
    );
    assert!(stopped.contains("preview_stopped"), "{stopped}");
    assert!(listed(&mut app).is_empty());
    let again = app.handle_preview_stop(
        "req".into(),
        PreviewStopParams {
            preview_id: preview.preview_id,
        },
    );
    assert_eq!(error_code(&again), "preview_not_found");
}

#[test]
fn bad_roots_and_entries_are_refused_before_anything_starts() {
    let root = TempDir::new("preview-bad");
    let mut app = test_app();
    assert_eq!(
        error_code(&start(&mut app, None, "relative/dir", None)),
        "invalid_params"
    );
    let missing = root.join("missing");
    assert_eq!(
        error_code(&start(&mut app, None, missing.to_str().unwrap(), None)),
        "not_found"
    );
    assert_eq!(
        error_code(&start(&mut app, None, root.str(), Some("/etc/hosts"))),
        "invalid_params"
    );
    assert!(listed(&mut app).is_empty());
}

#[tokio::test]
async fn a_container_pane_serves_its_own_folder_without_running_anything_yet() {
    let (mut app, pane_id) = app_with(Kind::Container);
    let preview = started(&start(
        &mut app,
        Some(&pane_id),
        "/srv/site/",
        Some("/srv/site/index.html"),
    ));
    assert_eq!(preview.root, "/srv/site");
    let location = preview.location.unwrap();
    assert_eq!(location.kind, PaneLocationKind::Container);
    assert_eq!(location.container.as_deref(), Some("web"));
    assert!(preview.url.contains("/index.html?herdr-token="));
}

#[tokio::test]
async fn an_ssh_pane_is_its_own_place_and_paths_must_be_absolute() {
    let (mut app, pane_id) = app_with(Kind::Ssh);
    let preview = started(&start(&mut app, Some(&pane_id), "/home/me/site", None));
    let location = preview.location.unwrap();
    assert_eq!(location.kind, PaneLocationKind::Ssh);
    assert_eq!(location.ssh_host.as_deref(), Some("devbox"));
    assert_eq!(
        error_code(&start(&mut app, Some(&pane_id), "site", None)),
        "invalid_params"
    );
    assert_eq!(
        error_code(&start(&mut app, Some(&pane_id), "/home/me/../etc", None)),
        "invalid_params"
    );
}

#[tokio::test]
async fn a_pane_running_on_this_host_serves_from_this_host() {
    let root = TempDir::new("preview-host-pane");
    let (mut app, pane_id) = app_with(Kind::Host);
    let preview = started(&start(&mut app, Some(&pane_id), root.str(), None));
    assert!(preview.location.is_none());
}

#[tokio::test]
async fn an_unknown_pane_is_reported() {
    let mut app = test_app();
    assert_eq!(
        error_code(&start(&mut app, Some("w9-9"), "/srv", None)),
        "pane_not_found"
    );
}
