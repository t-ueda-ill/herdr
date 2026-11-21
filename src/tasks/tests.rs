use std::time::{Duration, Instant};

use super::{
    AgentSnapshot, CancelError, Effect, KeyConflict, NewTask, Now, TaskId, TaskTable,
    ACCEPT_TIMEOUT, FINISHED_TASKS_KEPT,
};
use crate::api::schema::{AgentStatus, Location, TaskEvidence, TaskState};
use crate::terminal::TerminalId;

struct Clock {
    start: Instant,
}

impl Clock {
    fn new() -> Self {
        Self {
            start: Instant::now(),
        }
    }

    fn at(&self, millis: u64) -> Now {
        Now {
            at: self.start + Duration::from_millis(millis),
            unix_ms: 1_000_000 + millis,
        }
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
        result_place: super::ResultPlace::Host,
    }
}

fn asked_by(terminal_name: &str, text: &str, requester: &str) -> NewTask {
    NewTask {
        requester: Some(terminal(requester)),
        ..task(terminal_name, text)
    }
}

fn keyed(terminal_name: &str, text: &str, key: &str) -> NewTask {
    NewTask {
        key: Some(key.into()),
        ..task(terminal_name, text)
    }
}

fn agent(status: AgentStatus, seq: u64) -> AgentSnapshot {
    AgentSnapshot {
        status,
        seq,
        hook: false,
    }
}

fn state(table: &TaskTable, id: TaskId) -> TaskState {
    table.get(id).expect("task").state
}

#[test]
fn a_task_for_an_idle_agent_is_sent_at_once() {
    let clock = Clock::new();
    let mut table = TaskTable::default();

    let (id, effects) = table.submit(task("a", "fix"), agent(AgentStatus::Idle, 3), clock.at(0));

    assert_eq!(effects, vec![Effect::Submit(id), Effect::Emit(id)]);
    let task = table.get(id).unwrap();
    assert_eq!(task.state, TaskState::Submitted);
    assert_eq!(task.submitted_at_ms, Some(1_000_000));
}

#[test]
fn a_task_for_a_done_agent_is_sent_at_once() {
    let clock = Clock::new();
    let mut table = TaskTable::default();

    let (id, _) = table.submit(task("a", "fix"), agent(AgentStatus::Done, 3), clock.at(0));

    assert_eq!(state(&table, id), TaskState::Submitted);
}

#[test]
fn a_task_for_a_working_agent_waits_in_the_queue() {
    let clock = Clock::new();
    let mut table = TaskTable::default();

    let (id, effects) = table.submit(
        task("a", "fix"),
        agent(AgentStatus::Working, 3),
        clock.at(0),
    );

    assert_eq!(effects, vec![Effect::Emit(id)]);
    assert_eq!(state(&table, id), TaskState::Queued);
}

#[test]
fn a_second_task_waits_behind_the_first_even_when_the_agent_is_idle() {
    let clock = Clock::new();
    let mut table = TaskTable::default();
    table.submit(task("a", "one"), agent(AgentStatus::Idle, 3), clock.at(0));

    let (second, effects) =
        table.submit(task("a", "two"), agent(AgentStatus::Idle, 3), clock.at(10));

    assert_eq!(effects, vec![Effect::Emit(second)]);
    assert_eq!(state(&table, second), TaskState::Queued);
}

#[test]
fn working_after_the_send_marks_the_task_running_with_screen_evidence() {
    let clock = Clock::new();
    let mut table = TaskTable::default();
    let (id, _) = table.submit(task("a", "fix"), agent(AgentStatus::Idle, 3), clock.at(0));

    let effects =
        table.on_agent_status(&terminal("a"), agent(AgentStatus::Working, 4), clock.at(50));

    assert_eq!(effects, vec![Effect::Emit(id)]);
    let task = table.get(id).unwrap();
    assert_eq!(task.state, TaskState::Running);
    assert_eq!(task.evidence, Some(TaskEvidence::Screen));
}

