//! Holding input while a possible drop is checked off the client loop
//! (design §3.2 step 5). Why hold at all: if the paste turns out to be plain
//! text, it must reach the pane before anything typed after it.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// How long a drop check may take before the paste is treated as text.
pub(crate) const DROP_CHECK_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub(crate) struct PendingDrop<E> {
    pub(crate) job: u64,
    original: E,
    held: VecDeque<E>,
    deadline: Instant,
}

/// What to feed back into the loop, in order.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Resolved<E> {
    /// The paste itself, when it was not a drop.
    pub(crate) original: Option<E>,
    pub(crate) held: VecDeque<E>,
}

impl<E> PendingDrop<E> {
    pub(crate) fn new(job: u64, original: E, now: Instant) -> Self {
        Self {
            job,
            original,
            held: VecDeque::new(),
            deadline: now + DROP_CHECK_TIMEOUT,
        }
    }

    pub(crate) fn hold(&mut self, event: E) {
        self.held.push_back(event);
    }

    pub(crate) fn deadline(&self) -> Instant {
        self.deadline
    }

    pub(crate) fn expired(&self, now: Instant) -> bool {
        now >= self.deadline
    }

    pub(crate) fn resolve(self, is_drop: bool) -> Resolved<E> {
        Resolved {
            original: (!is_drop).then_some(self.original),
            held: self.held,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_held_during_a_check_follows_the_paste_when_it_was_plain_text() {
        let now = Instant::now();
        let mut pending = PendingDrop::new(7, "paste", now);
        pending.hold("a");
        pending.hold("b");

        let resolved = pending.resolve(false);

        assert_eq!(resolved.original, Some("paste"));
        assert_eq!(Vec::from(resolved.held), ["a", "b"]);
    }

    #[test]
    fn a_confirmed_drop_swallows_the_paste_but_replays_held_input() {
        let mut pending = PendingDrop::new(7, "paste", Instant::now());
        pending.hold("a");

        let resolved = pending.resolve(true);

        assert_eq!(resolved.original, None);
        assert_eq!(Vec::from(resolved.held), ["a"]);
    }

    #[test]
    fn a_check_expires_after_two_seconds() {
        let now = Instant::now();
        let pending = PendingDrop::new(1, (), now);

        assert!(!pending.expired(now + Duration::from_millis(1999)));
        assert!(pending.expired(now + DROP_CHECK_TIMEOUT));
        assert_eq!(pending.deadline(), now + DROP_CHECK_TIMEOUT);
    }
}
