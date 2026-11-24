//! Additive methods that open a coding-agent CLI beside a pane, on the
//! server's machine, to read what that pane shows and advise the user.
use serde::{Deserialize, Serialize};

/// A coding-agent CLI the server can open as an assist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AssistProduct {
    Claude,
    Codex,
    Copilot,
    /// A product a newer server adds.
    #[serde(other)]
    Unknown,
}

/// Where the assist opens next to the pane it reads.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum AssistPlacement {
    /// Right of a wide pane, below a narrow one.
    #[default]
    Auto,
    Right,
    Below,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AssistProductInfo {
    pub product: AssistProduct,
    /// The executable found on the server's `PATH`.
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AssistStartParams {
    /// The pane the assist reads.
    pub pane_id: String,
    /// Defaults to the first product found, in the order claude, codex, copilot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product: Option<AssistProduct>,
    #[serde(default)]
    pub placement: AssistPlacement,
    /// Focus the assist. A pane that already has an assist focuses it either way.
    #[serde(default = "super::default_true")]
    pub focus: bool,
}

/// `pane_id` is the assist or the pane it reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AssistRefreshParams {
    pub pane_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AssistInfo {
    /// The assist's own pane.
    pub pane_id: String,
    /// Absent once the pane it read has closed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_pane_id: Option<String>,
    pub product: AssistProduct,
    /// The file that holds what the assist read. Only the server's user can read it.
    pub context_path: String,
}
