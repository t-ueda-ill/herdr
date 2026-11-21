//! `task.*` methods and the glue that drives `TaskTable` from agent status
//! changes. The table holds the rules; this file only runs its effects.

use crate::api::schema::{
    AgentPromptParams, EventData, EventEnvelope, EventKind, Location, ResponseResult, TaskInfo,
    TaskListParams, TaskSubmitParams, TaskTarget,
};
use crate::app::App;
use crate::tasks::{AgentSnapshot, CancelError, Effect, KeyConflict, NewTask, Now, TaskId};
use crate::terminal::TerminalId;

use super::super::api_helpers::pane_agent_status;
use super::responses::{encode_error, encode_success};

impl App {
    pub(super) fn handle_task_submit(&mut self, id: String, params: TaskSubmitParams) -> String {
        if let Some(key) = &params.key {
            if key.is_empty() || key.len() > MAX_TASK_KEY_LEN {
                return encode_error(
                    id,
                    "invalid_params",
                    format!("task key must be 1 to {MAX_TASK_KEY_LEN} bytes"),
                );
            }
            // Why before the agent checks: a retry must get its task back even
            // when the agent now waits for input or its pane is gone.
            match self
                .state
                .tasks
                .find_by_key(key, &params.target, &params.text)
            {
                Ok(Some(task)) => return self.task_response(id, task),
                Ok(None) => {}
                Err(conflict) => return task_key_conflict(id, key, conflict),
            }
        }
        let prompt = AgentPromptParams {
            target: params.target.clone(),
            text: params.text.clone(),
            wait: None,
        };
        // Why not a separate check: a task must refuse exactly what `agent.prompt` refuses.
        let validated = match self.validate_agent_prompt(id.clone(), &prompt) {
            Ok(validated) => validated,
            Err(response) => return response,
        };
        let Some(terminal) = self
            .state
            .workspaces
            .get(validated.ws_idx)
            .and_then(|workspace| workspace.terminal_id(validated.pane_id))
            .cloned()
        else {
            return encode_error(id, "agent_not_found", "agent terminal is gone");
        };
        let Some(agent) =
            self.task_agent_snapshot_at(validated.ws_idx, validated.pane_id, &terminal)
        else {
            return encode_error(id, "agent_not_found", "agent terminal is gone");
        };
        // Why ignore an unknown pane: the caller may run in a pane of another server.
        let requester = params
            .requested_by
            .as_deref()
            .and_then(|pane| self.parse_pane_id(pane))
            .and_then(|(ws_idx, pane)| self.state.workspaces.get(ws_idx)?.terminal_id(pane))
            .cloned();
        if self.state.tasks.would_cycle(&terminal, requester.as_ref()) {
            return encode_error(
                id,
                "task_cycle",
                format!(
                    "{} already waits for the asking pane; the task would never start",
                    params.target
                ),
            );
        }
        let location = self
            .pane_info(validated.ws_idx, validated.pane_id)
            .and_then(|pane| pane.location)
            .unwrap_or_else(|| Location::new(None, None));
        let key = params.key.clone();
        let submitted = self.state.tasks.submit_idempotent(
            NewTask {
                terminal,
                target: params.target,
                result_dir: params
                    .result_file
                    .then(|| task_result_dir(&location, self.state.tasks.run_id())),
                location,
                text: params.text,
                key: params.key,
                requester,
            },
            agent,
            task_now(),
        );
        let (task, effects) = match submitted {
            Ok(submitted) => submitted,
            Err(conflict) => {
                return task_key_conflict(id, key.as_deref().unwrap_or_default(), conflict)
            }
        };
        self.run_task_effects(effects);
        self.task_response(id, task)
    }

    fn task_response(&self, id: String, task: TaskId) -> String {
        match self.task_info(task) {
            Some(task) => encode_success(id, ResponseResult::TaskInfo { task }),
            None => encode_error(id, "task_not_found", "task was dropped"),
        }
    }

    pub(super) fn handle_task_get(&mut self, id: String, target: TaskTarget) -> String {
        match TaskId::parse(&target.task_id).and_then(|task| self.task_info(task)) {
            Some(task) => encode_success(id, ResponseResult::TaskInfo { task }),
            None => task_not_found(id, &target.task_id),
        }
    }

