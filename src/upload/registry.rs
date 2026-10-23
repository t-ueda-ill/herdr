//! The app thread's table of live upload sessions: the shared transfer table
//! with the upload vocabulary.

use crate::layout::PaneId;
use crate::offhost::PaneEnvironment;
use crate::terminal::TerminalId;

use super::path::Failure;
use super::worker::UploadJob;
use crate::transfer::registry::{TransferHandle, TransferJob, TransferRegistry, Vocabulary};

/// Server-wide and per-owner session caps. Why not share the directory read
/// slots: a session lives for minutes and would starve listings.
pub(crate) const MAX_SESSIONS: usize = 4;
pub(crate) const MAX_SESSIONS_PER_OWNER: usize = 2;

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

pub(crate) type UploadHandle = TransferHandle<UploadJob>;
pub(crate) type UploadRegistry = TransferRegistry<UploadJob>;
pub(crate) use crate::transfer::registry::wait_for_cleanup;

impl TransferJob for UploadJob {
    const VOCABULARY: Vocabulary = Vocabulary {
        noun: "upload",
        id_prefix: "up",
        busy_code: "upload_busy",
        not_found_code: "upload_not_found",
        cancelled_code: "upload_cancelled",
        in_flight: "writes",
        max_sessions: MAX_SESSIONS,
        max_sessions_per_owner: MAX_SESSIONS_PER_OWNER,
    };

    fn refuse(self, failure: Failure) {
        UploadJob::refuse(self, failure);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::sync::Arc;

    use super::super::worker::{PHASE_COMMITTING, PHASE_DONE};
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
