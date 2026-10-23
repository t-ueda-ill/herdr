//! The client loop's side of an upload: deciding whether a paste starts one,
//! holding input while it is checked, driving the worker, and carrying out
//! the effects `transfer` returns.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use tracing::warn;

use crate::client::endpoint::{ClientEndpointId, EndpointRegistry, EndpointSendOutcome};
use crate::client::events::ClientLoopEvent;
use crate::client::state::ClientState;
use crate::protocol::{ClientMessage, ClientPaneInputEvent};

use super::drop_text::{resolve_drop_paths, split_drop_text};
use super::gate::DropGate;
use super::pending::PendingDrop;
use super::transfer::{DropRequest, JobId, UploadEffect, UploadTarget, Uploads, WorkbenchTarget};
use super::worker::{WorkerCommand, WorkerEvent, WorkerHandle};
use super::UploadSettings;

/// The progress card redraws at most this often (design §3.5).
const PROGRESS_REPAINT_INTERVAL: Duration = Duration::from_millis(100);

/// The paste as local paths with names, from its text alone.
pub(crate) fn drop_paths(text: &str) -> Option<Vec<std::path::PathBuf>> {
    let tokens = split_drop_text(text, crate::platform::drop_path_syntax())?;
    resolve_drop_paths(&tokens.tokens, crate::platform::dropped_path)
}

/// What became of a paste offered as a possible drop.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DropStart {
    /// Not a drop: handle the input as usual.
    NotADrop,
    /// Held until the worker says whether every path exists.
    Pending,
    /// A drop while another upload runs; refused with a notice.
    Busy,
}

/// A paste onto a file workbench tree, already known to be addressed there.
pub(crate) struct WorkbenchDrop {
    pub(crate) paths: Vec<std::path::PathBuf>,
    pub(crate) target: WorkbenchTarget,
    pub(crate) endpoint_id: ClientEndpointId,
    pub(crate) generation: u64,
    pub(crate) chunk_cap: usize,
    /// A download runs. Why a flag and not a check here: the two share one
    /// progress card, and this runtime does not know about the other.
    pub(crate) download_busy: bool,
}

/// Paths that might be a drop, with how the paste should be treated.
pub(crate) struct DropCandidate {
    pub(crate) text: String,
    pub(crate) target: UploadTarget,
    pub(crate) endpoint_id: ClientEndpointId,
    pub(crate) generation: u64,
    pub(crate) chunk_cap: usize,
    pub(crate) can_confirm: bool,
    /// A download runs; see `WorkbenchDrop::download_busy`.
    pub(crate) download_busy: bool,
}

pub(crate) struct UploadRuntime {
    uploads: Uploads,
    settings: UploadSettings,
    pending: Option<PendingDrop<ClientLoopEvent>>,
    worker: Option<WorkerHandle>,
    replay: VecDeque<ClientLoopEvent>,
    /// Direct attach: the connection that answered the upload probe.
    direct_ready: Option<(ClientEndpointId, u64)>,
    progress_painted: Option<Instant>,
}

impl UploadRuntime {
    pub(crate) fn new(settings: UploadSettings) -> Self {
        Self {
            uploads: Uploads::default(),
            settings,
            pending: None,
            worker: None,
            replay: VecDeque::new(),
            direct_ready: None,
            progress_painted: None,
        }
    }

    pub(crate) fn busy(&self) -> bool {
        self.uploads.busy()
    }

    #[cfg(all(test, unix))]
    pub(crate) fn busy_for_test(&self) -> bool {
        self.uploads.busy()
    }

    pub(crate) fn upload_on_drop(&self) -> bool {
        self.settings.upload_on_drop
    }

    pub(crate) fn settings_mut(&mut self) -> &mut UploadSettings {
        &mut self.settings
    }

    pub(crate) fn mark_direct_ready(&mut self, endpoint_id: ClientEndpointId, generation: u64) {
        self.direct_ready = Some((endpoint_id, generation));
    }

    pub(crate) fn direct_ready(&self, endpoint_id: &ClientEndpointId, generation: u64) -> bool {
        self.direct_ready
            .as_ref()
            .is_some_and(|(ready, ready_generation)| {
                ready == endpoint_id && *ready_generation == generation
            })
    }

    pub(crate) fn gate(
        &self,
        bridge_active: bool,
        server_supports: bool,
        has_target: bool,
    ) -> bool {
        DropGate {
            upload_on_drop: self.settings.upload_on_drop,
            bridge_active,
            server_supports,
            has_target,
        }
        .uploads()
    }

    /// Input arriving while a drop is checked waits behind it.
    pub(crate) fn holding(&self) -> bool {
        self.pending.is_some()
    }