    pub(super) fn handle_task_list(&mut self, id: String, params: TaskListParams) -> String {
        let tasks = self
            .state
            .tasks
            .iter()
            .filter(|task| {
                params
                    .location
                    .as_deref()
                    .is_none_or(|filter| Location::id_matches(&task.location.id, filter))
            })
            .map(|task| task.id)
            .collect::<Vec<_>>()
            .into_iter()
            .filter_map(|task| self.task_info(task))
            .collect();
        encode_success(id, ResponseResult::TaskList { tasks })
    }

    pub(super) fn handle_task_cancel(&mut self, id: String, target: TaskTarget) -> String {
        let Some(task) = TaskId::parse(&target.task_id) else {
            return task_not_found(id, &target.task_id);
        };
        match self.state.tasks.cancel(task, task_now()) {
            Ok(effects) => self.run_task_effects(effects),
            Err(CancelError::NotFound) => return task_not_found(id, &target.task_id),
            Err(CancelError::AlreadySent(state)) => {
                return encode_error(
                    id,
                    "task_already_sent",
                    format!(
                        "task {} is {state:?}; only queued tasks can be cancelled",
                        target.task_id
                    ),
                );
            }
        }
        match self.task_info(task) {
            Some(task) => encode_success(id, ResponseResult::TaskInfo { task }),
            None => task_not_found(id, &target.task_id),
        }
    }

