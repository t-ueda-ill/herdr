use std::time::Duration;

use crate::api::schema::{
    ErrorResponse, Method, PaneDirectoryIndexParams, PaneDirectoryListParams,
    PaneDirectoryOpenEditorParams, PaneFileReadParams, Request,
};
use crate::app::App;
use crate::container::{ContainerLaunch, ContainerRef, ContainerShell};
use crate::events::PaneEditorProbeResult;
use crate::offhost::exec::Programs;
use crate::offhost::{PaneEnvironment, PaneTarget};
use crate::ssh_host::SshLaunch;
use crate::terminal::TerminalRuntime;
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

fn launch(workdir: Option<&str>) -> ContainerLaunch {
    ContainerLaunch {
        reference: ContainerRef::Name("scratch".into()),
        canonical: "scratch".into(),
        id: "cid".into(),
        workdir: workdir.map(Into::into),
        start_dir: None,
        user: Some("dev".into()),
        shell: ContainerShell::BashOrSh,
        ssh: None,
    }
}

fn ssh(control_path: Option<&str>) -> SshLaunch {
    SshLaunch::new("devbox".into(), control_path.map(Into::into)).unwrap()
}

/// How a pane's shell runs. `Host` carries no launch metadata at all;
/// `ColdRestored` keeps it while the runtime is a host shell.
#[derive(Clone)]
enum PaneKind {
    Host,
    Container(ContainerLaunch),
    Ssh(SshLaunch),
    ColdRestoredContainer(ContainerLaunch),
}

/// Two panes in one workspace: the first is focused and of `kind`; the second
/// is a host pane. Callers run on tokio because test runtimes need it.
fn app_with_pane(kind: PaneKind, reported_cwd: Option<&str>) -> (App, String, String) {
    app_with_panes(kind, PaneKind::Host, reported_cwd)
}

fn app_with_panes(
    first: PaneKind,
    second: PaneKind,
    reported_cwd: Option<&str>,
) -> (App, String, String) {
    let mut app = test_app();
    let mut workspace = Workspace::test_new("files");
    let first_pane = workspace.tabs[0].root_pane;
    let second_pane = workspace.test_split(ratatui::layout::Direction::Horizontal);
    workspace.tabs[0].layout.focus_pane(first_pane);
    app.state.workspaces = vec![workspace];
    app.state.ensure_test_terminals();
    app.state.active = Some(0);
    app.state.selected = 0;
    for (pane, kind) in [(first_pane, first), (second_pane, second)] {
        let mut runtime = TerminalRuntime::test_with_scrollback_bytes(20, 5, 1000, b"");
        match &kind {
            PaneKind::Container(_) => runtime.test_set_in_container(true),
            PaneKind::Ssh(_) => runtime.test_set_in_ssh(true),
            PaneKind::Host | PaneKind::ColdRestoredContainer(_) => {}
        }
        if let Some(cwd) = reported_cwd {
            runtime.test_set_reported_cwd(cwd.into());
        }
        let terminal_id = app.state.workspaces[0].terminal_id(pane).unwrap().clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        match &kind {
            PaneKind::Container(launch) | PaneKind::ColdRestoredContainer(launch) => {
                terminal.container = Some(launch.clone());
            }
            PaneKind::Ssh(launch) => terminal.ssh = Some(launch.clone()),
            PaneKind::Host => {}
        }
        app.state.insert_test_runtime(pane, runtime);
    }
    let first_id = app.public_pane_id(0, first_pane).unwrap();
    let second_id = app.public_pane_id(0, second_pane).unwrap();
    (app, first_id, second_id)
}

fn list(pane_id: &str, path: Option<&str>) -> Request {
    Request {
        id: "req".into(),
        method: Method::PaneDirectoryList(PaneDirectoryListParams {
            pane_id: pane_id.into(),
            path: path.map(Into::into),
            trust_repository: false,
        }),
    }
}

