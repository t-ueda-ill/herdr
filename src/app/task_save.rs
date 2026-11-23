//! Writes the task table to `tasks.json` off the app thread, a moment after
//! it changes, and once more on shutdown and before a handoff.

use std::time::{Duration, Instant};

use super::App;
use crate::persist::tasks::LoadedTasks;
use crate::tasks::record::Unfinished;

/// Bounds how late a change reaches the file; one write covers a burst.
const TASK_SAVE_DEBOUNCE: Duration = Duration::from_secs(1);
/// How long the last save waits for result copies still being read.
const CAPTURE_WAIT: Duration = Duration::from_secs(2);

#[derive(Debug, Default)]
pub(crate) struct TaskSave {
    pub(crate) deadline: Option<Instant>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// False when sessions are not persisted, or a newer herdr owns the file.
    enabled: bool,
    /// A handoff saved the table for the replacement, which owns the file
    /// from then on unless the handoff rolls back.
    handed_off: bool,
    /// Sessions persist, but the file is kept as it is: a newer herdr wrote
    /// it, or it could not be read.
    // Why allowed: only the Unix live handoff reads it; loading sets it on
    // every platform.
    #[cfg_attr(not(unix), allow(dead_code))]
    kept: bool,
}

impl TaskSave {
    fn writable(&self) -> bool {
        self.enabled && !self.handed_off
    }
}

/// The table to start with, and whether later changes may be written.
pub(crate) fn load_task_table(
    persist: bool,
    carry_unfinished: bool,
) -> (crate::tasks::TaskTable, TaskSave) {
    if !persist {
        return (crate::tasks::TaskTable::default(), TaskSave::default());
    }
    let now = crate::tasks::Now {
        at: Instant::now(),
        unix_ms: crate::tasks::unix_ms(),
    };
    match crate::persist::tasks::load_tasks() {
        LoadedTasks::Loaded(file) => (
            crate::tasks::TaskTable::from_file(
                file,
                now,
                if carry_unfinished {
                    Unfinished::Carry
                } else {
                    Unfinished::Lose
                },
            ),
            TaskSave {
                enabled: true,
                // Why save at once: after a restart, tasks that were running are lost now.
                deadline: Some(Instant::now()),
                ..TaskSave::default()
            },
        ),
        LoadedTasks::Empty => (
            crate::tasks::TaskTable::default(),
            TaskSave {
                enabled: true,
                ..TaskSave::default()
            },
        ),
        LoadedTasks::Newer | LoadedTasks::Unreadable => (
            crate::tasks::TaskTable::default(),
            TaskSave {
                kept: true,
                ..TaskSave::default()
            },
        ),
    }
}

impl App {
    pub(crate) fn schedule_task_save(&mut self) {
        if self.task_save.writable() {
            self.task_save
                .deadline
                .get_or_insert_with(|| Instant::now() + TASK_SAVE_DEBOUNCE);
        }
    }

    pub(crate) fn start_background_task_save(&mut self) {
        if self
            .task_save
            .thread
            .as_ref()
            .is_some_and(std::thread::JoinHandle::is_finished)
        {
            if let Some(thread) = self.task_save.thread.take() {
                let _ = thread.join();
            }
        }
        if self.task_save.thread.is_some() {
            self.task_save.deadline = Some(Instant::now() + Duration::from_millis(250));
            return;
        }
        self.task_save.deadline = None;
        if !self.task_save.writable() {
            return;
        }
        let file = self.state.tasks.to_file();
        match std::thread::Builder::new()
            .name("herdr-task-save".into())
            .spawn(move || write_task_file(&file))
        {
            Ok(thread) => self.task_save.thread = Some(thread),
            Err(err) => {
                tracing::warn!(err = %err, "failed to spawn task save thread; saving inline");
                write_task_file(&self.state.tasks.to_file());
            }
        }
    }

