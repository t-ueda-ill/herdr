use super::*;
use crate::protocol::endpoint::{
    EndpointUploadReady, DOWNLOAD_RESPONSE_KIND, UPLOAD_READY_KIND, UPLOAD_RESPONSE_KIND,
};
use crate::server::client_transport::TransferLane;
use crate::upload::test_support::TempDir;

fn connect_terminal_client(
    server: &mut HeadlessServer,
    client_id: u64,
) -> std::sync::mpsc::Receiver<Vec<u8>> {
    let (writer, control_rx, _render_rx) = test_client_writer();
    server.handle_server_event(ServerEvent::ClientConnected {
        client_id,
        cols: 80,
        rows: 24,
        cell_width_px: 0,
        cell_height_px: 0,
        pixel_mouse: false,
        writer,
    });
    control_rx
}

fn connect_shell_client(
    server: &mut HeadlessServer,
    client_id: u64,
) -> std::sync::mpsc::Receiver<Vec<u8>> {
    let (writer, control_rx, _render_rx) = test_client_writer();
    server.handle_server_event(ServerEvent::ClientShellConnected {
        client_id,
        surface_cols: 80,
        surface_rows: 24,
        cell_width_px: 0,
        cell_height_px: 0,
        pixel_mouse: false,
        direct_graphics: false,
        endpoint_keybindings: false,
        mouse_capture: false,
        surface_active: true,
        surface_reuse: false,
        surface_delta: false,
        surface_scroll: false,
        writer,
    });
    control_rx
}

/// The data of the first endpoint control of `kind` the client was sent.
fn sent_control(control_rx: &std::sync::mpsc::Receiver<Vec<u8>>, kind: &str) -> Option<String> {
    // The test writer forwards from its own thread, so messages arrive late.
    while let Ok(bytes) = control_rx.recv_timeout(Duration::from_secs(5)) {
        if let ServerMessage::EndpointControl { kind: sent, data } = read_server_message(bytes) {
            if sent == kind {
                return Some(data);
            }
        }
    }
    None
}

/// Feeds pump answers back into the server until one reaches the client.
async fn deliver_transfer_response(server: &mut HeadlessServer) {
    let event = tokio::time::timeout(Duration::from_secs(10), server.server_event_rx.recv())
        .await
        .expect("a transfer response should arrive")
        .expect("server event channel open");
    assert!(matches!(
        event,
        ServerEvent::ClientTransferResponseReady { .. }
    ));
    server.handle_server_event(event);
}

fn begin(destination: &str) -> Box<api::schema::Request> {
    Box::new(api::schema::Request {
        id: "u1".into(),
        method: api::schema::Method::PaneUploadBegin(api::schema::PaneUploadBeginParams {
            pane_id: None,
            terminal_id: None,
            destination: Some(destination.into()),
            names: vec!["a".into()],
            entries: 1,
            bytes: 0,
        }),
    })
}

