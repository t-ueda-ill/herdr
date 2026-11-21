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
    /// Delete the local result file of a task the table no longer keeps.
    RemoveResultFile(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NewTask {
    pub(crate) terminal: TerminalId,
    pub(crate) target: String,
    pub(crate) location: Location,
    pub(crate) text: String,
    pub(crate) key: Option<String>,
    /// Folder on the task's location for its result file; none asks for no file.
    pub(crate) result_dir: Option<String>,
    /// Where the pane's shell ran at submit, so the file stays readable later.
    pub(crate) result_place: ResultPlace,
}

/// Where `task.result` reads the result file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResultPlace {
    Host,
    OffHost(Box<crate::offhost::PaneEnvironment>),
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
    pub(crate) key: Option<String>,
    pub(crate) result_path: Option<String>,
    pub(crate) result_place: ResultPlace,
    /// The end of the pane when the task finished; the fallback result.
    pub(crate) screen_tail: Option<String>,
    submit_seq: u64,
    accept_deadline: Option<Instant>,
}

impl Task {
    /// The text written to the pane: the caller's text, then the result-file line.
    pub(crate) fn prompt(&self) -> std::borrow::Cow<'_, str> {
        match &self.result_path {
            // Why not localized: Herdr does not know which language the agent uses.
            Some(path) => format!(
                "{}\n\nWhen you finish, write only your result to {path} (if the folder is missing, create it so that only you can read it).",
                self.text
            )
            .into(),
            None => self.text.as_str().into(),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CancelError {
    NotFound,
    AlreadySent(TaskState),
}

/// The key already names a task with another target or text.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct KeyConflict {
    pub(crate) existing: TaskId,
}

#[derive(Debug, Default)]
pub(crate) struct TaskTable {
    next_id: u64,
    tasks: HashMap<TaskId, Task>,
    /// Kept as long as the task is kept.
    keys: HashMap<String, TaskId>,
    lanes: HashMap<TerminalId, Lane>,
    finished: VecDeque<TaskId>,
}

#[derive(Debug)]
struct Lane {
    active: Option<TaskId>,
    queue: VecDeque<TaskId>,
    agent: AgentSnapshot,
}

/// 128 bits from SipHash keys that std seeds from the OS: another user on the
/// host must not guess a result folder and create it first.
pub(crate) fn unguessable_name() -> String {
    use std::hash::{BuildHasher, Hasher};
    let half = || {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u32(std::process::id());
        hasher.finish()
    };
    format!("{:016x}{:016x}", half(), half())
}

impl TaskTable {
    /// The task an earlier submit with this key made, if it is still kept.
    pub(crate) fn find_by_key(
        &self,
        key: &str,
        target: &str,
        text: &str,
    ) -> Result<Option<TaskId>, KeyConflict> {
        let Some(&id) = self.keys.get(key) else {
            return Ok(None);
        };
        // Why not compare terminals: the retry must still find a task whose
        // pane is gone, and the caller repeats the target it sent.
        match self.tasks.get(&id) {
            Some(task) if task.target == target && task.text == text => Ok(Some(id)),
            Some(_) => Err(KeyConflict { existing: id }),
            None => Ok(None),
        }
    }

    /// `submit`, except that a known key returns its task and sends nothing.
    pub(crate) fn submit_idempotent(
        &mut self,
        new: NewTask,
        agent: AgentSnapshot,
        now: Now,
    ) -> Result<(TaskId, Vec<Effect>), KeyConflict> {
        if let Some(key) = &new.key {
            if let Some(id) = self.find_by_key(key, &new.target, &new.text)? {
                return Ok((id, Vec::new()));
            }
        }
        Ok(self.submit(new, agent, now))
    }

    pub(crate) fn submit(
        &mut self,
        new: NewTask,
        agent: AgentSnapshot,
        now: Now,
    ) -> (TaskId, Vec<Effect>) {
        self.next_id += 1;
        let id = TaskId(self.next_id);
        let terminal = new.terminal.clone();
        if let Some(key) = &new.key {
            self.keys.insert(key.clone(), id);
        }
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
                key: new.key,
                result_path: new
                    .result_dir
                    .map(|dir| format!("{dir}/{}.md", id.public())),
                result_place: new.result_place,
                screen_tail: None,
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
        if let Some(task) = self.tasks.get_mut(&id) {
            // The prompt never reached the pane.
            task.submitted_at_ms = None;
        }
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

    /// Keeps the first tail only: a later read can show the next task's turn.
    pub(crate) fn save_screen_tail(&mut self, id: TaskId, tail: String) {
        if let Some(task) = self.tasks.get_mut(&id) {
            task.screen_tail.get_or_insert(tail);
        }
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
                let Some(dropped) = self.tasks.remove(&old) else {
                    continue;
                };
                if let Some(key) = dropped.key {
                    self.keys.remove(&key);
                }
                // Why not remote files: removing them needs the remote; the
                // temp cleaner there removes them.
                if let (Some(path), ResultPlace::Host) = (dropped.result_path, dropped.result_place)
                {
                    effects.push(Effect::RemoveResultFile(path));
                }
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
