//! Additive methods that serve a folder to a browser on this machine's
//! loopback, render formats such as Markdown, and reload open pages when a
//! file under the folder changes.
use serde::{Deserialize, Serialize};

use super::pane_directory::PaneLocation;

/// Without `pane_id`, or with a pane whose shell runs on this host, the folder
/// is on the server's filesystem; with an ssh or container pane it is read in
/// that pane's environment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PreviewStartParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    /// Absolute folder served as the site root. Starting the same folder in the
    /// same place again returns the running preview.
    pub root: String,
    /// Absolute path under `root` the returned URL opens. Defaults to the root,
    /// which serves its `index.html`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PreviewStopParams {
    pub preview_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PreviewInfo {
    pub preview_id: String,
    pub root: String,
    /// Absent for the server's own filesystem.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<PaneLocation>,
    pub port: u16,
    /// Loopback URL that signs a browser in and opens the entry. It carries
    /// the session token, so treat it as a secret.
    pub url: String,
}