    /// Waits for a running write, then writes the table as it is now.
    pub(crate) fn save_tasks_now(&mut self) {
        if let Some(thread) = self.task_save.thread.take() {
            let _ = thread.join();
        }
        self.task_save.deadline = None;
        if self.task_save.writable() {
            write_task_file(&self.state.tasks.to_file());
        }
    }

    /// Lets copies in progress land before the last save. Call it before
    /// the panes are frozen or the session is saved: it handles events,
    /// which can open or close panes.
    pub(crate) fn settle_result_captures(&mut self) {
        if self.task_save.writable() {
            self.wait_for_result_captures(CAPTURE_WAIT);
        }
    }

    #[cfg(unix)]
    /// Writes the table for the replacement server and stops later writes,
    /// so this server cannot overwrite what the replacement saves.
    /// Why no wait for copies here: handling events now could change panes
    /// that the transfer snapshot already holds.
    pub(crate) fn save_tasks_for_handoff(&mut self) -> std::io::Result<()> {
        // Why refuse: the replacement reads tasks only from the file, so
        // unfinished ones would be lost with this server.
        if self.task_save.kept
            && self
                .state
                .tasks
                .iter()
                .any(|task| !task.state.is_finished())
        {
            return Err(std::io::Error::other(
                "unfinished tasks cannot be handed over: the task file is kept as it is",
            ));
        }
        if let Some(thread) = self.task_save.thread.take() {
            let _ = thread.join();
        }
        self.task_save.deadline = None;
        if self.task_save.writable() {
            crate::persist::tasks::save_tasks(&self.state.tasks.to_file())?;
        }
        self.task_save.handed_off = true;
        Ok(())
    }

    #[cfg(unix)]
    /// The handoff rolled back: this server owns the file again.
    pub(crate) fn resume_task_saving(&mut self) {
        self.task_save.handed_off = false;
        self.schedule_task_save();
    }

    /// The last save before the server exits. Why no wait for copies here:
    /// `settle_result_captures` runs before the session save instead.
    pub(crate) fn save_tasks_on_shutdown(&mut self) {
        self.save_tasks_now();
    }
}

fn write_task_file(file: &crate::tasks::record::TaskFile) {
    if let Err(err) = crate::persist::tasks::save_tasks(file) {
        tracing::warn!(err = %err, "failed to save the task file");
    }
}

#[cfg(test)]
mod tests {
    use crate::api::schema::{AgentStatus, Location, TaskState};
    use crate::app::{App, AppPolicy};
    use crate::config::Config;
    use crate::events::AppEvent;
    use crate::tasks::{AgentSnapshot, NewTask, Now, ResultPlace};

    const PERSIST: AppPolicy = AppPolicy {
        restore_session: false,
        persist_session: true,
        persist_plugin_registry: false,
        background_updates: false,
        carry_unfinished_tasks: false,
    };

