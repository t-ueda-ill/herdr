use std::time::{Duration, Instant};

use super::{SavedResult, TaskFile, Unfinished, TASK_FILE_VERSION};
use crate::api::schema::{AgentStatus, Location, TaskState};
use crate::tasks::{AgentSnapshot, Effect, NewTask, Now, ResultPlace, TaskId, TaskTable};
use crate::terminal::TerminalId;

fn now(millis: u64) -> Now {
    Now {
        at: Instant::now() + Duration::from_millis(millis),
        unix_ms: 1_000_000 + millis,
    }
}

fn terminal(name: &str) -> TerminalId {
    serde_json::from_value(serde_json::json!(name)).expect("terminal id")
}

fn task(terminal_name: &str, text: &str) -> NewTask {
    NewTask {
        terminal: terminal(terminal_name),
        target: "reviewer".into(),
        location: Location::new(None, None),
        text: text.into(),
        key: None,
        result_dir: None,
        requester: None,
        result_place: ResultPlace::Host,
    }
}

fn agent(status: AgentStatus, seq: u64) -> AgentSnapshot {
    AgentSnapshot {
        status,
        seq,
        hook: true,
    }
}

/// Sends one task to an idle agent and lets the agent finish it.
fn completed(table: &mut TaskTable, new: NewTask) -> TaskId {
    let terminal = new.terminal.clone();
    let (id, _) = table.submit(new, agent(AgentStatus::Idle, 1), now(0));
    table.on_agent_status(&terminal, agent(AgentStatus::Working, 2), now(10));
    table.on_agent_status(&terminal, agent(AgentStatus::Idle, 3), now(20));
    id
}

/// Through JSON, as the file on disk.
fn reload(table: &TaskTable) -> TaskTable {
    let json = serde_json::to_string(&table.to_file()).expect("encode");
    let file: TaskFile = serde_json::from_str(&json).expect("decode");
    TaskTable::from_file(file, now(5_000), Unfinished::Lose)
}

#[test]
fn a_finished_task_comes_back_with_its_request_state_and_results() {
    let mut table = TaskTable::default();
    let id = completed(&mut table, task("a", "review PR 75"));
    table.save_screen_tail(id, "screen end".into());
    table.save_result(
        id,
        SavedResult {
            text: "VERDICT: approve".into(),
            truncated: false,
        },
    );

    let restored = reload(&table);

    let task = restored.get(id).expect("restored task");
    assert_eq!(task.text, "review PR 75");
    assert_eq!(task.state, TaskState::Completed);
    assert_eq!(task.terminal, terminal("a"));
    assert_eq!(task.finished_at_ms, Some(1_000_020));
    assert_eq!(task.screen_tail.as_deref(), Some("screen end"));
    assert_eq!(
        task.saved_result
            .as_ref()
            .map(|result| result.text.as_str()),
        Some("VERDICT: approve")
    );
}

#[test]
fn the_requester_and_parent_task_come_back() {
    let mut table = TaskTable::default();
    let (parent, _) = table.submit(task("chief", "plan"), agent(AgentStatus::Idle, 1), now(0));
    let child = completed(
        &mut table,
        NewTask {
            requester: Some(terminal("chief")),
            ..task("a", "review")
        },
    );

    let restored = reload(&table);

    let child = restored.get(child).expect("child");
    assert_eq!(child.requester, Some(terminal("chief")));
    assert_eq!(child.parent, Some(parent));
}

#[test]
fn a_task_that_had_not_finished_comes_back_lost_and_is_not_sent_again() {
    let mut table = TaskTable::default();
    let (running, _) = table.submit(task("a", "fix"), agent(AgentStatus::Idle, 1), now(0));
    table.on_agent_status(&terminal("a"), agent(AgentStatus::Working, 2), now(10));
    let (queued, _) = table.submit(
        task("a", "then this"),
        agent(AgentStatus::Working, 2),
        now(20),
    );

    let mut restored = reload(&table);

    for id in [running, queued] {
        let task = restored.get(id).expect("restored task");
        assert_eq!(task.state, TaskState::Lost);
        assert_eq!(
            task.reason.as_deref(),
            Some("the server stopped before the task finished")
        );
        assert_eq!(task.finished_at_ms, Some(1_005_000));
    }
    let (next, effects) =
        restored.submit(task("a", "new"), agent(AgentStatus::Idle, 1), now(6_000));
    assert_eq!(effects, vec![Effect::Submit(next), Effect::Emit(next)]);
}