    /// Called for every agent status change. Why not look the pane up from the
    /// terminal: this runs on the detection path, so it must stay O(1).
    pub(crate) fn advance_tasks_for_pane(&mut self, ws_idx: usize, pane_id: crate::layout::PaneId) {
        let Some(terminal) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.terminal_id(pane_id))
            .cloned()
        else {
            return;
        };
        if !self.state.tasks.has_lane(&terminal) {
            return;
        }
        let Some(agent) = self.task_agent_snapshot_at(ws_idx, pane_id, &terminal) else {
            return;
        };
        let effects = self
            .state
            .tasks
            .on_agent_status(&terminal, agent, task_now());
        self.run_task_effects(effects);
    }

    pub(crate) fn end_tasks_for_terminal(&mut self, terminal: &TerminalId, reason: &str) {
        let effects = self
            .state
            .tasks
            .on_terminal_gone(terminal, reason, task_now());
        self.run_task_effects(effects);
    }

    pub(crate) fn expire_due_tasks(&mut self, now: std::time::Instant) {
        if self
            .state
            .tasks
            .next_deadline()
            .is_none_or(|deadline| deadline > now)
        {
            return;
        }
        let effects = self.state.tasks.on_tick(Now {
            at: now,
            unix_ms: unix_ms(),
        });
        self.run_task_effects(effects);
    }

    pub(crate) fn fail_task_submission(&mut self, task: TaskId, reason: String) {
        let effects = self.state.tasks.submit_failed(task, reason, task_now());
        self.run_task_effects(effects);
    }

    fn run_task_effects(&mut self, effects: Vec<crate::tasks::Effect>) {
        let mut pending = std::collections::VecDeque::from(effects);
        while let Some(effect) = pending.pop_front() {
            match effect {
                Effect::Emit(task) => {
                    if let Some(task) = self.task_info(task) {
                        self.emit_event(EventEnvelope {
                            event: EventKind::TaskUpdated,
                            data: EventData::TaskUpdated { task },
                        });
                    }
                }
                Effect::Submit(task) => {
                    if let Err(reason) = self.write_task_prompt(task) {
                        pending.extend(self.state.tasks.submit_failed(task, reason, task_now()));
                    }
                }
            }
        }
    }

    /// Writes the prompt with the same checks and encoding as `agent.prompt`.
    fn write_task_prompt(&mut self, task: TaskId) -> Result<(), String> {
        let (terminal, text) = self
            .state
            .tasks
            .get(task)
            .map(|task| (task.terminal.clone(), task.prompt().into_owned()))
            .ok_or_else(|| "task was dropped".to_string())?;
        let pane = self
            .terminal_public_pane_id(&terminal)
            .ok_or_else(|| "agent pane is gone".to_string())?;
        let prompt = AgentPromptParams {
            target: pane,
            text,
            wait: None,
        };
        let validated = self
            .validate_agent_prompt(task.public(), &prompt)
            .map_err(|response| response_error_message(&response))?;
        let (_, _, completion) = self
            .submit_agent_prompt(validated, &prompt)
            .map_err(|response| response_error_message(&response))?;
        let event_tx = self.event_tx.clone();
        crate::thread_spawn::spawn_named("herdr-task-submit", move || {
            let reason = match completion.recv() {
                Ok(Ok(())) => return,
                Ok(Err(err)) => err.to_string(),
                Err(_) => "pty actor closed".to_string(),
            };
            let _ =
                event_tx.blocking_send(crate::events::AppEvent::TaskSubmitFailed { task, reason });
        })
        .map(|_| ())
        .map_err(|err| format!("could not watch the prompt write: {err}"))
    }

    fn task_agent_snapshot_at(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        terminal: &TerminalId,
    ) -> Option<AgentSnapshot> {
        let state = self.state.terminals.get(terminal)?;
        let seen = self.state.workspaces.get(ws_idx)?.pane_state(pane_id)?.seen;
        Some(AgentSnapshot {
            status: pane_agent_status(state.state, seen),
            seq: state.last_agent_state_change_seq.unwrap_or(0),
            hook: state.full_lifecycle_hook_authority_active(),
        })
    }

    fn task_info(&self, task: TaskId) -> Option<TaskInfo> {
        let task = self.state.tasks.get(task)?;
        Some(TaskInfo {
            task_id: task.id.public(),
            state: task.state,
            target: task.target.clone(),
            terminal_id: task.terminal.to_string(),
            pane_id: self.terminal_public_pane_id(&task.terminal),
            location: task.location.clone(),
            text: task.text.clone(),
            evidence: task.evidence,
            created_at_ms: task.created_at_ms,
            submitted_at_ms: task.submitted_at_ms,
            finished_at_ms: task.finished_at_ms,
            reason: task.reason.clone(),
            key: task.key.clone(),
            result_path: task.result_path.clone(),
            requester_terminal_id: task.requester.as_ref().map(ToString::to_string),
            parent_task_id: task.parent.map(TaskId::public),
        })
    }

    pub(super) fn terminal_pane(
        &self,
        terminal: &TerminalId,
    ) -> Option<(usize, crate::layout::PaneId)> {
        self.terminal_target_for_terminal_id(terminal.as_str())
            .map(|target| (target.ws_idx, target.pane_id))
    }

    pub(super) fn terminal_public_pane_id(&self, terminal: &TerminalId) -> Option<String> {
        let (ws_idx, pane_id) = self.terminal_pane(terminal)?;
        self.public_pane_id(ws_idx, pane_id)
    }
}

const MAX_TASK_KEY_LEN: usize = 256;

fn task_key_conflict(id: String, key: &str, conflict: KeyConflict) -> String {
    encode_error(
        id,
        "task_key_conflict",
        format!(
            "key {key} belongs to {} with another target or text",
            conflict.existing.public()
        ),
    )
}

/// A folder directly in the temp folder: a shared parent such as
/// `/tmp/herdr-task/` could belong to another user, who could then swap our
/// folder; the sticky temp folder lets only us remove ours.
fn task_result_dir(location: &Location, run: &str) -> String {
    match location.kind {
        crate::api::schema::PaneLocationKind::Host => {
            let dir = std::env::temp_dir().join(format!("herdr-task-{run}"));
            match crate::platform::create_private_dir(&dir) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
                // Why not refuse the task: the agent can still make the folder.
                Err(err) => {
                    tracing::warn!(err = %err, dir = %dir.display(), "could not create the task result folder")
                }
            }
            dir.display().to_string()
        }
        // Why not the pane's directory: the result must not land in the repository.
        _ => format!("/tmp/herdr-task-{run}"),
    }
}

