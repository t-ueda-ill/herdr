use std::sync::mpsc;
use std::time::Duration;

use base64::Engine as _;

use super::*;
use crate::api::schema::{
    ErrorResponse, PaneUploadCancelParams, PaneUploadCollision, PaneUploadEntryHeader,
    PaneUploadEntryKind, PaneUploadFinishParams, PaneUploadWriteParams, SuccessResponse,
};
use crate::container::{ContainerLaunch, ContainerRef, ContainerShell};
use crate::events::AppEvent;
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

#[derive(Clone)]
enum Kind {
    Host,
    Container(Option<&'static str>),
    Ssh,
}

fn launch(workdir: Option<&str>) -> ContainerLaunch {
    ContainerLaunch {
        reference: ContainerRef::Name("scratch".into()),
        canonical: "scratch".into(),
        id: "cid".into(),
        workdir: workdir.map(Into::into),
        start_dir: None,
        user: None,
        shell: ContainerShell::BashOrSh,
        ssh: None,
    }
}

/// One workspace with a single pane of `kind`.
fn app_with(kind: Kind, reported_cwd: Option<&str>) -> (App, String) {
    let mut app = test_app();
    let workspace = Workspace::test_new("uploads");
    let pane = workspace.tabs[0].root_pane;
    app.state.workspaces = vec![workspace];
    app.state.ensure_test_terminals();
    app.state.active = Some(0);
    let mut runtime = TerminalRuntime::test_with_scrollback_bytes(20, 5, 1000, b"");
    match kind {
        Kind::Container(_) => runtime.test_set_in_container(true),
        Kind::Ssh => runtime.test_set_in_ssh(true),
        Kind::Host => {}
    }
    if let Some(cwd) = reported_cwd {
        runtime.test_set_reported_cwd(cwd.into());
    }
    let terminal_id = app.state.workspaces[0].terminal_id(pane).unwrap().clone();
    let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
    match kind {
        Kind::Container(workdir) => terminal.container = Some(launch(workdir)),
        Kind::Ssh => {
            terminal.ssh = Some(SshLaunch::new("devbox".into(), Some("/tmp/c".into())).unwrap())
        }
        Kind::Host => {}
    }
    app.state.insert_test_runtime(pane, runtime);
    let pane_id = app.public_pane_id(0, pane).unwrap();
    (app, pane_id)
}

fn begin_params(
    pane_id: Option<&str>,
    destination: Option<&str>,
    names: &[&str],
) -> PaneUploadBeginParams {
    PaneUploadBeginParams {
        pane_id: pane_id.map(Into::into),
        terminal_id: None,
        destination: destination.map(Into::into),
        names: names.iter().map(|name| (*name).to_owned()).collect(),
        entries: names.len() as u64,
        bytes: 1_000,
    }
}

fn send(app: &mut App, origin: UploadOrigin, method: Method) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    let handled = app.handle_pane_upload_request(
        origin,
        Request {
            id: "req".into(),
            method,
        },
        Box::new(move |response| {
            let _ = tx.send(response);
        }),
    );
    assert!(handled);
    rx
}

