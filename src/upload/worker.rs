//! One thread per upload session. It owns the session state and the sink, so
//! every filesystem or process call happens here and never on the app thread.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;

use crate::api::schema::{
    PaneLocation, PaneUploadCollision, PaneUploadEntryHeader, PaneUploadFinished,
    PaneUploadStarted, ResponseResult,
};

use super::path::Failure;
use super::session::{Session, WriteRequest};
use super::sink::{Committed, UploadSink};
use super::{UPLOAD_CHUNK_BYTES, UPLOAD_WINDOW};

pub(crate) const PHASE_STAGING: u8 = 0;
/// `C` was sent: the receiver ignores signals and must be left to finish.
pub(crate) const PHASE_COMMITTING: u8 = 1;
pub(crate) const PHASE_DONE: u8 = 2;

/// Shared between the app thread, the worker, and a remote sink's watchdog.
/// Why not a job on the channel for cancel: the channel is bounded, and a
/// cancel must get through even when it is full.
#[derive(Debug, Default)]
pub(crate) struct Control {
    cancel: AtomicBool,
    phase: AtomicU8,
}

impl Control {
    pub(crate) fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Acquire)
    }

    pub(crate) fn phase(&self) -> u8 {
        self.phase.load(Ordering::Acquire)
    }

    pub(crate) fn set_phase(&self, phase: u8) {
        self.phase.store(phase, Ordering::Release);
    }
}

/// Answers one request. Built by whoever received it (API socket or endpoint control).
pub(crate) type Reply = Box<dyn FnOnce(Result<ResponseResult, Failure>) + Send>;

pub(crate) struct WriteJob {
    pub(crate) path: String,
    pub(crate) entry: Option<PaneUploadEntryHeader>,
    pub(crate) offset: u64,
    pub(crate) data: String,
    pub(crate) last: bool,
}

pub(crate) enum UploadJob {
    Begin(Reply),
    Write(WriteJob, Reply),
    Finish(PaneUploadCollision, Reply),
}