#[test]
fn a_hook_reported_state_is_recorded_as_hook_evidence() {
    let clock = Clock::new();
    let mut table = TaskTable::default();
    let (id, _) = table.submit(task("a", "fix"), agent(AgentStatus::Idle, 3), clock.at(0));

    table.on_agent_status(
        &terminal("a"),
        AgentSnapshot {
            status: AgentStatus::Working,
            seq: 4,
            hook: true,
        },
        clock.at(50),
    );

    assert_eq!(table.get(id).unwrap().evidence, Some(TaskEvidence::Hook));
}

#[test]
fn a_state_change_from_before_the_send_does_not_accept_the_task() {
    let clock = Clock::new();
    let mut table = TaskTable::default();
    let (id, _) = table.submit(task("a", "fix"), agent(AgentStatus::Idle, 3), clock.at(0));

    let effects =
        table.on_agent_status(&terminal("a"), agent(AgentStatus::Working, 3), clock.at(50));

    assert!(effects.is_empty());
    assert_eq!(state(&table, id), TaskState::Submitted);
}

#[test]
fn blocked_after_the_send_needs_input_and_work_resumes_it() {
    let clock = Clock::new();
    let mut table = TaskTable::default();
    let (id, _) = table.submit(task("a", "fix"), agent(AgentStatus::Idle, 3), clock.at(0));

    table.on_agent_status(&terminal("a"), agent(AgentStatus::Blocked, 4), clock.at(50));
    assert_eq!(state(&table, id), TaskState::NeedsInput);
    assert!(table.get(id).unwrap().evidence.is_some());

    table.on_agent_status(&terminal("a"), agent(AgentStatus::Working, 5), clock.at(90));
    assert_eq!(state(&table, id), TaskState::Running);
}

#[test]
fn idle_or_done_after_the_task_was_taken_completes_it() {
    for end in [AgentStatus::Idle, AgentStatus::Done] {
        let clock = Clock::new();
        let mut table = TaskTable::default();
        let (id, _) = table.submit(task("a", "fix"), agent(AgentStatus::Idle, 3), clock.at(0));
        table.on_agent_status(&terminal("a"), agent(AgentStatus::Working, 4), clock.at(50));

        table.on_agent_status(&terminal("a"), agent(end, 5), clock.at(900));

        let task = table.get(id).unwrap();
        assert_eq!(task.state, TaskState::Completed, "{end:?}");
        assert_eq!(task.finished_at_ms, Some(1_000_900));
    }
}

#[test]
fn an_agent_that_answers_its_question_and_stops_completes_the_task() {
    let clock = Clock::new();
    let mut table = TaskTable::default();
    let (id, _) = table.submit(task("a", "fix"), agent(AgentStatus::Idle, 3), clock.at(0));
    table.on_agent_status(&terminal("a"), agent(AgentStatus::Blocked, 4), clock.at(50));

    table.on_agent_status(&terminal("a"), agent(AgentStatus::Idle, 5), clock.at(90));

    assert_eq!(state(&table, id), TaskState::Completed);
}

#[test]
fn completion_sends_the_next_queued_task() {
    let clock = Clock::new();
    let mut table = TaskTable::default();
    let (first, _) = table.submit(task("a", "one"), agent(AgentStatus::Idle, 3), clock.at(0));
    let (second, _) = table.submit(task("a", "two"), agent(AgentStatus::Idle, 3), clock.at(10));
    table.on_agent_status(&terminal("a"), agent(AgentStatus::Working, 4), clock.at(50));

    let effects = table.on_agent_status(&terminal("a"), agent(AgentStatus::Done, 5), clock.at(900));

    assert_eq!(state(&table, first), TaskState::Completed);
    assert_eq!(state(&table, second), TaskState::Submitted);
    assert!(effects.contains(&Effect::Emit(first)));
    assert!(effects.contains(&Effect::Submit(second)));
    assert!(effects.contains(&Effect::Emit(second)));
}

