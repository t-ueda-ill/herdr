use serde::{Deserialize, Serialize};

use crate::cli::directory::bindings;
use crate::cli::projection::{cli_params, Field};

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
    pub struct DirectoryListParams => DirectoryListProjection {
        /// Workspace whose focused pane supplies the root when `pane_id` is absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub workspace_id: Option<String> => Field::new(None).input(bindings::workspace().order(0)),
        /// Pane whose working directory is the root. Defaults to the focused pane.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub pane_id: Option<String> => Field::new(None).input(bindings::pane().order(1)),
        /// Absolute directory to list. Defaults to the root.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub path: Option<String> => Field::new(None).input(bindings::path("Directory to list instead of the pane's working directory").order(2)),
        /// Passes `safe.directory` for the discovered repository so Git accepts a checkout owned by another user.
        #[serde(default, skip_serializing_if = "super::is_false")]
        pub trust_repository: bool => Field::new(false).input(bindings::trust_repository().order(3)),
    }
}

// Why not share `DirectoryListParams`: each method's params are frozen by its
// own shape digest, so a later index-only field must not change `directory.list`.
cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
    pub struct DirectoryIndexParams => DirectoryIndexProjection {
        /// Workspace whose focused pane supplies the root when `pane_id` is absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub workspace_id: Option<String> => Field::new(None).input(bindings::workspace().order(0)),
        /// Pane whose working directory is the root. Defaults to the focused pane.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub pane_id: Option<String> => Field::new(None).input(bindings::pane().order(1)),
        /// Absolute path inside the repository to index. Defaults to the root.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub path: Option<String> => Field::new(None).input(bindings::path("Path inside the repository to index instead of the pane's working directory").order(2)),
        /// Passes `safe.directory` for the discovered repository so Git accepts a checkout owned by another user.
        #[serde(default, skip_serializing_if = "super::is_false")]
        pub trust_repository: bool => Field::new(false).input(bindings::trust_repository().order(3)),
    }
}

cli_params! {
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
    pub struct PaneOpenEditorParams => PaneOpenEditorProjection {
        /// Workspace whose focused pane is the target when `pane_id` is absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub workspace_id: Option<String> => Field::new(None).input(bindings::workspace().order(0)),
        /// Pane the editor overlays. It must be the focused pane of the active workspace.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub pane_id: Option<String> => Field::new(None).input(bindings::pane().order(1)),
        /// Absolute path of the file to edit. Directories are rejected.
        pub path: String => Field::new(String::new()).input(bindings::required_path("File to open in the editor").order(2)),
    }
}

/// How `pane.open_file` shows the file. Append-closed: a new mode needs a new method.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum OpenFileMode {
    /// `$EDITOR`, which may create a missing file.
    #[default]
    Editor,
    /// `$PAGER`, then `less`, `more`, or `cat`. The file must exist.
    Pager,
}

// Why not extend `PaneOpenEditorParams`: its shape is frozen, and an older
// server would ignore `line` and `mode` yet still report success.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneOpenFileParams {
    /// Workspace whose focused pane is the target when `pane_id` is absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// Pane the editor or pager overlays. It must be the focused pane of the active workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    /// Absolute path of the file to open. Directories are rejected.
    pub path: String,
    /// 1-based line to start at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<std::num::NonZeroU32>,
    #[serde(default)]
    pub mode: OpenFileMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DirectoryEntryKind {
    File,
    Dir,
    Symlink,
    /// Sockets, FIFOs, devices, and any kind a newer server adds.
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DirectoryEntry {
    pub name: String,
    pub kind: DirectoryEntryKind,
    /// What a `symlink` entry resolves to; absent for other kinds and for dangling links.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_kind: Option<DirectoryEntryKind>,
    /// Size in bytes of a regular file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// Modification time of the entry itself, in milliseconds since the Unix epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtime: Option<u64>,
    /// The name starts with a dot.
    pub hidden: bool,
    /// Git ignores the entry. Absent outside a Git work tree or when Git failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ignored: Option<bool>,
}