fn answered(rx: &mpsc::Receiver<String>) -> String {
    rx.recv_timeout(Duration::from_secs(10))
        .expect("the request should be answered")
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

fn started(
    app: &mut App,
    origin: UploadOrigin,
    params: PaneUploadBeginParams,
) -> crate::api::schema::PaneUploadStarted {
    let response = answered(&send(app, origin, Method::PaneUploadBegin(params)));
    match success(&response) {
        ResponseResult::PaneUploadStarted(started) => started,
        other => panic!("{other:?}"),
    }
}

fn write_file(upload_id: &str, path: &str, body: &[u8]) -> Method {
    Method::PaneUploadWrite(PaneUploadWriteParams {
        upload_id: upload_id.into(),
        path: path.into(),
        entry: Some(PaneUploadEntryHeader {
            kind: PaneUploadEntryKind::File,
            mode: 0o640,
            mtime: None,
            size: Some(body.len() as u64),
            link_target: None,
        }),
        offset: 0,
        data: base64::engine::general_purpose::STANDARD.encode(body),
        last: true,
    })
}

fn finish(upload_id: &str) -> Method {
    Method::PaneUploadFinish(PaneUploadFinishParams {
        upload_id: upload_id.into(),
        collision: PaneUploadCollision::Fail,
    })
}

#[tokio::test]
async fn a_host_pane_upload_lands_in_the_requested_directory() {
    let (mut app, pane_id) = app_with(Kind::Host, None);
    let dest = TempDir::new("app-host");

    let begin = started(
        &mut app,
        UploadOrigin::Api,
        begin_params(Some(&pane_id), Some(dest.str()), &["a.txt"]),
    );
    let wrote = answered(&send(
        &mut app,
        UploadOrigin::Api,
        write_file(&begin.upload_id, "a.txt", b"hi"),
    ));
    let finished = answered(&send(&mut app, UploadOrigin::Api, finish(&begin.upload_id)));

    assert_eq!(begin.location.kind, PaneLocationKind::Host);
    assert!(matches!(
        success(&wrote),
        ResponseResult::PaneUploadProgress(_)
    ));
    let ResponseResult::PaneUploadFinished(finished) = success(&finished) else {
        panic!("{finished}")
    };
    assert_eq!(finished.paths.len(), 1);
    assert_eq!(std::fs::read(dest.join("a.txt")).unwrap(), b"hi");
}

#[tokio::test]
async fn a_host_pane_without_a_destination_uploads_into_its_working_directory() {
    let dest = TempDir::new("app-cwd");
    let (mut app, pane_id) = app_with(Kind::Host, Some(dest.str()));

    let begin = started(
        &mut app,
        UploadOrigin::Api,
        begin_params(Some(&pane_id), None, &["x"]),
    );

    assert_eq!(
        begin.destination,
        std::fs::canonicalize(dest.path())
            .unwrap()
            .to_str()
            .unwrap()
    );
    let cancel = Method::PaneUploadCancel(PaneUploadCancelParams {
        upload_id: begin.upload_id,
    });
    assert!(matches!(
        success(&answered(&send(&mut app, UploadOrigin::Api, cancel))),
        ResponseResult::Ok {}
    ));
}

#[tokio::test]
async fn off_host_targets_resolve_to_the_reported_directory_then_workdir_then_login_directory() {
    let cases = [
        (Kind::Container(Some("/work")), Some("/srv/app"), "/srv/app"),
        (Kind::Container(Some("/work")), None, "/work"),
        (Kind::Container(None), None, ""),
        (Kind::Ssh, None, ""),
        (Kind::Ssh, Some("/home/u/p"), "/home/u/p"),
    ];
    for (kind, reported, want) in cases {
        let (app, pane_id) = app_with(kind, reported);

        let target = app
            .resolve_upload_target(
                &UploadOrigin::Api,
                &begin_params(Some(&pane_id), None, &["x"]),
            )
            .unwrap_or_else(|failure| panic!("{failure:?}"));

        assert!(matches!(target.environment, UploadEnvironment::OffHost(_)));
        match target.destination {
            Destination::Remote(destination) => assert_eq!(destination, want),
            Destination::Host(path) => panic!("host destination {path:?}"),
        }
    }
}

#[tokio::test]
async fn a_default_destination_with_control_characters_is_refused() {
    for kind in [Kind::Host, Kind::Container(None), Kind::Ssh] {
        let (app, pane_id) = app_with(kind, Some("/tmp/x\u{1b}[201~\nboom"));

        let failure = app
            .resolve_upload_target(
                &UploadOrigin::Api,
                &begin_params(Some(&pane_id), None, &["x"]),
            )
            .err()
            .unwrap();

        assert_eq!(failure.0, "invalid_params", "{}", failure.1);
    }
}

#[tokio::test]
async fn an_explicit_off_host_destination_must_be_absolute() {
    let (app, pane_id) = app_with(Kind::Container(None), None);

    let failure = app
        .resolve_upload_target(
            &UploadOrigin::Api,
            &begin_params(Some(&pane_id), Some("rel/dir"), &["x"]),
        )
        .err()
        .unwrap();

    assert_eq!(failure.0, "invalid_params");
}

#[tokio::test]
async fn without_a_pane_or_terminal_the_destination_is_required_and_absolute() {
    let mut app = test_app();

    let missing = answered(&send(
        &mut app,
        UploadOrigin::Api,
        Method::PaneUploadBegin(begin_params(None, None, &["x"])),
    ));
    let relative = answered(&send(
        &mut app,
        UploadOrigin::Api,
        Method::PaneUploadBegin(begin_params(None, Some("tmp/x"), &["x"])),
    ));

    assert_eq!(error_code(&missing), "invalid_params");
    assert_eq!(error_code(&relative), "invalid_params");
}

#[tokio::test]
async fn names_that_could_be_mistaken_for_a_stage_are_refused() {
    let mut app = test_app();
    let dest = TempDir::new("app-names");

    let response = answered(&send(
        &mut app,
        UploadOrigin::Api,
        Method::PaneUploadBegin(begin_params(None, Some(dest.str()), &[".herdr-upload-x"])),
    ));

    assert_eq!(error_code(&response), "invalid_params");
}

#[tokio::test]
async fn a_direct_attach_can_only_target_its_own_terminal() {
    let (app, pane_id) = app_with(Kind::Host, None);
    let origin = UploadOrigin::AttachedClient {
        client_id: 3,
        terminal_id: "term_x".into(),
    };

    let failure = app
        .resolve_upload_target(&origin, &begin_params(Some(&pane_id), None, &["x"]))
        .err()
        .unwrap();

    assert_eq!(failure.0, "invalid_params");
}

#[tokio::test]
async fn a_shell_client_naming_a_terminal_must_mean_its_open_popup() {
    let (app, _) = app_with(Kind::Host, None);
    let terminal = app.state.terminals.keys().next().unwrap().to_string();
    let mut params = begin_params(None, None, &["x"]);
    params.terminal_id = Some(terminal);

    let failure = app
        .resolve_upload_target(&UploadOrigin::ShellClient(1), &params)
        .err()
        .unwrap();

    assert_eq!(failure.0, "stale_pane_target");
}

#[tokio::test]
async fn another_clients_upload_id_is_not_found() {
    let (mut app, pane_id) = app_with(Kind::Host, None);
    let dest = TempDir::new("app-owner");
    let begin = started(
        &mut app,
        UploadOrigin::ShellClient(1),
        begin_params(Some(&pane_id), Some(dest.str()), &["a"]),
    );

    let response = answered(&send(
        &mut app,
        UploadOrigin::ShellClient(2),
        write_file(&begin.upload_id, "a", b"x"),
    ));

    assert_eq!(error_code(&response), "upload_not_found");
}

#[tokio::test]
async fn a_pane_that_dies_cancels_its_upload_and_removes_the_stage() {
    let (mut app, pane_id) = app_with(Kind::Host, None);
    let dest = TempDir::new("app-died");
    let begin = started(
        &mut app,
        UploadOrigin::Api,
        begin_params(Some(&pane_id), Some(dest.str()), &["a"]),
    );
    let (_, pane) = app.parse_pane_id(&pane_id).unwrap();

    app.handle_internal_event(AppEvent::PaneDied {
        pane_id: pane,
        exit_reason: crate::platform::ChildExitReason::Exited,
    });
    let response = answered(&send(
        &mut app,
        UploadOrigin::Api,
        write_file(&begin.upload_id, "a", b"x"),
    ));

    assert_eq!(error_code(&response), "upload_cancelled");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::fs::read_dir(dest.path()).unwrap().next().is_some() {
        assert!(
            std::time::Instant::now() < deadline,
            "the stage should be removed"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[tokio::test]
async fn a_pane_whose_shell_moved_off_host_makes_the_upload_stale() {
    let (mut app, pane_id) = app_with(Kind::Host, None);
    let dest = TempDir::new("app-stale");
    let begin = started(
        &mut app,
        UploadOrigin::Api,
        begin_params(Some(&pane_id), Some(dest.str()), &["a"]),
    );
    let (ws_idx, pane) = app.parse_pane_id(&pane_id).unwrap();
    let terminal_id = app.state.workspaces[ws_idx]
        .terminal_id(pane)
        .unwrap()
        .clone();
    app.state.terminals.get_mut(&terminal_id).unwrap().container = Some(launch(None));
    let mut runtime = TerminalRuntime::test_with_scrollback_bytes(20, 5, 1000, b"");
    runtime.test_set_in_container(true);
    app.state.insert_test_runtime(pane, runtime);

    let response = answered(&send(
        &mut app,
        UploadOrigin::Api,
        write_file(&begin.upload_id, "a", b"x"),
    ));
    let again = answered(&send(
        &mut app,
        UploadOrigin::Api,
        write_file(&begin.upload_id, "a", b"x"),
    ));

    assert_eq!(error_code(&response), "stale_pane_target");
    assert_eq!(error_code(&again), "upload_cancelled");
}

#[tokio::test]
async fn other_methods_are_left_to_the_caller() {
    let mut app = test_app();
    let (tx, _rx) = mpsc::channel();

    let handled = app.handle_deferred_pane_upload_api_request(
        Request {
            id: "r".into(),
            method: Method::Ping(crate::api::schema::PingParams::default()),
        },
        tx,
    );

    assert!(!handled);
}