#[test]
fn task_numbers_continue_after_the_saved_tasks() {
    let mut table = TaskTable::default();
    let first = completed(&mut table, task("a", "one"));

    let mut restored = reload(&table);
    let (next, _) = restored.submit(task("a", "two"), agent(AgentStatus::Idle, 1), now(6_000));

    assert!(next > first);
}

#[test]
fn a_retry_with_a_saved_key_returns_the_saved_task() {
    let mut table = TaskTable::default();
    let id = completed(
        &mut table,
        NewTask {
            key: Some("run42/review".into()),
            ..task("a", "review")
        },
    );

    let mut restored = reload(&table);
    let (again, effects) = restored
        .submit_idempotent(
            NewTask {
                key: Some("run42/review".into()),
                ..task("a", "review")
            },
            agent(AgentStatus::Idle, 1),
            now(6_000),
        )
        .expect("same key and text");

    assert_eq!(again, id);
    assert!(effects.is_empty());
}

#[test]
fn an_off_host_result_file_is_not_read_again_after_a_restart() {
    let mut table = TaskTable::default();
    let environment = crate::offhost::PaneEnvironment::Ssh(
        crate::ssh_host::SshLaunch::new("devbox".into(), None).expect("ssh launch"),
    );
    let id = completed(
        &mut table,
        NewTask {
            result_dir: Some("/tmp/herdr-task-x".into()),
            result_place: ResultPlace::OffHost(Box::new(environment)),
            ..task("a", "review")
        },
    );

    let restored = reload(&table);

    let task = restored.get(id).expect("restored task");
    assert_eq!(task.result_place, ResultPlace::Unreachable);
    assert_eq!(
        task.result_path.as_deref(),
        Some("/tmp/herdr-task-x/t_1.md")
    );
}

#[test]
fn a_file_with_more_tasks_than_the_limit_keeps_the_newest() {
    let mut table = TaskTable::default();
    let ids: Vec<TaskId> = (0..crate::tasks::FINISHED_TASKS_KEPT + 3)
        .map(|n| completed(&mut table, task("a", &format!("task {n}"))))
        .collect();
    let mut file = table.to_file();
    assert_eq!(file.tasks.len(), crate::tasks::FINISHED_TASKS_KEPT);
    // An older herdr with a higher limit can leave more in the file.
    let mut older = file.tasks[0].clone();
    older.id = 0;
    file.tasks.insert(0, older);

    let restored = TaskTable::from_file(file, now(9_000), Unfinished::Lose);

    assert_eq!(restored.iter().count(), crate::tasks::FINISHED_TASKS_KEPT);
    assert!(restored.get(*ids.last().expect("ids")).is_some());
}

#[test]
fn the_file_records_its_version() {
    assert_eq!(TaskTable::default().to_file().version, TASK_FILE_VERSION);
}

#[test]
fn a_long_result_is_cut_at_the_limit_on_a_character_boundary() {
    let bytes = "あ".repeat(super::SAVED_RESULT_LIMIT).into_bytes(); // 3 bytes each

    let saved = SavedResult::from_prefix(bytes);

    assert!(saved.truncated);
    assert!(saved.text.len() <= super::SAVED_RESULT_LIMIT);
    assert!(saved.text.chars().all(|c| c == 'あ'));
}

