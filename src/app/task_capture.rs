//! Copies of result files, read on workers a few at a time. Why not the
//! directory read slots: a busy file browser must not cost a copy.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use super::App;
use crate::tasks::TaskId;

const CAPTURES_AT_ONCE: usize = 2;
/// One copy per kept task is the most that can still be useful.
const QUEUE_LIMIT: usize = crate::tasks::FINISHED_TASKS_KEPT;

#[derive(Debug, Default)]
pub(crate) struct TaskCaptures {
    queue: VecDeque<TaskId>,
    running: usize,
}

impl TaskCaptures {
    pub(crate) fn enqueue(&mut self, task: TaskId) {
        if self.queue.contains(&task) {
            return;
        }
        if self.queue.len() >= QUEUE_LIMIT {
            self.queue.pop_front();
        }
        self.queue.push_back(task);
    }

    /// The next task to read, while fewer than the limit run.
    pub(crate) fn next_to_start(&mut self) -> Option<TaskId> {
        (self.running < CAPTURES_AT_ONCE)
            .then(|| self.queue.pop_front())
            .flatten()
    }

    pub(crate) fn started(&mut self) {
        self.running += 1;
    }

    pub(crate) fn finished(&mut self) {
        self.running = self.running.saturating_sub(1);
    }

    pub(crate) fn idle(&self) -> bool {
        self.running == 0 && self.queue.is_empty()
    }
}

impl App {
    /// Lets copies in progress land before the last save, for at most
    /// `limit`. Other events that arrive meanwhile are handled as usual.
    pub(crate) fn wait_for_result_captures(&mut self, limit: Duration) {
        let deadline = Instant::now() + limit;
        while !self.task_captures.idle() && Instant::now() < deadline {
            match self.event_rx.try_recv() {
                Ok(event) => {
                    self.handle_internal_event_with_render_impact(event);
                }
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u64) -> TaskId {
        TaskId::parse(&format!("t_{n}")).expect("task id")
    }

    #[test]
    fn at_most_two_copies_run_and_the_rest_wait_their_turn() {
        let mut captures = TaskCaptures::default();
        for n in 1..=3 {
            captures.enqueue(id(n));
        }

        let first = captures.next_to_start();
        captures.started();
        let second = captures.next_to_start();
        captures.started();

        assert_eq!((first, second), (Some(id(1)), Some(id(2))));
        assert_eq!(captures.next_to_start(), None);
        captures.finished();
        assert_eq!(captures.next_to_start(), Some(id(3)));
    }

    #[test]
    fn a_task_is_queued_once() {
        let mut captures = TaskCaptures::default();
        captures.enqueue(id(1));
        captures.enqueue(id(1));

        assert_eq!(captures.next_to_start(), Some(id(1)));
        assert_eq!(captures.next_to_start(), None);
    }

    #[test]
    fn a_full_queue_drops_its_oldest_task() {
        let mut captures = TaskCaptures::default();
        for n in 0..=QUEUE_LIMIT as u64 {
            captures.enqueue(id(n));
        }

        assert_eq!(captures.next_to_start(), Some(id(1)));
    }

    #[test]
    fn captures_are_idle_only_with_nothing_queued_or_running() {
        let mut captures = TaskCaptures::default();
        assert!(captures.idle());
        captures.enqueue(id(1));
        assert!(!captures.idle());
        let _ = captures.next_to_start();
        captures.started();
        assert!(!captures.idle());
        captures.finished();
        assert!(captures.idle());
    }
}
