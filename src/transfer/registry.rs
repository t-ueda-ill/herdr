//! The app thread's table of live transfer sessions. Every operation here is a
//! map lookup or an atomic store: no I/O, no joins, no waiting.

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::Arc;

use crate::upload::path::Failure;
use crate::upload::registry::{UploadEnvironment, UploadOwner, UploadTargetKey};
use crate::upload::worker::{Control, PHASE_COMMITTING, PHASE_DONE};

/// Cancelled ids remembered so late requests get the "cancelled" code.
const REMEMBERED_CANCELS: usize = 64;

/// How a direction names itself in ids, error codes, and messages.
pub(crate) struct Vocabulary {
    /// "upload": the noun in messages ("no upload up-1").
    pub(crate) noun: &'static str,
    pub(crate) id_prefix: &'static str,
    pub(crate) busy_code: &'static str,
    pub(crate) not_found_code: &'static str,
    pub(crate) cancelled_code: &'static str,
    /// What a client must not have more of in flight than the window allows.
    pub(crate) in_flight: &'static str,
    /// Server-wide and per-owner session caps. Why not share the directory read
    /// slots: a session lives for minutes and would starve listings.
    pub(crate) max_sessions: usize,
    pub(crate) max_sessions_per_owner: usize,
}

/// A request queued to a session's worker.
pub(crate) trait TransferJob {
    const VOCABULARY: Vocabulary;

    /// Answers a job that will never run.
    fn refuse(self, failure: Failure);
}

pub(crate) struct TransferHandle<J> {
    pub(crate) owner: UploadOwner,
    pub(crate) target: UploadTargetKey,
    pub(crate) environment: UploadEnvironment,
    pub(crate) jobs: SyncSender<J>,
    pub(crate) control: Arc<Control>,
}

pub(crate) struct TransferRegistry<J> {
    handles: HashMap<String, TransferHandle<J>>,
    cancelled: VecDeque<String>,
    next: u64,
    boot: String,
}

// Why not derive: that would require `J: Default`.
impl<J> Default for TransferRegistry<J> {
    fn default() -> Self {
        Self {
            handles: HashMap::new(),
            cancelled: VecDeque::new(),
            next: 0,
            boot: String::new(),
        }
    }
}

impl<J: TransferJob> TransferRegistry<J> {
    pub(crate) fn cancelled_failure() -> Failure {
        let vocabulary = J::VOCABULARY;
        (
            vocabulary.cancelled_code,
            format!("the {} was cancelled", vocabulary.noun),
        )
    }

    fn not_found(id: &str) -> Failure {
        let vocabulary = J::VOCABULARY;
        (
            vocabulary.not_found_code,
            format!("no {} {id}", vocabulary.noun),
        )
    }

    pub(crate) fn next_id(&mut self) -> String {
        if self.boot.is_empty() {
            self.boot = crate::upload::stage_token()[..8].to_owned();
        }
        self.next += 1;
        format!("{}-{}-{}", J::VOCABULARY.id_prefix, self.boot, self.next)
    }

    fn prune(&mut self) {
        self.handles
            .retain(|_, handle| handle.control.phase() != PHASE_DONE);
    }

    pub(crate) fn admit(&mut self, owner: UploadOwner) -> Result<(), Failure> {
        self.prune();
        let vocabulary = J::VOCABULARY;
        let owned = self
            .handles
            .values()
            .filter(|handle| handle.owner == owner)
            .count();
        if self.handles.len() >= vocabulary.max_sessions
            || owned >= vocabulary.max_sessions_per_owner
        {
            return Err((
                vocabulary.busy_code,
                format!(
                    "too many {}s are in progress; retry when one finishes",
                    vocabulary.noun
                ),
            ));
        }
        Ok(())
    }

    pub(crate) fn insert(&mut self, id: String, handle: TransferHandle<J>) {
        self.handles.insert(id, handle);
    }

    /// The session `id`, if `owner` started it and it is still running.
    pub(crate) fn get(
        &mut self,
        id: &str,
        owner: UploadOwner,
    ) -> Result<&TransferHandle<J>, Failure> {
        self.prune();
        if self.cancelled.iter().any(|cancelled| cancelled == id) {
            return Err(Self::cancelled_failure());
        }
        // Why the same error for another owner's id: it must not reveal that
        // the id exists.
        self.handles
            .get(id)
            .filter(|handle| handle.owner == owner)
            .ok_or_else(|| Self::not_found(id))
    }

    /// Queues a job without blocking, or answers it with why it cannot run.
    /// A full queue means the client ignored the window.
    pub(crate) fn send(&mut self, id: &str, owner: UploadOwner, job: J) {
        let handle = match self.get(id, owner) {
            Ok(handle) => handle,
            Err(failure) => return job.refuse(failure),
        };
        match handle.jobs.try_send(job) {
            Ok(()) => {}
            Err(TrySendError::Full(job)) => {
                let vocabulary = J::VOCABULARY;
                job.refuse((
                    vocabulary.busy_code,
                    format!(
                        "more {} are in flight than the {} window allows",
                        vocabulary.in_flight, vocabulary.noun
                    ),
                ));
            }
            Err(TrySendError::Disconnected(job)) => job.refuse(Self::not_found(id)),
        }
    }