#[test]
fn a_result_of_exactly_the_limit_is_not_truncated() {
    let saved = SavedResult::from_prefix(vec![b'a'; super::SAVED_RESULT_LIMIT]);

    assert!(!saved.truncated);
    assert_eq!(saved.text.len(), super::SAVED_RESULT_LIMIT);
}

#[test]
fn bytes_that_are_not_utf8_are_kept_as_replacement_characters() {
    let saved = SavedResult::from_prefix(vec![b'o', b'k', 0xff]);

    assert_eq!(saved.text, "ok\u{fffd}");
}

#[test]
fn a_short_result_is_kept_whole() {
    let saved = SavedResult::from_prefix(b"ok".to_vec());

    assert_eq!(
        saved,
        SavedResult {
            text: "ok".into(),
            truncated: false
        }
    );
}

/// Through JSON, as a handoff replacement server reads it.
fn hand_off(table: &TaskTable) -> TaskTable {
    let json = serde_json::to_string(&table.to_file()).expect("encode");
    let file: TaskFile = serde_json::from_str(&json).expect("decode");
    TaskTable::from_file(file, now(5_000), Unfinished::Carry)
}

#[test]
fn a_handoff_keeps_a_running_task_running_until_the_agent_finishes() {
    let mut table = TaskTable::default();
    let (id, _) = table.submit(task("a", "fix"), agent(AgentStatus::Idle, 1), now(0));
    table.on_agent_status(&terminal("a"), agent(AgentStatus::Working, 2), now(10));

    let mut carried = hand_off(&table);

    assert_eq!(carried.get(id).expect("task").state, TaskState::Running);
    carried.on_agent_status(&terminal("a"), agent(AgentStatus::Idle, 1), now(6_000));
    assert_eq!(carried.get(id).expect("task").state, TaskState::Completed);
}

#[test]
fn a_handoff_sends_a_queued_task_once_the_agent_is_free() {
    let mut table = TaskTable::default();
    let (first, _) = table.submit(task("a", "fix"), agent(AgentStatus::Idle, 1), now(0));
    table.on_agent_status(&terminal("a"), agent(AgentStatus::Working, 2), now(10));
    let (queued, _) = table.submit(
        task("a", "then this"),
        agent(AgentStatus::Working, 2),
        now(20),
    );

    let mut carried = hand_off(&table);

    assert_eq!(carried.get(queued).expect("task").state, TaskState::Queued);
    let effects = carried.on_agent_status(&terminal("a"), agent(AgentStatus::Idle, 1), now(6_000));
    assert_eq!(
        carried.get(first).expect("task").state,
        TaskState::Completed
    );
    assert!(effects.contains(&Effect::Submit(queued)));
}

#[test]
fn a_handoff_gives_a_submitted_task_a_new_acceptance_window() {
    let mut table = TaskTable::default();
    let (id, _) = table.submit(task("a", "fix"), agent(AgentStatus::Idle, 1), now(0));

    let mut carried = hand_off(&table);

    assert_eq!(carried.get(id).expect("task").state, TaskState::Submitted);
    let later = Now {
        at: now(5_000).at + crate::tasks::ACCEPT_TIMEOUT + Duration::from_millis(100),
        unix_ms: 1_010_000,
    };
    carried.on_tick(later);
    assert_eq!(carried.get(id).expect("task").state, TaskState::Stalled);
}

#[test]
fn a_handoff_does_not_count_running_tasks_against_the_finished_limit() {
    let mut table = TaskTable::default();
    for n in 0..crate::tasks::FINISHED_TASKS_KEPT {
        completed(&mut table, task("b", &format!("task {n}")));
    }
    let (running, _) = table.submit(task("a", "fix"), agent(AgentStatus::Idle, 1), now(0));
    table.on_agent_status(&terminal("a"), agent(AgentStatus::Working, 2), now(10));

    let carried = hand_off(&table);

    assert_eq!(
        carried.get(running).expect("task").state,
        TaskState::Running
    );
    assert_eq!(
        carried.iter().count(),
        crate::tasks::FINISHED_TASKS_KEPT + 1
    );
}
