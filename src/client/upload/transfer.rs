//! One dropped upload from scan to paste, as a pure state machine (design
//! §3.4). It never reads files or sends anything: each call returns the
//! effects the client loop carries out, so every ordering rule is testable.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::api::schema::{
    Method, PaneUploadBeginParams, PaneUploadCancelParams, PaneUploadCollision,
    PaneUploadFinishParams, Request, ResponseResult,
};
use crate::client::endpoint::ClientEndpointId;
use crate::config::UploadLimits;
use crate::upload::source::ScanError;

use super::limits::{assess, format_bytes, Assessment};

pub(crate) type JobId = u64;

/// Begin and write answers must come within this long (design §3.4).
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Chunk size over anything but a same-machine socket. Why not the server's
/// 512 KiB: keystrokes share the endpoint stream, and two 700 KB frames on a
/// 1 MB/s link would delay them by over a second (design §1.6).
pub(crate) const REMOTE_CHUNK_BYTES: usize = 128 * 1024;

/// Where the paste goes once the upload is published: fixed at drop time,
/// even if focus moves meanwhile (design §3.7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UploadTarget {
    Pane(String),
    Popup(String),
    /// A directly attached terminal; `bracketed` repeats how the drop came.
    // Why allow: direct attach is Unix-only, yet the transfer stays portable.
    #[cfg_attr(windows, allow(dead_code))]
    Direct {
        terminal_id: String,
        bracketed: bool,
    },
    /// A folder of a file workbench tree (spec Q1, Q18): nothing is pasted,
    /// the user picks the collision policy, and the tab lists the folder again.
    Workbench(WorkbenchTarget),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkbenchTarget {
    /// The investigation tab's id.
    pub(crate) tab: u64,
    /// The folder as the tree names it. Why not the server's answer: that one
    /// is resolved (`cd -P`), and the tree keys its listings by its own path.
    pub(crate) destination: String,
    /// A live pane of an ssh/container location; `None` for the server's host.
    pub(crate) pane_id: Option<String>,
    /// The environment the tab browses (`Host` for the server's own disk).
    pub(crate) location: crate::api::schema::PaneLocation,
}

