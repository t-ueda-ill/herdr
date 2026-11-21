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

impl TaskState {
    /// No later change can happen.
    pub fn is_finished(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Stalled | Self::Lost | Self::Cancelled
        )
    }
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Where the agent was asked to write its result, on the task's location.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_path: Option<String>,
}

/// Where the text of `task.result` came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskResultSource {
    /// The result file the prompt asked for.
    File,
    /// The last lines of the agent's pane.
    Screen,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TaskResult {
    pub task_id: String,
    pub source: TaskResultSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TaskSubmitParams {
    /// Agent name, terminal id, or pane id, as `agent.prompt` takes it.
    pub target: String,
    pub text: String,
    /// Retry key. A second submit with the same key, target, and text returns
    /// the first task and sends nothing; another target or text is a conflict.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Append a line that asks the agent to write its result to `result_path`.
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub result_file: bool,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TaskWaitParams {
    pub task_id: String,
    /// States that end the wait early. A finished task always ends it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub until: Vec<TaskState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}
