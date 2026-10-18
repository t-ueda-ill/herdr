use std::path::PathBuf;

use crate::api::schema::{
    ContainerOpenTarget, PaneSplitParams, SshOpenTarget, TabCreateParams, WorkspaceCreateParams,
};
use crate::app::App;

/// The create params nested in a deferred `container.open` or `ssh.open`.
///
/// Why not one schema enum for both methods: each method's target type name
/// is part of its frozen wire shape, so they stay separate on the wire and
/// meet only here.
pub(in crate::app) enum OpenTarget<'a> {
    Workspace(&'a mut WorkspaceCreateParams),
    Tab(&'a mut TabCreateParams),
    Split(&'a mut PaneSplitParams),
}

impl<'a> From<&'a mut ContainerOpenTarget> for OpenTarget<'a> {
    fn from(target: &'a mut ContainerOpenTarget) -> Self {
        match target {
            ContainerOpenTarget::Workspace(params) => Self::Workspace(params),
            ContainerOpenTarget::Tab(params) => Self::Tab(params),
            ContainerOpenTarget::Split(params) => Self::Split(params),
        }
    }
}

impl<'a> From<&'a mut SshOpenTarget> for OpenTarget<'a> {
    fn from(target: &'a mut SshOpenTarget) -> Self {
        match target {
            SshOpenTarget::Workspace(params) => Self::Workspace(params),
            SshOpenTarget::Tab(params) => Self::Tab(params),
            SshOpenTarget::Split(params) => Self::Split(params),
        }
    }
}

impl App {
    /// Pins the host cwd and target ids at request time. Why not resolve them
    /// on completion: the lookup can take seconds, and focus moving in the
    /// meantime must not retarget the launch or its planned workdir.
    pub(in crate::app) fn pin_open_target(
        &self,
        target: OpenTarget<'_>,
    ) -> Result<PathBuf, (String, String)> {
        let (host_cwd, cwd_slot) = match target {
            OpenTarget::Workspace(params) => (self.workspace_create_cwd(params)?, &mut params.cwd),
            OpenTarget::Tab(params) => {
                let (ws_idx, cwd) = self
                    .tab_create_location(params.workspace_id.as_deref(), params.cwd.as_deref())?;
                params.workspace_id = Some(self.public_workspace_id(ws_idx));
                (cwd, &mut params.cwd)
            }
            OpenTarget::Split(params) => {
                let (ws_idx, pane_id, cwd) = self.pane_split_location(params)?;
                if let Some(public_pane_id) = self.public_pane_id(ws_idx, pane_id) {
                    params.target_pane_id = Some(public_pane_id);
                }
                (cwd, &mut params.cwd)
            }
        };
        // Why not to_string_lossy: a lossy path names a different directory.
        // Leaving a non-UTF-8 cwd unset lets the create handler resolve it again.
        if let Some(text) = host_cwd.to_str() {
            *cwd_slot = Some(text.to_owned());
        }
        Ok(host_cwd)
    }
}
