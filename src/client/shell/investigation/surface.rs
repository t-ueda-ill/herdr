//! Investigation presentation never owns terminal geometry. Reuse the negotiated
//! surface lease and the existing endpoint activation fence when returning.
use super::*;
use crate::api::schema::{ClientShellSurfaceSetParams, Method};
impl ClientShellState {
    pub(crate) fn investigation_surface_paused(
        &self,
        endpoint: &ClientEndpointId,
        generation: u64,
        boot: &str,
    ) -> bool {
        self.workbench.pause.as_ref().is_some_and(|p| {
            p.endpoint == *endpoint && p.generation == generation && p.boot == boot
        })
    }
    pub(crate) fn reconcile_investigation_surface(
        &mut self,
        generation: u64,
        boot: &str,
        surface_active: bool,
        lane_busy: bool,
        outcome: &mut ClientShellInput,
    ) -> bool {
        if self.workbench.pause.as_ref().is_some_and(|p| {
            p.endpoint != self.active_endpoint_id || p.generation != generation || p.boot != boot
        }) {
            self.workbench.pause = None;
            self.workbench.terminal_requests.clear();
            self.workbench.terminal_input.clear();
            self.workbench.terminal_requested = false;
        }
        let wants_terminal = !self.investigation_active() || self.workbench.terminal_requested;
        if !wants_terminal
            && surface_active
            && self.workbench.pause.is_none()
            && !lane_busy
            && self.supports_endpoint_method(&Method::ClientShellSurfaceSet(
                ClientShellSurfaceSetParams { active: false },
            ))
        {
            self.workbench.pause = Some(SurfacePause {
                endpoint: self.active_endpoint_id.clone(),
                generation,
                boot: boot.into(),
                confirmed: false,
                resume_requested: false,
            });
            if !self.push_endpoint_method_with_kind(
                Method::ClientShellSurfaceSet(ClientShellSurfaceSetParams { active: false }),
                PendingEndpointKind::FileSurfacePause,
                outcome,
            ) {
                self.workbench.pause = None;
            }
        }
        let resume = wants_terminal
            && !surface_active
            && !lane_busy
            && self.workbench.pause.as_ref().is_some_and(|p| p.confirmed);
        if resume {
            if let Some(pause) = self.workbench.pause.as_mut() {
                pause.resume_requested = true;
            }
        }
        resume
    }
    pub(crate) fn defer_investigation_terminal_action(&mut self, action: ClientShellAction) {
        self.workbench.terminal_requests.push(action);
        self.workbench.terminal_requested = true;
    }
    pub(crate) fn defer_investigation_terminal_input(&mut self, input: ClientMessage) {
        self.workbench.terminal_input.push(input);
        self.workbench.terminal_requested = true;
    }
    pub(crate) fn take_investigation_terminal_actions(&mut self) -> Vec<ClientShellAction> {
        self.workbench.pause = None;
        std::mem::take(&mut self.workbench.terminal_requests)
    }
    pub(crate) fn take_investigation_terminal_input(&mut self) -> Vec<ClientMessage> {
        let focused = self.focused_pane_id();
        let input=std::mem::take(&mut self.workbench.terminal_input).into_iter().filter(|m|matches!(m,ClientMessage::ClientShellPaneInput{pane_id,..} if Some(pane_id)==focused.as_ref())).collect();
        if !self.workbench.editor_opening && self.workbench.editor.is_none() {
            self.workbench.terminal_requested = false;
        }
        input
    }
    pub(crate) fn file_surface_pause(&self) -> Option<&SurfacePause> {
        self.workbench.pause.as_ref()
    }
}
