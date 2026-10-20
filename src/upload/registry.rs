//! The app thread's table of live upload sessions. Every operation here is a
//! map lookup or an atomic store: no I/O, no joins, no waiting.

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::Arc;

use crate::layout::PaneId;
use crate::offhost::PaneEnvironment;
use crate::terminal::TerminalId;

use super::path::Failure;
use super::worker::{Control, UploadJob, PHASE_COMMITTING, PHASE_DONE};

/// Server-wide and per-owner session caps. Why not share the directory read
/// slots: a session lives for minutes and would starve listings.
pub(crate) const MAX_SESSIONS: usize = 4;
pub(crate) const MAX_SESSIONS_PER_OWNER: usize = 2;
/// Cancelled ids remembered so late requests get `upload_cancelled`.
const REMEMBERED_CANCELS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UploadOwner {
    /// The API socket: one connection per request, so no connection to watch.
    Api,
    Client(u64),
}

/// What a session writes into; it ends when the pane or terminal does.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct UploadTargetKey {
    pub(crate) pane: Option<PaneId>,
    pub(crate) terminal: Option<TerminalId>,
    /// The workspace pane's public id, to look it up again on every request.
    pub(crate) public_pane: Option<String>,
}

/// Where the target's shell ran at begin; a change makes the session stale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UploadEnvironment {
    Host,
    OffHost(Box<PaneEnvironment>),
}

pub(crate) struct UploadHandle {
    pub(crate) owner: UploadOwner,
    pub(crate) target: UploadTargetKey,
    pub(crate) environment: UploadEnvironment,
    pub(crate) jobs: SyncSender<UploadJob>,
    pub(crate) control: Arc<Control>,
}

#[derive(Default)]
pub(crate) struct UploadRegistry {
    handles: HashMap<String, UploadHandle>,
    cancelled: VecDeque<String>,
    next: u64,
    boot: String,
}

fn not_found(id: &str) -> Failure {
    ("upload_not_found", format!("no upload {id}"))
}

impl UploadRegistry {
    pub(crate) fn next_id(&mut self) -> String {
        if self.boot.is_empty() {
            self.boot = super::stage_token()[..8].to_owned();
        }
        self.next += 1;
        format!("up-{}-{}", self.boot, self.next)
    }

    fn prune(&mut self) {
        self.handles
            .retain(|_, handle| handle.control.phase() != PHASE_DONE);
    }

