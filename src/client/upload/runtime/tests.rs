// Why allow: the end-to-end cases need Unix scratch trees, so Windows runs
// only the portable ones and leaves the shared harness partly unused.
#![cfg_attr(windows, allow(dead_code, unused_imports))]

use super::*;
use crate::api::schema::{
    PaneLocation, PaneLocationKind, PaneUploadFinished, PaneUploadProgress, PaneUploadStarted,
    Request, ResponseResult, SuccessResponse,
};
use crate::client::endpoint::{EndpointNegotiation, EndpointTransport};
use std::sync::{mpsc, Arc, Mutex};

#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Vec<ClientMessage>>>);

impl EndpointTransport for Recorder {
    fn send(&mut self, message: &ClientMessage) -> std::io::Result<()> {
        self.0.lock().unwrap().push(message.clone());
        Ok(())
    }
}

impl Recorder {
    fn take(&self) -> Vec<ClientMessage> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }

    /// Upload requests sent since the last call, as (id, method name).
    fn requests(&self) -> Vec<(String, String)> {
        self.take()
            .into_iter()
            .filter_map(|message| match message {
                ClientMessage::EndpointControl { kind, data }
                    if kind == crate::protocol::endpoint::UPLOAD_REQUEST_KIND =>
                {
                    let request: Request = serde_json::from_str(&data).unwrap();
                    Some((
                        request.id.clone(),
                        crate::api::api_method_name(&request.method).to_owned(),
                    ))
                }
                _ => None,
            })
            .collect()
    }
}

fn success(id: &str, result: ResponseResult) -> String {
    serde_json::to_string(&SuccessResponse {
        id: id.into(),
        result,
    })
    .unwrap()
}

fn location() -> PaneLocation {
    PaneLocation {
        kind: PaneLocationKind::Container,
        ssh_host: None,
        container: Some("box".into()),
    }
}

struct Harness {
    state: ClientState,
    endpoints: EndpointRegistry,
    sent: Recorder,
    events: mpsc::Receiver<WorkerEvent>,
    emit: mpsc::Sender<WorkerEvent>,
}

impl Harness {
    fn new() -> Self {
        let sent = Recorder::default();
        let endpoints = EndpointRegistry::new(sent.clone(), 1, EndpointNegotiation::default());
        let (emit, events) = mpsc::channel();
        Self {
            state: ClientState::test_new(),
            endpoints,
            sent,
            events,
            emit,
        }
    }

    fn offer(&mut self, text: &str) -> DropStart {
        let emit = self.emit.clone();
        let replay = plain_event(text);
        self.state.uploads.offer_drop(
            DropCandidate {
                text: text.to_owned(),
                target: UploadTarget::Pane("p1".into()),
                endpoint_id: ClientEndpointId::Local,
                generation: 1,
                chunk_cap: crate::upload::UPLOAD_CHUNK_BYTES,
                can_confirm: true,
            },
            move || replay,
            move |event| emit.send(event).is_ok(),
            Instant::now(),
        )
    }

    /// Feeds the next worker event through the runtime and executor.
    fn pump(&mut self) -> WorkerEvent {
        let event = self
            .events
            .recv_timeout(Duration::from_secs(5))
            .expect("worker event");
        let effects = self
            .state
            .uploads
            .worker_event(event.clone(), Instant::now());
        apply_upload_effects(
            &mut self.state,
            &mut self.endpoints,
            effects,
            Instant::now(),
        );
        event
    }

    fn answer(&mut self, id: &str, result: ResponseResult) {
        let effects = self.state.uploads.response(
            &ClientEndpointId::Local,
            1,
            &success(id, result),
            Instant::now(),
        );
        apply_upload_effects(
            &mut self.state,
            &mut self.endpoints,
            effects,
            Instant::now(),
        );
    }
}

#[cfg(unix)]
fn plain_event(text: &str) -> ClientLoopEvent {
    ClientLoopEvent::StdinPlain(text.as_bytes().to_vec())
}

#[cfg(windows)]
fn plain_event(text: &str) -> ClientLoopEvent {
    ClientLoopEvent::StdinEventsPlain(vec![crate::protocol::ClientInputEvent::Paste {
        text: text.to_owned(),
    }])
}

