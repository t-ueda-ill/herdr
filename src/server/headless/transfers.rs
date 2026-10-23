//! Endpoint-control transport for `pane.upload.*` and `pane.download.*`. Why
//! not the client-shell command lane: it holds one request in flight per
//! client, so a long transfer would stall every other shell command, and direct
//! attach clients cannot use it at all. Responses come back from session
//! workers through one pump thread.

use super::*;
use crate::app::{DownloadOrigin, UploadOrigin, UploadResponder};
use crate::protocol::endpoint::{EndpointUploadReady, UPLOAD_READY_KIND};
use crate::server::client_transport::TransferLane;

/// What the pump carries: which client, which transfer, the encoded response.
pub(super) type TransferResponse = (u64, TransferLane, String);

/// Starts the thread that turns worker answers into server events.
/// Why a thread: workers and the app thread answer through a std channel that
/// never blocks; only this thread waits on the bounded server event queue.
pub(super) fn spawn_transfer_response_pump(
    server_event_tx: mpsc::Sender<ServerEvent>,
) -> std::sync::mpsc::Sender<TransferResponse> {
    let (tx, rx) = std::sync::mpsc::channel::<TransferResponse>();
    let spawned = crate::thread_spawn::spawn_named("herdr-transfer-response", move || {
        while let Ok((client_id, lane, data)) = rx.recv() {
            if server_event_tx
                .blocking_send(ServerEvent::ClientTransferResponseReady {
                    client_id,
                    lane,
                    data,
                })
                .is_err()
            {
                break;
            }
        }
    });
    if let Err(error) = spawned {
        warn!(%error, "failed to start the transfer response thread; transfers will not answer");
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
    fn transfer_responder(&self, client_id: u64, lane: TransferLane) -> UploadResponder {
        let responses = self.transfer_responses.clone();
        Box::new(move |data| {
            let _ = responses.send((client_id, lane, data));
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

    pub(super) fn handle_client_transfer_request(
        &mut self,
        client_id: u64,
        lane: TransferLane,
        request: Box<api::schema::Request>,
    ) -> bool {
        if !self.clients.contains_key(&client_id) {
            return false;
        }
        let respond = self.transfer_responder(client_id, lane);
        match lane {
            TransferLane::Upload => self.handle_client_upload_request(client_id, request, respond),
            TransferLane::Download => {
                self.handle_client_download_request(client_id, request, respond)
            }
        }
    }

    fn handle_client_upload_request(
        &mut self,
        client_id: u64,
        request: Box<api::schema::Request>,
        respond: UploadResponder,
    ) -> bool {
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

    /// Why only a client shell: downloads are driven by the shell's workbench,
    /// and a direct attach holds one terminal and has nowhere to save files.
    fn handle_client_download_request(
        &mut self,
        client_id: u64,
        request: Box<api::schema::Request>,
        respond: UploadResponder,
    ) -> bool {
        let is_shell = self
            .clients
            .get(&client_id)
            .is_some_and(|client| matches!(client.mode, ClientConnectionMode::ClientShell));
        if !is_shell {
            respond(error_json(
                request.id,
                "unsupported_endpoint_command",
                "downloads need a client shell",
            ));
            return false;
        }
        self.dispatch_download(DownloadOrigin::ShellClient(client_id), *request, respond)
    }

    /// Starts a download request unless the server is going away.
    pub(super) fn dispatch_download(
        &mut self,
        origin: DownloadOrigin,
        request: api::schema::Request,
        respond: UploadResponder,
    ) -> bool {
        if self.handoff_in_progress || self.shutting_down {
            respond(error_json(
                request.id,
                "download_cancelled",
                "the server is stopping or handing off",
            ));
            return false;
        }
        self.app
            .handle_pane_download_request(origin, request, respond);
        false
    }

    pub(super) fn handle_client_transfer_request_error(
        &mut self,
        client_id: u64,
        lane: TransferLane,
        request_id: String,
        code: &'static str,
        message: String,
    ) -> bool {
        if self.clients.contains_key(&client_id) {
            (self.transfer_responder(client_id, lane))(error_json(request_id, code, message));
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

    pub(super) fn handle_client_transfer_response(
        &mut self,
        client_id: u64,
        lane: TransferLane,
        data: String,
    ) -> bool {
        if self.clients.contains_key(&client_id) {
            self.send_to_client(
                client_id,
                ServerMessage::EndpointControl {
                    kind: lane.response_kind().into(),
                    data,
                },
            );
        }
        false
    }

    /// Server stop and live handoff: sessions are never carried over, so give
    /// upload workers a moment to remove their stages. Downloads leave nothing
    /// on the server, so they are only told to stop.
    pub(super) fn abort_transfers(&mut self, wait: Duration) {
        let controls = self.app.upload_sessions.abort_all();
        if !controls.is_empty() && !wait.is_zero() {
            crate::upload::registry::wait_for_cleanup(&controls, wait);
        }
        self.app.download_sessions.abort_all();
    }
}
