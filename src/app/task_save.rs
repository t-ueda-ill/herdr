//! Writes the task table to `tasks.json` off the app thread, a moment after
//! it changes, and once more on shutdown and before a handoff.

use std::time::{Duration, Instant};

use super::App;
use crate::persist::tasks::LoadedTasks;
use crate::tasks::record::Unfinished;

/// Bounds how late a change reaches the file; one write covers a burst.
const TASK_SAVE_DEBOUNCE: Duration = Duration::from_secs(1);

#[derive(Debug, Default)]
pub(crate) struct TaskSave {
    pub(crate) deadline: Option<Instant>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// False when sessions are not persisted, or a newer herdr owns the file.
    enabled: bool,
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
        LoadedTasks::Newer => (crate::tasks::TaskTable::default(), TaskSave::default()),
    }
}

impl App {
    pub(crate) fn schedule_task_save(&mut self) {
        if self.task_save.enabled {
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
        if !self.task_save.enabled {
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
        if self.task_save.enabled {
            write_task_file(&self.state.tasks.to_file());
        }
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

    #[tokio::test]
    async fn a_kept_result_schedules_a_save() {
        let _home = ConfigHome::new("schedule");
        let mut app = app(PERSIST);
        let id = finish_one_task(&mut app, "plan");

        app.keep_task_result(id, "the plan".into());

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