impl WorkbenchTarget {
    /// How a notice names the tab's environment.
    fn location_label(&self) -> String {
        let location = &self.location;
        match (&location.container, &location.ssh_host) {
            (Some(name), Some(host)) => format!("{name}@{host}"),
            (Some(name), None) => name.clone(),
            (None, Some(host)) => host.clone(),
            (None, None) => "the host".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DropRequest {
    pub(crate) endpoint_id: ClientEndpointId,
    pub(crate) generation: u64,
    pub(crate) target: UploadTarget,
    pub(crate) trailing_space: bool,
    /// Largest chunk this connection should carry.
    pub(crate) chunk_cap: usize,
    /// Shell mode can show a dialog; a direct attach cannot.
    pub(crate) can_confirm: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ScanSummary {
    pub(crate) names: Vec<String>,
    pub(crate) entries: u64,
    pub(crate) bytes: u64,
    pub(crate) skipped_special: u64,
    pub(crate) warnings: Vec<String>,
}

/// One write the worker prepared, already serialized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WriteOut {
    pub(crate) request_id: String,
    pub(crate) data: String,
    pub(crate) bytes: u64,
    pub(crate) completes_entry: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UploadEffect {
    /// An `endpoint.upload.request.v1` control with this JSON request.
    Send {
        endpoint_id: ClientEndpointId,
        generation: u64,
        data: String,
    },
    StartWriter {
        job: JobId,
        upload_id: String,
        chunk_bytes: usize,
        window: u32,
    },
    /// The worker may prepare one more write.
    Credit {
        job: JobId,
    },
    StopWorker {
        job: JobId,
    },
    Paste {
        endpoint_id: ClientEndpointId,
        generation: u64,
        target: UploadTarget,
        text: String,
    },
    Confirm {
        job: JobId,
        title: String,
        detail: String,
    },
    CloseConfirm {
        job: JobId,
    },
    Notice {
        failed: bool,
        title: String,
        body: String,
    },
    /// A tree drop began: ask how to publish before any byte is sent.
    Ask {
        job: JobId,
        tab: u64,
        destination: String,
        summary: String,
        /// Names the server found in the destination at begin.
        conflicts: Vec<String>,
        /// Why the question is asked again (a name appeared before publishing).
        note: Option<String>,
    },
    /// A tree drop was published into `destination` under these names.
    WorkbenchDone {
        tab: u64,
        destination: String,
        names: Vec<String>,
    },
    /// The tab's notice line. Why besides `Notice`: the workbench hides toasts.
    TabNotice {
        tab: u64,
        text: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UploadProgressView {
    pub(crate) job: JobId,
    pub(crate) title: String,
    pub(crate) body: String,
    /// False once finish is sent: the server is publishing and will not stop.
    pub(crate) cancellable: bool,
}

#[derive(Debug)]
struct InFlight {
    request_id: String,
    bytes: u64,
    completes_entry: bool,
    deadline: Instant,
}

#[derive(Debug)]
enum Phase {
    /// The worker is checking that every path exists.
    Checking,
    Scanning,
    Confirming,
    /// A tree drop's session exists; the user picks the collision policy.
    /// `writes_done` when a finish met a new conflict and only finish remains.
    Asking {
        writes_done: bool,
    },
    Beginning {
        request_id: String,
        deadline: Instant,
    },
    Sending {
        in_flight: VecDeque<InFlight>,
        writes_done: bool,
    },
    Finishing {
        request_id: String,
        deadline: Instant,
    },
}

#[derive(Debug)]
struct Job {
    id: JobId,
    request: DropRequest,
    phase: Phase,
    summary: ScanSummary,
    upload_id: Option<String>,
    /// Chunk size and window from begin, kept while a tree drop asks.
    writer: Option<(usize, u32)>,
    collision: PaneUploadCollision,
    conflicts: Vec<String>,
    acked_entries: u64,
    acked_bytes: u64,
}

impl Job {
    fn request_id(&self, suffix: &str) -> String {
        format!("upload:{}:{suffix}", self.id)
    }

    fn send(&self, request: &Request) -> Option<UploadEffect> {
        let data = serde_json::to_string(request).ok()?;
        Some(UploadEffect::Send {
            endpoint_id: self.request.endpoint_id.clone(),
            generation: self.request.generation,
            data,
        })
    }

    fn workbench(&self) -> Option<&WorkbenchTarget> {
        match &self.request.target {
            UploadTarget::Workbench(target) => Some(target),
            _ => None,
        }
    }

    fn ask(&self, note: Option<String>) -> Option<UploadEffect> {
        let target = self.workbench()?;
        let summary = &self.summary;
        let mut text = format!(
            "{} · {}",
            plural(summary.entries, "item"),
            format_bytes(summary.bytes)
        );
        if summary.skipped_special > 0 {
            text.push_str(&format!(
                " · skipping {}",
                plural(summary.skipped_special, "special file")
            ));
        }
        Some(UploadEffect::Ask {
            job: self.id,
            tab: target.tab,
            destination: target.destination.clone(),
            summary: text,
            conflicts: self.conflicts.clone(),
            note,
        })
    }

    /// Best effort: the server also cleans up on idle, disconnect, or pane exit.
    fn cancel_request(&self) -> Option<UploadEffect> {
        let upload_id = self.upload_id.clone()?;
        self.send(&Request {
            id: self.request_id("cancel"),
            method: Method::PaneUploadCancel(PaneUploadCancelParams { upload_id }),
        })
    }
}

/// Which pending request an answer belongs to.
#[derive(Clone, Copy)]
enum Answered {
    Begin,
    Write(usize),
    Finish,
}

/// A begin still in flight when its job ended (cancel or timeout). Why keep
/// it: the server may still open the session, which would hold a stage and
/// one of the client's two session slots until its idle limit.
#[derive(Debug)]
struct OrphanedBegin {
    job: JobId,
    endpoint_id: ClientEndpointId,
    generation: u64,
    request_id: String,
}

/// More than this many unanswered begins means the server is not answering;
/// it then cleans up on its own idle limit.
const MAX_ORPHANED_BEGINS: usize = 8;

#[derive(Debug, Default)]
pub(crate) struct Uploads {
    next_job: JobId,
    job: Option<Job>,
    orphans: Vec<OrphanedBegin>,
}

fn failed(body: impl Into<String>) -> UploadEffect {
    UploadEffect::Notice {
        failed: true,
        title: "Upload failed".into(),
        body: body.into(),
    }
}

fn plural(count: u64, one: &str) -> String {
    if count == 1 {
        format!("1 {one}")
    } else {
        format!("{count} {one}s")
    }
}

impl Uploads {
    /// One upload at a time. Why not queue a second drop: its paths would
    /// otherwise reach the remote shell as plain text, and the server allows
    /// only two sessions per client anyway.
    pub(crate) fn busy(&self) -> bool {
        self.job.is_some()
    }

    pub(crate) fn start(&mut self, request: DropRequest) -> JobId {
        self.next_job += 1;
        let id = self.next_job;
        self.job = Some(Job {
            id,
            request,
            phase: Phase::Checking,
            summary: ScanSummary::default(),
            upload_id: None,
            writer: None,
            // Terminal panes never ask (spec Q4); a tree drop sets the choice.
            collision: PaneUploadCollision::Rename,
            conflicts: Vec::new(),
            acked_entries: 0,
            acked_bytes: 0,
        });
        id
    }

    fn job_mut(&mut self, job: JobId) -> Option<&mut Job> {
        self.job.as_mut().filter(|current| current.id == job)
    }

    /// Remembers the current job's begin if it has not been answered yet.
    fn orphan_begin(&mut self) {
        let Some(job) = self.job.as_ref() else {
            return;
        };
        let Phase::Beginning { request_id, .. } = &job.phase else {
            return;
        };
        if self.orphans.len() == MAX_ORPHANED_BEGINS {
            self.orphans.remove(0);
        }
        self.orphans.push(OrphanedBegin {
            job: job.id,
            endpoint_id: job.request.endpoint_id.clone(),
            generation: job.request.generation,
            request_id: request_id.clone(),
        });
    }

    /// Cancels the session a forgotten begin opened, once it is answered.
    fn orphan_answer(
        &mut self,
        endpoint_id: &ClientEndpointId,
        generation: u64,
        data: &str,
    ) -> Option<Vec<UploadEffect>> {
        if self.orphans.is_empty() {
            return None;
        }
        let value = serde_json::from_str::<serde_json::Value>(data).ok()?;
        let id = value.get("id").and_then(serde_json::Value::as_str)?;
        let index = self.orphans.iter().position(|orphan| {
            &orphan.endpoint_id == endpoint_id
                && orphan.generation == generation
                && orphan.request_id == id
        })?;
        let orphan = self.orphans.remove(index);
        let Ok(ResponseResult::PaneUploadStarted(started)) = crate::upload::reply::result_of(value)
        else {
            return Some(Vec::new());
        };
        let request = Request {
            id: format!("upload:{}:cancel", orphan.job),
            method: Method::PaneUploadCancel(PaneUploadCancelParams {
                upload_id: started.upload_id,
            }),
        };
        let data = serde_json::to_string(&request).ok()?;
        Some(vec![UploadEffect::Send {
            endpoint_id: orphan.endpoint_id,
            generation: orphan.generation,
            data,
        }])
    }

    /// Ends the job and stops its worker, adding `effects` after the stop.
    /// A tree drop's failures also go to its tab, where toasts are hidden.
    fn finish_job(&mut self, mut effects: Vec<UploadEffect>) -> Vec<UploadEffect> {
        if let Some(job) = self.job.take() {
            if let Some(target) = job.workbench() {
                let tab_notices: Vec<_> = effects
                    .iter()
                    .filter_map(|effect| match effect {
                        UploadEffect::Notice {
                            failed: true,
                            title,
                            body,
                        } => Some(UploadEffect::TabNotice {
                            tab: target.tab,
                            text: format!("{}: {body}", title.to_lowercase()),
                        }),
                        _ => None,
                    })
                    .collect();
                effects.extend(tab_notices);
            }
            effects.insert(0, UploadEffect::StopWorker { job: job.id });
            if matches!(job.phase, Phase::Confirming | Phase::Asking { .. }) {
                effects.insert(1, UploadEffect::CloseConfirm { job: job.id });
            }
        }
        effects
    }

    /// The existence check answered. A paste that is not a drop ends the job
    /// silently; the caller pastes the text instead.
    pub(crate) fn checked(&mut self, job: JobId, is_drop: bool) -> Vec<UploadEffect> {
        let Some(current) = self.job_mut(job) else {
            return Vec::new();
        };
        if !matches!(current.phase, Phase::Checking) {
            return Vec::new();
        }
        if is_drop {
            current.phase = Phase::Scanning;
            Vec::new()
        } else if current.workbench().is_some() {
            // Nothing to paste into a tree, so say why nothing happened.
            self.finish_job(vec![failed("a dropped path was not found")])
        } else {
            self.finish_job(Vec::new())
        }
    }

    /// The check took too long; treat the paste as text.
    pub(crate) fn abandon(&mut self, job: JobId) -> Vec<UploadEffect> {
        if self.job_mut(job).is_none() {
            return Vec::new();
        }
        self.finish_job(Vec::new())
    }

    pub(crate) fn scanned(
        &mut self,
        job: JobId,
        result: Result<ScanSummary, ScanError>,
        limits: &UploadLimits,
        now: Instant,
    ) -> Vec<UploadEffect> {
        let Some(current) = self.job_mut(job) else {
            return Vec::new();
        };
        if !matches!(current.phase, Phase::Scanning) {
            return Vec::new();
        }
        let summary = match result {
            Ok(summary) => summary,
            Err(ScanError::TooManyEntries(max)) => {
                return self.finish_job(vec![failed(format!(
                    "more than {max} items; raise ui.upload_max_entries to send more"
                ))]);
            }
            Err(ScanError::Other(message)) => return self.finish_job(vec![failed(message)]),
        };
        let assessment = assess(summary.entries, summary.bytes, limits);
        current.summary = summary;
        match assessment {
            Assessment::Refuse(reason) => self.finish_job(vec![failed(reason)]),
            // The tree asks after begin anyway, with the counts in the question.
            Assessment::Confirm if current.workbench().is_some() => self.begin(job, now),
            Assessment::Confirm if !current.request.can_confirm => self.finish_job(vec![failed(
                "too large for a direct attach; use `herdr pane upload`",
            )]),
            Assessment::Confirm => {
                current.phase = Phase::Confirming;
                let summary = &current.summary;
                let mut detail = format!(
                    "{} · {} into the pane's working directory",
                    plural(summary.entries, "item"),
                    format_bytes(summary.bytes)
                );
                if summary.skipped_special > 0 {
                    detail.push_str(&format!(
                        " · skipping {}",
                        plural(summary.skipped_special, "special file")
                    ));
                }
                vec![UploadEffect::Confirm {
                    job,
                    title: "Upload dropped files?".into(),
                    detail,
                }]
            }
            Assessment::Proceed => self.begin(job, now),
        }
    }

    pub(crate) fn confirmed(
        &mut self,
        job: JobId,
        accept: bool,
        now: Instant,
    ) -> Vec<UploadEffect> {
        let Some(current) = self.job_mut(job) else {
            return Vec::new();
        };
        if !matches!(current.phase, Phase::Confirming) {
            return Vec::new();
        }
        if !accept {
            // Why not finish_job: the dialog that answered is already closed.
            self.job = None;
            return vec![UploadEffect::StopWorker { job }];
        }
        self.begin(job, now)
    }

    fn begin(&mut self, job: JobId, now: Instant) -> Vec<UploadEffect> {
        let Some(current) = self.job_mut(job) else {
            return Vec::new();
        };
        let (pane_id, terminal_id, destination) = match &current.request.target {
            UploadTarget::Pane(pane_id) => (Some(pane_id.clone()), None, None),
            UploadTarget::Popup(terminal_id) | UploadTarget::Direct { terminal_id, .. } => {
                (None, Some(terminal_id.clone()), None)
            }
            UploadTarget::Workbench(target) => (
                target.pane_id.clone(),
                None,
                Some(target.destination.clone()),
            ),
        };
        let request_id = current.request_id("begin");
        let request = Request {
            id: request_id.clone(),
            method: Method::PaneUploadBegin(PaneUploadBeginParams {
                pane_id,
                terminal_id,
                destination,
                names: current.summary.names.clone(),
                entries: current.summary.entries,
                bytes: current.summary.bytes,
            }),
        };
        let Some(send) = current.send(&request) else {
            return self.finish_job(vec![failed("the upload request could not be encoded")]);
        };
        if matches!(&send, UploadEffect::Send { data, .. }
            if data.len() > crate::server::client_commands::MAX_ENDPOINT_COMMAND_BYTES)
        {
            return self.finish_job(vec![failed("too many or too long names in one drop")]);
        }
        current.phase = Phase::Beginning {
            request_id,
            deadline: now + REQUEST_TIMEOUT,
        };
        vec![send]
    }

    /// The worker prepared a write; it never prepares more than the window.
    pub(crate) fn outgoing(
        &mut self,
        job: JobId,
        write: WriteOut,
        now: Instant,
    ) -> Vec<UploadEffect> {
        let Some(current) = self.job_mut(job) else {
            return Vec::new();
        };
        let Phase::Sending { in_flight, .. } = &mut current.phase else {
            return Vec::new();
        };
        in_flight.push_back(InFlight {
            request_id: write.request_id,
            bytes: write.bytes,
            completes_entry: write.completes_entry,
            deadline: now + REQUEST_TIMEOUT,
        });
        vec![UploadEffect::Send {
            endpoint_id: current.request.endpoint_id.clone(),
            generation: current.request.generation,
            data: write.data,
        }]
    }

    pub(crate) fn writes_done(&mut self, job: JobId, now: Instant) -> Vec<UploadEffect> {
        let Some(current) = self.job_mut(job) else {
            return Vec::new();
        };
        let Phase::Sending { writes_done, .. } = &mut current.phase else {
            return Vec::new();
        };
        *writes_done = true;
        self.finish_when_acked(job, now)
    }

    /// Finish only after every write is answered (design §3.4), so the
    /// server's job queue cannot overflow behind them.
    fn finish_when_acked(&mut self, job: JobId, now: Instant) -> Vec<UploadEffect> {
        let Some(current) = self.job_mut(job) else {
            return Vec::new();
        };
        let Phase::Sending {
            in_flight,
            writes_done: true,
        } = &current.phase
        else {
            return Vec::new();
        };
        if !in_flight.is_empty() {
            return Vec::new();
        }
        let Some(upload_id) = current.upload_id.clone() else {
            return Vec::new();
        };
        let request_id = current.request_id("finish");
        let request = Request {
            id: request_id.clone(),
            method: Method::PaneUploadFinish(PaneUploadFinishParams {
                upload_id,
                collision: current.collision,
            }),
        };
        let Some(send) = current.send(&request) else {
            return Vec::new();
        };
        let entries = current.summary.entries;
        current.phase = Phase::Finishing {
            request_id,
            deadline: now + REQUEST_TIMEOUT + Duration::from_secs(entries / 1000),
        };
        vec![send]
    }

    /// The tree dialog answered: `None` cancels the begun session; a policy
    /// starts the writes, or retries finish after a late conflict.
    pub(crate) fn chosen(
        &mut self,
        job: JobId,
        collision: Option<PaneUploadCollision>,
        now: Instant,
    ) -> Vec<UploadEffect> {
        let Some(current) = self.job_mut(job) else {
            return Vec::new();
        };
        let Phase::Asking { writes_done } = current.phase else {
            return Vec::new();
        };
        let Some(collision) = collision else {
            let cancel = current.cancel_request();
            return self.finish_job(
                cancel
                    .into_iter()
                    .chain([UploadEffect::Notice {
                        failed: true,
                        title: "Upload cancelled".into(),
                        body: "Nothing was published".into(),
                    }])
                    .collect(),
            );
        };
        current.collision = collision;
        current.phase = Phase::Sending {
            in_flight: VecDeque::new(),
            writes_done,
        };
        if writes_done {
            return self.finish_when_acked(job, now);
        }
        let (Some(upload_id), Some((chunk_bytes, window))) =
            (current.upload_id.clone(), current.writer)
        else {
            return Vec::new();
        };
        vec![UploadEffect::StartWriter {
            job,
            upload_id,
            chunk_bytes,
            window,
        }]
    }

    pub(crate) fn worker_failed(&mut self, job: JobId, message: String) -> Vec<UploadEffect> {
        let Some(current) = self.job_mut(job) else {
            return Vec::new();
        };
        let cancel = current.cancel_request();
        self.finish_job(cancel.into_iter().chain([failed(message)]).collect())
    }

    /// An answer on an upload control from `endpoint_id`.
    pub(crate) fn response(
        &mut self,
        endpoint_id: &ClientEndpointId,
        generation: u64,
        data: &str,
        now: Instant,
    ) -> Vec<UploadEffect> {
        if let Some(effects) = self.orphan_answer(endpoint_id, generation, data) {
            return effects;
        }
        let Some(current) = self.job.as_mut() else {
            return Vec::new();
        };
        if &current.request.endpoint_id != endpoint_id || current.request.generation != generation {
            return Vec::new();
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
            return Vec::new();
        };
        let Some(id) = value.get("id").and_then(serde_json::Value::as_str) else {
            return Vec::new();
        };
        let id = id.to_owned();
        let result = crate::upload::reply::result_of(value);
        let job = current.id;
        let answered = match &current.phase {
            Phase::Beginning { request_id, .. } if *request_id == id => Answered::Begin,
            Phase::Sending { in_flight, .. } => {
                match in_flight.iter().position(|write| write.request_id == id) {
                    Some(index) => Answered::Write(index),
                    None => return Vec::new(),
                }
            }
            Phase::Finishing { request_id, .. } if *request_id == id => Answered::Finish,
            _ => return Vec::new(),
        };
        match (answered, result) {
            (Answered::Begin, Ok(ResponseResult::PaneUploadStarted(started))) => {
                // UPLOAD_CHUNK_BYTES is nonzero, so the bounds are ordered.
                let cap = current
                    .request
                    .chunk_cap
                    .clamp(1, crate::upload::UPLOAD_CHUNK_BYTES);
                let chunk_bytes = usize::try_from(started.chunk_bytes)
                    .unwrap_or(usize::MAX)
                    .clamp(1, cap);
                let window = started.window.clamp(1, crate::upload::UPLOAD_WINDOW);
                current.upload_id = Some(started.upload_id.clone());
                // Why only the tree: its destination is a path in the tab's
                // environment, but the server resolves the pane again, and a
                // pane whose ssh/docker session just ended is a host shell,
                // so the same path would be published on the server's own
                // disk. Why not terminal drops: they intentionally go
                // wherever the pane is now.
                let moved = current
                    .workbench()
                    .filter(|target| target.location != started.location)
                    .map(WorkbenchTarget::location_label);
                if let Some(label) = moved {
                    let mut effects: Vec<UploadEffect> =
                        current.cancel_request().into_iter().collect();
                    effects.push(failed(format!("the pane is no longer in {label}")));
                    return self.finish_job(effects);
                }
                if current.workbench().is_some() {
                    current.writer = Some((chunk_bytes, window));
                    current.conflicts = started.conflicts;
                    current.phase = Phase::Asking { writes_done: false };
                    return current.ask(None).into_iter().collect();
                }
                current.phase = Phase::Sending {
                    in_flight: VecDeque::new(),
                    writes_done: false,
                };
                vec![UploadEffect::StartWriter {
                    job,
                    upload_id: started.upload_id,
                    chunk_bytes,
                    window,
                }]
            }
            (Answered::Begin, Ok(other)) => self.finish_job(vec![failed(format!(
                "unexpected answer to begin: {other:?}"
            ))]),
            // No session exists after a refused begin, so nothing to cancel.
            (Answered::Begin, Err((_, message))) => self.finish_job(vec![failed(message)]),
            (Answered::Write(index), Ok(_)) => {
                if let Phase::Sending { in_flight, .. } = &mut current.phase {
                    if let Some(write) = in_flight.remove(index) {
                        current.acked_bytes += write.bytes;
                        current.acked_entries += u64::from(write.completes_entry);
                    }
                }
                let mut effects = vec![UploadEffect::Credit { job }];
                effects.extend(self.finish_when_acked(job, now));
                effects
            }
            (Answered::Finish, Ok(ResponseResult::PaneUploadFinished(finished)))
                if current.workbench().is_some() =>
            {
                let Some(target) = current.workbench().cloned() else {
                    return Vec::new();
                };
                let names: Vec<String> = finished
                    .paths
                    .iter()
                    .map(|path| published_name(path, &finished.destination))
                    .collect();
                let text = format!(
                    "uploaded {} to {}",
                    plural(names.len() as u64, "item"),
                    target.destination
                );
                self.finish_job(vec![
                    UploadEffect::WorkbenchDone {
                        tab: target.tab,
                        destination: target.destination.clone(),
                        names,
                    },
                    UploadEffect::Notice {
                        failed: false,
                        title: "Uploaded".into(),
                        body: text.clone(),
                    },
                    UploadEffect::TabNotice {
                        tab: target.tab,
                        text,
                    },
                ])
            }
            // A name appeared after begin: the stage is kept, so ask again
            // instead of renaming silently (spec Q4: the tree always asks).
            (Answered::Finish, Err((code, message)))
                if code == "upload_conflict" && current.workbench().is_some() =>
            {
                current.phase = Phase::Asking { writes_done: true };
                current.conflicts.clear();
                current.ask(Some(message)).into_iter().collect()
            }
            (Answered::Finish, Ok(ResponseResult::PaneUploadFinished(finished))) => {
                // Why refuse rather than quote: `ESC [201~` ends a bracketed
                // paste and a newline submits an agent's prompt, whatever
                // the quoting. The server refuses such destinations too.
                let pasteable = !finished
                    .paths
                    .iter()
                    .any(|path| path.chars().any(char::is_control));
                let text =
                    super::paste::pasted_paths(&finished.paths, current.request.trailing_space);
                let mut body = format!(
                    "{} to {}",
                    plural(finished.paths.len() as u64, "item"),
                    finished.destination
                );
                let skipped = current.summary.skipped_special;
                if skipped > 0 {
                    body.push_str(&format!(" · skipped {}", plural(skipped, "special file")));
                }
                let warnings = (current.summary.warnings.len() + finished.warnings.len()) as u64;
                if warnings > 0 {
                    body.push_str(&format!(" · {}", plural(warnings, "warning")));
                }
                let mut effects = Vec::new();
                if pasteable {
                    effects.push(UploadEffect::Paste {
                        endpoint_id: current.request.endpoint_id.clone(),
                        generation: current.request.generation,
                        target: current.request.target.clone(),
                        text,
                    });
                } else {
                    body.push_str(" · not pasted: a path contains control characters");
                }
                effects.push(UploadEffect::Notice {
                    failed: false,
                    title: "Uploaded".into(),
                    body,
                });
                self.finish_job(effects)
            }
            (Answered::Finish, Ok(other)) => {
                let cancel = current.cancel_request();
                self.finish_job(
                    cancel
                        .into_iter()
                        .chain([failed(format!("unexpected answer to finish: {other:?}"))])
                        .collect(),
                )
            }
            // A refused write or publish keeps the server session; terminal
            // drops do not retry, so release its stage.
            (Answered::Write(_) | Answered::Finish, Err((_, message))) => {
                let cancel = current.cancel_request();
                self.finish_job(cancel.into_iter().chain([failed(message)]).collect())
            }
        }
    }

    /// The user asked to stop. Publishing cannot be interrupted (design §2.3).
    pub(crate) fn cancel(&mut self, job: JobId) -> Vec<UploadEffect> {
        let Some(current) = self.job_mut(job) else {
            return Vec::new();
        };
        if matches!(current.phase, Phase::Finishing { .. }) {
            return Vec::new();
        }
        let cancel = current.cancel_request();
        self.orphan_begin();
        self.finish_job(
            cancel
                .into_iter()
                .chain([UploadEffect::Notice {
                    failed: true,
                    title: "Upload cancelled".into(),
                    body: "Nothing was published".into(),
                }])
                .collect(),
        )
    }

    /// The job's connection is gone; the server cleans up on disconnect.
    pub(crate) fn endpoint_lost(
        &mut self,
        endpoint_id: &ClientEndpointId,
        generation: u64,
    ) -> Vec<UploadEffect> {
        let lost = self.job.as_ref().is_some_and(|job| {
            &job.request.endpoint_id == endpoint_id && job.request.generation == generation
        });
        // The server drops a lost connection's sessions itself.
        self.orphans
            .retain(|orphan| &orphan.endpoint_id != endpoint_id || orphan.generation != generation);
        if !lost {
            return Vec::new();
        }
        self.finish_job(vec![failed("the connection to the server was lost")])
    }

    pub(crate) fn tick(&mut self, now: Instant) -> Vec<UploadEffect> {
        let Some(current) = self.job.as_ref() else {
            return Vec::new();
        };
        let expired = match &current.phase {
            Phase::Beginning { deadline, .. } | Phase::Finishing { deadline, .. } => {
                *deadline <= now
            }
            Phase::Sending { in_flight, .. } => in_flight.iter().any(|write| write.deadline <= now),
            Phase::Checking | Phase::Scanning | Phase::Confirming | Phase::Asking { .. } => false,
        };
        if !expired {
            return Vec::new();
        }
        let cancel = current.cancel_request();
        self.orphan_begin();
        self.finish_job(
            cancel
                .into_iter()
                .chain([failed("the server did not answer in time")])
                .collect(),
        )
    }

    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        match &self.job.as_ref()?.phase {
            Phase::Beginning { deadline, .. } | Phase::Finishing { deadline, .. } => {
                Some(*deadline)
            }
            Phase::Sending { in_flight, .. } => in_flight.iter().map(|write| write.deadline).min(),
            Phase::Checking | Phase::Scanning | Phase::Confirming | Phase::Asking { .. } => None,
        }
    }

    pub(crate) fn progress(&self) -> Option<UploadProgressView> {
        let job = self.job.as_ref()?;
        let summary = &job.summary;
        let (title, cancellable) = match job.phase {
            Phase::Checking | Phase::Confirming | Phase::Asking { .. } => return None,
            Phase::Scanning => ("Preparing upload", true),
            Phase::Beginning { .. } | Phase::Sending { .. } => ("Uploading", true),
            Phase::Finishing { .. } => ("Publishing…", false),
        };
        let body = if matches!(job.phase, Phase::Scanning) {
            "click to cancel".to_owned()
        } else {
            let counts = format!(
                "{}/{} · {:.1}/{}",
                job.acked_entries,
                summary.entries,
                job.acked_bytes as f64 / 1_000_000.0,
                format_bytes(summary.bytes)
            );
            if cancellable {
                format!("{counts} — click to cancel")
            } else {
                counts
            }
        };
        Some(UploadProgressView {
            job: job.id,
            title: title.into(),
            body,
            cancellable,
        })
    }
}

/// The last component of a published path. Why strip the destination
/// first: a Windows server joins with `\`, which is an ordinary character in
/// a POSIX name.
fn published_name(path: &str, destination: &str) -> String {
    path.strip_prefix(destination)
        .map(|rest| rest.trim_start_matches(['/', '\\']))
        .filter(|rest| !rest.is_empty())
        .or_else(|| path.rsplit('/').next())
        .unwrap_or(path)
        .to_owned()
}

#[cfg(test)]
mod tests;