#[test]
fn the_next_task_needs_its_own_state_change_to_be_taken() {
    let clock = Clock::new();
    let mut table = TaskTable::default();
    table.submit(task("a", "one"), agent(AgentStatus::Idle, 3), clock.at(0));
    let (second, _) = table.submit(task("a", "two"), agent(AgentStatus::Idle, 3), clock.at(10));
    table.on_agent_status(&terminal("a"), agent(AgentStatus::Working, 4), clock.at(50));
    table.on_agent_status(&terminal("a"), agent(AgentStatus::Idle, 5), clock.at(900));

    table.on_agent_status(&terminal("a"), agent(AgentStatus::Done, 5), clock.at(950));
    assert_eq!(state(&table, second), TaskState::Submitted);

    table.on_agent_status(
        &terminal("a"),
        agent(AgentStatus::Working, 6),
        clock.at(990),
    );
    assert_eq!(state(&table, second), TaskState::Running);
}

#[test]
fn a_queued_task_is_sent_when_a_busy_agent_becomes_idle() {
    let clock = Clock::new();
    let mut table = TaskTable::default();
    let (id, _) = table.submit(
        task("a", "fix"),
        agent(AgentStatus::Working, 3),
        clock.at(0),
    );

    let effects = table.on_agent_status(&terminal("a"), agent(AgentStatus::Idle, 4), clock.at(700));

    assert_eq!(effects, vec![Effect::Submit(id), Effect::Emit(id)]);
    assert_eq!(state(&table, id), TaskState::Submitted);
}

#[test]
fn a_blocked_agent_keeps_its_queue_waiting() {
    let clock = Clock::new();
    let mut table = TaskTable::default();
    let (id, _) = table.submit(
        task("a", "fix"),
        agent(AgentStatus::Working, 3),
        clock.at(0),
    );

    let effects = table.on_agent_status(
        &terminal("a"),
        agent(AgentStatus::Blocked, 4),
        clock.at(700),
    );

    assert!(effects.is_empty());
    assert_eq!(state(&table, id), TaskState::Queued);
}

#[test]
fn no_reaction_in_the_window_stalls_the_task_and_frees_the_lane() {
    let clock = Clock::new();
    let mut table = TaskTable::default();
    let (first, _) = table.submit(task("a", "one"), agent(AgentStatus::Idle, 3), clock.at(0));
    let (second, _) = table.submit(task("a", "two"), agent(AgentStatus::Idle, 3), clock.at(10));
    let window = ACCEPT_TIMEOUT.as_millis() as u64;
    assert_eq!(table.next_deadline(), Some(clock.at(window).at));

    assert!(table.on_tick(clock.at(window - 1)).is_empty());
    let effects = table.on_tick(clock.at(window));

    let stalled = table.get(first).unwrap();
    assert_eq!(stalled.state, TaskState::Stalled);
    assert!(stalled.reason.is_some());
    assert_eq!(state(&table, second), TaskState::Submitted);
    assert!(effects.contains(&Effect::Submit(second)));
}

#[test]
fn a_late_reaction_does_not_revive_a_stalled_task() {
    let clock = Clock::new();
    let mut table = TaskTable::default();
    let (id, _) = table.submit(task("a", "fix"), agent(AgentStatus::Idle, 3), clock.at(0));
    table.on_tick(clock.at(ACCEPT_TIMEOUT.as_millis() as u64));

    table.on_agent_status(
        &terminal("a"),
        agent(AgentStatus::Working, 4),
        clock.at(6_000),
    );

    assert_eq!(state(&table, id), TaskState::Stalled);
}

#[test]
fn an_accepted_task_has_no_deadline() {
    let clock = Clock::new();
    let mut table = TaskTable::default();
    table.submit(task("a", "fix"), agent(AgentStatus::Idle, 3), clock.at(0));
    table.on_agent_status(&terminal("a"), agent(AgentStatus::Working, 4), clock.at(50));

    assert_eq!(table.next_deadline(), None);
}

