use serde::{Deserialize, Serialize};

use super::panes::PaneSplitParams;
use super::tabs::TabCreateParams;
use super::workspaces::WorkspaceCreateParams;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SshOpenParams {
    /// `Host` alias from the user's ssh config, optionally prefixed with `user@`.
    pub host: String,
    pub target: SshOpenTarget,
}

// Nests the frozen create params instead of adding an `ssh` field to them: an older
// server would ignore that field and report success after starting a host shell.
// Why not reuse `ContainerOpenTarget`: its schema name is part of the frozen
// `container.open` shape, and this method needs a target named for itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SshOpenTarget {
    Workspace(WorkspaceCreateParams),
    Tab(TabCreateParams),
    Split(PaneSplitParams),
}

impl SshOpenTarget {
    pub fn focus(&self) -> bool {
        match self {
            Self::Workspace(params) => params.focus,
            Self::Tab(params) => params.focus,
            Self::Split(params) => params.focus,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct SshHostsParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SshHostInfoDto {
    pub alias: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
}
