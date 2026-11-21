use serde::{Deserialize, Serialize};

use super::Location;

/// Lifecycle of one prompt sent through `task.submit`. Separate from
/// `AgentStatus`, which describes the agent and not this prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    /// Waiting in Herdr's queue; nothing was sent yet.
    Queued,
    /// Written to the pane; waiting for the agent to react.
    Submitted,
    /// The agent showed `working` after the send.
    Running,
    /// The agent showed `blocked` after the send.
    NeedsInput,
    /// The agent became idle or done after it took the prompt.
    Completed,
    /// No reaction within the acceptance window.
    Stalled,
    /// The write failed, the pane closed, or the agent ended.
    Lost,
    /// Removed from the queue before it was sent.
    Cancelled,
    /// A state a newer server adds.
    #[serde(other)]
    Unknown,
}

/// What showed that the agent took the prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskEvidence {
    /// A lifecycle hook of an official integration reported the state.
    Hook,
    /// Screen detection reported the state.
    Screen,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TaskInfo {
    /// `t_<n>`; numbers restart with the server.
    pub task_id: String,
    pub state: TaskState,
    /// The agent target as the caller gave it.
    pub target: String,
    pub terminal_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    pub location: Location,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<TaskEvidence>,
    pub created_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submitted_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at_ms: Option<u64>,
    /// Why the task stalled, was lost, or was cancelled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TaskSubmitParams {
    /// Agent name, terminal id, or pane id, as `agent.prompt` takes it.
    pub target: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TaskTarget {
    pub task_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TaskListParams {
    /// Keeps tasks at this location id; `ssh:<alias>` also keeps its containers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
}