#[test]
fn a_failed_write_loses_the_task_and_sends_the_next() {
    let clock = Clock::new();
    let mut table = TaskTable::default();
    let (first, _) = table.submit(task("a", "one"), agent(AgentStatus::Idle, 3), clock.at(0));
    let (second, _) = table.submit(task("a", "two"), agent(AgentStatus::Idle, 3), clock.at(10));

    let effects = table.submit_failed(first, "pty closed".into(), clock.at(20));

    let lost = table.get(first).unwrap();
    assert_eq!(lost.state, TaskState::Lost);
    assert_eq!(lost.reason.as_deref(), Some("pty closed"));
    assert!(effects.contains(&Effect::Submit(second)));
}

#[test]
fn a_gone_terminal_loses_the_active_task_and_cancels_the_queue() {
    let clock = Clock::new();
    let mut table = TaskTable::default();
    let (first, _) = table.submit(task("a", "one"), agent(AgentStatus::Idle, 3), clock.at(0));
    let (second, _) = table.submit(task("a", "two"), agent(AgentStatus::Idle, 3), clock.at(10));

    let effects = table.on_terminal_gone(&terminal("a"), "pane closed", clock.at(20));

    assert_eq!(state(&table, first), TaskState::Lost);
    assert_eq!(state(&table, second), TaskState::Cancelled);
    assert!(!effects
        .iter()
        .any(|effect| matches!(effect, Effect::Submit(_))));
    assert_eq!(
        table.get(first).unwrap().reason.as_deref(),
        Some("pane closed")
    );
}

#[test]
fn only_a_queued_task_can_be_cancelled() {
    let clock = Clock::new();
    let mut table = TaskTable::default();
    let (first, _) = table.submit(task("a", "one"), agent(AgentStatus::Idle, 3), clock.at(0));
    let (second, _) = table.submit(task("a", "two"), agent(AgentStatus::Idle, 3), clock.at(10));

    assert_eq!(
        table.cancel(first, clock.at(20)),
        Err(CancelError::AlreadySent(TaskState::Submitted))
    );
    assert_eq!(
        table.cancel(second, clock.at(20)),
        Ok(vec![Effect::Emit(second)])
    );
    assert_eq!(state(&table, second), TaskState::Cancelled);
    assert_eq!(
        table.cancel(TaskId(999), clock.at(20)),
        Err(CancelError::NotFound)
    );
}

#[test]
fn a_cancelled_task_is_never_sent() {
    let clock = Clock::new();
    let mut table = TaskTable::default();
    let (id, _) = table.submit(
        task("a", "fix"),
        agent(AgentStatus::Working, 3),
        clock.at(0),
    );
    table.cancel(id, clock.at(10)).unwrap();

    let effects = table.on_agent_status(&terminal("a"), agent(AgentStatus::Idle, 4), clock.at(700));

    assert!(effects.is_empty());
}

#[test]
fn another_terminal_does_not_move_the_task() {
    let clock = Clock::new();
    let mut table = TaskTable::default();
    let (id, _) = table.submit(task("a", "fix"), agent(AgentStatus::Idle, 3), clock.at(0));

    let effects =
        table.on_agent_status(&terminal("b"), agent(AgentStatus::Working, 4), clock.at(50));

    assert!(effects.is_empty());
    assert_eq!(state(&table, id), TaskState::Submitted);
}