    pub(crate) fn admit(&mut self, owner: UploadOwner) -> Result<(), Failure> {
        self.prune();
        let owned = self
            .handles
            .values()
            .filter(|handle| handle.owner == owner)
            .count();
        if self.handles.len() >= MAX_SESSIONS || owned >= MAX_SESSIONS_PER_OWNER {
            return Err((
                "upload_busy",
                "too many uploads are in progress; retry when one finishes".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn insert(&mut self, id: String, handle: UploadHandle) {
        self.handles.insert(id, handle);
    }

    /// The session `id`, if `owner` started it and it is still running.
    pub(crate) fn get(&mut self, id: &str, owner: UploadOwner) -> Result<&UploadHandle, Failure> {
        self.prune();
        if self.cancelled.iter().any(|cancelled| cancelled == id) {
            return Err(super::worker::cancelled());
        }
        // Why the same error for another owner's id: it must not reveal that
        // the id exists.
        self.handles
            .get(id)
            .filter(|handle| handle.owner == owner)
            .ok_or_else(|| not_found(id))
    }

    /// Queues a job without blocking, or answers it with why it cannot run.
    /// A full queue means the client ignored the window.
    pub(crate) fn send(&mut self, id: &str, owner: UploadOwner, job: UploadJob) {
        let handle = match self.get(id, owner) {
            Ok(handle) => handle,
            Err(failure) => return job.refuse(failure),
        };
        match handle.jobs.try_send(job) {
            Ok(()) => {}
            Err(TrySendError::Full(job)) => job.refuse((
                "upload_busy",
                "more writes are in flight than the upload window allows".into(),
            )),
            Err(TrySendError::Disconnected(job)) => job.refuse(not_found(id)),
        }
    }

    /// Cancels `id` unless it is publishing, which must run to its end.
    pub(crate) fn cancel(&mut self, id: &str, owner: UploadOwner) -> Result<(), Failure> {
        let phase = self.get(id, owner)?.control.phase();
        if phase == PHASE_COMMITTING {
            return Err((
                "upload_busy",
                "the upload is being published and can no longer be cancelled".into(),
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

    fn abort_where(&mut self, matches: impl Fn(&UploadHandle) -> bool) -> usize {
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

    pub(crate) fn abort_pane(&mut self, pane: PaneId) -> usize {
        self.abort_where(|handle| handle.target.pane == Some(pane))
    }

    pub(crate) fn abort_terminal(&mut self, terminal: &TerminalId) -> usize {
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

    fn handle(owner: UploadOwner, pane: Option<u32>) -> (UploadHandle, mpsc::Receiver<UploadJob>) {
        let (jobs, rx) = mpsc::sync_channel(1);
        (
            UploadHandle {
                owner,
                target: UploadTargetKey {
                    pane: pane.map(PaneId::from_raw),
                    terminal: None,
                    public_pane: None,
                },
                environment: UploadEnvironment::Host,
                jobs,
                control: Arc::default(),
            },
            rx,
        )
    }

    #[test]
    fn sessions_are_capped_per_owner_and_overall() {
        let mut registry = UploadRegistry::default();
        let mut keep = Vec::new();
        for owner in [UploadOwner::Client(1), UploadOwner::Client(1)] {
            registry.admit(owner).unwrap();
            let (handle, rx) = handle(owner, None);
            keep.push(rx);
            let id = registry.next_id();
            registry.insert(id, handle);
        }

        assert_eq!(
            registry.admit(UploadOwner::Client(1)).unwrap_err().0,
            "upload_busy"
        );
        for owner in [UploadOwner::Client(2), UploadOwner::Api] {
            registry.admit(owner).unwrap();
            let (handle, rx) = handle(owner, None);
            keep.push(rx);
            let id = registry.next_id();
            registry.insert(id, handle);
        }
        assert_eq!(
            registry.admit(UploadOwner::Client(3)).unwrap_err().0,
            "upload_busy"
        );
    }

    #[test]
    fn finished_sessions_free_their_slot() {
        let mut registry = UploadRegistry::default();
        let (handle, _rx) = handle(UploadOwner::Api, None);
        let control = handle.control.clone();
        registry.insert("up-1".into(), handle);

        control.set_phase(PHASE_DONE);

        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn another_owners_id_looks_missing() {
        let mut registry = UploadRegistry::default();
        let (handle, _rx) = handle(UploadOwner::Client(1), None);
        registry.insert("up-1".into(), handle);

        let error = registry.get("up-1", UploadOwner::Client(2)).err().unwrap();

        assert_eq!(error.0, "upload_not_found");
    }

    #[test]
    fn a_full_queue_is_upload_busy_and_a_cancelled_id_is_upload_cancelled() {
        let mut registry = UploadRegistry::default();
        let (handle, _rx) = handle(UploadOwner::Api, None);
        let control = handle.control.clone();
        registry.insert("up-1".into(), handle);
        let (tx, answers) = mpsc::channel();
        let job = |tx: &mpsc::Sender<&'static str>| {
            let tx = tx.clone();
            UploadJob::Begin(Box::new(move |answer| {
                let _ = tx.send(answer.err().map_or("ok", |(code, _)| code));
            }))
        };

        registry.send("up-1", UploadOwner::Api, job(&tx));
        registry.send("up-1", UploadOwner::Api, job(&tx));
        registry.cancel("up-1", UploadOwner::Api).unwrap();
        registry.send("up-1", UploadOwner::Api, job(&tx));

        assert_eq!(answers.try_recv(), Ok("upload_busy"));
        assert!(control.is_cancelled());
        assert_eq!(answers.try_recv(), Ok("upload_cancelled"));
    }

    #[test]
    fn a_publishing_session_cannot_be_cancelled() {
        let mut registry = UploadRegistry::default();
        let (handle, _rx) = handle(UploadOwner::Api, None);
        let control = handle.control.clone();
        registry.insert("up-1".into(), handle);
        control.set_phase(PHASE_COMMITTING);

        let error = registry.cancel("up-1", UploadOwner::Api).unwrap_err();

        assert_eq!(error.0, "upload_busy");
        assert!(!control.is_cancelled());
    }

    #[test]
    fn owner_pane_and_global_aborts_cancel_only_their_sessions() {
        let mut registry = UploadRegistry::default();
        let mut controls = Vec::new();
        let mut keep = Vec::new();
        for (id, owner, pane) in [
            ("a", UploadOwner::Client(1), Some(1)),
            ("b", UploadOwner::Client(2), Some(2)),
            ("c", UploadOwner::Api, None),
        ] {
            let (handle, rx) = handle(owner, pane);
            controls.push(handle.control.clone());
            keep.push(rx);
            registry.insert(id.into(), handle);
        }

        assert_eq!(registry.abort_owner(UploadOwner::Client(1)), 1);
        assert_eq!(registry.abort_pane(PaneId::from_raw(2)), 1);
        assert!(controls[0].is_cancelled() && controls[1].is_cancelled());
        assert!(!controls[2].is_cancelled());
        assert_eq!(registry.abort_all().len(), 1);
        assert!(controls[2].is_cancelled());
    }
}