impl UploadJob {
    /// Answers a job that will never run.
    pub(crate) fn refuse(self, failure: Failure) {
        match self {
            Self::Begin(reply) | Self::Write(_, reply) | Self::Finish(_, reply) => {
                reply(Err(failure));
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Timeouts {
    /// Until the first write. Long: the client may be waiting for a person to
    /// answer a conflict dialog.
    pub(crate) first_write: Duration,
    pub(crate) between_writes: Duration,
    /// How often an idle worker looks at the cancel flag.
    pub(crate) tick: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            first_write: Duration::from_secs(600),
            between_writes: Duration::from_secs(120),
            tick: Duration::from_millis(100),
        }
    }
}

pub(crate) fn cancelled() -> Failure {
    ("upload_cancelled", "the upload was cancelled".into())
}

enum WriteError {
    /// The write broke a rule; the session is unchanged and may continue.
    Rejected(Failure),
    /// The sink failed; the upload is over.
    Fatal(Failure),
}

pub(crate) struct Worker {
    pub(crate) id: String,
    pub(crate) session: Session,
    pub(crate) sink: Box<dyn UploadSink>,
    pub(crate) control: Arc<Control>,
    pub(crate) location: PaneLocation,
    pub(crate) jobs: Receiver<UploadJob>,
    pub(crate) timeouts: Timeouts,
}

impl Worker {
    pub(crate) fn run(mut self) {
        let destination = match self.jobs.recv() {
            Ok(UploadJob::Begin(reply)) => match self.begin(reply) {
                Some(destination) => destination,
                None => return self.close(),
            },
            Ok(UploadJob::Write(_, reply) | UploadJob::Finish(_, reply)) => {
                reply(Err(cancelled()));
                return self.close();
            }
            Err(_) => return self.close(),
        };
        let mut last_request = Instant::now();
        loop {
            if self.control.is_cancelled() {
                break;
            }
            let limit = if self.session.has_writes() {
                self.timeouts.between_writes
            } else {
                self.timeouts.first_write
            };
            let idle = last_request.elapsed();
            if idle >= limit {
                tracing::info!(
                    "upload abandoned after {}s without requests",
                    idle.as_secs()
                );
                break;
            }
            let job = match self
                .jobs
                .recv_timeout((limit - idle).min(self.timeouts.tick))
            {
                Ok(job) => job,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => break,
            };
            last_request = Instant::now();
            match job {
                UploadJob::Begin(reply) => reply(Err((
                    "invalid_params",
                    "the upload has already begun".into(),
                ))),
                UploadJob::Write(job, reply) => {
                    if self.control.is_cancelled() {
                        reply(Err(cancelled()));
                        break;
                    }
                    match self.write(job) {
                        Ok(()) => reply(Ok(ResponseResult::PaneUploadProgress(
                            self.session.progress(),
                        ))),
                        Err(WriteError::Rejected(failure)) => reply(Err(failure)),
                        Err(WriteError::Fatal(failure)) => {
                            reply(Err(failure));
                            break;
                        }
                    }
                }
                UploadJob::Finish(collision, reply) => {
                    if self.control.is_cancelled() {
                        reply(Err(cancelled()));
                        break;
                    }
                    match self.finish(collision, &destination) {
                        Ok(Some(finished)) => {
                            reply(Ok(ResponseResult::PaneUploadFinished(finished)));
                            return self.close();
                        }
                        Ok(None) => {}
                        Err(WriteError::Rejected(failure)) => reply(Err(failure)),
                        Err(WriteError::Fatal(failure)) => {
                            reply(Err(failure));
                            return self.close();
                        }
                    }
                }
            }
        }
        self.sink.abort();
        self.close();
    }

    fn begin(&mut self, reply: Reply) -> Option<String> {
        if self.control.is_cancelled() {
            reply(Err(cancelled()));
            return None;
        }
        let names = self.session.names().to_vec();
        match self.sink.begin(&names) {
            Ok(began) => {
                let destination = began.destination.clone();
                reply(Ok(ResponseResult::PaneUploadStarted(PaneUploadStarted {
                    upload_id: self.id.clone(),
                    location: self.location.clone(),
                    destination: began.destination,
                    conflicts: began.conflicts,
                    chunk_bytes: UPLOAD_CHUNK_BYTES as u64,
                    window: UPLOAD_WINDOW,
                })));
                Some(destination)
            }
            Err(failure) => {
                self.sink.abort();
                reply(Err(failure));
                None
            }
        }
    }

    fn write(&mut self, job: WriteJob) -> Result<(), WriteError> {
        let data = base64::engine::general_purpose::STANDARD
            .decode(job.data.as_bytes())
            .map_err(|error| {
                WriteError::Rejected(("invalid_params", format!("data is not base64: {error}")))
            })?;
        let plan = self
            .session
            .accept(WriteRequest {
                path: &job.path,
                entry: job.entry.as_ref(),
                offset: job.offset,
                data_len: data.len(),
                last: job.last,
            })
            .map_err(WriteError::Rejected)?;
        if let Some(start) = &plan.start {
            self.sink.start(start).map_err(WriteError::Fatal)?;
        }
        if !data.is_empty() {
            self.sink.data(&data).map_err(WriteError::Fatal)?;
        }
        if plan.end_file {
            self.sink.end_file().map_err(WriteError::Fatal)?;
        }
        Ok(())
    }

    /// `Ok(None)`: a conflict was answered and the session stays open.
    fn finish(
        &mut self,
        collision: PaneUploadCollision,
        destination: &str,
    ) -> Result<Option<PaneUploadFinished>, WriteError> {
        self.session
            .check_complete()
            .map_err(WriteError::Rejected)?;
        if collision == PaneUploadCollision::Unknown {
            return Err(WriteError::Rejected((
                "invalid_params",
                "unknown collision policy".into(),
            )));
        }
        self.control.set_phase(PHASE_COMMITTING);
        let committed = self.sink.commit(
            collision,
            &self.session.nested_directories(),
            &self.session.top_directories(),
        );
        match committed {
            Ok(Committed::Published { paths, warnings }) => {
                let paths = self
                    .session
                    .names()
                    .iter()
                    .filter_map(|name| {
                        paths
                            .iter()
                            .find(|(published, _)| published == name)
                            .map(|(_, path)| path.clone())
                    })
                    .collect();
                Ok(Some(PaneUploadFinished {
                    location: self.location.clone(),
                    destination: destination.to_owned(),
                    paths,
                    warnings,
                }))
            }
            Ok(Committed::Conflict(names)) => {
                self.control.set_phase(PHASE_STAGING);
                Err(WriteError::Rejected((
                    "upload_conflict",
                    format!("already exists in the destination: {}", names.join(", ")),
                )))
            }
            Err(failure) => Err(WriteError::Fatal(failure)),
        }
    }

    /// Marks the session done and answers whatever was queued behind it.
    fn close(self) {
        self.control.set_phase(PHASE_DONE);
        while let Ok(job) = self.jobs.try_recv() {
            job.refuse(cancelled());
        }
    }
}

#[cfg(all(test, unix))]
mod tests;