    struct ConfigHome {
        path: std::path::PathBuf,
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    impl ConfigHome {
        fn new(name: &str) -> Self {
            let guard = crate::config::test_config_env_lock()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let path =
                std::env::temp_dir().join(format!("herdr-task-save-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::env::set_var("XDG_CONFIG_HOME", &path);
            std::env::remove_var(crate::session::SESSION_ENV_VAR);
            Self {
                path,
                _guard: guard,
            }
        }
    }

    impl Drop for ConfigHome {
        fn drop(&mut self) {
            std::env::remove_var("XDG_CONFIG_HOME");
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn app(policy: AppPolicy) -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(
            &Config::default(),
            policy,
            None,
            api_rx,
            crate::api::EventHub::default(),
        )
    }

    fn now() -> Now {
        Now {
            at: std::time::Instant::now(),
            unix_ms: 1_000_000,
        }
    }

    fn agent(status: AgentStatus, seq: u64) -> AgentSnapshot {
        AgentSnapshot {
            status,
            seq,
            hook: true,
        }
    }

    /// One task that an idle agent took and finished.
    fn finish_one_task(app: &mut App, text: &str) -> crate::tasks::TaskId {
        let terminal: crate::terminal::TerminalId =
            serde_json::from_value(serde_json::json!("term_saved")).expect("terminal id");
        let (id, _) = app.state.tasks.submit(
            NewTask {
                terminal: terminal.clone(),
                target: "reviewer".into(),
                location: Location::new(None, None),
                text: text.into(),
                key: None,
                result_dir: None,
                requester: None,
                result_place: ResultPlace::Host,
            },
            agent(AgentStatus::Idle, 1),
            now(),
        );
        app.state
            .tasks
            .on_agent_status(&terminal, agent(AgentStatus::Working, 2), now());
        app.state
            .tasks
            .on_agent_status(&terminal, agent(AgentStatus::Idle, 3), now());
        id
    }

    #[tokio::test]
    async fn a_new_server_reads_the_tasks_the_last_one_saved() {
        let _home = ConfigHome::new("roundtrip");
        let mut first = app(PERSIST);
        let id = finish_one_task(&mut first, "review PR 75");
        first.save_tasks_now();

        let second = app(PERSIST);

        let task = second.state.tasks.get(id).expect("restored task");
        assert_eq!(task.text, "review PR 75");
        assert_eq!(task.state, TaskState::Completed);
    }

    /// One task that an idle agent took and still works on.
    fn start_one_task(app: &mut App) -> crate::tasks::TaskId {
        let terminal: crate::terminal::TerminalId =
            serde_json::from_value(serde_json::json!("term_running")).expect("terminal id");
        let (id, _) = app.state.tasks.submit(
            NewTask {
                terminal: terminal.clone(),
                target: "reviewer".into(),
                location: Location::new(None, None),
                text: "fix".into(),
                key: None,
                result_dir: None,
                requester: None,
                result_place: ResultPlace::Host,
            },
            agent(AgentStatus::Idle, 1),
            now(),
        );
        app.state
            .tasks
            .on_agent_status(&terminal, agent(AgentStatus::Working, 2), now());
        id
    }

    #[tokio::test]
    async fn a_restarted_server_marks_a_running_task_lost() {
        let _home = ConfigHome::new("restart-running");
        let mut first = app(PERSIST);
        let id = start_one_task(&mut first);
        first.save_tasks_now();

        let second = app(PERSIST);

        assert_eq!(
            second.state.tasks.get(id).expect("task").state,
            TaskState::Lost
        );
    }

    #[tokio::test]
    async fn a_handoff_replacement_keeps_a_running_task_running() {
        let _home = ConfigHome::new("handoff-running");
        let mut first = app(PERSIST);
        let id = start_one_task(&mut first);
        first.save_tasks_now();

        let second = app(AppPolicy {
            carry_unfinished_tasks: true,
            ..PERSIST
        });

        assert_eq!(
            second.state.tasks.get(id).expect("task").state,
            TaskState::Running
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_handoff_save_that_fails_is_reported() {
        let _home = ConfigHome::new("handoff-fails");
        let path = crate::persist::tasks::tasks_path();
        let mut app = app(PERSIST);
        finish_one_task(&mut app, "plan");
        // A folder where the file belongs makes the rename fail.
        std::fs::create_dir_all(&path).unwrap();

        assert!(app.save_tasks_for_handoff().is_err());
    }

    #[tokio::test]
    async fn an_unreadable_task_file_is_never_overwritten() {
        let _home = ConfigHome::new("unreadable");
        let path = crate::persist::tasks::tasks_path();
        std::fs::create_dir_all(&path).unwrap();
        let mut app = app(PERSIST);
        finish_one_task(&mut app, "plan");

        app.save_tasks_on_shutdown();

        assert!(path.is_dir());
        assert!(!app.task_save.writable());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_handoff_with_unfinished_tasks_is_refused_while_the_file_is_kept() {
        let _home = ConfigHome::new("handoff-kept");
        let path = crate::persist::tasks::tasks_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"version":99,"next_id":1,"tasks":[]}"#).unwrap();
        let mut app = app(PERSIST);
        start_one_task(&mut app);

        assert!(app.save_tasks_for_handoff().is_err());
    }

    #[tokio::test]
    async fn the_shutdown_save_leaves_other_events_for_later() {
        let _home = ConfigHome::new("shutdown-events");
        let mut app = app(PERSIST);
        let id = finish_one_task(&mut app, "plan");
        app.task_captures.enqueue(id);
        let _ = app.task_captures.next_to_start();
        app.task_captures.started();
        app.event_tx
            .try_send(AppEvent::TerminalBell {
                pane_id: crate::layout::PaneId::from_raw(9999),
                count: 1,
            })
            .unwrap();

        app.save_tasks_on_shutdown();

        assert!(matches!(
            app.event_rx.try_recv(),
            Ok(AppEvent::TerminalBell { .. })
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_handoff_save_leaves_other_events_for_later() {
        let _home = ConfigHome::new("handoff-events");
        let mut app = app(PERSIST);
        let id = finish_one_task(&mut app, "plan");
        app.task_captures.enqueue(id);
        let _ = app.task_captures.next_to_start();
        app.task_captures.started();
        app.event_tx
            .try_send(AppEvent::TerminalBell {
                pane_id: crate::layout::PaneId::from_raw(9999),
                count: 1,
            })
            .unwrap();

        app.save_tasks_for_handoff().unwrap();

        assert!(matches!(
            app.event_rx.try_recv(),
            Ok(AppEvent::TerminalBell { .. })
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn after_a_handoff_this_server_writes_nothing_more() {
        let _home = ConfigHome::new("handoff-owner");
        let mut app = app(PERSIST);
        finish_one_task(&mut app, "first");
        app.save_tasks_for_handoff().unwrap();
        let handed = std::fs::read_to_string(crate::persist::tasks::tasks_path()).unwrap();

        finish_one_task(&mut app, "second");
        app.schedule_task_save();
        app.save_tasks_on_shutdown();

        assert!(app.task_save.deadline.is_none());
        assert_eq!(
            std::fs::read_to_string(crate::persist::tasks::tasks_path()).unwrap(),
            handed
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_rolled_back_handoff_saves_again() {
        let _home = ConfigHome::new("handoff-rollback");
        let mut app = app(PERSIST);
        finish_one_task(&mut app, "first");
        app.save_tasks_for_handoff().unwrap();

        app.resume_task_saving();
        let second = finish_one_task(&mut app, "second");
        app.save_tasks_now();

        let reloaded = super::super::App::new(
            &Config::default(),
            PERSIST,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        assert!(reloaded.state.tasks.get(second).is_some());
    }

    #[tokio::test]
    async fn a_kept_result_schedules_a_save() {
        let _home = ConfigHome::new("schedule");
        let mut app = app(PERSIST);
        let id = finish_one_task(&mut app, "plan");

        app.keep_task_result(id, Ok(b"the plan".to_vec()));

        assert!(app.task_save.deadline.is_some());
    }

    #[tokio::test]
    async fn without_session_persistence_nothing_is_read_or_saved() {
        let home = ConfigHome::new("off");
        let mut first = app(PERSIST);
        finish_one_task(&mut first, "plan");
        first.save_tasks_now();

        let mut second = app(AppPolicy::TEST);
        second.schedule_task_save();

        assert_eq!(second.state.tasks.iter().count(), 0);
        assert!(second.task_save.deadline.is_none());
        drop(home);
    }

    #[tokio::test]
    async fn a_file_from_a_newer_herdr_is_left_as_it_is() {
        let _home = ConfigHome::new("newer");
        let path = crate::persist::tasks::tasks_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let newer = r#"{"version":99,"next_id":1,"tasks":[]}"#;
        std::fs::write(&path, newer).unwrap();

        let mut app = app(PERSIST);
        finish_one_task(&mut app, "plan");
        app.save_tasks_now();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), newer);
    }
}
