//! Decides what to do with in-band agent claims for one off-host pane.
//!
//! A claim grants the agents list and notifications only. It never grants
//! input access (`runtime_hosts_agent` stays non-exempt), because any byte
//! stream that reaches the pane can forge it. Only a declaration made through
//! the API (`pane.declare_agent`) does, and it outranks every claim.

use std::time::{Duration, Instant};

use super::osc::{OffHostAgentClaim, OffHostClaimMarks, OffHostClaimUpdate};
use super::AgentIdentitySource;
use crate::detect::{parse_agent_label, Agent};

/// A forged or looping claim stream must not make the pane flap its identity.
const ADOPTION_MIN_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OffHostClaimAction {
    None,
    Adopt(Agent),
    /// A user declared the agent through the API.
    Declare(Agent),
    /// The claimed agent said it exited, or its declaration was withdrawn.
    Release,
}

#[derive(Debug, Default)]
pub(super) struct OffHostClaimWatcher {
    seen_generation: u64,
    seen_released: u64,
    /// A release was reported and the identity has not cleared yet. A start
    /// seen now would meet the old identity and be dropped as "already known".
    awaiting_clear: bool,
    last_adoption: Option<Instant>,
}

fn is_claimed(identity: Option<(Agent, AgentIdentitySource)>) -> bool {
    matches!(identity, Some((_, AgentIdentitySource::OffHostClaim)))
}

/// Reconciles the pane identity with the declared agent of its connection.
pub(super) fn declared_identity_action(
    off_host: bool,
    declared: Option<Agent>,
    identity: Option<(Agent, AgentIdentitySource)>,
) -> OffHostClaimAction {
    if !off_host {
        return OffHostClaimAction::None;
    }
    match (declared, identity) {
        // A process-derived identity is positive local evidence.
        (_, Some((_, AgentIdentitySource::Process))) => OffHostClaimAction::None,
        (Some(declared), Some((current, AgentIdentitySource::Declared))) if declared == current => {
            OffHostClaimAction::None
        }
        (Some(declared), _) => OffHostClaimAction::Declare(declared),
        (None, Some((_, AgentIdentitySource::Declared))) => OffHostClaimAction::Release,
        (None, _) => OffHostClaimAction::None,
    }
}

impl OffHostClaimWatcher {
    /// Marks every claim as handled so a stale claim is not adopted again
    /// after the pane's identity was released on purpose.
    pub(super) fn consume(&mut self, marks: OffHostClaimMarks) {
        self.seen_generation = marks.generation;
        self.seen_released = marks.released_generation;
        self.awaiting_clear = false;
        self.last_adoption = None;
    }

    /// Hands a live `Started` claim back to `poll`. A reset that the user did
    /// not ask for (manifest reload, hook authority) drops the identity while
    /// the remote agent keeps running; without this its exit would never land.
    pub(super) fn rewind(&mut self, marks: OffHostClaimMarks) {
        self.seen_generation = marks.rewound_generation;
        self.seen_released = marks.released_generation;
        self.awaiting_clear = false;
        self.last_adoption = None;
    }

