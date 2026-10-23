use super::*;

/// Internal events for the client event loop.
pub(super) enum ClientLoopEvent {
    #[cfg(unix)]
    StdinInput(Vec<u8>),
    /// A paste a drop check found to be plain text, replayed without being
    /// offered as a drop again.
    #[cfg(unix)]
    StdinPlain(Vec<u8>),
    #[cfg(unix)]
    PixelMouse(Vec<u8>, crate::input::mouse::HostGeometry),
    #[cfg(unix)]
    DirectGraphicsResponse(direct_graphics::Response),
    #[cfg(windows)]
    StdinEvents(Vec<crate::protocol::ClientInputEvent>),
    #[cfg(windows)]
    StdinEventsPlain(Vec<crate::protocol::ClientInputEvent>),
    #[cfg(windows)]
    NotificationActivated(shell::ClientSystemNotificationTarget),
    Resize(u16, u16, u32, u32, bool),
    TerminalUnavailable(io::Error),
    ServerMessage {
        endpoint_id: endpoint::ClientEndpointId,
        generation: u64,
        message: Box<ServerMessage>,
    },
    ServerDisconnected {
        endpoint_id: endpoint::ClientEndpointId,
        generation: u64,
    },
    EndpointSupervisor(endpoint::EndpointSupervisorEvent),
    EndpointCatalog(Result<Vec<endpoint::SavedSshEndpoint>, String>),
    HostPower(crate::platform::HostPowerEvent),
    Upload(upload::worker::WorkerEvent),
    Download(download::worker::WorkerEvent),
    ActivateEndpoint {
        endpoint_id: endpoint::ClientEndpointId,
        target: Option<shell::ClientEndpointFocusTarget>,
        /// A superseded handoff deliberately starts a fresh target-on epoch even when source and
        /// latest target have the same identity after restoration.
        force: bool,
    },
    Timer,
}

impl ClientLoopEvent {
    /// Input that must wait behind a paste being checked as a drop.
    pub(super) fn is_input(&self) -> bool {
        match self {
            #[cfg(unix)]
            Self::StdinInput(_) | Self::StdinPlain(_) | Self::PixelMouse(..) => true,
            #[cfg(windows)]
            Self::StdinEvents(_) | Self::StdinEventsPlain(_) => true,
            _ => false,
        }
    }
}
