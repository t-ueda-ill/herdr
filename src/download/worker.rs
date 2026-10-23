//! One thread per download session. It owns the source and the packer, answers
//! `read`s in order, and does no I/O of its own: the source's thread reads
//! files or a helper's output, so a read that never returns cannot stop this
//! thread from answering a heartbeat or noticing a cancel.

use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::api::schema::{PaneDownloadChunk, PaneDownloadStarted, PaneLocation, ResponseResult};
use crate::transfer::registry::{TransferJob, Vocabulary};
use crate::upload::path::Failure;
use crate::upload::worker::{Control, Reply, PHASE_DONE};

use super::frame::{Frame, Limits, Packer};
use super::source::{cancelled, DownloadSource};
use super::{DOWNLOAD_WINDOW, MAX_DOWNLOAD_REPLY_JSON};

/// Server-wide and per-owner session caps. Why not share upload's: a long
/// upload would then starve downloads of slots.
pub(crate) const MAX_SESSIONS: usize = 4;
pub(crate) const MAX_SESSIONS_PER_OWNER: usize = 2;

/// What the JSON around a result adds: the request id and two keys. Why a
/// constant: the worker does not know how the answer will be wrapped.
const REPLY_ENVELOPE_BYTES: usize = 512;

/// How long a read that already has content waits for more frames to fill its
/// answer. Why: fewer, fuller answers without delaying a slow source's data.
const PACK_WAIT: Duration = Duration::from_millis(20);

pub(crate) enum DownloadJob {
    Begin(Reply),
    Read { seq: u64, reply: Reply },
}

impl DownloadJob {
    /// Answers a job that will never run.
    pub(crate) fn refuse(self, failure: Failure) {
        match self {
            Self::Begin(reply) | Self::Read { reply, .. } => reply(Err(failure)),
        }
    }
}

impl TransferJob for DownloadJob {
    const VOCABULARY: Vocabulary = Vocabulary {
        noun: "download",
        id_prefix: "dl",
        busy_code: "download_busy",
        not_found_code: "download_not_found",
        cancelled_code: "download_cancelled",
        in_flight: "reads",
        max_sessions: MAX_SESSIONS,
        max_sessions_per_owner: MAX_SESSIONS_PER_OWNER,
    };

    fn refuse(self, failure: Failure) {
        DownloadJob::refuse(self, failure);
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Timeouts {
    /// How often a wait looks at the cancel flag and for new reads.
    pub(crate) tick: Duration,
    /// How long `begin` may wait for the source's walk, on top of its own start-up.
    pub(crate) walk: Duration,
    /// From the answer to `begin` until the first read.
    pub(crate) first_read: Duration,
    pub(crate) between_reads: Duration,
    /// A read the source cannot answer yet is answered with no records after this.
    pub(crate) heartbeat: Duration,
    /// How long the source may wait on its own I/O before the stream fails.
    pub(crate) stall: Duration,
    /// How long a helper process gets to stop at each step.
    pub(crate) grace: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            tick: Duration::from_millis(100),
            walk: Duration::from_secs(120),
            first_read: Duration::from_secs(120),
            between_reads: Duration::from_secs(120),
            heartbeat: Duration::from_secs(25),
            stall: Duration::from_secs(120),
            grace: Duration::from_secs(2),
        }
    }
}

/// A read that arrived while another was being answered.
struct Waiting {
    seq: u64,
    arrived: Instant,
    reply: Reply,
}

/// What a session needs to start.
pub(crate) struct Setup {
    pub(crate) id: String,
    pub(crate) source: Box<dyn DownloadSource>,
    /// Top-level names, in the order requested.
    pub(crate) names: Vec<String>,
    pub(crate) limits: Limits,
    pub(crate) control: Arc<Control>,
    pub(crate) location: PaneLocation,
    pub(crate) jobs: Receiver<DownloadJob>,
    pub(crate) timeouts: Timeouts,
}

pub(crate) struct Worker {
    id: String,
    source: Box<dyn DownloadSource>,
    names: Vec<String>,
    limits: Limits,
    control: Arc<Control>,
    location: PaneLocation,
    jobs: Receiver<DownloadJob>,
    timeouts: Timeouts,
    /// Reads past the one being answered, in the order they came.
    waiting: VecDeque<Waiting>,
    /// The stream ended cleanly, so late reads find nothing rather than a cancel.
    finished: bool,
}