    pub(crate) fn hold(&mut self, event: ClientLoopEvent) {
        if let Some(pending) = self.pending.as_mut() {
            pending.hold(event);
        }
    }

    /// The next event to process before waiting for new ones.
    pub(crate) fn next_replay(&mut self) -> Option<ClientLoopEvent> {
        self.replay.pop_front()
    }

    /// When the loop must wake for upload timers.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        let progress = self
            .uploads
            .progress()
            .and(self.progress_painted)
            .map(|painted| painted + PROGRESS_REPAINT_INTERVAL);
        [
            self.pending.as_ref().map(PendingDrop::deadline),
            self.uploads.next_deadline(),
            progress,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Offers a paste that passed the gate. `plain` makes the event that
    /// replays the paste, unoffered, if it turns out not to be a drop.
    pub(crate) fn offer_drop(
        &mut self,
        candidate: DropCandidate,
        plain: impl FnOnce() -> ClientLoopEvent,
        emit: impl Fn(WorkerEvent) -> bool + Send + 'static,
        now: Instant,
    ) -> DropStart {
        let Some(tokens) = split_drop_text(&candidate.text, crate::platform::drop_path_syntax())
        else {
            return DropStart::NotADrop;
        };
        let Some(paths) = resolve_drop_paths(&tokens.tokens, crate::platform::dropped_path) else {
            return DropStart::NotADrop;
        };
        if self.uploads.busy() || candidate.download_busy {
            // Why check existence here, unlike a first drop: there is no
            // worker free to check it, and refusing text that names no local
            // file would swallow an ordinary paste. Rare, so a stat on the
            // loop is acceptable.
            let exist = paths
                .iter()
                .all(|path| std::fs::symlink_metadata(path).is_ok());
            return if exist {
                DropStart::Busy
            } else {
                DropStart::NotADrop
            };
        }
        let job = self.uploads.start(DropRequest {
            endpoint_id: candidate.endpoint_id,
            generation: candidate.generation,
            target: candidate.target,
            trailing_space: tokens.trailing_space,
            chunk_cap: candidate.chunk_cap,
            can_confirm: candidate.can_confirm,
        });
        match super::worker::spawn(
            job,
            paths,
            self.settings.limits.max_entries,
            crate::upload::source::DuplicateNames::Rename,
            emit,
        ) {
            Ok(worker) => self.worker = Some(worker),
            Err(error) => {
                warn!(%error, "could not start the upload reader; pasting the paths instead");
                self.uploads.abandon(job);
                return DropStart::NotADrop;
            }
        }
        self.pending = Some(PendingDrop::new(job, plain(), now));
        DropStart::Pending
    }

    /// Starts a tree drop. Why no held input: a paste onto the tree is never
    /// replayed as text, so later keys need not wait behind the check.
    pub(crate) fn offer_workbench_drop(
        &mut self,
        drop: WorkbenchDrop,
        emit: impl Fn(WorkerEvent) -> bool + Send + 'static,
    ) -> DropStart {
        if self.uploads.busy() || drop.download_busy {
            return DropStart::Busy;
        }
        let job = self.uploads.start(DropRequest {
            endpoint_id: drop.endpoint_id,
            generation: drop.generation,
            target: UploadTarget::Workbench(drop.target),
            trailing_space: false,
            chunk_cap: drop.chunk_cap,
            can_confirm: true,
        });
        match super::worker::spawn(
            job,
            drop.paths,
            self.settings.limits.max_entries,
            // Two entries of one folder cannot share a name (design §3.2).
            crate::upload::source::DuplicateNames::Reject,
            emit,
        ) {
            Ok(worker) => {
                self.worker = Some(worker);
                DropStart::Pending
            }
            Err(error) => {
                warn!(%error, "could not start the upload reader");
                self.uploads.abandon(job);
                DropStart::NotADrop
            }
        }
    }

    fn resolve_pending(&mut self, job: JobId, is_drop: bool) {
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.job == job)
        {
            if let Some(pending) = self.pending.take() {
                let resolved = pending.resolve(is_drop);
                // Why front: held input must run before anything read after it.
                for event in resolved.held.into_iter().rev() {
                    self.replay.push_front(event);
                }
                if let Some(original) = resolved.original {
                    self.replay.push_front(original);
                }
            }
        }
    }

