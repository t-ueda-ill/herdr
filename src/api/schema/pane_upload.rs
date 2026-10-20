//! Additive methods that copy files from a client into the environment a pane's
//! shell sees (this host, a container, or an ssh host). A transfer is staged
//! beside its destination and published all at once by `pane.upload.finish`.
use serde::{Deserialize, Serialize};

use super::pane_directory::PaneLocation;

/// Without `pane_id` and `terminal_id` the target is the server's own filesystem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneUploadBeginParams {
    /// Workspace pane whose environment receives the files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    /// Popup or directly attached terminal whose environment receives the files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_id: Option<String>,
    /// Absolute directory inside the target environment. Defaults to the pane's working
    /// directory, then its login directory. Required without a pane or terminal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<String>,
    /// Top-level names, as they will be published: 1 to 1,024 unique names.
    pub names: Vec<String>,
    /// Declared totals. Entries count files, directories, and symlinks.
    pub entries: u64,
    pub bytes: u64,
}

/// Body of a `pane.upload.begin` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneUploadStarted {
    pub upload_id: String,
    pub location: PaneLocation,
    /// The resolved physical destination directory.
    pub destination: String,
    /// Names that already exist in the destination.
    pub conflicts: Vec<String>,
    /// Largest decoded chunk a `pane.upload.write` may carry.
    pub chunk_bytes: u64,
    /// Writes that may be in flight at once on one ordered stream.
    pub window: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PaneUploadEntryKind {
    File,
    Directory,
    /// Sent as the link itself; its target is never followed.
    Symlink,
    /// A kind a newer client sends. Rejected with `invalid_params`.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneUploadEntryHeader {
    pub kind: PaneUploadEntryKind,
    /// Permission bits, at most 0o777.
    pub mode: u32,
    /// Modification time in UNIX seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtime: Option<i64>,
    /// Required for files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// Required for symlinks: 1 to 4,096 bytes without control characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_target: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneUploadWriteParams {
    pub upload_id: String,
    /// `/`-separated relative path whose first component is one of the begin names.
    pub path: String,
    /// Required on the first chunk of an entry (offset 0) and omitted afterwards.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<PaneUploadEntryHeader>,
    #[serde(default)]
    pub offset: u64,
    /// Standard padded base64 of at most `chunk_bytes` decoded bytes.
    #[serde(default)]
    pub data: String,
    /// The entry's final chunk. Always true for directories and symlinks.
    #[serde(default)]
    pub last: bool,
}

/// Body of a `pane.upload.write` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneUploadProgress {
    pub received_entries: u64,
    pub received_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PaneUploadCollision {
    /// Publish nothing when a name exists; the upload stays open for another finish.
    Fail,
    /// Publish a colliding name as `name (1).ext`.
    Rename,
    /// Replace an existing entry, directories as a whole.
    Replace,
    /// A policy a newer client sends. Rejected with `invalid_params`.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneUploadFinishParams {
    pub upload_id: String,
    pub collision: PaneUploadCollision,
}

/// Body of a `pane.upload.finish` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneUploadFinished {
    pub location: PaneLocation,
    pub destination: String,
    /// Published absolute paths, in the order of the begin names.
    pub paths: Vec<String>,
    /// Non-fatal problems, such as a modification time that could not be set.
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneUploadCancelParams {
    pub upload_id: String,
}