#[test]
fn only_the_newest_finished_tasks_are_kept() {
    let clock = Clock::new();
    let mut table = TaskTable::default();
    let mut ids = Vec::new();
    for n in 0..=FINISHED_TASKS_KEPT {
        let (id, _) = table.submit(
            task("a", "x"),
            agent(AgentStatus::Working, 3),
            clock.at(n as u64),
        );
        table.cancel(id, clock.at(n as u64)).unwrap();
        ids.push(id);
    }
    let (live, _) = table.submit(
        task("a", "live"),
        agent(AgentStatus::Working, 3),
        clock.at(9_999),
    );

    assert!(table.get(ids[0]).is_none());
    assert!(table.get(ids[1]).is_some());
    assert!(table.get(live).is_some());
    assert_eq!(table.iter().count(), FINISHED_TASKS_KEPT + 1);
}

#[test]
fn task_ids_round_trip_through_their_public_form() {
    let id = TaskId(12);
    assert_eq!(id.public(), "t_12");
    assert_eq!(TaskId::parse("t_12"), Some(id));
    assert_eq!(TaskId::parse("12"), None);
    assert_eq!(TaskId::parse("t_x"), None);
}

#[test]
fn an_agent_in_an_unknown_state_keeps_the_task_queued_until_it_is_idle() {
    let clock = Clock::new();
    let mut table = TaskTable::default();

    let (id, effects) = table.submit(
        task("a", "fix"),
        agent(AgentStatus::Unknown, 3),
        clock.at(0),
    );
    assert_eq!(effects, vec![Effect::Emit(id)]);
    assert_eq!(state(&table, id), TaskState::Queued);

    table.on_agent_status(&terminal("a"), agent(AgentStatus::Idle, 4), clock.at(100));
    assert_eq!(state(&table, id), TaskState::Submitted);
}

#[test]
fn a_repeated_key_for_the_same_prompt_returns_the_first_task_and_sends_nothing() {
    let clock = Clock::new();
    let mut table = TaskTable::default();
    let (first, _) = table
        .submit_idempotent(
            keyed("a", "fix", "run/1"),
            agent(AgentStatus::Idle, 3),
            clock.at(0),
        )
        .unwrap();

    let (again, effects) = table
        .submit_idempotent(
            keyed("a", "fix", "run/1"),
            agent(AgentStatus::Idle, 3),
            clock.at(10),
        )
        .unwrap();

    assert_eq!(again, first);
    assert!(effects.is_empty());
    assert_eq!(table.iter().count(), 1);
    assert_eq!(table.get(first).unwrap().key.as_deref(), Some("run/1"));
}

#[test]
fn a_repeated_key_after_the_task_finished_still_returns_it() {
    let clock = Clock::new();
    let mut table = TaskTable::default();
    let (first, _) = table
        .submit_idempotent(
            keyed("a", "fix", "run/1"),
            agent(AgentStatus::Working, 3),
            clock.at(0),
        )
        .unwrap();
    table.cancel(first, clock.at(5)).unwrap();

    let (again, effects) = table
        .submit_idempotent(
            keyed("a", "fix", "run/1"),
            agent(AgentStatus::Idle, 4),
            clock.at(10),
        )
        .unwrap();

    assert_eq!(again, first);
    assert!(effects.is_empty());
    assert_eq!(state(&table, first), TaskState::Cancelled);
}

#[test]
fn a_repeated_key_with_another_prompt_is_a_conflict() {
    let clock = Clock::new();
    let mut table = TaskTable::default();
    let (first, _) = table
        .submit_idempotent(
            keyed("a", "fix", "run/1"),
            agent(AgentStatus::Idle, 3),
            clock.at(0),
        )
        .unwrap();

    let result = table.submit_idempotent(
        keyed("a", "other", "run/1"),
        agent(AgentStatus::Idle, 3),
        clock.at(10),
    );

    assert_eq!(result.unwrap_err(), KeyConflict { existing: first });
    assert_eq!(table.iter().count(), 1);
}