fn dispatch(app: &mut App, request: Request) -> std::sync::mpsc::Receiver<String> {
    let (respond_to, response_rx) = std::sync::mpsc::channel();
    assert!(app.handle_deferred_pane_directory_api_request(request, respond_to));
    response_rx
}

fn answered(app: &mut App, request: Request) -> String {
    dispatch(app, request)
        .recv_timeout(Duration::from_secs(10))
        .expect("the request should be answered")
}

fn error_code(response: &str) -> String {
    serde_json::from_str::<ErrorResponse>(response)
        .unwrap_or_else(|err| panic!("expected error ({err}): {response}"))
        .error
        .code
}

#[tokio::test]
async fn requests_for_a_host_pane_are_rejected_as_invalid_params() {
    let (mut app, host_pane, _) = app_with_pane(PaneKind::Host, None);
    let slots = app.directory_read_slots.available_permits();

    let response = dispatch(&mut app, list(&host_pane, None))
        .try_recv()
        .expect("rejected before a worker starts");

    assert_eq!(error_code(&response), "invalid_params");
    assert_eq!(app.directory_read_slots.available_permits(), slots);
}

#[tokio::test]
async fn a_cold_restored_container_pane_counts_as_a_host_pane() {
    let (mut app, pane, _) =
        app_with_pane(PaneKind::ColdRestoredContainer(launch(Some("/work"))), None);

    let response = answered(&mut app, list(&pane, None));

    assert_eq!(error_code(&response), "invalid_params");
}

#[tokio::test]
async fn an_unknown_pane_is_not_found() {
    let (mut app, _, _) = app_with_pane(PaneKind::Container(launch(None)), None);

    let response = answered(&mut app, list("w404:p1", None));

    assert_eq!(error_code(&response), "pane_not_found");
}

#[tokio::test]
async fn every_method_rejects_host_panes_and_bad_paths_without_a_worker() {
    let (mut app, pane, _) = app_with_pane(PaneKind::Container(launch(None)), None);
    let slots = app.directory_read_slots.available_permits();
    let requests = [
        Method::PaneDirectoryList(PaneDirectoryListParams {
            pane_id: pane.clone(),
            path: Some("relative".into()),
            trust_repository: false,
        }),
        Method::PaneDirectoryIndex(PaneDirectoryIndexParams {
            pane_id: pane.clone(),
            path: Some("/a/../b".into()),
            trust_repository: false,
        }),
        Method::PaneFileRead(PaneFileReadParams {
            pane_id: pane.clone(),
            path: "".into(),
            revision: None,
            poll: false,
        }),
        Method::PaneDirectoryOpenEditor(PaneDirectoryOpenEditorParams {
            pane_id: pane.clone(),
            path: "../x".into(),
        }),
    ];

    for method in requests {
        let request = Request {
            id: "req".into(),
            method,
        };
        let response = dispatch(&mut app, request)
            .try_recv()
            .expect("rejected before a worker starts");
        assert_eq!(error_code(&response), "invalid_params", "{response}");
    }
    assert_eq!(app.directory_read_slots.available_permits(), slots);
}

#[tokio::test]
async fn reads_answer_busy_when_every_directory_slot_is_taken() {
    let (mut app, pane, _) = app_with_pane(PaneKind::Container(launch(Some("/work"))), None);
    app.directory_read_slots = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
    let _held = app
        .directory_read_slots
        .clone()
        .try_acquire_owned()
        .unwrap();

    let response = answered(&mut app, list(&pane, None));

    assert_eq!(error_code(&response), "directory_busy");
}

#[tokio::test]
async fn an_ssh_pane_without_a_control_path_is_answered_ssh_unavailable_without_spawning_ssh() {
    let (mut app, pane, _) = app_with_pane(PaneKind::Ssh(ssh(None)), Some("/home/u"));

    let response = answered(&mut app, list(&pane, None));

    assert_eq!(error_code(&response), "ssh_unavailable");
    assert!(response.contains("without a prompt"), "{response}");
}

