//! Additive JSON methods; these types are not part of a named terminal codec.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FileReadParams {
    pub path: String,
    #[serde(default)]
    pub revision: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FilesSearchParams {
    pub root: String,
    pub query: String,
    #[serde(default)]
    pub include: String,
    #[serde(default)]
    pub exclude: String,
    #[serde(default)]
    pub regex: bool,
    #[serde(default)]
    pub case_sensitive: bool,
    #[serde(default)]
    pub whole_word: bool,
    #[serde(default)]
    pub include_ignored: bool,
    /// Opaque client-owned cancellation key. New generations supersede old work.
    pub search_id: String,
    pub generation: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FileDocument {
    pub path: String,
    pub revision: String,
    pub text: String,
    pub unchanged: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FileMatch {
    pub path: String,
    pub revision: String,
    /// UTF-8 byte offsets into the original document, not into the displayed line.
    pub start: usize,
    pub end: usize,
    pub line: usize,
    pub text: String,
    pub matched: String,
    pub replacement: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FileChange {
    pub path: String,
    pub revision: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FilesReplaceParams {
    pub changes: Vec<FileChange>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FileOperation {
    Copy,
    Move,
    CreateFile,
    CreateDirectory,
    Trash,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FileCollision {
    Skip,
    Rename,
    Replace,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FilesOperateParams {
    pub operation: FileOperation,
    pub paths: Vec<String>,
    pub destination: Option<String>,
    pub collision: FileCollision,
    /// Destructive actions require an explicit affirmative request.
    #[serde(default)]
    pub confirmed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FileOutcome {
    pub path: String,
    pub destination: Option<String>,
    pub error: Option<String>,
    pub revision: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FilesCancelParams {
    pub search_id: String,
    pub generation: u64,
}
