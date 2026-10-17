use serde::{Deserialize, Serialize};

use crate::cli::projection::{cli_params, Field};

use super::panes::PaneSplitParams;
use super::tabs::TabCreateParams;
use super::workspaces::WorkspaceCreateParams;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ContainerOpenParams {
    /// Docker container to open the shell in: `project/service` for a Compose service, otherwise a container name or ID.
    pub container: String,
    /// `Host` alias of the user's ssh config whose docker CLI runs the shell; the local docker when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_host: Option<String>,
    pub target: ContainerOpenTarget,
}

// Nests the frozen create params instead of adding a `container` field to them: an older
// server would ignore that field and report success after starting a host shell.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContainerOpenTarget {
    Workspace(WorkspaceCreateParams),
    Tab(TabCreateParams),
    Split(PaneSplitParams),
}

impl ContainerOpenTarget {
    pub fn focus(&self) -> bool {
        match self {
            Self::Workspace(params) => params.focus,
            Self::Tab(params) => params.focus,
            Self::Split(params) => params.focus,
        }
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
    pub struct ContainerListParams => ContainerListProjection {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub workspace_id: Option<String> => Field::new(None).input(
            super::tabs::workspace_option("ID").help("Only show containers matching this workspace"),
        ),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub ssh_host: Option<String> => Field::new(None).input(
            crate::cli::projection::optional_text("ssh", "HOST")
                .help("List containers of the docker on this ssh Host alias instead of the local docker"),
        ),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ContainerInfoDto {
    pub reference: String,
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// Set when the container runs on this ssh Host alias rather than the local docker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_host: Option<String>,
    pub is_default: bool,
}