/// Counts what `serde_json` would write without keeping it.
struct Counter(usize);

impl std::io::Write for Counter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0 += buf.len();
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn json_len(result: &ResponseResult) -> usize {
    let mut counter = Counter(0);
    // A value that cannot be encoded counts as too large, so it is never sent.
    if serde_json::to_writer(&mut counter, result).is_err() {
        return usize::MAX;
    }
    counter.0
}

/// Whether `result`, once wrapped as a response, stays under
/// [`MAX_DOWNLOAD_REPLY_JSON`] and so fits one frame.
pub(crate) fn fits_one_reply(result: &ResponseResult) -> bool {
    json_len(result).saturating_add(REPLY_ENVELOPE_BYTES) <= MAX_DOWNLOAD_REPLY_JSON
}

/// Takes the reads that arrived meanwhile. A read beyond the window is refused
/// at once, so the client learns it overran without waiting for the source.
fn absorb(jobs: &Receiver<DownloadJob>, waiting: &mut VecDeque<Waiting>) -> Result<(), Failure> {
    loop {
        match jobs.try_recv() {
            Ok(DownloadJob::Read { seq, reply }) => {
                // One read is being answered, so only window - 1 may wait.
                if 1 + waiting.len() >= DOWNLOAD_WINDOW as usize {
                    reply(Err((
                        "download_busy",
                        "more reads are in flight than the download window allows".into(),
                    )));
                } else {
                    waiting.push_back(Waiting {
                        seq,
                        arrived: Instant::now(),
                        reply,
                    });
                }
            }
            Ok(DownloadJob::Begin(reply)) => reply(Err((
                "invalid_params",
                "the download has already begun".into(),
            ))),
            Err(TryRecvError::Empty) => return Ok(()),
            // Why cancelled: the registry drops its sender only when it aborts.
            Err(TryRecvError::Disconnected) => return Err(cancelled()),
        }
    }
}

impl Worker {
    pub(crate) fn new(setup: Setup) -> Self {
        let Setup {
            id,
            source,
            names,
            limits,
            control,
            location,
            jobs,
            timeouts,
        } = setup;
        Self {
            id,
            source,
            names,
            limits,
            control,
            location,
            jobs,
            timeouts,
            waiting: VecDeque::new(),
            finished: false,
        }
    }

    /// Answers the queued begin of a worker that never ran.
    pub(crate) fn refuse_begin(&self, failure: Failure) {
        if let Ok(job) = self.jobs.try_recv() {
            job.refuse(failure);
        }
    }

    pub(crate) fn run(mut self) {
        let began = match self.jobs.recv() {
            Ok(DownloadJob::Begin(reply)) => self.begin(reply),
            Ok(DownloadJob::Read { reply, .. }) => {
                reply(Err(cancelled()));
                false
            }
            Err(_) => false,
        };
        if began {
            self.serve();
        }
        // Why answer and free the slot before stopping the source: stopping a
        // helper can take its whole grace, and neither the queued reads nor
        // the owner's next download should wait for that.
        self.close();
        // Why on every path: a helper process outlives a dropped source only
        // until its next write, which may be never.
        self.source.abort();
        // Reads the app queued while the source stopped would otherwise be
        // dropped unanswered with the channel.
        self.refuse_queued();
    }

    fn begin(&mut self, reply: Reply) -> bool {
        if self.control.is_cancelled() {
            reply(Err(cancelled()));
            return false;
        }
        match self.source.begin() {
            Ok(began) => {
                reply(Ok(ResponseResult::PaneDownloadStarted(
                    PaneDownloadStarted {
                        download_id: self.id.clone(),
                        location: self.location.clone(),
                        names: self.names.clone(),
                        entries: began.entries,
                        bytes: began.bytes,
                        chunk_bytes: self.limits.chunk_bytes as u64,
                        window: DOWNLOAD_WINDOW,
                    },
                )));
                true
            }
            Err(failure) => {
                reply(Err(failure));
                false
            }
        }
    }