#[test]
fn a_repeated_key_for_another_target_is_a_conflict() {
    let clock = Clock::new();
    let mut table = TaskTable::default();
    let (first, _) = table
        .submit_idempotent(
            keyed("a", "fix", "run/1"),
            agent(AgentStatus::Idle, 3),
            clock.at(0),
        )
        .unwrap();
    let other_target = NewTask {
        target: "implementer".into(),
        ..keyed("b", "fix", "run/1")
    };

    let result = table.submit_idempotent(other_target, agent(AgentStatus::Idle, 3), clock.at(10));

    assert_eq!(result.unwrap_err(), KeyConflict { existing: first });
}

#[test]
fn a_key_is_found_without_the_terminal_of_its_task() {
    let clock = Clock::new();
    let mut table = TaskTable::default();
    let (first, _) = table
        .submit_idempotent(
            keyed("a", "fix", "run/1"),
            agent(AgentStatus::Idle, 3),
            clock.at(0),
        )
        .unwrap();
    table.on_terminal_gone(&terminal("a"), "pane closed", clock.at(5));

    assert_eq!(
        table.find_by_key("run/1", "reviewer", "fix"),
        Ok(Some(first))
    );
    assert_eq!(table.find_by_key("run/2", "reviewer", "fix"), Ok(None));
    assert_eq!(
        table.find_by_key("run/1", "reviewer", "other"),
        Err(KeyConflict { existing: first })
    );
}

#[test]
fn tasks_without_a_key_are_never_merged() {
    let clock = Clock::new();
    let mut table = TaskTable::default();

    let (first, _) = table
        .submit_idempotent(
            task("a", "fix"),
            agent(AgentStatus::Working, 3),
            clock.at(0),
        )
        .unwrap();
    let (second, _) = table
        .submit_idempotent(
            task("a", "fix"),
            agent(AgentStatus::Working, 3),
            clock.at(10),
        )
        .unwrap();

    assert_ne!(first, second);
}

#[test]
fn a_key_is_forgotten_with_its_dropped_task() {
    let clock = Clock::new();
    let mut table = TaskTable::default();
    let (old, _) = table
        .submit_idempotent(
            keyed("a", "x", "run/old"),
            agent(AgentStatus::Working, 3),
            clock.at(0),
        )
        .unwrap();
    table.cancel(old, clock.at(0)).unwrap();
    for n in 1..=FINISHED_TASKS_KEPT {
        let (id, _) = table.submit(
            task("a", "x"),
            agent(AgentStatus::Working, 3),
            clock.at(n as u64),
        );
        table.cancel(id, clock.at(n as u64)).unwrap();
    }
    assert!(table.get(old).is_none());

    let (fresh, _) = table
        .submit_idempotent(
            keyed("a", "other", "run/old"),
            agent(AgentStatus::Working, 3),
            clock.at(9_999),
        )
        .unwrap();

    assert_ne!(fresh, old);
}

#[test]
fn a_result_dir_names_one_result_file_per_task_and_asks_for_it_in_the_prompt() {
    let clock = Clock::new();
    let mut table = TaskTable::default();
    let new = NewTask {
        result_dir: Some("/tmp/herdr-task/boot1".into()),
        ..task("a", "plan the fix")
    };

    let (id, _) = table.submit(new, agent(AgentStatus::Idle, 3), clock.at(0));

    let task = table.get(id).unwrap();
    let path = format!("/tmp/herdr-task/boot1/{}.md", id.public());
    assert_eq!(task.result_path.as_deref(), Some(path.as_str()));
    assert_eq!(task.text, "plan the fix");
    let prompt = task.prompt();
    assert!(prompt.starts_with("plan the fix\n\n"), "{prompt}");
    assert!(prompt.contains(&path), "{prompt}");
}

#[test]
fn without_a_result_dir_the_prompt_is_the_text_as_given() {
    let clock = Clock::new();
    let mut table = TaskTable::default();

    let (id, _) = table.submit(task("a", "fix"), agent(AgentStatus::Idle, 3), clock.at(0));

    let task = table.get(id).unwrap();
    assert_eq!(task.result_path, None);
    assert_eq!(task.prompt(), "fix");
}