#[cfg(unix)]
#[test]
fn a_dropped_file_is_uploaded_and_its_remote_path_pasted_into_the_dropping_pane() {
    let dir = crate::upload::test_support::TempDir::new("runtime-drop");
    let file = dir.join("a b.txt");
    std::fs::write(&file, b"hello").unwrap();
    let mut harness = Harness::new();
    let text = format!("'{}' ", file.display());

    assert_eq!(harness.offer(&text), DropStart::Pending);
    assert!(harness.state.uploads.holding());
    harness
        .state
        .uploads
        .hold(ClientLoopEvent::StdinInput(b"x".to_vec()));
    assert!(matches!(
        harness.pump(),
        WorkerEvent::Checked { is_drop: true, .. }
    ));
    assert!(!harness.state.uploads.holding());
    assert!(matches!(
        harness.state.uploads.next_replay(),
        Some(ClientLoopEvent::StdinInput(data)) if data == b"x"
    ));
    assert!(harness.state.uploads.next_replay().is_none());

    harness.pump();
    assert_eq!(
        harness.sent.requests(),
        [("upload:1:begin".to_owned(), "pane.upload.begin".to_owned())]
    );
    assert!(harness
        .state
        .shell
        .as_ref()
        .unwrap()
        .upload_progress_for_test()
        .is_some());

    harness.answer(
        "upload:1:begin",
        ResponseResult::PaneUploadStarted(PaneUploadStarted {
            upload_id: "up-1".into(),
            location: location(),
            destination: "/root".into(),
            conflicts: Vec::new(),
            chunk_bytes: 512 * 1024,
            window: 2,
        }),
    );
    harness.pump();
    assert_eq!(
        harness.sent.requests(),
        [("upload:1:w0".to_owned(), "pane.upload.write".to_owned())]
    );
    assert!(matches!(harness.pump(), WorkerEvent::WritesDone { .. }));
    assert!(harness.sent.requests().is_empty());

    harness.answer(
        "upload:1:w0",
        ResponseResult::PaneUploadProgress(PaneUploadProgress {
            received_entries: 1,
            received_bytes: 5,
        }),
    );
    assert_eq!(
        harness.sent.requests(),
        [(
            "upload:1:finish".to_owned(),
            "pane.upload.finish".to_owned()
        )]
    );

    harness.answer(
        "upload:1:finish",
        ResponseResult::PaneUploadFinished(PaneUploadFinished {
            location: location(),
            destination: "/root".into(),
            paths: vec!["/root/a b.txt".into()],
            warnings: Vec::new(),
        }),
    );
    assert_eq!(
        harness.sent.take(),
        [ClientMessage::ClientShellPaneInput {
            pane_id: "p1".into(),
            events: vec![ClientPaneInputEvent::Paste("'/root/a b.txt' ".into())],
        }]
    );
    let shell = harness.state.shell.as_ref().unwrap();
    assert!(shell.upload_progress_for_test().is_none());
    assert_eq!(
        shell.endpoint_notice_for_test().map(|(title, _)| title),
        Some("Uploaded")
    );
}

#[cfg(unix)]
#[test]
fn a_paste_naming_a_missing_path_is_replayed_as_text_before_held_input() {
    let dir = crate::upload::test_support::TempDir::new("runtime-missing");
    let mut harness = Harness::new();
    let text = format!("{}", dir.join("missing").display());

    assert_eq!(harness.offer(&text), DropStart::Pending);
    harness
        .state
        .uploads
        .hold(ClientLoopEvent::StdinInput(b"k".to_vec()));
    harness.pump();

    assert!(matches!(
        harness.state.uploads.next_replay(),
        Some(ClientLoopEvent::StdinPlain(data)) if data == text.as_bytes()
    ));
    assert!(matches!(
        harness.state.uploads.next_replay(),
        Some(ClientLoopEvent::StdinInput(data)) if data == b"k"
    ));
    assert!(harness.sent.requests().is_empty());
}

#[test]
fn text_that_is_not_absolute_paths_is_never_offered_to_the_worker() {
    let mut harness = Harness::new();

    assert_eq!(harness.offer("hello world"), DropStart::NotADrop);
    assert!(!harness.state.uploads.holding());
}

