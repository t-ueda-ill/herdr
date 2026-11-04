use std::sync::mpsc;
use std::time::Duration;

use base64::Engine as _;

use super::*;
use crate::api::schema::{
    ErrorResponse, PaneDownloadCancelParams, PaneDownloadChunk, PaneDownloadReadParams,
    PaneDownloadStarted, PaneUploadEntryKind, SuccessResponse,
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
    Container,
    Ssh,
}

/// One workspace with a single pane of `kind`.
fn app_with(kind: Kind) -> (App, String) {
    let mut app = test_app();
    let workspace = Workspace::test_new("downloads");
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
                reference: ContainerRef::Name("scratch".into()),
                canonical: "scratch".into(),
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

fn begin_params(pane_id: Option<&str>, paths: &[&str]) -> PaneDownloadBeginParams {
    PaneDownloadBeginParams {
        pane_id: pane_id.map(Into::into),
        paths: paths.iter().map(|path| (*path).to_owned()).collect(),
        max_bytes: None,
    }
}

fn send(app: &mut App, origin: DownloadOrigin, method: Method) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    let handled = app.handle_pane_download_request(
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

fn begin(
    app: &mut App,
    origin: DownloadOrigin,
    params: PaneDownloadBeginParams,
) -> Result<PaneDownloadStarted, String> {
    let response = answered(&send(app, origin, Method::PaneDownloadBegin(params)));
    match serde_json::from_str::<SuccessResponse>(&response) {
        Ok(SuccessResponse {
            result: ResponseResult::PaneDownloadStarted(started),
            ..
        }) => Ok(started),
        _ => Err(error_code(&response)),
    }
}

fn read(app: &mut App, origin: DownloadOrigin, id: &str, seq: u64) -> String {
    answered(&send(
        app,
        origin,
        Method::PaneDownloadRead(PaneDownloadReadParams {
            download_id: id.into(),
            seq,
        }),
    ))
}

fn chunk(response: &str) -> PaneDownloadChunk {
    match success(response) {
        ResponseResult::PaneDownloadChunk(chunk) => chunk,
        other => panic!("{other:?}"),
    }
}

fn refused(paths: &[&str]) -> String {
    let mut app = test_app();
    begin(&mut app, DownloadOrigin::Api, begin_params(None, paths))
        .expect_err("the begin should be refused")
}

#[tokio::test]
async fn a_host_download_without_a_pane_streams_the_files_to_done() {
    let mut app = test_app();
    let dir = TempDir::new("app-download");
    std::fs::create_dir(dir.join("tree")).unwrap();
    std::fs::write(dir.join("tree/a.txt"), b"hello").unwrap();
    let path = dir.join("tree").to_str().unwrap().to_owned();

    let started = begin(
        &mut app,
        DownloadOrigin::Api,
        begin_params(None, &[path.as_str()]),
    )
    .unwrap();

    assert_eq!(started.names, vec!["tree".to_owned()]);
    assert_eq!(
        started.location.kind,
        crate::api::schema::PaneLocationKind::Host
    );
    assert_eq!(started.window, 2);
    let mut paths = Vec::new();
    let mut body = Vec::new();
    for seq in 0..10 {
        let answer = chunk(&read(
            &mut app,
            DownloadOrigin::Api,
            &started.download_id,
            seq,
        ));
        for record in &answer.records {
            paths.push(record.path.clone());
            body.extend(
                base64::engine::general_purpose::STANDARD
                    .decode(&record.data)
                    .unwrap(),
            );
            if let Some(entry) = &record.entry {
                assert!(matches!(
                    entry.kind,
                    PaneUploadEntryKind::Directory | PaneUploadEntryKind::File
                ));
            }
        }
        if answer.done {
            assert_eq!(paths, vec!["tree".to_owned(), "tree/a.txt".to_owned()]);
            assert_eq!(body, b"hello");
            return;
        }
    }
    panic!("the download never finished");
}

#[tokio::test]
async fn a_host_pane_reads_the_servers_own_files() {
    let (mut app, pane_id) = app_with(Kind::Host);
    let dir = TempDir::new("app-download-pane");
    std::fs::write(dir.join("f"), b"x").unwrap();
    let path = dir.join("f").to_str().unwrap().to_owned();

    let started = begin(
        &mut app,
        DownloadOrigin::ShellClient(3),
        begin_params(Some(&pane_id), &[path.as_str()]),
    )
    .unwrap();

    assert_eq!(started.names, vec!["f".to_owned()]);
    assert_eq!(started.entries, 1);
    assert_eq!(started.bytes, 1);
}

#[tokio::test]
async fn container_and_ssh_panes_resolve_to_their_environment() {
    for kind in [Kind::Container, Kind::Ssh] {
        let (app, pane_id) = app_with(kind);

        let source = app
            .resolve_download_source(&begin_params(Some(&pane_id), &["/etc"]))
            .unwrap();

        assert!(matches!(source.environment, UploadEnvironment::OffHost(_)));
        assert_eq!(source.key.public_pane.as_deref(), Some(pane_id.as_str()));
        assert!(source.key.terminal.is_none());
    }
}

#[tokio::test]
async fn an_unknown_or_dead_pane_is_refused() {
    let mut app = test_app();

    let error = begin(
        &mut app,
        DownloadOrigin::Api,
        begin_params(Some("w9-p9"), &["/etc"]),
    )
    .unwrap_err();

    assert_eq!(error, "pane_not_found");
}

#[tokio::test]
async fn paths_that_are_not_absolute_plain_and_distinctly_named_are_refused() {
    let too_many: Vec<String> = (0..1_025).map(|n| format!("/tmp/n{n}")).collect();
    let long = format!("/{}", "a".repeat(4_097));
    let near_limit = format!("/{}", "a".repeat(4_000));
    let over_the_total: Vec<String> = (0..70).map(|n| format!("{near_limit}{n}")).collect();
    let mut cases: Vec<Vec<&str>> = vec![
        vec![],
        vec!["relative/path"],
        vec!["/tmp/../etc"],
        vec!["/tmp/with\nnewline"],
        vec!["/tmp/a/x", "/tmp/b/x"],
        vec!["/"],
        vec!["/tmp/.herdr-upload-1"],
        vec![long.as_str()],
    ];
    cases.push(too_many.iter().map(String::as_str).collect());
    cases.push(over_the_total.iter().map(String::as_str).collect());

    for paths in cases {
        assert_eq!(refused(&paths), "invalid_params", "{paths:?}");
    }
}

#[tokio::test]
async fn off_host_paths_must_name_an_item() {
    let (mut app, pane_id) = app_with(Kind::Container);

    for path in ["/etc/", "/", "etc"] {
        let error = begin(
            &mut app,
            DownloadOrigin::Api,
            begin_params(Some(&pane_id), &[path]),
        )
        .unwrap_err();
        assert_eq!(error, "invalid_params", "{path}");
    }
}

#[tokio::test]
async fn max_bytes_is_refused_at_zero_and_clamped_to_the_server_chunk() {
    let mut app = test_app();
    let dir = TempDir::new("app-download-chunk");
    std::fs::write(dir.join("f"), b"x").unwrap();
    let path = dir.join("f").to_str().unwrap().to_owned();
    let with = |max_bytes| PaneDownloadBeginParams {
        max_bytes: Some(max_bytes),
        ..begin_params(None, &[path.as_str()])
    };

    assert_eq!(
        begin(&mut app, DownloadOrigin::Api, with(0)).unwrap_err(),
        "invalid_params"
    );
    let big = begin(&mut app, DownloadOrigin::Api, with(u64::MAX)).unwrap();
    let small = begin(&mut app, DownloadOrigin::Api, with(1_000)).unwrap();
    let default = begin(
        &mut app,
        DownloadOrigin::ShellClient(1),
        begin_params(None, &[path.as_str()]),
    )
    .unwrap();

    assert_eq!(big.chunk_bytes, DOWNLOAD_CHUNK_BYTES as u64);
    assert_eq!(small.chunk_bytes, 1_000);
    assert_eq!(default.chunk_bytes, DOWNLOAD_CHUNK_BYTES as u64);
}

#[tokio::test]
async fn a_missing_top_level_path_fails_the_begin_with_not_found() {
    let mut app = test_app();
    let dir = TempDir::new("app-download-missing");
    let path = dir.join("nope").to_str().unwrap().to_owned();

    let error = begin(
        &mut app,
        DownloadOrigin::Api,
        begin_params(None, &[path.as_str()]),
    )
    .unwrap_err();

    assert_eq!(error, "not_found");
}

#[tokio::test]
async fn one_owner_may_hold_two_downloads_and_the_server_four() {
    let mut app = test_app();
    let dir = TempDir::new("app-download-caps");
    std::fs::write(dir.join("f"), b"x").unwrap();
    let path = dir.join("f").to_str().unwrap().to_owned();
    let params = || begin_params(None, &[path.as_str()]);

    for _ in 0..2 {
        begin(&mut app, DownloadOrigin::ShellClient(1), params()).unwrap();
    }
    let third = begin(&mut app, DownloadOrigin::ShellClient(1), params()).unwrap_err();
    for owner in [DownloadOrigin::ShellClient(2), DownloadOrigin::Api] {
        begin(&mut app, owner, params()).unwrap();
    }
    let fifth = begin(&mut app, DownloadOrigin::ShellClient(3), params()).unwrap_err();

    assert_eq!(third, "download_busy");
    assert_eq!(fifth, "download_busy");
}

#[tokio::test]
async fn another_owners_download_id_is_not_found() {
    let mut app = test_app();
    let dir = TempDir::new("app-download-owner");
    std::fs::write(dir.join("f"), b"x").unwrap();
    let path = dir.join("f").to_str().unwrap().to_owned();
    let started = begin(
        &mut app,
        DownloadOrigin::ShellClient(1),
        begin_params(None, &[path.as_str()]),
    )
    .unwrap();

    let response = read(
        &mut app,
        DownloadOrigin::ShellClient(2),
        &started.download_id,
        0,
    );

    assert_eq!(error_code(&response), "download_not_found");
}

#[tokio::test]
async fn a_cancelled_download_refuses_later_reads_with_download_cancelled() {
    let mut app = test_app();
    let dir = TempDir::new("app-download-cancel");
    std::fs::write(dir.join("f"), b"x").unwrap();
    let path = dir.join("f").to_str().unwrap().to_owned();
    let started = begin(
        &mut app,
        DownloadOrigin::Api,
        begin_params(None, &[path.as_str()]),
    )
    .unwrap();

    let cancelled = answered(&send(
        &mut app,
        DownloadOrigin::Api,
        Method::PaneDownloadCancel(PaneDownloadCancelParams {
            download_id: started.download_id.clone(),
        }),
    ));
    let after = read(&mut app, DownloadOrigin::Api, &started.download_id, 0);

    assert!(matches!(success(&cancelled), ResponseResult::Ok {}));
    assert_eq!(error_code(&after), "download_cancelled");
}

#[tokio::test]
async fn a_pane_that_dies_cancels_its_download() {
    let (mut app, pane_id) = app_with(Kind::Host);
    let dir = TempDir::new("app-download-died");
    std::fs::write(dir.join("f"), b"x").unwrap();
    let path = dir.join("f").to_str().unwrap().to_owned();
    let started = begin(
        &mut app,
        DownloadOrigin::Api,
        begin_params(Some(&pane_id), &[path.as_str()]),
    )
    .unwrap();
    let (_, pane) = app.parse_pane_id(&pane_id).unwrap();

    app.handle_internal_event(AppEvent::PaneDied {
        pane_id: pane,
        exit_reason: crate::platform::ChildExitReason::Exited,
        exit_code: None,
    });
    let response = read(&mut app, DownloadOrigin::Api, &started.download_id, 0);

    assert_eq!(error_code(&response), "download_cancelled");
}

#[tokio::test]
async fn a_pane_whose_shell_moved_off_host_makes_the_download_stale() {
    let (mut app, pane_id) = app_with(Kind::Host);
    let dir = TempDir::new("app-download-stale");
    std::fs::write(dir.join("f"), b"x").unwrap();
    let path = dir.join("f").to_str().unwrap().to_owned();
    let started = begin(
        &mut app,
        DownloadOrigin::Api,
        begin_params(Some(&pane_id), &[path.as_str()]),
    )
    .unwrap();
    let (ws_idx, pane) = app.parse_pane_id(&pane_id).unwrap();
    let terminal_id = app.state.workspaces[ws_idx]
        .terminal_id(pane)
        .unwrap()
        .clone();
    app.state.terminals.get_mut(&terminal_id).unwrap().container = Some(ContainerLaunch {
        reference: ContainerRef::Name("scratch".into()),
        canonical: "scratch".into(),
        id: "cid".into(),
        workdir: None,
        start_dir: None,
        user: None,
        shell: ContainerShell::BashOrSh,
        ssh: None,
    });
    let mut runtime = TerminalRuntime::test_with_scrollback_bytes(20, 5, 1000, b"");
    runtime.test_set_in_container(true);
    app.state.insert_test_runtime(pane, runtime);

    let response = read(&mut app, DownloadOrigin::Api, &started.download_id, 0);
    let again = read(&mut app, DownloadOrigin::Api, &started.download_id, 0);

    assert_eq!(error_code(&response), "stale_pane_target");
    assert_eq!(error_code(&again), "download_cancelled");
}

#[tokio::test]
async fn a_client_that_disconnects_loses_its_downloads() {
    let mut app = test_app();
    let dir = TempDir::new("app-download-owner-gone");
    std::fs::write(dir.join("f"), b"x").unwrap();
    let path = dir.join("f").to_str().unwrap().to_owned();
    let started = begin(
        &mut app,
        DownloadOrigin::ShellClient(9),
        begin_params(None, &[path.as_str()]),
    )
    .unwrap();

    app.download_sessions.abort_owner(UploadOwner::Client(9));
    let response = read(
        &mut app,
        DownloadOrigin::ShellClient(9),
        &started.download_id,
        0,
    );

    assert_eq!(error_code(&response), "download_cancelled");
}

#[tokio::test]
async fn other_methods_are_left_to_the_caller() {
    let mut app = test_app();
    let (tx, _rx) = mpsc::channel();

    let handled = app.handle_deferred_pane_download_api_request(
        Request {
            id: "r".into(),
            method: Method::Ping(crate::api::schema::PingParams::default()),
        },
        tx,
    );

    assert!(!handled);
}