#[test]
fn each_table_names_its_own_unguessable_result_folder() {
    let first = TaskTable::default();
    let second = TaskTable::default();

    assert_ne!(first.run_id(), second.run_id());
    assert_eq!(first.run_id().len(), 32);
    assert!(first.run_id().chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn a_task_asked_from_a_terminal_running_a_task_records_that_task_as_its_parent() {
    let clock = Clock::new();
    let mut table = TaskTable::default();
    let (chief, _) = table.submit(
        task("chief", "lead"),
        agent(AgentStatus::Idle, 3),
        clock.at(0),
    );
    table.on_agent_status(
        &terminal("chief"),
        agent(AgentStatus::Working, 4),
        clock.at(10),
    );

    let (child, _) = table.submit(
        asked_by("planner", "plan", "chief"),
        agent(AgentStatus::Idle, 3),
        clock.at(20),
    );

    let child = table.get(child).unwrap();
    assert_eq!(child.requester, Some(terminal("chief")));
    assert_eq!(child.parent, Some(chief));
}

#[test]
fn a_task_asked_from_a_terminal_without_a_task_records_only_the_requester() {
    let clock = Clock::new();
    let mut table = TaskTable::default();

    let (child, _) = table.submit(
        asked_by("planner", "plan", "chief"),
        agent(AgentStatus::Idle, 3),
        clock.at(0),
    );

    let child = table.get(child).unwrap();
    assert_eq!(child.requester, Some(terminal("chief")));
    assert_eq!(child.parent, None);
}

#[test]
fn asking_ones_own_terminal_is_a_cycle() {
    let table = TaskTable::default();

    assert!(table.would_cycle(&terminal("chief"), Some(&terminal("chief"))));
}

#[test]
fn asking_back_the_terminal_that_one_works_for_is_a_cycle() {
    let clock = Clock::new();
    let mut table = TaskTable::default();
    table.submit(
        asked_by("planner", "plan", "chief"),
        agent(AgentStatus::Idle, 3),
        clock.at(0),
    );
    table.on_agent_status(
        &terminal("planner"),
        agent(AgentStatus::Working, 4),
        clock.at(10),
    );
    table.submit(
        asked_by("helper", "look", "planner"),
        agent(AgentStatus::Idle, 3),
        clock.at(20),
    );
    table.on_agent_status(
        &terminal("helper"),
        agent(AgentStatus::Working, 4),
        clock.at(30),
    );

    assert!(table.would_cycle(&terminal("chief"), Some(&terminal("helper"))));
    assert!(table.would_cycle(&terminal("planner"), Some(&terminal("helper"))));
    assert!(!table.would_cycle(&terminal("reviewer"), Some(&terminal("helper"))));
    assert!(!table.would_cycle(&terminal("chief"), None));
}

#[test]
fn a_finished_task_no_longer_links_its_terminal_to_the_requester() {
    let clock = Clock::new();
    let mut table = TaskTable::default();
    table.submit(
        asked_by("planner", "plan", "chief"),
        agent(AgentStatus::Idle, 3),
        clock.at(0),
    );
    table.on_agent_status(
        &terminal("planner"),
        agent(AgentStatus::Working, 4),
        clock.at(10),
    );
    table.on_agent_status(
        &terminal("planner"),
        agent(AgentStatus::Idle, 5),
        clock.at(20),
    );

    assert!(!table.would_cycle(&terminal("chief"), Some(&terminal("planner"))));
}

#[test]
fn the_first_saved_screen_tail_of_a_task_stays() {
    let clock = Clock::new();
    let mut table = TaskTable::default();
    let (id, _) = table.submit(task("a", "fix"), agent(AgentStatus::Idle, 3), clock.at(0));

    table.save_screen_tail(id, "at the end".into());
    table.save_screen_tail(id, "later".into());

    assert_eq!(
        table.get(id).unwrap().screen_tail.as_deref(),
        Some("at the end")
    );
}
