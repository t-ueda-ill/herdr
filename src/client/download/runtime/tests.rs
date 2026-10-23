use std::sync::{mpsc, Arc, Mutex};

use base64::Engine as _;

use super::*;
use crate::api::schema::{
    ErrorBody, ErrorResponse, PaneDownloadChunk, PaneDownloadRecord, PaneDownloadStarted,
    PaneLocation, PaneLocationKind, PaneUploadEntryHeader, PaneUploadEntryKind, Request,
    ResponseResult, SuccessResponse,
};
use crate::client::endpoint::{EndpointNegotiation, EndpointTransport};
use crate::upload::test_support::TempDir;

#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Vec<ClientMessage>>>);

impl EndpointTransport for Recorder {
    fn send(&mut self, message: &ClientMessage) -> std::io::Result<()> {
        self.0.lock().unwrap().push(message.clone());
        Ok(())
    }
}

impl Recorder {
    /// Download requests sent since the last call, as (id, method name).
    fn requests(&self) -> Vec<(String, String)> {
        std::mem::take(&mut *self.0.lock().unwrap())
            .into_iter()
            .filter_map(|message| match message {
                ClientMessage::EndpointControl { kind, data }
                    if kind == crate::protocol::endpoint::DOWNLOAD_REQUEST_KIND =>
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

fn host() -> PaneLocation {
    PaneLocation {
        kind: PaneLocationKind::Host,
        ssh_host: None,
        container: None,
    }
}

fn success(id: &str, result: ResponseResult) -> String {
    serde_json::to_string(&SuccessResponse {
        id: id.into(),
        result,
    })
    .unwrap()
}

fn error(id: &str, code: &str, message: &str) -> String {
    serde_json::to_string(&ErrorResponse {
        id: id.into(),
        error: ErrorBody {
            code: code.into(),
            message: message.into(),
        },
    })
    .unwrap()
}

fn started() -> ResponseResult {
    ResponseResult::PaneDownloadStarted(PaneDownloadStarted {
        download_id: "dl-1".into(),
        location: host(),
        names: vec!["a.txt".into()],
        entries: 1,
        bytes: 3,
        chunk_bytes: 128 * 1024,
        window: 2,
    })
}

fn file_record(path: &str, body: &[u8]) -> PaneDownloadRecord {
    PaneDownloadRecord {
        path: path.into(),
        entry: Some(PaneUploadEntryHeader {
            kind: PaneUploadEntryKind::File,
            mode: 0o644,
            mtime: Some(1_700_000_000),
            size: Some(body.len() as u64),
            link_target: None,
        }),
        offset: 0,
        data: base64::engine::general_purpose::STANDARD.encode(body),
        last: true,
    }
}

fn chunk(seq: u64, records: Vec<PaneDownloadRecord>, done: bool, warnings: &[&str]) -> String {
    success(
        &format!("download:1:r{seq}"),
        ResponseResult::PaneDownloadChunk(PaneDownloadChunk {
            seq,
            records,
            done,
            sent_entries: 1,
            sent_bytes: 3,
            warnings: warnings.iter().map(|text| (*text).to_owned()).collect(),
        }),
    )
}

struct Harness {
    state: ClientState,
    endpoints: EndpointRegistry,
    sent: Recorder,
    events: mpsc::Receiver<WorkerEvent>,
    emit: mpsc::Sender<WorkerEvent>,
    tab: u64,
}

impl Harness {
    fn new() -> Self {
        let sent = Recorder::default();
        let endpoints = EndpointRegistry::new(sent.clone(), 1, EndpointNegotiation::default());
        let (emit, events) = mpsc::channel();
        let mut state = ClientState::test_new();
        let tab = state
            .shell
            .as_mut()
            .unwrap()
            .push_test_investigation("/srv");
        Self {
            state,
            endpoints,
            sent,
            events,
            emit,
            tab,
        }
    }

    fn start(&mut self, destination: &std::path::Path) {
        let emit = self.emit.clone();
        let effects = self.state.downloads.start(
            DownloadRequest {
                endpoint_id: ClientEndpointId::Local,
                generation: 1,
                tab: self.tab,
                pane_id: None,
                location: host(),
                paths: vec!["/srv/a.txt".into()],
                max_bytes: 128 * 1024,
            },
            Destination {
                input: destination.display().to_string(),
                default_dir: String::new(),
                home: None,
                syntax: super::super::destination::PathSyntax::Posix,
            },
            move |event| emit.send(event).is_ok(),
        );
        self.apply(effects);
    }

    fn apply(&mut self, effects: Vec<DownloadEffect>) {
        apply_download_effects(
            &mut self.state,
            &mut self.endpoints,
            effects,
            Instant::now(),
        );
    }

    /// Feeds the next worker event through the runtime and executor.
    fn pump(&mut self) -> WorkerEvent {
        let event = self
            .events
            .recv_timeout(Duration::from_secs(5))
            .expect("worker event");
        let effects = self
            .state
            .downloads
            .worker_event(event.clone(), Instant::now());
        self.apply(effects);
        event
    }

    fn answer(&mut self, data: &str) {
        let effects = self
            .state
            .downloads
            .response(&ClientEndpointId::Local, 1, data.to_owned());
        self.apply(effects);
    }

    fn notice(&self) -> Option<(String, String)> {
        self.state
            .shell
            .as_ref()
            .unwrap()
            .endpoint_notice_for_test()
            .map(|(title, body)| (title.to_owned(), body.to_owned()))
    }
}

fn stages(dir: &std::path::Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| name.starts_with(".herdr-upload-"))
        .collect()
}

#[test]
fn begin_reads_and_the_done_answer_publish_the_file_and_say_so() {
    let dir = TempDir::new("dl-runtime-flow");
    let mut harness = Harness::new();
    harness.start(dir.path());

    assert!(matches!(harness.pump(), WorkerEvent::Prepared { .. }));
    assert_eq!(
        harness.sent.requests(),
        [(
            "download:1:begin".to_owned(),
            "pane.download.begin".to_owned()
        )]
    );
    harness.answer(&success("download:1:begin", started()));
    assert_eq!(harness.pump(), WorkerEvent::Opened { job: 1 });
    assert_eq!(
        harness.sent.requests(),
        [
            ("download:1:r0".to_owned(), "pane.download.read".to_owned()),
            ("download:1:r1".to_owned(), "pane.download.read".to_owned())
        ]
    );
    refresh_download_progress(&mut harness.state, Instant::now(), true);
    let card = harness
        .state
        .shell
        .as_ref()
        .unwrap()
        .upload_progress_for_test()
        .cloned();
    assert_eq!(card.unwrap().title, "Downloading");

    harness.answer(&chunk(0, vec![file_record("a.txt", b"hi!")], false, &[]));
    assert!(matches!(
        harness.pump(),
        WorkerEvent::Applied { seq: 0, .. }
    ));
    assert_eq!(
        harness.sent.requests(),
        [("download:1:r2".to_owned(), "pane.download.read".to_owned())]
    );
    harness.answer(&chunk(1, Vec::new(), true, &[]));
    assert!(matches!(
        harness.pump(),
        WorkerEvent::Applied { seq: 1, .. }
    ));
    assert!(matches!(harness.pump(), WorkerEvent::Published { .. }));

    assert_eq!(std::fs::read(dir.join("a.txt")).unwrap(), b"hi!");
    assert!(stages(dir.path()).is_empty());
    assert!(!harness.state.downloads.busy());
    let (title, body) = harness.notice().expect("a notice");
    assert_eq!(title, "Downloaded");
    assert!(body.starts_with("1 item to "), "{body}");
    assert_eq!(
        harness
            .state
            .shell
            .as_ref()
            .unwrap()
            .workbench_notice_for_test(harness.tab)
            .unwrap(),
        format!(
            "downloaded 1 item to {}",
            crate::platform::file_path_display(&std::fs::canonicalize(dir.path()).unwrap())
        )
    );
    assert!(harness
        .state
        .shell
        .as_ref()
        .unwrap()
        .upload_progress_for_test()
        .is_none());
}

#[test]
fn a_failed_read_leaves_no_stage_and_a_session_ending_error_sends_no_cancel() {
    let dir = TempDir::new("dl-runtime-error");
    let mut harness = Harness::new();
    harness.start(dir.path());
    harness.pump();
    harness.sent.requests();
    harness.answer(&success("download:1:begin", started()));
    harness.pump();
    assert_eq!(stages(dir.path()).len(), 1);
    harness.sent.requests();

    harness.answer(&error(
        "download:1:r0",
        "download_source_changed",
        "a.txt changed while downloading",
    ));

    assert!(harness.sent.requests().is_empty());
    assert!(!harness.state.downloads.busy());
    let (title, body) = harness.notice().expect("a notice");
    assert_eq!(title, "Download failed");
    assert_eq!(body, "a.txt changed while downloading. Nothing was saved.");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !stages(dir.path()).is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(stages(dir.path()).is_empty(), "the stage is removed");
}

#[test]
fn a_read_error_that_leaves_the_session_alive_cancels_it_and_removes_the_stage() {
    let dir = TempDir::new("dl-runtime-busy");
    let mut harness = Harness::new();
    harness.start(dir.path());
    harness.pump();
    harness.answer(&success("download:1:begin", started()));
    harness.pump();
    assert_eq!(stages(dir.path()).len(), 1);
    harness.sent.requests();

    harness.answer(&error(
        "download:1:r0",
        "download_busy",
        "read 0 is running",
    ));

    assert_eq!(
        harness.sent.requests(),
        [(
            "download:1:cancel".to_owned(),
            "pane.download.cancel".to_owned()
        )]
    );
    assert!(!harness.state.downloads.busy());
    let deadline = Instant::now() + Duration::from_secs(5);
    while !stages(dir.path()).is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(stages(dir.path()).is_empty(), "the stage is removed");
}

#[test]
fn a_write_that_fails_cancels_the_session_and_removes_the_stage() {
    let dir = TempDir::new("dl-runtime-cancel");
    let mut harness = Harness::new();
    harness.start(dir.path());
    harness.pump();
    harness.answer(&success("download:1:begin", started()));
    harness.pump();
    harness.sent.requests();

    // A record for a name the server never declared.
    harness.answer(&chunk(0, vec![file_record("zzz", b"x")], false, &[]));
    assert!(matches!(harness.pump(), WorkerEvent::Failed { .. }));

    assert_eq!(
        harness.sent.requests(),
        [(
            "download:1:cancel".to_owned(),
            "pane.download.cancel".to_owned()
        )]
    );
    assert!(!harness.state.downloads.busy());
    assert!(stages(dir.path()).is_empty());
}

#[test]
fn clicking_the_card_cancels_through_the_download_runtime() {
    let dir = TempDir::new("dl-runtime-click");
    let mut harness = Harness::new();
    harness.start(dir.path());
    harness.pump();
    harness.answer(&success("download:1:begin", started()));
    harness.pump();
    harness.sent.requests();
    refresh_download_progress(&mut harness.state, Instant::now(), true);
    let job = harness
        .state
        .shell
        .as_ref()
        .unwrap()
        .upload_progress_for_test()
        .map(|card| card.job)
        .expect("the card");

    let effects = harness.state.downloads.cancel(job);
    harness.apply(effects);

    assert_eq!(
        harness.sent.requests(),
        [(
            "download:1:cancel".to_owned(),
            "pane.download.cancel".to_owned()
        )]
    );
    assert!(harness
        .state
        .shell
        .as_ref()
        .unwrap()
        .upload_progress_for_test()
        .is_none());
    assert_eq!(harness.notice().unwrap().0, "Download cancelled");
}

#[test]
fn a_second_download_is_announced_too() {
    let first = TempDir::new("dl-runtime-first");
    let second = TempDir::new("dl-runtime-second");
    let mut harness = Harness::new();
    for (round, dir) in [&first, &second].into_iter().enumerate() {
        harness.start(dir.path());
        harness.pump();
        // The request id carries the job number.
        let job = round as u64 + 1;
        harness.answer(&success(&format!("download:{job}:begin"), started()));
        harness.pump();
        harness.answer(&success(
            &format!("download:{job}:r0"),
            ResponseResult::PaneDownloadChunk(PaneDownloadChunk {
                seq: 0,
                records: vec![file_record("a.txt", b"hi!")],
                done: true,
                sent_entries: 1,
                sent_bytes: 3,
                warnings: Vec::new(),
            }),
        ));
        harness.pump();
        harness.pump();
        let (title, body) = harness.notice().expect("a notice");
        assert_eq!(title, "Downloaded");
        assert!(
            body.contains(dir.path().file_name().unwrap().to_str().unwrap()),
            "{body}"
        );
        assert!(std::fs::read(dir.join("a.txt")).is_ok());
    }
}

#[test]
fn a_name_with_escape_characters_cannot_reach_the_notice() {
    let dir = TempDir::new("dl-runtime-escape");
    let mut harness = Harness::new();
    harness.start(dir.path());
    harness.pump();
    harness.answer(&success("download:1:begin", started()));
    harness.pump();

    harness.answer(&chunk(
        0,
        vec![file_record("a.txt", b"x")],
        true,
        &["skipped \u{1b}]0;owned\u{7}"],
    ));
    harness.pump();
    harness.pump();

    let (_, body) = harness.notice().expect("a notice");
    assert!(!body.chars().any(char::is_control), "{body:?}");
    assert!(body.contains("\\u{1b}"), "{body}");
}

#[test]
fn a_download_that_cannot_prepare_its_folder_never_sends_begin() {
    let dir = TempDir::new("dl-runtime-nofolder");
    let mut harness = Harness::new();
    harness.start(&dir.join("typo"));

    harness.pump();

    assert!(harness.sent.requests().is_empty());
    assert!(!harness.state.downloads.busy());
    assert!(harness.notice().unwrap().1.starts_with("folder not found"));
}

#[test]
fn the_upload_refresh_leaves_a_downloads_card_alone() {
    let dir = TempDir::new("dl-runtime-card");
    let mut harness = Harness::new();
    harness.start(dir.path());
    harness.pump();
    refresh_download_progress(&mut harness.state, Instant::now(), true);

    crate::client::upload::runtime::refresh_upload_progress(
        &mut harness.state,
        Instant::now(),
        true,
    );

    let card = harness
        .state
        .shell
        .as_ref()
        .unwrap()
        .upload_progress_for_test()
        .cloned();
    assert_eq!(
        card.map(|card| card.title),
        Some("Preparing download".into())
    );
}
