//! Additive methods that read the filesystem a pane's shell sees when that shell
//! runs off-host (ssh, container). They are separate from `directory.*` because
//! those shapes are frozen and mean "this host's filesystem".
use serde::{Deserialize, Serialize};

use super::directory::DirectoryEntry;
use super::files::FileDocument;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PaneLocationKind {
    /// The server's own filesystem. `pane.directory.*` rejects host panes with
    /// `invalid_params`; `pane.upload.*` reports it for uploads to this host.
    Host,
    Ssh,
    Container,
    /// A kind a newer server adds.
    #[serde(other)]
    Unknown,
}

/// The environment a pane's shell runs in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneLocation {
    pub kind: PaneLocationKind,
    /// `ssh`: the alias. `container`: the alias of the ssh host whose docker runs it, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_host: Option<String>,
    /// `container`: the canonical container name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneDirectoryListParams {
    /// Off-host pane whose environment is listed.
    pub pane_id: String,
    /// Absolute directory inside the pane's environment. Defaults to the pane's working directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Passes `safe.directory` for the discovered repository.
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub trust_repository: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneDirectoryIndexParams {
    pub pane_id: String,
    /// Absolute path inside the pane's environment. Defaults to the pane's working directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub trust_repository: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneFileReadParams {
    pub pane_id: String,
    /// Absolute path inside the pane's environment.
    pub path: String,
    /// Revision the client already holds; a match answers `unchanged` without content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// A background refresh: the request gets a short deadline so it cannot starve other work.
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub poll: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneDirectoryOpenEditorParams {
    pub pane_id: String,
    /// Absolute path of the file to edit inside the pane's environment. Directories are rejected.
    pub path: String,
}

/// Body of a `pane.directory.list` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneDirectoryListing {
    pub location: PaneLocation,
    pub root: String,
    pub path: String,
    pub canonical: String,
    pub entries: Vec<DirectoryEntry>,
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omitted: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unlistable: Option<u64>,
}

/// Body of a `pane.directory.index` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneDirectoryIndexing {
    pub location: PaneLocation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_root: Option<String>,
    pub entries: Vec<String>,
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_error: Option<String>,
}

/// Body of a `pane.file.read` result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneFileContent {
    pub location: PaneLocation,
    pub document: FileDocument,
}
