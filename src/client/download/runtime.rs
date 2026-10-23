//! The client loop's side of a download: starting the writer thread, routing
//! its events and the server's answers into `transfer`, and carrying out the
//! effects that returns.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use tracing::warn;

use crate::client::endpoint::{ClientEndpointId, EndpointRegistry, EndpointSendOutcome};
use crate::client::state::ClientState;
use crate::client::upload::transfer::{JobId, UploadProgressView};
use crate::protocol::ClientMessage;

use super::transfer::{DownloadEffect, DownloadRequest, Downloads};
use super::worker::{Destination, WorkerCommand, WorkerEvent, WorkerHandle};

/// The progress card redraws at most this often, as an upload's does.
const PROGRESS_REPAINT_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Default)]
pub(crate) struct DownloadRuntime {
    downloads: Downloads,
    worker: Option<WorkerHandle>,
    progress_painted: Option<Instant>,
}

impl DownloadRuntime {
    /// Whether a download runs. Uploads and downloads share one progress card,
    /// so the loop starts neither while the other runs.
    pub(crate) fn busy(&self) -> bool {
        self.downloads.busy()
    }

    /// Starts the writer thread for a new download. The effects are only
    /// non-empty when the thread could not start.
    pub(crate) fn start(
        &mut self,
        request: DownloadRequest,
        destination: Destination,
        emit: impl Fn(WorkerEvent) -> bool + Send + 'static,
    ) -> Vec<DownloadEffect> {
        let job = self.downloads.start(request);
        match super::worker::spawn(job, destination, emit) {
            Ok(worker) => {
                self.worker = Some(worker);
                Vec::new()
            }
            Err(error) => {
                warn!(%error, "could not start the download writer");
                self.downloads
                    .worker_failed(job, "the download could not be started".into())
            }
        }
    }

    pub(crate) fn worker_event(&mut self, event: WorkerEvent, now: Instant) -> Vec<DownloadEffect> {
        match event {
            WorkerEvent::Prepared { job, result } => self.downloads.prepared(job, result, now),
            WorkerEvent::Opened { job } => self.downloads.opened(job, now),
            WorkerEvent::Applied {
                job,
                seq,
                entries,
                bytes,
            } => self.downloads.applied(job, seq, entries, bytes, now),
            WorkerEvent::Published {
                job,
                destination,
                paths,
                warnings,
            } => self.downloads.published(job, destination, paths, warnings),
            WorkerEvent::Failed { job, message } => self.downloads.worker_failed(job, message),
        }
    }

    pub(crate) fn response(
        &mut self,
        endpoint_id: &ClientEndpointId,
        generation: u64,
        data: String,
    ) -> Vec<DownloadEffect> {
        self.downloads.response(endpoint_id, generation, data)
    }

    /// The user clicked the card.
    pub(crate) fn cancel(&mut self, job: JobId) -> Vec<DownloadEffect> {
        self.downloads.cancel(job)
    }

    pub(crate) fn endpoint_lost(
        &mut self,
        endpoint_id: &ClientEndpointId,
        generation: u64,
    ) -> Vec<DownloadEffect> {
        self.downloads.endpoint_lost(endpoint_id, generation)
    }

    pub(crate) fn tick(&mut self, now: Instant) -> Vec<DownloadEffect> {
        self.downloads.tick(now)
    }

    /// When the loop must wake for download timers.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        let progress = self
            .downloads
            .progress()
            .and(self.progress_painted)
            .map(|painted| painted + PROGRESS_REPAINT_INTERVAL);
        [self.downloads.next_deadline(), progress]
            .into_iter()
            .flatten()
            .min()
    }

    /// Hands the worker its commands, giving back every other effect. Why by
    /// value: an `Apply` carries up to about 1.3 MB that a copy would double.
    fn worker_command(&mut self, effect: DownloadEffect) -> Option<DownloadEffect> {
        match effect {
            DownloadEffect::StartWriter { job, names } => {
                self.worker_for(job, WorkerCommand::Open { names });
                None
            }
            DownloadEffect::Apply { job, seq, data } => {
                self.worker_for(job, WorkerCommand::Apply { seq, data });
                None
            }
            DownloadEffect::StopWorker { job } => {
                if self.worker.as_ref().is_some_and(|worker| worker.job == job) {
                    self.worker = None;
                }
                None
            }
            other => Some(other),
        }
    }

    fn worker_for(&self, job: JobId, command: WorkerCommand) {
        if let Some(worker) = self.worker.as_ref().filter(|worker| worker.job == job) {
            worker.send(command);
        }
    }

    /// The card's current view, if it is due for a repaint.
    fn progress_due(&mut self, now: Instant, force: bool) -> Option<Option<UploadProgressView>> {
        let view = self.downloads.progress();
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

/// Carries out download effects, then refreshes the progress card. Returns
/// whether the shell needs a repaint.
pub(crate) fn apply_download_effects(
    state: &mut ClientState,
    endpoints: &mut EndpointRegistry,
    effects: Vec<DownloadEffect>,
    now: Instant,
) -> bool {
    let mut queue: VecDeque<DownloadEffect> = effects.into();
    let mut repaint = false;
    while let Some(effect) = queue.pop_front() {
        let Some(effect) = state.downloads.worker_command(effect) else {
            continue;
        };
        match effect {
            DownloadEffect::Send {
                endpoint_id,
                generation,
                data,
            } => {
                let sent = endpoints.accepts(&endpoint_id, generation)
                    && endpoints.send_to(
                        &endpoint_id,
                        &ClientMessage::EndpointControl {
                            kind: crate::protocol::endpoint::DOWNLOAD_REQUEST_KIND.into(),
                            data,
                        },
                    ) == EndpointSendOutcome::Sent;
                if !sent {
                    queue.extend(state.downloads.endpoint_lost(&endpoint_id, generation));
                }
            }
            DownloadEffect::TabNotice { tab, text } => {
                if let Some(shell) = state.shell.as_mut() {
                    repaint |= shell.set_workbench_notice(tab, text);
                }
            }
            DownloadEffect::Notice {
                failed,
                title,
                body,
            } => match state.shell.as_mut() {
                Some(shell) => repaint |= shell.push_download_notice(failed, title, body),
                None => crate::client::handle_notify(
                    crate::protocol::NotifyKind::Toast,
                    &title,
                    Some(&body),
                    &state.sound_config,
                ),
            },
            DownloadEffect::StartWriter { .. }
            | DownloadEffect::Apply { .. }
            | DownloadEffect::StopWorker { .. } => {}
        }
    }
    repaint | refresh_download_progress(state, now, false)
}

/// Copies the card's view into the shell, throttled. Returns whether it changed.
pub(crate) fn refresh_download_progress(
    state: &mut ClientState,
    now: Instant,
    force: bool,
) -> bool {
    // Why not while an upload runs: the card is shared, and this would clear it.
    if state.uploads.busy() {
        return false;
    }
    let Some(shell) = state.shell.as_mut() else {
        return false;
    };
    match state.downloads.progress_due(now, force) {
        Some(view) => shell.set_upload_progress(view),
        None => false,
    }
}

#[cfg(all(test, unix))]
mod tests;