    fn serve(&mut self) {
        let mut packer = Packer::new(self.names.clone(), self.limits);
        let mut expected = 0u64;
        let mut last_activity = Instant::now();
        loop {
            if self.control.is_cancelled() {
                return;
            }
            let read = match self.waiting.pop_front() {
                Some(read) => read,
                None => {
                    let limit = if expected == 0 {
                        self.timeouts.first_read
                    } else {
                        self.timeouts.between_reads
                    };
                    let idle = last_activity.elapsed();
                    if idle >= limit {
                        tracing::info!(
                            "download abandoned after {}s without reads",
                            idle.as_secs()
                        );
                        return;
                    }
                    match self
                        .jobs
                        .recv_timeout((limit - idle).min(self.timeouts.tick))
                    {
                        Ok(DownloadJob::Read { seq, reply }) => Waiting {
                            seq,
                            arrived: Instant::now(),
                            reply,
                        },
                        Ok(DownloadJob::Begin(reply)) => {
                            reply(Err((
                                "invalid_params",
                                "the download has already begun".into(),
                            )));
                            continue;
                        }
                        Err(RecvTimeoutError::Timeout) => continue,
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
            };
            if read.seq != expected {
                (read.reply)(Err((
                    "invalid_params",
                    format!("expected read {expected}, got {}", read.seq),
                )));
                continue;
            }
            let answer = self.answer(&mut packer, &read);
            last_activity = Instant::now();
            match answer {
                Ok(chunk) => {
                    let result = ResponseResult::PaneDownloadChunk(chunk);
                    // Why checked here: only the worker can end the session, and
                    // an answer that overflows a frame would be dropped by the
                    // server with the client waiting for it.
                    if !fits_one_reply(&result) {
                        tracing::error!(seq = read.seq, "download answer exceeds the frame budget");
                        (read.reply)(Err((
                            "download_failed",
                            "an answer did not fit in one frame".into(),
                        )));
                        return;
                    }
                    let done =
                        matches!(&result, ResponseResult::PaneDownloadChunk(chunk) if chunk.done);
                    (read.reply)(Ok(result));
                    if done {
                        self.finished = true;
                        return;
                    }
                    expected += 1;
                }
                Err(failure) => {
                    (read.reply)(Err(failure));
                    return;
                }
            }
        }
    }

    /// Builds the answer to one read from what the source has, waiting no
    /// longer than the heartbeat from when the read arrived.
    fn answer(
        &mut self,
        packer: &mut Packer,
        read: &Waiting,
    ) -> Result<PaneDownloadChunk, Failure> {
        let Self {
            source,
            jobs,
            control,
            timeouts,
            waiting,
            ..
        } = self;
        let deadline = read.arrived + timeouts.heartbeat;
        // Whether a frame that makes records has come: after one, only a
        // moment more is spent filling the answer.
        let mut content = false;
        let mut next = || -> Result<Option<Frame>, Failure> {
            loop {
                absorb(jobs, waiting)?;
                if control.is_cancelled() {
                    return Err(cancelled());
                }
                let now = Instant::now();
                let wait = if content {
                    PACK_WAIT
                } else {
                    deadline.saturating_duration_since(now)
                }
                .min(timeouts.tick);
                match source.next(wait, control)? {
                    Some(frame) => {
                        content |=
                            matches!(frame, Frame::Entry { .. } | Frame::Data(_) | Frame::End);
                        return Ok(Some(frame));
                    }
                    None if content || Instant::now() >= deadline => return Ok(None),
                    None => {}
                }
            }
        };
        packer.fill(read.seq, &mut next)
    }

    /// Why a late read is refused: the stream ended cleanly, or it did not.
    fn ending(&self) -> Failure {
        if self.finished {
            ("download_not_found", "the download is finished".to_owned())
        } else {
            cancelled()
        }
    }

    /// Marks the session done and answers whatever was queued behind it.
    fn close(&mut self) {
        let failure = self.ending();
        for read in self.waiting.drain(..) {
            (read.reply)(Err(failure.clone()));
        }
        self.control.set_phase(PHASE_DONE);
        self.refuse_queued();
    }

    fn refuse_queued(&self) {
        let failure = self.ending();
        while let Ok(job) = self.jobs.try_recv() {
            job.refuse(failure.clone());
        }
    }
}

#[cfg(test)]
mod tests;
