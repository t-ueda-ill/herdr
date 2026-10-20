//! Endpoint-control transport for `pane.upload.*`. Why not the client-shell
//! command lane: it holds one request in flight per client, so a long upload
//! would stall every other shell command, and direct attach clients cannot use
//! it at all. Responses come back from session workers through one pump thread.

use super::*;
use crate::app::{UploadOrigin, UploadResponder};
use crate::protocol::endpoint::{EndpointUploadReady, UPLOAD_READY_KIND, UPLOAD_RESPONSE_KIND};

/// Starts the thread that turns worker answers into server events.
/// Why a thread: workers and the app thread answer through a std channel that
/// never blocks; only this thread waits on the bounded server event queue.
pub(super) fn spawn_upload_response_pump(
    server_event_tx: mpsc::Sender<ServerEvent>,
) -> std::sync::mpsc::Sender<(u64, String)> {
    let (tx, rx) = std::sync::mpsc::channel::<(u64, String)>();
    let spawned = crate::thread_spawn::spawn_named("herdr-upload-response", move || {
        while let Ok((client_id, data)) = rx.recv() {
            if server_event_tx
                .blocking_send(ServerEvent::ClientUploadResponseReady { client_id, data })
                .is_err()
            {
                break;
            }
        }
    });
    if let Err(error) = spawned {
        warn!(%error, "failed to start the upload response thread; uploads will not answer");
    }
    tx
}

fn error_json(id: String, code: &str, message: impl Into<String>) -> String {
    serde_json::to_string(&api::schema::ErrorResponse {
        id,
        error: api::schema::ErrorBody {
            code: code.into(),
            message: message.into(),
        },
    })
    .unwrap_or_else(|_| "{}".into())
}

impl HeadlessServer {
    fn upload_responder(&self, client_id: u64) -> UploadResponder {
        let responses = self.upload_responses.clone();
        Box::new(move |data| {
            let _ = responses.send((client_id, data));
        })
    }

    fn upload_origin(&self, client_id: u64) -> Option<UploadOrigin> {
        match &self.clients.get(&client_id)?.mode {
            ClientConnectionMode::ClientShell => Some(UploadOrigin::ShellClient(client_id)),
            ClientConnectionMode::TerminalAttach { terminal_id } => {
                Some(UploadOrigin::AttachedClient {
                    client_id,
                    terminal_id: terminal_id.clone(),
                })
            }
            ClientConnectionMode::TerminalPending
            | ClientConnectionMode::TerminalObserve { .. } => None,
        }
    }

    pub(super) fn handle_client_upload_request(
        &mut self,
        client_id: u64,
        request: Box<api::schema::Request>,
    ) -> bool {
        if !self.clients.contains_key(&client_id) {
            return false;
        }
        let respond = self.upload_responder(client_id);
        let Some(origin) = self.upload_origin(client_id) else {
            respond(error_json(
                request.id,
                "unsupported_endpoint_command",
                "uploads need a client shell or an attached terminal",
            ));
            return false;
        };
        if self.handoff_in_progress || self.shutting_down {
            respond(error_json(
                request.id,
                "upload_cancelled",
                "the server is stopping or handing off",
            ));
            return false;
        }
        self.app
            .handle_pane_upload_request(origin, *request, respond);
        false
    }

    pub(super) fn handle_client_upload_request_error(
        &mut self,
        client_id: u64,
        request_id: String,
        code: &'static str,
        message: String,
    ) -> bool {
        if self.clients.contains_key(&client_id) {
            (self.upload_responder(client_id))(error_json(request_id, code, message));
        }
        false
    }

    pub(super) fn handle_client_upload_probe(&mut self, client_id: u64) -> bool {
        let answers = self.clients.get(&client_id).is_some_and(|client| {
            !matches!(client.mode, ClientConnectionMode::TerminalObserve { .. })
        });
        if !answers {
            return false;
        }
        let ready = EndpointUploadReady {
            chunk_bytes: crate::upload::UPLOAD_CHUNK_BYTES as u64,
            window: crate::upload::UPLOAD_WINDOW,
        };
        if let Ok(data) = serde_json::to_string(&ready) {
            self.send_to_client(
                client_id,
                ServerMessage::EndpointControl {
                    kind: UPLOAD_READY_KIND.into(),
                    data,
                },
            );
        }
        false
    }

    pub(super) fn handle_client_upload_response(&mut self, client_id: u64, data: String) -> bool {
        if self.clients.contains_key(&client_id) {
            self.send_to_client(
                client_id,
                ServerMessage::EndpointControl {
                    kind: UPLOAD_RESPONSE_KIND.into(),
                    data,
                },
            );
        }
        false
    }

    /// Server stop and live handoff: sessions are never carried over, so give
    /// their workers a moment to remove their stages.
    pub(super) fn abort_uploads(&mut self, wait: Duration) {
        let controls = self.app.upload_sessions.abort_all();
        if !controls.is_empty() && !wait.is_zero() {
            crate::upload::registry::wait_for_cleanup(&controls, wait);
        }
    }
}
