//! Prompt tasks: pure bookkeeping, no PTY and no clock. The App feeds agent
//! status changes and time in, and runs the returned effects.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use crate::api::schema::{AgentStatus, Location, TaskEvidence, TaskState};
use crate::terminal::TerminalId;

/// How long a submitted prompt may go without a `working` or `blocked` state.
pub(crate) const ACCEPT_TIMEOUT: Duration = Duration::from_secs(5);
/// Finished tasks kept for `task.get` and `task.list`.
pub(crate) const FINISHED_TASKS_KEPT: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct TaskId(u64);

impl TaskId {
    pub(crate) fn public(self) -> String {
        format!("t_{}", self.0)
    }

    pub(crate) fn parse(text: &str) -> Option<Self> {
        text.strip_prefix("t_")?.parse().ok().map(Self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Now {
    pub(crate) at: Instant,
    pub(crate) unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Effect {
    /// Write the task's text to its terminal, then call `sent` or `submit_failed`.
    Submit(TaskId),
    /// Publish `task.updated` for the task.
    Emit(TaskId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NewTask {
    pub(crate) terminal: TerminalId,
    pub(crate) target: String,
    pub(crate) location: Location,
    pub(crate) text: String,
}

/// The agent as the App sees it at one moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AgentSnapshot {
    pub(crate) status: AgentStatus,
    /// `last_agent_state_change_seq` of the terminal.
    pub(crate) seq: u64,
    /// Lifecycle hooks, not the screen, own the state.
    pub(crate) hook: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Task {
    pub(crate) id: TaskId,
    pub(crate) terminal: TerminalId,
    pub(crate) target: String,
    pub(crate) location: Location,
    pub(crate) text: String,
    pub(crate) state: TaskState,
    pub(crate) evidence: Option<TaskEvidence>,
    pub(crate) created_at_ms: u64,
    pub(crate) submitted_at_ms: Option<u64>,
    pub(crate) finished_at_ms: Option<u64>,
    pub(crate) reason: Option<String>,
    submit_seq: u64,
    accept_deadline: Option<Instant>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CancelError {
    NotFound,
    AlreadySent(TaskState),
}

#[derive(Debug, Default)]
pub(crate) struct TaskTable {
    next_id: u64,
    tasks: HashMap<TaskId, Task>,
    lanes: HashMap<TerminalId, Lane>,
    finished: VecDeque<TaskId>,
}

#[derive(Debug)]
struct Lane {
    active: Option<TaskId>,
    queue: VecDeque<TaskId>,
    agent: AgentSnapshot,
}

impl TaskTable {
    pub(crate) fn submit(
        &mut self,
        new: NewTask,
        agent: AgentSnapshot,
        now: Now,
    ) -> (TaskId, Vec<Effect>) {
        self.next_id += 1;
        let id = TaskId(self.next_id);
        let terminal = new.terminal.clone();
        self.tasks.insert(
            id,
            Task {
                id,
                terminal: new.terminal,
                target: new.target,
                location: new.location,
                text: new.text,
                state: TaskState::Queued,
                evidence: None,
                created_at_ms: now.unix_ms,
                submitted_at_ms: None,
                finished_at_ms: None,
                reason: None,
                submit_seq: 0,
                accept_deadline: None,
            },
        );
        let lane = self.lanes.entry(terminal.clone()).or_insert_with(|| Lane {
            active: None,
            queue: VecDeque::new(),
            agent,
        });
        lane.agent = agent;
        lane.queue.push_back(id);
        let mut effects = Vec::new();
        self.dispatch_next(&terminal, now, &mut effects);
        if !effects.contains(&Effect::Emit(id)) {
            effects.push(Effect::Emit(id));
        }
        (id, effects)
    }

    pub(crate) fn submit_failed(&mut self, id: TaskId, reason: String, now: Now) -> Vec<Effect> {
        let mut effects = Vec::new();
        let Some(task) = self.tasks.get(&id) else {
            return effects;
        };
        if task.state != TaskState::Submitted {
            return effects;
        }
        let terminal = task.terminal.clone();
        self.finish(id, TaskState::Lost, Some(reason), now, &mut effects);
        self.release_lane(&terminal, id, now, &mut effects);
        effects
    }

    pub(crate) fn on_agent_status(
        &mut self,
        terminal: &TerminalId,
        agent: AgentSnapshot,
        now: Now,
    ) -> Vec<Effect> {
        let mut effects = Vec::new();
        let Some(lane) = self.lanes.get_mut(terminal) else {
            return effects;
        };
        lane.agent = agent;
        let Some(id) = lane.active else {
            self.dispatch_next(terminal, now, &mut effects);
            return effects;
        };
        let Some(task) = self.tasks.get_mut(&id) else {
            return effects;
        };
        let next = match (task.state, agent.status) {
            // Why not compare the status alone: a `working` from before the
            // send must not count as the agent taking this prompt.
            (TaskState::Submitted, _) if agent.seq <= task.submit_seq => None,
            (TaskState::Submitted, AgentStatus::Working) => Some(TaskState::Running),
            (TaskState::Submitted, AgentStatus::Blocked) => Some(TaskState::NeedsInput),
            (TaskState::Running, AgentStatus::Blocked) => Some(TaskState::NeedsInput),
            (TaskState::NeedsInput, AgentStatus::Working) => Some(TaskState::Running),
            (TaskState::Running | TaskState::NeedsInput, AgentStatus::Idle | AgentStatus::Done) => {
                Some(TaskState::Completed)
            }
            _ => None,
        };
        let Some(next) = next else {
            return effects;
        };
        if task.state == TaskState::Submitted {
            task.evidence = Some(if agent.hook {
                TaskEvidence::Hook
            } else {
                TaskEvidence::Screen
            });
            task.accept_deadline = None;
        }
        if next == TaskState::Completed {
            self.finish(id, next, None, now, &mut effects);
            self.release_lane(terminal, id, now, &mut effects);
        } else {
            task.state = next;
            effects.push(Effect::Emit(id));
        }
        effects
    }

    pub(crate) fn on_terminal_gone(
        &mut self,
        terminal: &TerminalId,
        reason: &str,
        now: Now,
    ) -> Vec<Effect> {
        let mut effects = Vec::new();
        let Some(lane) = self.lanes.remove(terminal) else {
            return effects;
        };
        if let Some(id) = lane.active {
            self.finish(
                id,
                TaskState::Lost,
                Some(reason.to_string()),
                now,
                &mut effects,
            );
        }
        for id in lane.queue {
            self.finish(
                id,
                TaskState::Cancelled,
                Some(reason.to_string()),
                now,
                &mut effects,
            );
        }
        effects
    }

    pub(crate) fn on_tick(&mut self, now: Now) -> Vec<Effect> {
        let mut effects = Vec::new();
        let due: Vec<(TaskId, TerminalId)> = self
            .lanes
            .iter()
            .filter_map(|(terminal, lane)| {
                let id = lane.active?;
                let deadline = self.tasks.get(&id)?.accept_deadline?;
                (deadline <= now.at).then(|| (id, terminal.clone()))
            })
            .collect();
        for (id, terminal) in due {
            let reason = format!(
                "no working or blocked state within {}ms",
                ACCEPT_TIMEOUT.as_millis()
            );
            self.finish(id, TaskState::Stalled, Some(reason), now, &mut effects);
            self.release_lane(&terminal, id, now, &mut effects);
        }
        effects
    }

    pub(crate) fn cancel(&mut self, id: TaskId, now: Now) -> Result<Vec<Effect>, CancelError> {
        let task = self.tasks.get(&id).ok_or(CancelError::NotFound)?;
        if task.state != TaskState::Queued {
            return Err(CancelError::AlreadySent(task.state));
        }
        if let Some(lane) = self.lanes.get_mut(&task.terminal) {
            lane.queue.retain(|queued| *queued != id);
        }
        let mut effects = Vec::new();
        self.finish(id, TaskState::Cancelled, None, now, &mut effects);
        Ok(effects)
    }

    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.lanes
            .values()
            .filter_map(|lane| self.tasks.get(&lane.active?)?.accept_deadline)
            .min()
    }

    /// Sends the head of the queue when the lane is free and the agent can take input.
    fn dispatch_next(&mut self, terminal: &TerminalId, now: Now, effects: &mut Vec<Effect>) {
        let Some(lane) = self.lanes.get_mut(terminal) else {
            return;
        };
        // Why not also `unknown`: the prompt could land in a turn that is
        // still running, and the task would then claim that turn's end.
        let ready = matches!(lane.agent.status, AgentStatus::Idle | AgentStatus::Done);
        if lane.active.is_some() || !ready {
            return;
        }
        let Some(id) = lane.queue.pop_front() else {
            return;
        };
        lane.active = Some(id);
        let seq = lane.agent.seq;
        if let Some(task) = self.tasks.get_mut(&id) {
            task.state = TaskState::Submitted;
            task.submit_seq = seq;
            task.submitted_at_ms = Some(now.unix_ms);
            task.accept_deadline = Some(now.at + ACCEPT_TIMEOUT);
        }
        effects.push(Effect::Submit(id));
        effects.push(Effect::Emit(id));
    }

    fn release_lane(
        &mut self,
        terminal: &TerminalId,
        id: TaskId,
        now: Now,
        effects: &mut Vec<Effect>,
    ) {
        if let Some(lane) = self.lanes.get_mut(terminal) {
            if lane.active == Some(id) {
                lane.active = None;
            }
        }
        self.dispatch_next(terminal, now, effects);
    }

    fn finish(
        &mut self,
        id: TaskId,
        state: TaskState,
        reason: Option<String>,
        now: Now,
        effects: &mut Vec<Effect>,
    ) {
        let Some(task) = self.tasks.get_mut(&id) else {
            return;
        };
        task.state = state;
        task.reason = reason;
        task.finished_at_ms = Some(now.unix_ms);
        task.accept_deadline = None;
        effects.push(Effect::Emit(id));
        self.finished.push_back(id);
        while self.finished.len() > FINISHED_TASKS_KEPT {
            if let Some(old) = self.finished.pop_front() {
                self.tasks.remove(&old);
            }
        }
    }

    /// True from the first task of the terminal until the terminal is gone,
    /// even when all its tasks are finished. Other terminals skip all task work.
    pub(crate) fn has_lane(&self, terminal: &TerminalId) -> bool {
        self.lanes.contains_key(terminal)
    }

    pub(crate) fn get(&self, id: TaskId) -> Option<&Task> {
        self.tasks.get(&id)
    }

    /// Tasks in the order they were created.
    pub(crate) fn iter(&self) -> impl Iterator<Item = &Task> {
        let mut tasks: Vec<&Task> = self.tasks.values().collect();
        tasks.sort_by_key(|task| task.id);
        tasks.into_iter()
    }
}

#[cfg(test)]
mod tests;