    /// Cancels `id` unless it is publishing, which must run to its end.
    pub(crate) fn cancel(&mut self, id: &str, owner: UploadOwner) -> Result<(), Failure> {
        let phase = self.get(id, owner)?.control.phase();
        if phase == PHASE_COMMITTING {
            let vocabulary = J::VOCABULARY;
            return Err((
                vocabulary.busy_code,
                format!(
                    "the {} is being published and can no longer be cancelled",
                    vocabulary.noun
                ),
            ));
        }
        self.abort(id);
        Ok(())
    }

    fn abort(&mut self, id: &str) {
        if let Some(handle) = self.handles.remove(id) {
            handle.control.cancel();
            self.cancelled.push_back(id.to_owned());
            while self.cancelled.len() > REMEMBERED_CANCELS {
                self.cancelled.pop_front();
            }
        }
    }

    fn abort_where(&mut self, matches: impl Fn(&TransferHandle<J>) -> bool) -> usize {
        let ids: Vec<String> = self
            .handles
            .iter()
            .filter(|(_, handle)| matches(handle))
            .map(|(id, _)| id.clone())
            .collect();
        for id in &ids {
            self.abort(id);
        }
        ids.len()
    }

    /// The session target changed or went away.
    pub(crate) fn abort_stale(&mut self, id: &str) {
        self.abort(id);
    }

    pub(crate) fn abort_owner(&mut self, owner: UploadOwner) -> usize {
        self.abort_where(|handle| handle.owner == owner)
    }

    pub(crate) fn abort_pane(&mut self, pane: crate::layout::PaneId) -> usize {
        self.abort_where(|handle| handle.target.pane == Some(pane))
    }

    pub(crate) fn abort_terminal(&mut self, terminal: &crate::terminal::TerminalId) -> usize {
        self.abort_where(|handle| handle.target.terminal.as_ref() == Some(terminal))
    }

    /// Server shutdown and live handoff: sessions are never carried over.
    /// Returns the controls so the caller may wait for cleanup to finish.
    pub(crate) fn abort_all(&mut self) -> Vec<Arc<Control>> {
        let controls = self
            .handles
            .values()
            .map(|handle| handle.control.clone())
            .collect();
        self.abort_where(|_| true);
        controls
    }

    #[cfg(test)]
    pub(crate) fn len(&mut self) -> usize {
        self.prune();
        self.handles.len()
    }
}

/// Waits up to `within` for aborted sessions to remove their stages.
pub(crate) fn wait_for_cleanup(controls: &[Arc<Control>], within: std::time::Duration) {
    let deadline = std::time::Instant::now() + within;
    while controls.iter().any(|control| control.phase() != PHASE_DONE)
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    /// A direction other than upload, to show nothing upload-specific is baked in.
    struct FakeJob(mpsc::Sender<&'static str>);

    impl TransferJob for FakeJob {
        const VOCABULARY: Vocabulary = Vocabulary {
            noun: "fetch",
            id_prefix: "fx",
            busy_code: "fetch_busy",
            not_found_code: "fetch_not_found",
            cancelled_code: "fetch_cancelled",
            in_flight: "reads",
            max_sessions: 3,
            max_sessions_per_owner: 1,
        };

        fn refuse(self, failure: Failure) {
            let _ = self.0.send(failure.0);
        }
    }

    fn handle(owner: UploadOwner) -> (TransferHandle<FakeJob>, mpsc::Receiver<FakeJob>) {
        let (jobs, rx) = mpsc::sync_channel(1);
        (
            TransferHandle {
                owner,
                target: UploadTargetKey::default(),
                environment: UploadEnvironment::Host,
                jobs,
                control: Arc::default(),
            },
            rx,
        )
    }

    #[test]
    fn ids_caps_and_errors_follow_the_jobs_vocabulary() {
        let mut registry = TransferRegistry::<FakeJob>::default();
        let mut keep = Vec::new();
        assert!(registry.next_id().starts_with("fx-"));

        registry.admit(UploadOwner::Client(1)).unwrap();
        let (first, rx) = handle(UploadOwner::Client(1));
        keep.push(rx);
        registry.insert("fx-1".into(), first);

        let busy = registry.admit(UploadOwner::Client(1)).unwrap_err();
        assert_eq!(busy.0, "fetch_busy");
        assert!(busy.1.contains("fetch"));
        assert_eq!(
            registry
                .get("fx-9", UploadOwner::Client(1))
                .err()
                .unwrap()
                .0,
            "fetch_not_found"
        );
    }

    #[test]
    fn cancelling_drops_the_handle_so_the_worker_side_disconnects() {
        let mut registry = TransferRegistry::<FakeJob>::default();
        let (handle, rx) = handle(UploadOwner::Api);
        let control = handle.control.clone();
        registry.insert("fx-1".into(), handle);

        registry.cancel("fx-1", UploadOwner::Api).unwrap();

        assert!(control.is_cancelled());
        assert_eq!(rx.try_recv().err(), Some(mpsc::TryRecvError::Disconnected));
        let (tx, answers) = mpsc::channel();
        registry.send("fx-1", UploadOwner::Api, FakeJob(tx));
        assert_eq!(answers.try_recv(), Ok("fetch_cancelled"));
    }

    #[test]
    fn a_full_queue_is_refused_with_the_busy_code() {
        let mut registry = TransferRegistry::<FakeJob>::default();
        let (handle, _rx) = handle(UploadOwner::Api);
        registry.insert("fx-1".into(), handle);
        let (tx, answers) = mpsc::channel();

        registry.send("fx-1", UploadOwner::Api, FakeJob(tx.clone()));
        registry.send("fx-1", UploadOwner::Api, FakeJob(tx));

        assert_eq!(answers.try_recv(), Ok("fetch_busy"));
    }
}