fn task_not_found(id: String, task_id: &str) -> String {
    encode_error(id, "task_not_found", format!("no task {task_id}"))
}

fn response_error_message(response: &str) -> String {
    serde_json::from_str::<serde_json::Value>(response)
        .ok()
        .and_then(|value| value["error"]["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| response.to_string())
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

fn task_now() -> Now {
    Now {
        at: std::time::Instant::now(),
        unix_ms: unix_ms(),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bytes::Bytes;

    use super::*;
    use crate::api::schema::{
        ErrorResponse, SuccessResponse, TaskEvidence, TaskResult, TaskResultSource, TaskState,
    };
    use crate::config::Config;
    use crate::detect::{Agent, AgentState};
    use crate::events::AppEvent;
    use crate::workspace::Workspace;

    struct Fixture {
        app: App,
        pane_id: crate::layout::PaneId,
        pty: tokio::sync::mpsc::Receiver<Bytes>,
    }

    fn fixture(state: AgentState) -> Fixture {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("agent")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = crate::app::Mode::Terminal;
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let state_ref = app.state.terminals.get_mut(&terminal).unwrap();
        state_ref.set_agent_name("reviewer".into());
        state_ref.set_detected_state(Some(Agent::OpenCode), state);
        let (runtime, pty) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80, 24, 0, b"", 8,
            );
        app.state.insert_test_runtime(pane_id, runtime);
        Fixture { app, pane_id, pty }
    }

    impl Fixture {
        fn submit(&mut self, text: &str) -> TaskInfo {
            self.submit_keyed(text, None)
        }

        fn submit_raw(&mut self, text: &str, key: Option<&str>, result_file: bool) -> String {
            self.submit_params(TaskSubmitParams {
                target: "reviewer".into(),
                text: text.into(),
                key: key.map(str::to_string),
                result_file,
                requested_by: None,
            })
        }

        fn submit_params(&mut self, params: TaskSubmitParams) -> String {
            self.app.handle_task_submit("req".into(), params)
        }

        fn own_pane(&self) -> String {
            self.app
                .public_pane_id(0, self.pane_id)
                .expect("public pane id")
        }

        fn submit_with(&mut self, text: &str, result_file: bool) -> TaskInfo {
            let response = self.submit_raw(text, None, result_file);
            Self::task_of(&response)
        }

        fn submit_keyed(&mut self, text: &str, key: Option<&str>) -> TaskInfo {
            let response = self.submit_raw(text, key, false);
            Self::task_of(&response)
        }

        fn task_of(response: &str) -> TaskInfo {
            let success: SuccessResponse = serde_json::from_str(response)
                .unwrap_or_else(|_| panic!("task.submit failed: {response}"));
            let ResponseResult::TaskInfo { task } = success.result else {
                panic!("expected task info: {response}");
            };
            task
        }

        fn get(&mut self, task_id: &str) -> TaskInfo {
            let response = self.app.handle_task_get(
                "req".into(),
                TaskTarget {
                    task_id: task_id.into(),
                },
            );
            let success: SuccessResponse = serde_json::from_str(&response).unwrap();
            let ResponseResult::TaskInfo { task } = success.result else {
                panic!("expected task info: {response}");
            };
            task
        }

        fn agent_becomes(&mut self, state: AgentState) {
            self.app.handle_internal_event(AppEvent::StateChanged {
                pane_id: self.pane_id,
                agent: Some(Agent::OpenCode),
                state,
                visible_blocker: state == AgentState::Blocked,
                visible_working: state == AgentState::Working,
                process_exited: false,
                observed_at: std::time::Instant::now(),
            });
        }

        fn written_text(&mut self) -> Vec<u8> {
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            let mut written = Vec::new();
            while std::time::Instant::now() < deadline {
                while let Ok(bytes) = self.pty.try_recv() {
                    written.extend_from_slice(&bytes);
                }
                if written.ends_with(b"\r") {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            written
        }

        fn result(&mut self, task_id: &str) -> Result<TaskResult, ErrorResponse> {
            let (tx, rx) = std::sync::mpsc::channel();
            let handled = self.app.handle_deferred_task_api_request(
                crate::api::schema::Request {
                    id: "req".into(),
                    method: crate::api::schema::Method::TaskResult(TaskTarget {
                        task_id: task_id.into(),
                    }),
                },
                tx,
            );
            assert!(handled, "task.result was not taken");
            let response = rx
                .recv_timeout(Duration::from_secs(5))
                .expect("task.result answered");
            match serde_json::from_str::<SuccessResponse>(&response) {
                Ok(success) => match success.result {
                    ResponseResult::TaskResult { result } => Ok(result),
                    _ => panic!("expected task result: {response}"),
                },
                Err(_) => Err(serde_json::from_str(&response)
                    .unwrap_or_else(|_| panic!("unexpected response: {response}"))),
            }
        }

        fn task_events(&self) -> Vec<TaskInfo> {
            self.app
                .event_hub
                .events_after(0)
                .into_iter()
                .filter_map(|(_, event)| match event.data {
                    EventData::TaskUpdated { task } => Some(task),
                    _ => None,
                })
                .collect()
        }
    }

    #[tokio::test]
    async fn a_task_for_an_idle_agent_writes_the_prompt_and_reports_submitted() {
        let mut fixture = fixture(AgentState::Idle);

        let task = fixture.submit("fix the test");

        assert_eq!(task.state, TaskState::Submitted);
        assert_eq!(task.location.id, "local");
        assert!(task.task_id.starts_with("t_"));
        assert_eq!(fixture.written_text(), b"fix the test\r");
        assert_eq!(
            fixture.task_events().last().map(|task| task.state),
            Some(TaskState::Submitted)
        );
    }

    #[tokio::test]
    async fn the_agent_working_then_idle_runs_then_completes_the_task() {
        let mut fixture = fixture(AgentState::Idle);
        let task = fixture.submit("fix");

        fixture.agent_becomes(AgentState::Working);
        let running = fixture.get(&task.task_id);
        assert_eq!(running.state, TaskState::Running);
        assert_eq!(running.evidence, Some(TaskEvidence::Screen));

        fixture.agent_becomes(AgentState::Idle);
        assert_eq!(fixture.get(&task.task_id).state, TaskState::Completed);
        let states: Vec<_> = fixture
            .task_events()
            .into_iter()
            .map(|task| task.state)
            .collect();
        assert_eq!(
            states,
            [
                TaskState::Submitted,
                TaskState::Running,
                TaskState::Completed
            ]
        );
    }

    #[tokio::test]
    async fn a_task_for_a_working_agent_waits_until_the_agent_is_idle() {
        let mut fixture = fixture(AgentState::Working);

        let task = fixture.submit("next");
        assert_eq!(task.state, TaskState::Queued);
        assert!(
            fixture.pty.try_recv().is_err(),
            "nothing is written while queued"
        );

        fixture.agent_becomes(AgentState::Idle);

        assert_eq!(fixture.get(&task.task_id).state, TaskState::Submitted);
        assert_eq!(fixture.written_text(), b"next\r");
    }

    #[tokio::test]
    async fn a_blocked_agent_refuses_the_task_like_agent_prompt() {
        let mut fixture = fixture(AgentState::Blocked);

        let response = fixture.app.handle_task_submit(
            "req".into(),
            TaskSubmitParams {
                target: "reviewer".into(),
                text: "x".into(),
                key: None,
                result_file: false,
                requested_by: None,
            },
        );

        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "agent_blocked");
        assert_eq!(fixture.app.state.tasks.iter().count(), 0);
    }

    #[tokio::test]
    async fn no_reaction_in_five_seconds_stalls_the_task() {
        let mut fixture = fixture(AgentState::Idle);
        let task = fixture.submit("fix");

        fixture
            .app
            .expire_due_tasks(std::time::Instant::now() + crate::tasks::ACCEPT_TIMEOUT);

        let stalled = fixture.get(&task.task_id);
        assert_eq!(stalled.state, TaskState::Stalled);
        assert!(stalled.reason.is_some());
    }

    #[tokio::test]
    async fn closing_the_pane_loses_the_task() {
        let mut fixture = fixture(AgentState::Idle);
        let task = fixture.submit("fix");
        let pane = fixture.app.public_pane_id(0, fixture.pane_id).unwrap();

        fixture.app.handle_pane_close(
            "req".into(),
            crate::api::schema::PaneTarget { pane_id: pane },
        );

        let lost = fixture.get(&task.task_id);
        assert_eq!(lost.state, TaskState::Lost);
        assert_eq!(lost.reason.as_deref(), Some("pane closed"));
    }

    #[tokio::test]
    async fn a_sent_task_cannot_be_cancelled() {
        let mut fixture = fixture(AgentState::Idle);
        let task = fixture.submit("fix");

        let response = fixture.app.handle_task_cancel(
            "req".into(),
            TaskTarget {
                task_id: task.task_id,
            },
        );

        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "task_already_sent");
    }

    #[tokio::test]
    async fn task_list_keeps_only_the_requested_location() {
        let mut fixture = fixture(AgentState::Working);
        fixture.submit("one");

        let list = |fixture: &mut Fixture, location: &str| {
            let response = fixture.app.handle_task_list(
                "req".into(),
                TaskListParams {
                    location: Some(location.into()),
                },
            );
            let success: SuccessResponse = serde_json::from_str(&response).unwrap();
            let ResponseResult::TaskList { tasks } = success.result else {
                panic!("expected task list");
            };
            tasks.len()
        };

        assert_eq!(list(&mut fixture, "local"), 1);
        assert_eq!(list(&mut fixture, "ssh:devbox"), 0);
    }

    #[tokio::test]
    async fn an_unknown_task_id_is_not_found() {
        let mut fixture = fixture(AgentState::Idle);

        let response = fixture.app.handle_task_get(
            "req".into(),
            TaskTarget {
                task_id: "t_99".into(),
            },
        );

        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "task_not_found");
    }

    #[tokio::test]
    async fn retrying_with_the_same_key_writes_the_prompt_once() {
        let mut fixture = fixture(AgentState::Idle);
        let first = fixture.submit_keyed("fix", Some("run/1"));
        assert_eq!(fixture.written_text(), b"fix\r");

        let again = fixture.submit_keyed("fix", Some("run/1"));

        assert_eq!(again.task_id, first.task_id);
        assert_eq!(again.key.as_deref(), Some("run/1"));
        std::thread::sleep(Duration::from_millis(50));
        assert!(
            fixture.pty.try_recv().is_err(),
            "the prompt was written twice"
        );
    }

    #[tokio::test]
    async fn retrying_while_the_agent_is_blocked_returns_the_waiting_task() {
        let mut fixture = fixture(AgentState::Idle);
        let first = fixture.submit_keyed("fix", Some("run/1"));
        fixture.agent_becomes(AgentState::Blocked);

        let again = fixture.submit_keyed("fix", Some("run/1"));

        assert_eq!(again.task_id, first.task_id);
        assert_eq!(again.state, TaskState::NeedsInput);
    }

    #[tokio::test]
    async fn a_key_reused_for_another_prompt_is_refused() {
        let mut fixture = fixture(AgentState::Idle);
        let first = fixture.submit_keyed("fix", Some("run/1"));

        let response = fixture.submit_raw("something else", Some("run/1"), false);

        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "task_key_conflict");
        assert!(error.error.message.contains(&first.task_id), "{response}");
    }

    #[tokio::test]
    async fn an_empty_key_is_refused() {
        let mut fixture = fixture(AgentState::Idle);

        let response = fixture.submit_raw("fix", Some(""), false);

        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "invalid_params");
    }

    #[tokio::test]
    async fn a_result_file_task_asks_for_its_file_in_the_prompt() {
        let mut fixture = fixture(AgentState::Idle);

        let task = fixture.submit_with("plan the fix", true);

        let path = task.result_path.clone().expect("result path");
        let folder = std::path::Path::new(&path).parent().expect("result folder");
        assert_eq!(folder.parent(), Some(std::env::temp_dir().as_path()));
        let name = folder.file_name().unwrap().to_string_lossy();
        assert!(name.starts_with("herdr-task-"), "{name}");
        assert!(path.ends_with(&format!("{}.md", task.task_id)), "{path}");
        let written = String::from_utf8_lossy(&fixture.written_text()).into_owned();
        assert!(written.starts_with("plan the fix"), "{written:?}");
        assert!(written.contains(&path), "{written:?}");
        assert_eq!(task.text, "plan the fix");
    }

    #[tokio::test]
    async fn the_result_of_an_unfinished_task_is_refused() {
        let mut fixture = fixture(AgentState::Idle);
        let task = fixture.submit("fix");

        let error = fixture.result(&task.task_id).unwrap_err();

        assert_eq!(error.error.code, "task_not_finished");
    }

    #[tokio::test]
    async fn the_result_of_an_unknown_task_is_not_found() {
        let mut fixture = fixture(AgentState::Idle);

        let error = fixture.result("t_404").unwrap_err();

        assert_eq!(error.error.code, "task_not_found");
    }

    #[tokio::test]
    async fn a_finished_task_returns_the_text_of_its_result_file() {
        let mut fixture = fixture(AgentState::Idle);
        let task = fixture.submit_with("plan", true);
        fixture.agent_becomes(AgentState::Working);
        fixture.agent_becomes(AgentState::Idle);
        let path = std::path::PathBuf::from(task.result_path.clone().unwrap());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "## plan\n1. fix the token check\n").unwrap();

        let result = fixture.result(&task.task_id);
        let _ = std::fs::remove_file(&path);

        let result = result.expect("task result");
        assert_eq!(result.source, TaskResultSource::File);
        assert_eq!(result.text, "## plan\n1. fix the token check\n");
        assert_eq!(result.path.as_deref(), task.result_path.as_deref());
    }

    #[tokio::test]
    async fn a_finished_task_without_a_result_file_returns_the_screen_tail() {
        let mut fixture = fixture(AgentState::Idle);
        let task = fixture.submit("fix");
        fixture.agent_becomes(AgentState::Working);
        fixture.agent_becomes(AgentState::Idle);

        let result = fixture.result(&task.task_id).expect("task result");

        assert_eq!(result.source, TaskResultSource::Screen);
        assert_eq!(result.path, None);
    }

    #[tokio::test]
    async fn a_missing_result_file_falls_back_to_the_screen_tail() {
        let mut fixture = fixture(AgentState::Idle);
        let task = fixture.submit_with("plan", true);
        fixture.agent_becomes(AgentState::Working);
        fixture.agent_becomes(AgentState::Idle);

        let result = fixture.result(&task.task_id).expect("task result");

        assert_eq!(result.source, TaskResultSource::Screen);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_local_result_folder_is_made_for_its_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let mut fixture = fixture(AgentState::Idle);

        let task = fixture.submit_with("plan", true);

        let path = std::path::PathBuf::from(task.result_path.unwrap());
        let folder = path.parent().unwrap();
        let mode = std::fs::metadata(folder).unwrap().permissions().mode() & 0o777;
        let _ = std::fs::remove_dir(folder);
        assert_eq!(mode, 0o700);
    }

    #[tokio::test]
    async fn an_agent_asking_its_own_pane_is_refused_as_a_cycle() {
        let mut fixture = fixture(AgentState::Idle);
        let own = fixture.own_pane();

        let response = fixture.submit_params(TaskSubmitParams {
            target: "reviewer".into(),
            text: "fix".into(),
            key: None,
            result_file: false,
            requested_by: Some(own),
        });

        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "task_cycle");
        assert!(fixture.task_events().is_empty());
    }

    #[tokio::test]
    async fn an_unknown_requester_pane_records_no_requester() {
        let mut fixture = fixture(AgentState::Idle);

        let response = fixture.submit_params(TaskSubmitParams {
            target: "reviewer".into(),
            text: "fix".into(),
            key: None,
            result_file: false,
            requested_by: Some("w_missing:p_9".into()),
        });

        let task = Fixture::task_of(&response);
        assert_eq!(task.requester_terminal_id, None);
        assert_eq!(task.parent_task_id, None);
    }
}
