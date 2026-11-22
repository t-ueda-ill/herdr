//! The task table as stored in `tasks.json`, so finished tasks and their
//! results outlive a server restart.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use super::{AgentSnapshot, Lane, ResultPlace, Task, TaskId, TaskTable, FINISHED_TASKS_KEPT};
use crate::api::schema::{AgentStatus, Location, TaskEvidence, TaskState};
use crate::terminal::TerminalId;

pub(crate) const TASK_FILE_VERSION: u32 = 1;
/// The largest result text copied into the file.
pub(crate) const SAVED_RESULT_LIMIT: usize = 64 * 1024;
const STOPPED_REASON: &str = "the server stopped before the task finished";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TaskFile {
    pub(crate) version: u32,
    pub(crate) next_id: u64,
    /// In id order.
    pub(crate) tasks: Vec<TaskRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TaskRecord {
    pub(crate) id: u64,
    pub(crate) terminal: TerminalId,
    pub(crate) target: String,
    pub(crate) location: Location,
    pub(crate) text: String,
    /// A state this herdr does not know reads as `unknown`, then becomes lost.
    pub(crate) state: TaskState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) evidence: Option<TaskEvidence>,
    pub(crate) created_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) submitted_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) finished_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) result_path: Option<String>,
    /// The file sat on ssh or in a container. Why not the place itself: the
    /// launch holds per-run paths such as the ssh control socket.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) off_host: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) requester: Option<TerminalId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) parent: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) result: Option<SavedResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) screen_tail: Option<String>,
}

/// The result file text, copied when the task finished.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SavedResult {
    pub(crate) text: String,
    /// The file was longer than `SAVED_RESULT_LIMIT`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) truncated: bool,
}

impl SavedResult {
    pub(crate) fn from_file_text(mut text: String) -> Self {
        let truncated = text.len() > SAVED_RESULT_LIMIT;
        if truncated {
            let mut end = SAVED_RESULT_LIMIT;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
        }
        Self { text, truncated }
    }
}

impl TaskTable {
    pub(crate) fn to_file(&self) -> TaskFile {
        TaskFile {
            version: TASK_FILE_VERSION,
            next_id: self.next_id,
            tasks: self.iter().map(TaskRecord::from).collect(),
        }
    }

    pub(crate) fn from_file(file: TaskFile, now: super::Now, unfinished: Unfinished) -> Self {
        let mut finished_tasks = Vec::new();
        let mut carried = Vec::new();
        for record in file.tasks {
            let task = record.into_task(now, unfinished);
            if task.state.is_finished() {
                finished_tasks.push(task);
            } else {
                carried.push(task);
            }
        }
        finished_tasks.sort_by_key(|task| (task.finished_at_ms, task.id));
        let skip = finished_tasks.len().saturating_sub(FINISHED_TASKS_KEPT);
        let finished_tasks: Vec<Task> = finished_tasks.into_iter().skip(skip).collect();
        carried.sort_by_key(|task| task.id);
        let next_id = finished_tasks
            .iter()
            .chain(&carried)
            .map(|task| task.id.0)
            .max()
            .unwrap_or_default()
            .max(file.next_id);
        let mut table = Self {
            next_id,
            ..Self::default()
        };
        for task in finished_tasks {
            table.finished.push_back(task.id);
            table.keep(task);
        }
        for task in carried {
            let lane = table
                .lanes
                .entry(task.terminal.clone())
                .or_insert_with(|| Lane {
                    active: None,
                    queue: VecDeque::new(),
                    // Why unknown: nothing is sent until the new server
                    // sees the agent idle or done.
                    agent: AgentSnapshot {
                        status: AgentStatus::Unknown,
                        seq: 0,
                        hook: false,
                    },
                });
            if task.state == TaskState::Queued {
                lane.queue.push_back(task.id);
            } else {
                lane.active = Some(task.id);
            }
            table.keep(task);
        }
        table
    }

    fn keep(&mut self, task: Task) {
        if let Some(key) = &task.key {
            self.keys.insert(key.clone(), task.id);
        }
        self.tasks.insert(task.id, task);
    }
}

/// What happens to tasks that had not finished when the file was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Unfinished {
    /// A restart: the agents restarted too, and a resend could repeat work
    /// that is stale or already done.
    Lose,
    /// A handoff: the agents keep running in the same panes.
    Carry,
}

impl From<&Task> for TaskRecord {
    fn from(task: &Task) -> Self {
        Self {
            id: task.id.0,
            terminal: task.terminal.clone(),
            target: task.target.clone(),
            location: task.location.clone(),
            text: task.text.clone(),
            state: task.state,
            evidence: task.evidence,
            created_at_ms: task.created_at_ms,
            submitted_at_ms: task.submitted_at_ms,
            finished_at_ms: task.finished_at_ms,
            reason: task.reason.clone(),
            key: task.key.clone(),
            result_path: task.result_path.clone(),
            off_host: !matches!(task.result_place, ResultPlace::Host),
            requester: task.requester.clone(),
            parent: task.parent.map(|parent| parent.0),
            result: task.saved_result.clone(),
            screen_tail: task.screen_tail.clone(),
        }
    }
}

impl TaskRecord {
    fn into_task(self, now: super::Now, unfinished: Unfinished) -> Task {
        // Why a newer herdr's state is lost too: it has no meaning here.
        let keeps_state = self.state.is_finished()
            || (unfinished == Unfinished::Carry && self.state != TaskState::Unknown);
        let (state, finished_at_ms, reason) = if keeps_state {
            (self.state, self.finished_at_ms, self.reason)
        } else {
            (
                TaskState::Lost,
                Some(now.unix_ms),
                Some(STOPPED_REASON.to_string()),
            )
        };
        // Why a new window: the new server cannot tell whether the agent
        // took the prompt before the handoff.
        let accept_deadline =
            (state == TaskState::Submitted).then(|| now.at + super::ACCEPT_TIMEOUT);
        Task {
            id: TaskId(self.id),
            terminal: self.terminal,
            target: self.target,
            location: self.location,
            text: self.text,
            state,
            evidence: self.evidence,
            created_at_ms: self.created_at_ms,
            submitted_at_ms: self.submitted_at_ms,
            finished_at_ms,
            reason,
            key: self.key,
            result_path: self.result_path,
            requester: self.requester,
            parent: self.parent.map(TaskId),
            result_place: if self.off_host {
                ResultPlace::Unreachable
            } else {
                ResultPlace::Host
            },
            screen_tail: self.screen_tail,
            saved_result: self.result,
            submit_seq: 0,
            accept_deadline,
        }
    }
}

#[cfg(test)]
mod tests;