    pub(crate) fn worker_event(&mut self, event: WorkerEvent, now: Instant) -> Vec<UploadEffect> {
        match event {
            WorkerEvent::Checked { job, is_drop } => {
                let effects = self.uploads.checked(job, is_drop);
                self.resolve_pending(job, is_drop);
                effects
            }
            WorkerEvent::Scanned { job, result } => {
                let limits = self.settings.limits;
                self.uploads.scanned(job, result, &limits, now)
            }
            WorkerEvent::Outgoing { job, write } => self.uploads.outgoing(job, write, now),
            WorkerEvent::WritesDone { job } => self.uploads.writes_done(job, now),
            WorkerEvent::Failed { job, message } => self.uploads.worker_failed(job, message),
        }
    }

    pub(crate) fn response(
        &mut self,
        endpoint_id: &ClientEndpointId,
        generation: u64,
        data: &str,
        now: Instant,
    ) -> Vec<UploadEffect> {
        self.uploads.response(endpoint_id, generation, data, now)
    }

    pub(crate) fn decision(
        &mut self,
        decision: crate::client::shell::ClientUploadDecision,
        now: Instant,
    ) -> Vec<UploadEffect> {
        match decision {
            crate::client::shell::ClientUploadDecision::Cancel(job) => self.uploads.cancel(job),
            crate::client::shell::ClientUploadDecision::Confirm { job, accept } => {
                self.uploads.confirmed(job, accept, now)
            }
            crate::client::shell::ClientUploadDecision::Collision { job, collision } => {
                self.uploads.chosen(job, collision, now)
            }
        }
    }

    fn decline(&mut self, job: JobId, now: Instant) -> Vec<UploadEffect> {
        self.uploads.confirmed(job, false, now)
    }

    pub(crate) fn endpoint_lost(
        &mut self,
        endpoint_id: &ClientEndpointId,
        generation: u64,
    ) -> Vec<UploadEffect> {
        if self
            .direct_ready
            .as_ref()
            .is_some_and(|(ready, ready_generation)| {
                ready == endpoint_id && *ready_generation == generation
            })
        {
            self.direct_ready = None;
        }
        self.uploads.endpoint_lost(endpoint_id, generation)
    }

    pub(crate) fn tick(&mut self, now: Instant) -> Vec<UploadEffect> {
        let mut effects = Vec::new();
        if let Some(job) = self
            .pending
            .as_ref()
            .filter(|pending| pending.expired(now))
            .map(|pending| pending.job)
        {
            effects.extend(self.uploads.abandon(job));
            self.resolve_pending(job, false);
        }
        effects.extend(self.uploads.tick(now));
        effects
    }

    fn worker_command(&mut self, effect: &UploadEffect) -> bool {
        match effect {
            UploadEffect::StartWriter {
                job,
                upload_id,
                chunk_bytes,
                window,
            } => {
                if let Some(worker) = self.worker.as_ref().filter(|worker| worker.job == *job) {
                    worker.send(WorkerCommand::Start {
                        upload_id: upload_id.clone(),
                        chunk_bytes: *chunk_bytes,
                        window: *window,
                    });
                }
                true
            }
            UploadEffect::Credit { job } => {
                if let Some(worker) = self.worker.as_ref().filter(|worker| worker.job == *job) {
                    worker.send(WorkerCommand::Credit);
                }
                true
            }
            UploadEffect::StopWorker { job } => {
                if self
                    .worker
                    .as_ref()
                    .is_some_and(|worker| worker.job == *job)
                {
                    self.worker = None;
                }
                true
            }
            _ => false,
        }
    }

    /// The card's current view, if it is due for a repaint.
    fn progress_due(
        &mut self,
        now: Instant,
        force: bool,
    ) -> Option<Option<super::transfer::UploadProgressView>> {
        let view = self.uploads.progress();
        let due = force
            || view.is_none()
            || self
                .progress_painted
                .is_none_or(|painted| now >= painted + PROGRESS_REPAINT_INTERVAL);
        if !due {
            return None;
        }
        self.progress_painted = view.as_ref().map(|_| now);
        Some(view)
    }
}

fn paste_message(target: &UploadTarget, text: String) -> Option<ClientMessage> {
    Some(match target {
        UploadTarget::Pane(pane_id) => ClientMessage::ClientShellPaneInput {
            pane_id: pane_id.clone(),
            events: vec![ClientPaneInputEvent::Paste(text)],
        },
        UploadTarget::Popup(terminal_id) => ClientMessage::ClientShellPopupInput {
            terminal_id: terminal_id.clone(),
            events: vec![ClientPaneInputEvent::Paste(text)],
        },
        UploadTarget::Direct { bracketed, .. } => {
            let data = if *bracketed {
                [b"\x1b[200~".as_slice(), text.as_bytes(), b"\x1b[201~"].concat()
            } else {
                text.into_bytes()
            };
            ClientMessage::Input { data }
        }
        UploadTarget::Workbench(_) => return None,
    })
}