#[tokio::test]
async fn a_worker_releases_its_slot_after_answering() {
    let (mut app, pane, _) = app_with_pane(PaneKind::Ssh(ssh(None)), None);
    app.directory_read_slots = std::sync::Arc::new(tokio::sync::Semaphore::new(1));

    answered(&mut app, list(&pane, None));

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while app.directory_read_slots.available_permits() != 1 {
        assert!(std::time::Instant::now() < deadline, "slot not released");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[tokio::test]
async fn the_reported_directory_is_the_root_before_any_fallback() {
    let (app, pane, _) = app_with_pane(
        PaneKind::Container(launch(Some("/work"))),
        Some("/srv/reported"),
    );

    let target = app.pane_target(&pane).unwrap();

    assert_eq!(target.root.as_deref(), Some("/srv/reported"));
    assert_eq!(target.cwd.as_deref(), Some("/srv/reported"));
}

#[tokio::test]
async fn a_container_without_a_report_starts_at_its_working_directory_then_the_filesystem_root() {
    let (with_workdir, pane, _) = app_with_pane(PaneKind::Container(launch(Some("/work"))), None);
    let (without_workdir, bare_pane, _) = app_with_pane(PaneKind::Container(launch(None)), None);

    let a = with_workdir.pane_target(&pane).unwrap();
    let b = without_workdir.pane_target(&bare_pane).unwrap();

    assert_eq!(a.root.as_deref(), Some("/work"));
    assert_eq!(b.root.as_deref(), Some("/"));
    assert_eq!(b.cwd, None);
}

#[tokio::test]
async fn an_ssh_pane_without_a_report_leaves_the_root_to_the_login_directory() {
    let (app, pane, _) = app_with_pane(PaneKind::Ssh(ssh(Some("/tmp/c"))), None);

    let target = app.pane_target(&pane).unwrap();

    assert_eq!(target.root, None);
    assert_eq!(
        target.environment,
        PaneEnvironment::Ssh(ssh(Some("/tmp/c")))
    );
}

#[tokio::test]
async fn the_editor_opens_only_over_the_focused_pane() {
    let (mut app, _, unfocused) = app_with_panes(
        PaneKind::Container(launch(None)),
        PaneKind::Ssh(ssh(Some("/tmp/c"))),
        None,
    );

    let request = Request {
        id: "req".into(),
        method: Method::PaneDirectoryOpenEditor(PaneDirectoryOpenEditorParams {
            pane_id: unfocused,
            path: "/etc/hosts".into(),
        }),
    };
    let response = dispatch(&mut app, request)
        .try_recv()
        .expect("rejected before a worker starts");

    assert_eq!(error_code(&response), "stale_pane_target");
}

fn probed(
    app: &App,
    pane_id: &str,
    environment: PaneEnvironment,
    probe: Result<(), (&'static str, String)>,
) -> (PaneEditorProbeResult, std::sync::mpsc::Receiver<String>) {
    let _ = app;
    let (respond_to, rx) = std::sync::mpsc::channel();
    (
        PaneEditorProbeResult {
            id: "req".into(),
            pane_id: pane_id.into(),
            path: "/work/a.rs".into(),
            environment,
            cwd: Some("/work".into()),
            probe,
            respond_to,
        },
        rx,
    )
}

#[tokio::test]
async fn a_failed_probe_is_forwarded_and_opens_no_overlay() {
    let (mut app, pane, _) = app_with_pane(PaneKind::Container(launch(Some("/work"))), None);
    let environment = app.pane_target(&pane).unwrap().environment;
    let panes_before = app.state.terminals.len();
    let (result, rx) = probed(
        &app,
        &pane,
        environment,
        Err(("invalid_params", "path is a directory: /work".into())),
    );

    let opened = app.handle_pane_editor_probed(result);

    assert!(!opened);
    assert_eq!(error_code(&rx.try_recv().unwrap()), "invalid_params");
    assert_eq!(app.state.terminals.len(), panes_before);
}

#[tokio::test]
async fn a_successful_probe_answers_with_the_editor_overlay_pane() {
    let (mut app, pane, _) = app_with_pane(PaneKind::Container(launch(Some("/work"))), None);
    let environment = app.pane_target(&pane).unwrap().environment;
    let (result, rx) = probed(&app, &pane, environment, Ok(()));
    // Why a fake: the overlay really spawns the program, and docker is not on
    // every machine that runs the tests.
    let docker = crate::noninteractive_process::fake_program::FakeProgram::new(
        "docker",
        "editor-overlay",
        "sleep 30",
    );
    let docker_path = docker.program();
    let programs = Programs {
        docker: &docker_path,
        ssh: std::path::Path::new("ssh"),
    };

    let opened = app.handle_pane_editor_probed_with(result, &programs);

    let response = rx.try_recv().unwrap();
    assert!(opened, "{response}");
    let value: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert_eq!(value["result"]["type"], "pane_info", "{response}");
    assert!(value["result"]["pane"]["pane_id"].is_string(), "{response}");
}

#[tokio::test]
async fn a_probe_for_a_pane_that_moved_to_another_environment_is_stale() {
    let (mut app, pane, _) = app_with_pane(PaneKind::Container(launch(Some("/work"))), None);
    let elsewhere = PaneEnvironment::Ssh(ssh(Some("/tmp/c")));
    let (result, rx) = probed(&app, &pane, elsewhere, Ok(()));

    let opened = app.handle_pane_editor_probed(result);

    assert!(!opened);
    assert_eq!(error_code(&rx.try_recv().unwrap()), "stale_pane_target");
}

#[tokio::test]
async fn a_probe_for_a_pane_that_closed_is_not_found() {
    let (mut app, _, _) = app_with_pane(PaneKind::Container(launch(Some("/work"))), None);
    let environment = PaneEnvironment::Container(Box::new(launch(Some("/work"))));
    let (result, rx) = probed(&app, "w404:p9", environment, Ok(()));

    let opened = app.handle_pane_editor_probed(result);

    assert!(!opened);
    assert_eq!(error_code(&rx.try_recv().unwrap()), "pane_not_found");
}

#[test]
fn pane_target_prefers_the_report_then_the_container_workdir_then_root() {
    let container = |workdir| PaneEnvironment::Container(Box::new(launch(workdir)));
    let resolve = |env, reported: Option<&str>| {
        let PaneTarget { root, cwd, .. } = PaneTarget::resolve(env, reported.map(Into::into));
        (root, cwd)
    };

    assert_eq!(
        resolve(container(Some("/work")), Some("/r")),
        (Some("/r".into()), Some("/r".into()))
    );
    assert_eq!(
        resolve(container(Some("/work")), None),
        (Some("/work".into()), Some("/work".into()))
    );
    assert_eq!(resolve(container(None), None), (Some("/".into()), None));
    assert_eq!(resolve(PaneEnvironment::Ssh(ssh(None)), None), (None, None));
    assert_eq!(
        resolve(PaneEnvironment::Ssh(ssh(None)), Some("/home/u")),
        (Some("/home/u".into()), Some("/home/u".into()))
    );
}

#[tokio::test]
async fn the_editor_overlay_starts_in_the_host_launch_directory_not_the_remote_one() {
    let (mut app, pane, _) = app_with_pane(
        PaneKind::Ssh(ssh(Some("/tmp/c"))),
        Some("/home/remote/only"),
    );
    let (ws_pane, _) = (app.parse_pane_id(&pane).unwrap().1, ());
    let terminal_id = app.state.workspaces[0]
        .terminal_id(ws_pane)
        .unwrap()
        .clone();
    app.state.terminals.get_mut(&terminal_id).unwrap().cwd = "/host/launch".into();

    assert_eq!(
        app.editor_host_cwd(&pane),
        Some(std::path::PathBuf::from("/host/launch"))
    );
}