#[test]
fn a_check_that_takes_too_long_replays_the_paste_as_text() {
    let mut harness = Harness::new();
    let path = if cfg!(windows) { r"C:\x\y" } else { "/x/y" };
    assert_eq!(harness.offer(path), DropStart::Pending);

    let effects = harness
        .state
        .uploads
        .tick(Instant::now() + super::super::pending::DROP_CHECK_TIMEOUT);

    assert!(effects.contains(&UploadEffect::StopWorker { job: 1 }));
    assert!(!harness.state.uploads.holding());
    assert!(harness.state.uploads.next_replay().is_some());
}

#[cfg(unix)]
#[test]
fn a_second_drop_while_one_runs_is_refused() {
    let dir = crate::upload::test_support::TempDir::new("runtime-busy");
    std::fs::write(dir.join("f"), b"").unwrap();
    let mut harness = Harness::new();
    let text = format!("{}", dir.join("f").display());
    harness.offer(&text);
    harness.pump();

    assert_eq!(harness.offer(&text), DropStart::Busy);
}

#[cfg(unix)]
#[test]
fn a_paste_of_missing_paths_while_one_runs_is_plain_text() {
    let dir = crate::upload::test_support::TempDir::new("runtime-busy-missing");
    std::fs::write(dir.join("f"), b"").unwrap();
    let mut harness = Harness::new();
    harness.offer(&format!("{}", dir.join("f").display()));
    harness.pump();

    let offered = harness.offer("/var/www/app/config.yml-that-is-not-here");

    assert_eq!(offered, DropStart::NotADrop);
}

#[test]
fn a_direct_attach_upload_needs_the_probe_answer_from_the_same_connection() {
    let mut runtime = UploadRuntime::new(UploadSettings::from_config(
        &crate::config::Config::default(),
    ));
    assert!(!runtime.direct_ready(&ClientEndpointId::Local, 1));

    runtime.mark_direct_ready(ClientEndpointId::Local, 1);

    assert!(runtime.direct_ready(&ClientEndpointId::Local, 1));
    assert!(!runtime.direct_ready(&ClientEndpointId::Local, 2));
    runtime.endpoint_lost(&ClientEndpointId::Local, 1);
    assert!(!runtime.direct_ready(&ClientEndpointId::Local, 1));
}

#[test]
fn a_direct_attach_paste_is_rebracketed_like_the_drop() {
    let target = UploadTarget::Direct {
        terminal_id: "t".into(),
        bracketed: true,
    };

    assert_eq!(
        paste_message(&target, "/a".into()),
        Some(ClientMessage::Input {
            data: b"\x1b[200~/a\x1b[201~".to_vec()
        })
    );
}

/// The collision of every finish sent since the last call.
fn finish_collisions(sent: &Recorder) -> Vec<crate::api::schema::PaneUploadCollision> {
    sent.take()
        .into_iter()
        .filter_map(|message| match message {
            ClientMessage::EndpointControl { data, .. } => {
                let request: Request = serde_json::from_str(&data).ok()?;
                match request.method {
                    crate::api::schema::Method::PaneUploadFinish(params) => Some(params.collision),
                    _ => None,
                }
            }
            _ => None,
        })
        .collect()
}