/// Carries out upload effects, then refreshes the progress card. Returns
/// whether the shell needs a repaint.
pub(crate) fn apply_upload_effects(
    state: &mut ClientState,
    endpoints: &mut EndpointRegistry,
    effects: Vec<UploadEffect>,
    now: Instant,
) -> bool {
    let mut queue: VecDeque<UploadEffect> = effects.into();
    let mut repaint = false;
    while let Some(effect) = queue.pop_front() {
        if state.uploads.worker_command(&effect) {
            if let (UploadEffect::StartWriter { .. }, None) = (&effect, state.shell.as_ref()) {
                crate::client::handle_notify(
                    crate::protocol::NotifyKind::Toast,
                    "Uploading",
                    Some("Detach to cancel"),
                    &state.sound_config,
                );
            }
            continue;
        }
        match effect {
            UploadEffect::Send {
                endpoint_id,
                generation,
                data,
            } => {
                let sent = endpoints.accepts(&endpoint_id, generation)
                    && endpoints.send_to(
                        &endpoint_id,
                        &ClientMessage::EndpointControl {
                            kind: crate::protocol::endpoint::UPLOAD_REQUEST_KIND.into(),
                            data,
                        },
                    ) == EndpointSendOutcome::Sent;
                if !sent {
                    queue.extend(state.uploads.endpoint_lost(&endpoint_id, generation));
                }
            }
            UploadEffect::Paste {
                endpoint_id,
                generation,
                target,
                text,
            } => {
                // The pane may have closed meanwhile; the server drops input
                // for an unknown pane, so nothing else is needed.
                if let Some(message) = paste_message(&target, text) {
                    if endpoints.accepts(&endpoint_id, generation) {
                        let _ = endpoints.send_to(&endpoint_id, &message);
                    }
                }
            }
            UploadEffect::Confirm { job, title, detail } => {
                let opened = state
                    .shell
                    .as_mut()
                    .is_some_and(|shell| shell.open_upload_confirm(job, title, detail));
                if opened {
                    repaint = true;
                } else {
                    queue.extend(state.uploads.decline(job, now));
                    queue.push_back(UploadEffect::Notice {
                        failed: true,
                        title: "Upload not started".into(),
                        body: "Close the open dialog, then drop the files again".into(),
                    });
                }
            }
            UploadEffect::Ask {
                job,
                tab,
                destination,
                summary,
                conflicts,
                note,
            } => {
                let question = crate::client::shell::ClientUploadQuestion {
                    job,
                    destination,
                    summary,
                    conflicts,
                    note,
                };
                let opened = state
                    .shell
                    .as_mut()
                    .is_some_and(|shell| shell.open_workbench_upload_dialog(tab, question));
                if opened {
                    repaint = true;
                } else {
                    queue.extend(state.uploads.uploads.chosen(job, None, now));
                    queue.push_back(UploadEffect::TabNotice {
                        tab,
                        text: "close the open dialog, then drop the files again".into(),
                    });
                }
            }
            UploadEffect::WorkbenchDone {
                tab,
                destination,
                names,
            } => {
                if let Some(shell) = state.shell.as_mut() {
                    repaint |= shell.workbench_upload_done(tab, destination, names);
                }
            }
            UploadEffect::TabNotice { tab, text } => {
                if let Some(shell) = state.shell.as_mut() {
                    repaint |= shell.set_workbench_notice(tab, text);
                }
            }
            UploadEffect::CloseConfirm { job } => {
                if let Some(shell) = state.shell.as_mut() {
                    repaint |= shell.close_upload_confirm(job);
                }
            }
            UploadEffect::Notice {
                failed,
                title,
                body,
            } => match state.shell.as_mut() {
                Some(shell) => repaint |= shell.push_upload_notice(failed, title, body),
                None => crate::client::handle_notify(
                    crate::protocol::NotifyKind::Toast,
                    &title,
                    Some(&body),
                    &state.sound_config,
                ),
            },
            UploadEffect::StartWriter { .. }
            | UploadEffect::Credit { .. }
            | UploadEffect::StopWorker { .. } => {}
        }
    }
    repaint | refresh_upload_progress(state, now, false)
}

/// Copies the card's view into the shell, throttled. Returns whether it changed.
pub(crate) fn refresh_upload_progress(state: &mut ClientState, now: Instant, force: bool) -> bool {
    // Why not while a download runs: the card is shared, and this would clear it.
    if state.downloads.busy() {
        return false;
    }
    let Some(shell) = state.shell.as_mut() else {
        return false;
    };
    match state.uploads.progress_due(now, force) {
        Some(view) => shell.set_upload_progress(view),
        None => false,
    }
}

#[cfg(test)]
mod tests;