    /// `fetch` receives the last handled generation and returns a newer claim.
    /// It is not called for a local pane, which must ignore claims entirely and
    /// pays no per-tick cost for them.
    pub(super) fn poll(
        &mut self,
        off_host: bool,
        identity: Option<(Agent, AgentIdentitySource)>,
        now: Instant,
        fetch: impl FnOnce(u64) -> Option<OffHostClaimUpdate>,
    ) -> OffHostClaimAction {
        // Why not consume claims while declared: once the declaration is
        // withdrawn, a remote agent that is still running is claimed again.
        if !off_host || matches!(identity, Some((_, AgentIdentitySource::Declared))) {
            return OffHostClaimAction::None;
        }
        if self.awaiting_clear && !is_claimed(identity) {
            self.awaiting_clear = false;
        }
        let Some(update) = fetch(self.seen_generation) else {
            return OffHostClaimAction::None;
        };
        let OffHostClaimUpdate {
            generation,
            claim,
            released_generation,
        } = update;
        match claim {
            OffHostAgentClaim::Exited => {
                self.seen_generation = generation;
                self.seen_released = released_generation;
                if is_claimed(identity) {
                    self.awaiting_clear = true;
                    OffHostClaimAction::Release
                } else {
                    OffHostClaimAction::None
                }
            }
            OffHostAgentClaim::Started { label } => {
                if self.awaiting_clear {
                    // Left unconsumed: it is adopted once the identity cleared.
                    return OffHostClaimAction::None;
                }
                if is_claimed(identity)
                    && released_generation > self.seen_released
                    && released_generation < generation
                {
                    // The agent exited and started again between two polls.
                    self.seen_released = released_generation;
                    self.awaiting_clear = true;
                    return OffHostClaimAction::Release;
                }
                self.seen_released = released_generation;
                let Some(agent) = parse_agent_label(&label) else {
                    self.seen_generation = generation;
                    return OffHostClaimAction::None;
                };
                // A process-derived identity is positive local evidence.
                if matches!(identity, Some((_, AgentIdentitySource::Process)))
                    || matches!(identity, Some((current, _)) if current == agent)
                {
                    self.seen_generation = generation;
                    return OffHostClaimAction::None;
                }
                if self
                    .last_adoption
                    .is_some_and(|last| now.duration_since(last) < ADOPTION_MIN_INTERVAL)
                {
                    // Left unconsumed so the next tick retries once the window passes.
                    return OffHostClaimAction::None;
                }
                self.seen_generation = generation;
                self.last_adoption = Some(now);
                OffHostClaimAction::Adopt(agent)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::osc::AgentOscStateTracker;
    use super::*;

    const CLAIMED: Option<(Agent, AgentIdentitySource)> =
        Some((Agent::Claude, AgentIdentitySource::OffHostClaim));

    fn update(
        generation: u64,
        claim: OffHostAgentClaim,
        released: u64,
    ) -> Option<OffHostClaimUpdate> {
        Some(OffHostClaimUpdate {
            generation,
            claim,
            released_generation: released,
        })
    }

    fn started(label: &str) -> Option<OffHostClaimUpdate> {
        update(
            1,
            OffHostAgentClaim::Started {
                label: label.into(),
            },
            0,
        )
    }

    fn start_bytes(token: &str, kind: &str) -> Vec<u8> {
        format!("\x1b]43737;herdr-agent;v=1;start;{token};{kind}\x07").into_bytes()
    }

    fn exit_bytes(token: &str) -> Vec<u8> {
        format!("\x1b]43737;herdr-agent;v=1;exit;{token}\x07").into_bytes()
    }

    fn poll_tracker(
        watcher: &mut OffHostClaimWatcher,
        tracker: &AgentOscStateTracker,
        identity: Option<(Agent, AgentIdentitySource)>,
        now: Instant,
    ) -> OffHostClaimAction {
        watcher.poll(true, identity, now, |seen| {
            tracker.offhost_claim_since(seen)
        })
    }

    const DECLARED: Option<(Agent, AgentIdentitySource)> =
        Some((Agent::Claude, AgentIdentitySource::Declared));

    #[test]
    fn a_declaration_on_a_local_pane_is_ignored() {
        assert_eq!(
            declared_identity_action(false, Some(Agent::Claude), None),
            OffHostClaimAction::None
        );
    }

    #[test]
    fn a_declaration_is_adopted_over_no_identity_or_a_claim() {
        for identity in [None, CLAIMED] {
            assert_eq!(
                declared_identity_action(true, Some(Agent::Claude), identity),
                OffHostClaimAction::Declare(Agent::Claude)
            );
        }
    }

    #[test]
    fn an_adopted_declaration_is_not_adopted_again() {
        assert_eq!(
            declared_identity_action(true, Some(Agent::Claude), DECLARED),
            OffHostClaimAction::None
        );
    }

    #[test]
    fn a_new_declaration_replaces_the_declared_agent() {
        assert_eq!(
            declared_identity_action(true, Some(Agent::Codex), DECLARED),
            OffHostClaimAction::Declare(Agent::Codex)
        );
    }

    #[test]
    fn a_declaration_does_not_replace_a_process_identity() {
        let identity = Some((Agent::Codex, AgentIdentitySource::Process));

        assert_eq!(
            declared_identity_action(true, Some(Agent::Claude), identity),
            OffHostClaimAction::None
        );
    }

    #[test]
    fn withdrawing_a_declaration_releases_only_a_declared_identity() {
        assert_eq!(
            declared_identity_action(true, None, DECLARED),
            OffHostClaimAction::Release
        );
        for identity in [
            None,
            CLAIMED,
            Some((Agent::Codex, AgentIdentitySource::Process)),
        ] {
            assert_eq!(
                declared_identity_action(true, None, identity),
                OffHostClaimAction::None
            );
        }
    }

    #[test]
    fn claims_wait_unread_while_a_declaration_holds_the_pane() {
        let mut watcher = OffHostClaimWatcher::default();

        let action = watcher.poll(true, DECLARED, Instant::now(), |_| {
            panic!("a declared pane must not consume claims")
        });

        assert_eq!(action, OffHostClaimAction::None);
        assert_eq!(
            watcher.poll(true, None, Instant::now(), |seen| {
                assert_eq!(seen, 0, "the claim stream is still unread");
                started("codex")
            }),
            OffHostClaimAction::Adopt(Agent::Codex)
        );
    }

    #[test]
    fn local_pane_ignores_claims_without_reading_them() {
        let mut watcher = OffHostClaimWatcher::default();

        let action = watcher.poll(false, None, Instant::now(), |_| {
            panic!("a local pane must not read claims")
        });

        assert_eq!(action, OffHostClaimAction::None);
    }

    #[test]
    fn off_host_pane_adopts_a_known_label_without_process_identity() {
        let mut watcher = OffHostClaimWatcher::default();

        let action = watcher.poll(true, None, Instant::now(), |_| started("claude"));

        assert_eq!(action, OffHostClaimAction::Adopt(Agent::Claude));
    }

    #[test]
    fn unknown_label_is_ignored_and_consumed() {
        let mut watcher = OffHostClaimWatcher::default();

        assert_eq!(
            watcher.poll(true, None, Instant::now(), |_| started("not-an-agent")),
            OffHostClaimAction::None
        );
        watcher.poll(true, None, Instant::now(), |seen| {
            assert_eq!(seen, 1);
            None
        });
    }

    #[test]
    fn process_identity_is_not_replaced_by_a_claim() {
        let mut watcher = OffHostClaimWatcher::default();
        let identity = Some((Agent::Codex, AgentIdentitySource::Process));

        let action = watcher.poll(true, identity, Instant::now(), |_| started("claude"));

        assert_eq!(action, OffHostClaimAction::None);
    }

    #[test]
    fn adoptions_are_rate_limited_to_one_per_window() {
        let mut watcher = OffHostClaimWatcher::default();
        let now = Instant::now();
        watcher.poll(true, None, now, |_| started("claude"));

        let second = |_| {
            update(
                2,
                OffHostAgentClaim::Started {
                    label: "codex".into(),
                },
                0,
            )
        };
        assert_eq!(
            watcher.poll(true, None, now + Duration::from_millis(500), second),
            OffHostClaimAction::None
        );
        assert_eq!(
            watcher.poll(true, None, now + ADOPTION_MIN_INTERVAL, second),
            OffHostClaimAction::Adopt(Agent::Codex)
        );
    }

    #[test]
    fn exit_releases_only_a_claimed_identity() {
        let exit = |_| update(1, OffHostAgentClaim::Exited, 1);
        let now = Instant::now();

        assert_eq!(
            OffHostClaimWatcher::default().poll(true, CLAIMED, now, exit),
            OffHostClaimAction::Release
        );
        let process = Some((Agent::Claude, AgentIdentitySource::Process));
        assert_eq!(
            OffHostClaimWatcher::default().poll(true, process, now, exit),
            OffHostClaimAction::None
        );
    }

    #[test]
    fn consumed_claims_are_not_adopted_again_after_a_graceful_release() {
        let mut tracker = AgentOscStateTracker::default();
        tracker.observe(&start_bytes("t1", "claude"));
        let mut watcher = OffHostClaimWatcher::default();
        let now = Instant::now();

        watcher.consume(tracker.offhost_claim_marks());

        assert_eq!(
            poll_tracker(&mut watcher, &tracker, None, now),
            OffHostClaimAction::None
        );
        tracker.observe(&start_bytes("t2", "claude"));
        assert_eq!(
            poll_tracker(&mut watcher, &tracker, None, now),
            OffHostClaimAction::Adopt(Agent::Claude)
        );
    }

    #[test]
    fn rewound_claim_is_adopted_again_and_its_exit_still_releases() {
        let mut tracker = AgentOscStateTracker::default();
        tracker.observe(&start_bytes("t1", "claude"));
        let mut watcher = OffHostClaimWatcher::default();
        let now = Instant::now();
        assert_eq!(
            poll_tracker(&mut watcher, &tracker, None, now),
            OffHostClaimAction::Adopt(Agent::Claude)
        );

        watcher.rewind(tracker.offhost_claim_marks());

        assert_eq!(
            poll_tracker(&mut watcher, &tracker, None, now),
            OffHostClaimAction::Adopt(Agent::Claude)
        );
        tracker.observe(&exit_bytes("t1"));
        assert_eq!(
            poll_tracker(&mut watcher, &tracker, CLAIMED, now),
            OffHostClaimAction::Release
        );
    }

    #[test]
    fn rewind_does_not_revive_an_agent_that_already_exited() {
        let mut tracker = AgentOscStateTracker::default();
        tracker.observe(&start_bytes("t1", "claude"));
        tracker.observe(&exit_bytes("t1"));
        let mut watcher = OffHostClaimWatcher::default();

        watcher.rewind(tracker.offhost_claim_marks());

        assert_eq!(
            poll_tracker(&mut watcher, &tracker, None, Instant::now()),
            OffHostClaimAction::None
        );
    }

    #[test]
    fn exit_followed_by_a_restart_between_polls_releases_before_adopting() {
        let mut tracker = AgentOscStateTracker::default();
        tracker.observe(&start_bytes("t1", "claude"));
        let mut watcher = OffHostClaimWatcher::default();
        let now = Instant::now();
        assert_eq!(
            poll_tracker(&mut watcher, &tracker, None, now),
            OffHostClaimAction::Adopt(Agent::Claude)
        );
        tracker.observe(&exit_bytes("t1"));
        tracker.observe(&start_bytes("t2", "claude"));

        assert_eq!(
            poll_tracker(&mut watcher, &tracker, CLAIMED, now),
            OffHostClaimAction::Release
        );
        // The old identity is still being cleared; the new start must wait.
        assert_eq!(
            poll_tracker(&mut watcher, &tracker, CLAIMED, now),
            OffHostClaimAction::None
        );
        assert_eq!(
            poll_tracker(&mut watcher, &tracker, None, now + ADOPTION_MIN_INTERVAL),
            OffHostClaimAction::Adopt(Agent::Claude)
        );
    }
}
