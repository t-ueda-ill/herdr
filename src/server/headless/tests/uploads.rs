use super::*;
use crate::protocol::endpoint::{EndpointUploadReady, UPLOAD_READY_KIND, UPLOAD_RESPONSE_KIND};
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
async fn deliver_upload_response(server: &mut HeadlessServer) {
    let event = tokio::time::timeout(Duration::from_secs(10), server.server_event_rx.recv())
        .await
        .expect("an upload response should arrive")
        .expect("server event channel open");
    assert!(matches!(
        event,
        ServerEvent::ClientUploadResponseReady { .. }
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

    server.handle_server_event(ServerEvent::ClientUploadRequest {
        client_id: 6,
        request: begin(dest.str()),
    });
    deliver_upload_response(&mut server).await;

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

    server.handle_server_event(ServerEvent::ClientUploadRequest {
        client_id: 7,
        request: begin("/tmp"),
    });
    deliver_upload_response(&mut server).await;

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
