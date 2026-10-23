//! Additive methods that stream files out of the environment a pane's shell sees
//! (this host, a container, or an ssh host) to the client. The client pulls the
//! stream with ordered `pane.download.read` calls; the server never pushes.
use serde::{Deserialize, Serialize};

use super::pane_directory::PaneLocation;
// Why not new header types: a record must feed the client's upload session as it
// is, and a second shape for the same meaning would force a conversion layer.
use super::pane_upload::PaneUploadEntryHeader;

/// Without `pane_id` the source is the server's own filesystem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneDownloadBeginParams {
    /// Workspace pane whose environment holds the paths.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    /// 1 to 1,024 absolute paths inside that environment, each at most 4,096 bytes
    /// and 256 KiB together. Their last components become the top-level names and
    /// must be unique and valid names. A symlink named here is followed; symlinks
    /// inside folders are sent as links.
    pub paths: Vec<String>,
    /// Decoded data bytes one read answer may carry, for the whole session.
    /// Defaults to the server's `chunk_bytes`; a larger value is clamped and 0 is
    /// refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<u64>,
}

/// Body of a `pane.download.begin` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneDownloadStarted {
    pub download_id: String,
    pub location: PaneLocation,
    /// Top-level names, in the order of `paths`.
    pub names: Vec<String>,
    /// Estimates from the walk at begin. The stream may differ when the tree changes.
    pub entries: u64,
    pub bytes: u64,
    /// Largest decoded data one read answer carries: `max_bytes` after clamping.
    pub chunk_bytes: u64,
    /// Reads that may be in flight at once on one ordered stream.
    pub window: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneDownloadReadParams {
    pub download_id: String,
    /// 0, 1, 2, ...: each read takes the next part of the stream.
    pub seq: u64,
}

/// One step of the stream. Same meaning as a `pane.upload.write`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneDownloadRecord {
    /// `/`-separated path whose first component is one of the begin names.
    pub path: String,
    /// On the first record of an entry (offset 0) only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<PaneUploadEntryHeader>,
    #[serde(default)]
    pub offset: u64,
    /// Standard padded base64.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub data: String,
    /// The entry's final record. Always true for directories and symlinks.
    #[serde(default)]
    pub last: bool,
}

/// Body of a `pane.download.read` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneDownloadChunk {
    pub seq: u64,
    /// May be empty: a heartbeat while the source is slow. Always empty with `done`.
    #[serde(default)]
    pub records: Vec<PaneDownloadRecord>,
    /// The stream ended cleanly. The session is gone after this answer.
    #[serde(default)]
    pub done: bool,
    pub sent_entries: u64,
    pub sent_bytes: u64,
    /// With `done`: skipped entries and other non-fatal problems, at most 32 KiB
    /// in total, control characters escaped.
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneDownloadCancelParams {
    pub download_id: String,
}