#[cfg(unix)]
#[test]
fn a_tree_drop_asks_after_begin_then_uploads_with_the_choice_and_lists_the_folder() {
    let dir = crate::upload::test_support::TempDir::new("runtime-tree-drop");
    let file = dir.join("a.txt");
    std::fs::write(&file, b"hello").unwrap();
    let mut harness = Harness::new();
    let tab = harness
        .state
        .shell
        .as_mut()
        .unwrap()
        .push_test_investigation("/srv");
    let emit = harness.emit.clone();

    let started = harness.state.uploads.offer_workbench_drop(
        WorkbenchDrop {
            paths: vec![file],
            target: WorkbenchTarget {
                tab,
                destination: "/srv".into(),
                pane_id: Some("p1".into()),
                location: location(),
            },
            endpoint_id: ClientEndpointId::Local,
            generation: 1,
            chunk_cap: crate::upload::UPLOAD_CHUNK_BYTES,
        },
        move |event| emit.send(event).is_ok(),
    );
    assert_eq!(started, DropStart::Pending);
    assert!(!harness.state.uploads.holding(), "tree drops hold no input");
    harness.pump();
    harness.pump();
    assert_eq!(
        harness.sent.requests(),
        [("upload:1:begin".to_owned(), "pane.upload.begin".to_owned())]
    );

    harness.answer(
        "upload:1:begin",
        ResponseResult::PaneUploadStarted(PaneUploadStarted {
            upload_id: "up-1".into(),
            location: location(),
            destination: "/srv".into(),
            conflicts: vec!["a.txt".into()],
            chunk_bytes: 512 * 1024,
            window: 2,
        }),
    );
    let shell = harness.state.shell.as_ref().unwrap();
    assert_eq!(
        shell.workbench_question_for_test(tab),
        Some((1, vec!["a.txt".to_owned()]))
    );
    assert!(
        harness.sent.requests().is_empty(),
        "nothing written before the answer"
    );

    let effects = harness.state.uploads.decision(
        crate::client::shell::ClientUploadDecision::Collision {
            job: 1,
            collision: Some(crate::api::schema::PaneUploadCollision::Replace),
        },
        Instant::now(),
    );
    apply_upload_effects(
        &mut harness.state,
        &mut harness.endpoints,
        effects,
        Instant::now(),
    );
    harness.pump();
    harness.pump();
    harness.sent.take();
    harness.answer(
        "upload:1:w0",
        ResponseResult::PaneUploadProgress(PaneUploadProgress {
            received_entries: 1,
            received_bytes: 5,
        }),
    );
    assert_eq!(
        finish_collisions(&harness.sent),
        [crate::api::schema::PaneUploadCollision::Replace]
    );

    harness.answer(
        "upload:1:finish",
        ResponseResult::PaneUploadFinished(PaneUploadFinished {
            location: location(),
            destination: "/srv".into(),
            paths: vec!["/srv/a.txt".into()],
            warnings: Vec::new(),
        }),
    );
    assert!(harness.sent.take().is_empty(), "nothing is pasted");
    let shell = harness.state.shell.as_ref().unwrap();
    assert_eq!(
        shell.workbench_refresh_for_test(tab),
        Some(&("/srv".to_owned(), Some("/srv/a.txt".to_owned())))
    );
    assert_eq!(
        shell.workbench_notice_for_test(tab),
        Some("uploaded 1 item to /srv")
    );
    assert!(!harness.state.uploads.uploads.busy());
}

#[test]
fn a_tree_question_with_no_tab_to_show_it_cancels_the_session() {
    let mut harness = Harness::new();
    let mut uploads = std::mem::take(&mut harness.state.uploads.uploads);
    let job = uploads.start(DropRequest {
        endpoint_id: ClientEndpointId::Local,
        generation: 1,
        target: UploadTarget::Workbench(WorkbenchTarget {
            tab: 999,
            destination: "/srv".into(),
            pane_id: None,
            location: location(),
        }),
        trailing_space: false,
        chunk_cap: crate::upload::UPLOAD_CHUNK_BYTES,
        can_confirm: true,
    });
    uploads.checked(job, true);
    let limits = harness.state.uploads.settings.limits;
    uploads.scanned(
        job,
        Ok(super::super::transfer::ScanSummary {
            names: vec!["a".into()],
            entries: 1,
            bytes: 1,
            ..Default::default()
        }),
        &limits,
        Instant::now(),
    );
    harness.state.uploads.uploads = uploads;
    harness.sent.take();

    harness.answer(
        "upload:1:begin",
        ResponseResult::PaneUploadStarted(PaneUploadStarted {
            upload_id: "up-1".into(),
            location: location(),
            destination: "/srv".into(),
            conflicts: Vec::new(),
            chunk_bytes: 512 * 1024,
            window: 2,
        }),
    );

    assert_eq!(
        harness.sent.requests(),
        [(
            "upload:1:cancel".to_owned(),
            "pane.upload.cancel".to_owned()
        )]
    );
    assert!(!harness.state.uploads.uploads.busy());
}