#[tokio::test]
async fn a_terminal_client_probe_is_answered_with_the_chunk_size_and_window() {
    let mut server = test_headless_server();
    let control_rx = connect_terminal_client(&mut server, 5);

    server.handle_server_event(ServerEvent::ClientUploadProbe { client_id: 5 });

    let data = sent_control(&control_rx, UPLOAD_READY_KIND).expect("ready control");
    let ready: EndpointUploadReady = serde_json::from_str(&data).unwrap();
    assert_eq!(ready.chunk_bytes, crate::upload::UPLOAD_CHUNK_BYTES as u64);
    assert_eq!(ready.window, crate::upload::UPLOAD_WINDOW);
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn a_shell_client_upload_is_answered_on_its_own_connection_and_ends_with_it() {
    let mut server = test_headless_server();
    let control_rx = connect_shell_client(&mut server, 6);
    let dest = TempDir::new("headless-upload");

    server.handle_server_event(ServerEvent::ClientTransferRequest {
        client_id: 6,
        lane: TransferLane::Upload,
        request: begin(dest.str()),
    });
    deliver_transfer_response(&mut server).await;

    let data = sent_control(&control_rx, UPLOAD_RESPONSE_KIND).expect("response control");
    let success: api::schema::SuccessResponse = serde_json::from_str(&data).unwrap();
    assert_eq!(success.id, "u1");
    let api::schema::ResponseResult::PaneUploadStarted(started) = success.result else {
        panic!("{data}")
    };
    server.handle_server_event(ServerEvent::ClientDisconnected { client_id: 6 });
    let after = server.app.upload_sessions.get(
        &started.upload_id,
        crate::upload::registry::UploadOwner::Client(6),
    );
    assert_eq!(after.err().map(|(code, _)| code), Some("upload_cancelled"));
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn an_unattached_terminal_client_cannot_upload() {
    let mut server = test_headless_server();
    let control_rx = connect_terminal_client(&mut server, 7);

    server.handle_server_event(ServerEvent::ClientTransferRequest {
        client_id: 7,
        lane: TransferLane::Upload,
        request: begin("/tmp"),
    });
    deliver_transfer_response(&mut server).await;

    let data = sent_control(&control_rx, UPLOAD_RESPONSE_KIND).expect("response control");
    let error: api::schema::ErrorResponse = serde_json::from_str(&data).unwrap();
    assert_eq!(error.error.code, "unsupported_endpoint_command");
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn api_socket_upload_requests_take_the_deferred_path() {
    let mut server = test_headless_server();
    let dest = TempDir::new("headless-api-upload");
    let (respond_to, response_rx) = std::sync::mpsc::channel();

    server.handle_api_request_with_shutdown_check(api::ApiRequestMessage {
        request: *begin(dest.str()),
        respond_to,
        response_write_complete: None,
    });

    let response = response_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the begin should be answered");
    assert!(response.contains("pane_upload_started"), "{response}");
    shutdown_test_runtimes(&mut server);
}

fn download_begin(path: &str) -> Box<api::schema::Request> {
    Box::new(api::schema::Request {
        id: "d1".into(),
        method: api::schema::Method::PaneDownloadBegin(api::schema::PaneDownloadBeginParams {
            pane_id: None,
            paths: vec![path.into()],
            max_bytes: None,
        }),
    })
}

fn one_file() -> (TempDir, String) {
    let dir = TempDir::new("headless-download");
    std::fs::write(dir.join("f"), b"x").unwrap();
    let path = dir.join("f").to_str().unwrap().to_owned();
    (dir, path)
}

#[tokio::test]
async fn a_shell_client_download_is_answered_on_its_own_connection_and_ends_with_it() {
    let mut server = test_headless_server();
    let control_rx = connect_shell_client(&mut server, 6);
    let (_dir, path) = one_file();

    server.handle_server_event(ServerEvent::ClientTransferRequest {
        client_id: 6,
        lane: TransferLane::Download,
        request: download_begin(&path),
    });
    deliver_transfer_response(&mut server).await;

    let data = sent_control(&control_rx, DOWNLOAD_RESPONSE_KIND).expect("response control");
    let success: api::schema::SuccessResponse = serde_json::from_str(&data).unwrap();
    assert_eq!(success.id, "d1");
    let api::schema::ResponseResult::PaneDownloadStarted(started) = success.result else {
        panic!("{data}")
    };
    server.handle_server_event(ServerEvent::ClientDisconnected { client_id: 6 });
    let after = server.app.download_sessions.get(
        &started.download_id,
        crate::upload::registry::UploadOwner::Client(6),
    );
    assert_eq!(
        after.err().map(|(code, _)| code),
        Some("download_cancelled")
    );
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn a_terminal_client_cannot_download() {
    let mut server = test_headless_server();
    let control_rx = connect_terminal_client(&mut server, 7);

    server.handle_server_event(ServerEvent::ClientTransferRequest {
        client_id: 7,
        lane: TransferLane::Download,
        request: download_begin("/tmp"),
    });
    deliver_transfer_response(&mut server).await;

    let data = sent_control(&control_rx, DOWNLOAD_RESPONSE_KIND).expect("response control");
    let error: api::schema::ErrorResponse = serde_json::from_str(&data).unwrap();
    assert_eq!(error.error.code, "unsupported_endpoint_command");
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn a_download_requested_while_the_server_hands_off_is_cancelled() {
    let mut server = test_headless_server();
    let control_rx = connect_shell_client(&mut server, 6);
    let (_dir, path) = one_file();
    server.handoff_in_progress = true;

    server.handle_server_event(ServerEvent::ClientTransferRequest {
        client_id: 6,
        lane: TransferLane::Download,
        request: download_begin(&path),
    });
    deliver_transfer_response(&mut server).await;

    let data = sent_control(&control_rx, DOWNLOAD_RESPONSE_KIND).expect("response control");
    let error: api::schema::ErrorResponse = serde_json::from_str(&data).unwrap();
    assert_eq!(error.error.code, "download_cancelled");
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn aborting_transfers_stops_downloads_and_uploads() {
    let mut server = test_headless_server();
    let control_rx = connect_shell_client(&mut server, 6);
    let (_dir, path) = one_file();
    let dest = TempDir::new("headless-abort-upload");
    let owner = crate::upload::registry::UploadOwner::Client(6);
    server.handle_server_event(ServerEvent::ClientTransferRequest {
        client_id: 6,
        lane: TransferLane::Download,
        request: download_begin(&path),
    });
    deliver_transfer_response(&mut server).await;
    let data = sent_control(&control_rx, DOWNLOAD_RESPONSE_KIND).expect("download response");
    let success: api::schema::SuccessResponse = serde_json::from_str(&data).unwrap();
    let api::schema::ResponseResult::PaneDownloadStarted(download) = success.result else {
        panic!("{data}")
    };
    server.handle_server_event(ServerEvent::ClientTransferRequest {
        client_id: 6,
        lane: TransferLane::Upload,
        request: begin(dest.str()),
    });
    deliver_transfer_response(&mut server).await;
    let data = sent_control(&control_rx, UPLOAD_RESPONSE_KIND).expect("upload response");
    let success: api::schema::SuccessResponse = serde_json::from_str(&data).unwrap();
    let api::schema::ResponseResult::PaneUploadStarted(upload) = success.result else {
        panic!("{data}")
    };
    assert!(server
        .app
        .download_sessions
        .get(&download.download_id, owner)
        .is_ok());
    assert!(server
        .app
        .upload_sessions
        .get(&upload.upload_id, owner)
        .is_ok());

    server.abort_transfers(Duration::ZERO);

    let download_after = server
        .app
        .download_sessions
        .get(&download.download_id, owner);
    let upload_after = server.app.upload_sessions.get(&upload.upload_id, owner);
    assert_eq!(
        download_after.err().map(|(code, _)| code),
        Some("download_cancelled")
    );
    assert_eq!(
        upload_after.err().map(|(code, _)| code),
        Some("upload_cancelled")
    );
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn api_socket_download_requests_take_the_deferred_path() {
    let mut server = test_headless_server();
    let (_dir, path) = one_file();
    let (respond_to, response_rx) = std::sync::mpsc::channel();

    server.handle_api_request_with_shutdown_check(api::ApiRequestMessage {
        request: *download_begin(&path),
        respond_to,
        response_write_complete: None,
    });

    let response = response_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the begin should be answered");
    assert!(response.contains("pane_download_started"), "{response}");
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn a_download_over_the_command_lane_belongs_to_its_client_and_ends_with_it() {
    let mut server = test_headless_server();
    let _control_rx = connect_shell_client(&mut server, 6);
    let (_dir, path) = one_file();
    let boot_id = server.client_shell_boot_id.clone();

    server.handle_server_event(ServerEvent::ClientShellEndpointRequest {
        client_id: 6,
        boot_id,
        request: download_begin(&path),
    });
    let event = tokio::time::timeout(Duration::from_secs(10), server.server_event_rx.recv())
        .await
        .expect("the lane should answer")
        .expect("server event channel open");
    let ServerEvent::ClientShellEndpointResponseChunkReady { data, .. } = &event else {
        panic!("expected the lane's response");
    };
    let success: api::schema::SuccessResponse = serde_json::from_slice(data).unwrap();
    let api::schema::ResponseResult::PaneDownloadStarted(started) = success.result else {
        panic!("expected a started download")
    };
    server.handle_server_event(event);

    server.handle_server_event(ServerEvent::ClientDisconnected { client_id: 6 });

    let after = server.app.download_sessions.get(
        &started.download_id,
        crate::upload::registry::UploadOwner::Client(6),
    );
    assert_eq!(
        after.err().map(|(code, _)| code),
        Some("download_cancelled")
    );
    shutdown_test_runtimes(&mut server);
}

mod worst_case_frames {
    use super::*;
    use crate::api::schema::{
        PaneDownloadChunk, PaneUploadEntryHeader, PaneUploadEntryKind, ResponseResult,
    };
    use crate::download::frame::{Frame, Limits, Packer};
    use crate::download::worker::fits_one_reply;
    use crate::download::{DOWNLOAD_CHUNK_BYTES, MAX_READ_META_BYTES, MAX_READ_RECORDS};

    fn header(kind: PaneUploadEntryKind, size: Option<u64>) -> PaneUploadEntryHeader {
        PaneUploadEntryHeader {
            kind,
            mode: 0o755,
            mtime: Some(1),
            size,
            link_target: None,
        }
    }

    fn entry(
        rel: &str,
        kind: PaneUploadEntryKind,
        size: Option<u64>,
        target: Option<&str>,
    ) -> Frame {
        Frame::Entry {
            rel: rel.as_bytes().to_vec(),
            header: header(kind, size),
            link_target: target.map(|target| target.as_bytes().to_vec()),
        }
    }

    /// The frame the server writes for `chunk`, as a worker's answer wrapped
    /// the way the real path wraps it: a long request id, the response, and
    /// the endpoint control around it.
    fn framed(chunk: PaneDownloadChunk) -> Result<Vec<u8>, protocol::FramingError> {
        let data = serde_json::to_string(&api::schema::SuccessResponse {
            id: format!("endpoint:boot-0000:7:{}", "r".repeat(128)),
            result: ResponseResult::PaneDownloadChunk(chunk),
        })
        .unwrap();
        HeadlessServer::frame_server_message(&ServerMessage::EndpointControl {
            kind: DOWNLOAD_RESPONSE_KIND.into(),
            data,
        })
    }

    fn fill(frames: Vec<Frame>) -> PaneDownloadChunk {
        let mut packer = Packer::new(vec!["t".into()], Limits::new(DOWNLOAD_CHUNK_BYTES));
        let mut frames = frames.into_iter();
        packer.fill(0, &mut || Ok(frames.next())).unwrap()
    }

    #[test]
    fn an_answer_at_every_budget_at_once_fits_the_reply_limit_and_one_frame() {
        // The top directory, then as many directories as leave room for one
        // file record, their paths all `"` (doubled by JSON) and summing to
        // just under the name budget, then a file that fills the data budget.
        let dirs = MAX_READ_RECORDS - 3;
        let names_left = MAX_READ_META_BYTES - "t".len() - "t/b".len() - 1;
        let (base, extra) = (names_left / dirs, names_left % dirs);
        let mut frames = vec![entry("t", PaneUploadEntryKind::Directory, None, None)];
        frames.extend((0..dirs).map(|i| {
            let len = base + usize::from(i < extra);
            entry(
                &format!("t/{}", "\"".repeat(len - 2)),
                PaneUploadEntryKind::Directory,
                None,
                None,
            )
        }));
        frames.push(entry(
            "t/b",
            PaneUploadEntryKind::File,
            Some(DOWNLOAD_CHUNK_BYTES as u64),
            None,
        ));
        frames.push(Frame::Data(vec![0xff; DOWNLOAD_CHUNK_BYTES]));
        frames.push(Frame::FileEnd(Ok(())));
        frames.push(Frame::End);

        let chunk = fill(frames);

        assert_eq!(chunk.records.len(), MAX_READ_RECORDS - 1);
        let names: usize = chunk.records.iter().map(|record| record.path.len()).sum();
        assert_eq!(names, MAX_READ_META_BYTES - 1);
        assert_eq!(chunk.sent_bytes, DOWNLOAD_CHUNK_BYTES as u64);
        let result = ResponseResult::PaneDownloadChunk(chunk.clone());
        assert!(fits_one_reply(&result));
        let frame = framed(chunk).expect("the answer should fit one frame");
        assert!(frame.len() - 4 <= MAX_FRAME_SIZE);
    }

    #[test]
    fn an_answer_full_of_quote_laden_link_targets_fits_one_frame() {
        let target = "\"".repeat(4_096);
        let mut frames = vec![entry("t", PaneUploadEntryKind::Directory, None, None)];
        frames.extend((0..70).map(|i| {
            entry(
                &format!("t/{i}"),
                PaneUploadEntryKind::Symlink,
                None,
                Some(&target),
            )
        }));
        frames.push(entry(
            "t/big",
            PaneUploadEntryKind::File,
            Some(DOWNLOAD_CHUNK_BYTES as u64),
            None,
        ));
        frames.push(Frame::Data(vec![0xff; DOWNLOAD_CHUNK_BYTES]));
        frames.push(Frame::FileEnd(Ok(())));
        frames.push(Frame::End);

        let chunk = fill(frames);

        assert!(chunk.records.len() > 60);
        assert!(fits_one_reply(&ResponseResult::PaneDownloadChunk(
            chunk.clone()
        )));
        let frame = framed(chunk).expect("the answer should fit one frame");
        assert!(frame.len() - 4 <= MAX_FRAME_SIZE);
    }

    #[test]
    fn the_finishing_answer_with_the_most_control_character_warnings_fits_one_frame() {
        let mut packer = Packer::new(vec!["t".into()], Limits::new(DOWNLOAD_CHUNK_BYTES));
        let mut frames = vec![
            entry("t", PaneUploadEntryKind::Directory, None, None),
            Frame::End,
        ];
        frames.splice(
            1..1,
            (0..2000).map(|_| Frame::WalkWarning("\u{1}".repeat(100))),
        );
        let mut frames = frames.into_iter();
        let first = packer.fill(0, &mut || Ok(frames.next())).unwrap();
        let last = packer.fill(1, &mut || Ok(frames.next())).unwrap();

        assert!(framed(first).is_ok());
        assert!(last.done && !last.warnings.is_empty());
        assert!(fits_one_reply(&ResponseResult::PaneDownloadChunk(
            last.clone()
        )));
        let frame = framed(last).expect("the finishing answer should fit one frame");
        assert!(frame.len() - 4 <= MAX_FRAME_SIZE);
    }
}
