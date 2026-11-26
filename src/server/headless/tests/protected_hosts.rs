use super::off_host_clipboard_fixture::{OffHostPaneFixture, OffHostShell};
use super::*;

fn protected_pane() -> OffHostPaneFixture {
    let launch =
        crate::ssh_host::SshLaunch::new("noref_prod".into(), None).expect("valid ssh destination");
    OffHostPaneFixture::new(OffHostShell::Ssh(launch), true)
}

fn read_request(pane_id: &str) -> api::schema::Request {
    api::schema::Request {
        id: "read".into(),
        method: api::schema::Method::PaneRead(api::schema::PaneReadParams {
            pane_id: pane_id.into(),
            source: api::schema::ReadSource::Recent,
            lines: None,
            format: api::schema::ReadFormat::Text,
            strip_ansi: true,
            intent: api::schema::ReadIntent::Passive,
        }),
    }
}

fn error_code(response: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(response).expect("json response");
    value["error"]["code"].as_str().map(str::to_owned)
}

#[tokio::test]
async fn the_public_socket_cannot_read_a_protected_pane() {
    let mut fixture = protected_pane();
    let request = read_request(fixture.pane_id());
    let (respond_to, response_rx) = std::sync::mpsc::channel();

    fixture
        .server
        .handle_api_request_with_shutdown_check(api::ApiRequestMessage {
            request,
            respond_to,
            response_write_complete: None,
        });

    let response = response_rx.recv().expect("response");
    assert_eq!(
        error_code(response.as_bytes()).as_deref(),
        Some("protected_host")
    );
    fixture.finish();
}

/// The client endpoint hands its requests to the same dispatch with
/// `client_local` set; the protection sits before it, on the public path only.
#[tokio::test]
async fn the_users_own_client_still_reads_a_protected_pane() {
    let mut fixture = protected_pane();
    let request = read_request(fixture.pane_id());
    let (respond_to, response_rx) = std::sync::mpsc::channel();

    fixture.server.handle_api_request_with_shutdown_check_inner(
        api::ApiRequestMessage {
            request,
            respond_to,
            response_write_complete: None,
        },
        false,
        true,
    );

    let response = response_rx.recv().expect("response");
    assert_eq!(error_code(response.as_bytes()), None, "{response}");
    fixture.finish();
}
