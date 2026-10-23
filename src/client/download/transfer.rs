//! One download from the dialog to the published folder, as a pure state
//! machine (design §4.4). It never touches files or sockets: each call returns
//! the effects the client loop carries out, so every ordering rule is testable.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::api::schema::{
    Method, PaneDownloadBeginParams, PaneDownloadCancelParams, PaneDownloadReadParams,
    PaneLocation, Request, ResponseResult,
};
use crate::client::endpoint::ClientEndpointId;
use crate::client::upload::limits::format_bytes;
use crate::client::upload::transfer::{JobId, UploadProgressView};
use crate::download::notice_text;

/// Begin walks the source before it answers (design §4.4).
const BEGIN_TIMEOUT: Duration = Duration::from_secs(180);

/// The server answers a slow source with an empty read every 25 seconds.
const READ_TIMEOUT: Duration = Duration::from_secs(60);

/// More than this many unanswered begins means the server is not answering;
/// it then ends the sessions on its own deadlines.
const MAX_ORPHANED_BEGINS: usize = 8;

/// What the dialog asked for, with where the bytes come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DownloadRequest {
    pub(crate) endpoint_id: ClientEndpointId,
    pub(crate) generation: u64,
    /// The investigation tab that started it, which shows the result too.
    pub(crate) tab: u64,
    /// A live pane of an ssh/container location; `None` for the server's host.
    pub(crate) pane_id: Option<String>,
    /// The environment the tab browses, compared with where the server finds
    /// the pane when the download begins.
    pub(crate) location: PaneLocation,
    pub(crate) paths: Vec<String>,
    /// The largest decoded data one read answer may carry.
    pub(crate) max_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DownloadEffect {
    /// An `endpoint.download.request.v1` control with this JSON request.
    Send {
        endpoint_id: ClientEndpointId,
        generation: u64,
        data: String,
    },
    /// The worker creates the stage for these names.
    StartWriter {
        job: JobId,
        names: Vec<String>,
    },
    /// The worker decodes this read answer and writes its records.
    Apply {
        job: JobId,
        seq: u64,
        data: String,
    },
    StopWorker {
        job: JobId,
    },
    Notice {
        failed: bool,
        title: String,
        body: String,
    },
    /// The tab's notice line. Why besides `Notice`: the workbench hides toasts.
    TabNotice {
        tab: u64,
        text: String,
    },
}

#[derive(Debug)]
struct InFlight {
    seq: u64,
    deadline: Instant,
}

#[derive(Debug)]
enum Phase {
    /// The worker is resolving the destination folder.
    Preparing,
    Beginning {
        request_id: String,
        deadline: Instant,
    },
    /// Begin was answered; the worker is creating the stage.
    Opening,
    Reading {
        in_flight: VecDeque<InFlight>,
        next_seq: u64,
    },
    /// The server said `done`; the worker is publishing.
    Saving,
}

#[derive(Debug)]
struct Job {
    id: JobId,
    request: DownloadRequest,
    phase: Phase,
    download_id: Option<String>,
    window: u32,
    estimate: (u64, u64),
    sent_entries: u64,
    sent_bytes: u64,
    /// The server dropped the session: a cancel would only be refused.
    server_ended: bool,
}

impl Job {
    fn request_id(&self, suffix: &str) -> String {
        format!("download:{}:{suffix}", self.id)
    }

    fn send(&self, request: &Request) -> Option<DownloadEffect> {
        let data = serde_json::to_string(request).ok()?;
        Some(DownloadEffect::Send {
            endpoint_id: self.request.endpoint_id.clone(),
            generation: self.request.generation,
            data,
        })
    }

    /// Best effort: the server also ends the session on its own deadlines, on
    /// a disconnect, and when the pane exits.
    fn cancel_request(&self) -> Option<DownloadEffect> {
        if self.server_ended {
            return None;
        }
        let download_id = self.download_id.clone()?;
        self.send(&Request {
            id: self.request_id("cancel"),
            method: Method::PaneDownloadCancel(PaneDownloadCancelParams { download_id }),
        })
    }

    /// Asks for the next part of the stream.
    fn next_read(&mut self, now: Instant) -> Option<DownloadEffect> {
        let download_id = self.download_id.clone()?;
        let Phase::Reading { next_seq, .. } = &self.phase else {
            return None;
        };
        let seq = *next_seq;
        let effect = self.send(&Request {
            id: format!("download:{}:r{seq}", self.id),
            method: Method::PaneDownloadRead(PaneDownloadReadParams { download_id, seq }),
        })?;
        if let Phase::Reading {
            in_flight,
            next_seq,
        } = &mut self.phase
        {
            *next_seq += 1;
            in_flight.push_back(InFlight {
                seq,
                deadline: now + READ_TIMEOUT,
            });
        }
        Some(effect)
    }
}

/// A begin still in flight when its job ended (cancel or timeout). Why keep
/// it: the server may still open the session, which would hold a source
/// process and one of the client's session slots until its deadlines.
#[derive(Debug)]
struct OrphanedBegin {
    job: JobId,
    endpoint_id: ClientEndpointId,
    generation: u64,
    request_id: String,
}

/// The parts of an answer the loop reads itself. Why not the whole answer: a
/// read answer carries up to about 1.3 MB of base64, which the worker decodes.
#[derive(Deserialize)]
struct Head {
    id: String,
    #[serde(default)]
    error: Option<ErrorHead>,
    #[serde(default)]
    result: Option<ResultHead>,
}

#[derive(Deserialize)]
struct ErrorHead {
    #[serde(default)]
    code: String,
    #[serde(default)]
    message: String,
}

#[derive(Deserialize)]
struct ResultHead {
    #[serde(default)]
    done: bool,
}

#[derive(Debug, Default)]
pub(crate) struct Downloads {
    next_job: JobId,
    job: Option<Job>,
    orphans: Vec<OrphanedBegin>,
}

fn failed(message: &str) -> DownloadEffect {
    let message = notice_text(message);
    DownloadEffect::Notice {
        failed: true,
        title: "Download failed".into(),
        body: format!("{}. Nothing was saved.", message.trim_end_matches('.')),
    }
}

fn plural(count: u64, one: &str) -> String {
    if count == 1 {
        format!("1 {one}")
    } else {
        format!("{count} {one}s")
    }
}

/// The job and read number of a request id `download:<job>:r<seq>`.
fn read_id(id: &str) -> Option<(JobId, u64)> {
    let rest = id.strip_prefix("download:")?;
    let (job, seq) = rest.split_once(":r")?;
    Some((job.parse().ok()?, seq.parse().ok()?))
}

impl Downloads {
    /// One transfer at a time. Why not queue a second: the progress card and
    /// its click target are single.
    pub(crate) fn busy(&self) -> bool {
        self.job.is_some()
    }

    pub(crate) fn start(&mut self, request: DownloadRequest) -> JobId {
        self.next_job += 1;
        let id = self.next_job;
        self.job = Some(Job {
            id,
            request,
            phase: Phase::Preparing,
            download_id: None,
            window: 1,
            estimate: (0, 0),
            sent_entries: 0,
            sent_bytes: 0,
            server_ended: false,
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
        id: &str,
        data: &str,
    ) -> Option<Vec<DownloadEffect>> {
        let index = self.orphans.iter().position(|orphan| {
            &orphan.endpoint_id == endpoint_id
                && orphan.generation == generation
                && orphan.request_id == id
        })?;
        let orphan = self.orphans.remove(index);
        let value = serde_json::from_str::<serde_json::Value>(data).ok()?;
        let Ok(ResponseResult::PaneDownloadStarted(started)) =
            crate::upload::reply::result_of(value)
        else {
            return Some(Vec::new());
        };
        let request = Request {
            id: format!("download:{}:cancel", orphan.job),
            method: Method::PaneDownloadCancel(PaneDownloadCancelParams {
                download_id: started.download_id,
            }),
        };
        let data = serde_json::to_string(&request).ok()?;
        Some(vec![DownloadEffect::Send {
            endpoint_id: orphan.endpoint_id,
            generation: orphan.generation,
            data,
        }])
    }

    /// Ends the job and stops its worker, adding `effects` after the stop.
    /// A failure also goes to the tab, where toasts are hidden.
    fn finish_job(&mut self, mut effects: Vec<DownloadEffect>) -> Vec<DownloadEffect> {
        if let Some(job) = self.job.take() {
            let tab_notices: Vec<_> = effects
                .iter()
                .filter_map(|effect| match effect {
                    DownloadEffect::Notice {
                        failed: true,
                        title,
                        body,
                    } => Some(DownloadEffect::TabNotice {
                        tab: job.request.tab,
                        text: format!("{}: {body}", title.to_lowercase()),
                    }),
                    _ => None,
                })
                .collect();
            effects.extend(tab_notices);
            effects.insert(0, DownloadEffect::StopWorker { job: job.id });
        }
        effects
    }

    /// Ends the job with a failure, cancelling the server's session first when
    /// it still has one.
    fn fail(&mut self, message: &str) -> Vec<DownloadEffect> {
        let cancel = self.job.as_ref().and_then(Job::cancel_request);
        self.finish_job(cancel.into_iter().chain([failed(message)]).collect())
    }

    /// The worker resolved the destination folder, or said why it cannot be
    /// used. Nothing was asked of the server before this.
    pub(crate) fn prepared(
        &mut self,
        job: JobId,
        result: Result<(), String>,
        now: Instant,
    ) -> Vec<DownloadEffect> {
        let Some(current) = self.job_mut(job) else {
            return Vec::new();
        };
        if !matches!(current.phase, Phase::Preparing) {
            return Vec::new();
        }
        if let Err(message) = result {
            return self.finish_job(vec![failed(&message)]);
        }
        let request_id = current.request_id("begin");
        let request = Request {
            id: request_id.clone(),
            method: Method::PaneDownloadBegin(PaneDownloadBeginParams {
                pane_id: current.request.pane_id.clone(),
                paths: current.request.paths.clone(),
                max_bytes: Some(current.request.max_bytes as u64),
            }),
        };
        let Some(send) = current.send(&request) else {
            return self.finish_job(vec![failed("the download request could not be encoded")]);
        };
        if matches!(&send, DownloadEffect::Send { data, .. }
            if data.len() > crate::server::client_commands::MAX_ENDPOINT_COMMAND_BYTES)
        {
            return self.finish_job(vec![failed("too many or too long paths in one download")]);
        }
        current.phase = Phase::Beginning {
            request_id,
            deadline: now + BEGIN_TIMEOUT,
        };
        vec![send]
    }

    /// The worker created the stage: the stream may start.
    pub(crate) fn opened(&mut self, job: JobId, now: Instant) -> Vec<DownloadEffect> {
        let Some(current) = self.job_mut(job) else {
            return Vec::new();
        };
        if !matches!(current.phase, Phase::Opening) {
            return Vec::new();
        }
        current.phase = Phase::Reading {
            in_flight: VecDeque::new(),
            next_seq: 0,
        };
        (0..current.window)
            .filter_map(|_| current.next_read(now))
            .collect()
    }

    /// The worker wrote one answer. Asking for the next only now keeps the
    /// reads at the server within the window and the writer from falling
    /// behind the network.
    pub(crate) fn applied(
        &mut self,
        job: JobId,
        _seq: u64,
        entries: u64,
        bytes: u64,
        now: Instant,
    ) -> Vec<DownloadEffect> {
        let Some(current) = self.job_mut(job) else {
            return Vec::new();
        };
        current.sent_entries = current.sent_entries.max(entries);
        current.sent_bytes = current.sent_bytes.max(bytes);
        if !matches!(current.phase, Phase::Reading { .. }) {
            return Vec::new();
        }
        current.next_read(now).into_iter().collect()
    }

    /// The worker published everything.
    pub(crate) fn published(
        &mut self,
        job: JobId,
        destination: String,
        paths: Vec<String>,
        warnings: Vec<String>,
    ) -> Vec<DownloadEffect> {
        let Some(current) = self.job_mut(job) else {
            return Vec::new();
        };
        if !matches!(current.phase, Phase::Saving) {
            return Vec::new();
        }
        let tab = current.request.tab;
        let destination = notice_text(&destination);
        let items = plural(paths.len() as u64, "item");
        let mut body = format!("{items} to {destination}");
        if let Some(first) = warnings.first() {
            let first: String = notice_text(first).chars().take(160).collect();
            body.push_str(&format!(
                " · {}: {first}",
                plural(warnings.len() as u64, "warning")
            ));
        }
        self.finish_job(vec![
            DownloadEffect::Notice {
                failed: false,
                title: "Downloaded".into(),
                body,
            },
            DownloadEffect::TabNotice {
                tab,
                text: format!("downloaded {items} to {destination}"),
            },
        ])
    }

    pub(crate) fn worker_failed(&mut self, job: JobId, message: String) -> Vec<DownloadEffect> {
        if self.job_mut(job).is_none() {
            return Vec::new();
        }
        self.fail(&message)
    }

    /// An answer on a download control from `endpoint_id`.
    pub(crate) fn response(
        &mut self,
        endpoint_id: &ClientEndpointId,
        generation: u64,
        data: String,
    ) -> Vec<DownloadEffect> {
        let Ok(head) = serde_json::from_str::<Head>(&data) else {
            return Vec::new();
        };
        if let Some(effects) = self.orphan_answer(endpoint_id, generation, &head.id, &data) {
            return effects;
        }
        let Some(current) = self.job.as_ref() else {
            return Vec::new();
        };
        if &current.request.endpoint_id != endpoint_id || current.request.generation != generation {
            return Vec::new();
        }
        match &current.phase {
            Phase::Beginning { request_id, .. } if *request_id == head.id => {
                self.begin_answered(&data, head)
            }
            Phase::Reading { .. } => self.read_answered(head, data),
            // Anything else is a straggler of a stream that already ended.
            _ => Vec::new(),
        }
    }

    fn begin_answered(&mut self, data: &str, head: Head) -> Vec<DownloadEffect> {
        let Some(current) = self.job.as_mut() else {
            return Vec::new();
        };
        if let Some(error) = head.error {
            // No session exists after a refused begin, so nothing to cancel.
            return self.finish_job(vec![failed(&error.message)]);
        }
        let result = serde_json::from_str::<serde_json::Value>(data)
            .map_err(|error| ("invalid_response".to_owned(), error.to_string()))
            .and_then(crate::upload::reply::result_of);
        let started = match result {
            Ok(ResponseResult::PaneDownloadStarted(started)) => started,
            Ok(other) => {
                return self.finish_job(vec![failed(&format!(
                    "unexpected answer to begin: {other:?}"
                ))]);
            }
            Err((_, message)) => return self.finish_job(vec![failed(&message)]),
        };
        current.download_id = Some(started.download_id.clone());
        if current.request.location != started.location {
            // Why: the server resolves the pane again, and a pane whose
            // ssh/docker session just ended is a host shell, so the same paths
            // would be read from the server's own disk.
            return self.fail("the pane now runs somewhere else; reopen the files view");
        }
        if started.names.is_empty() {
            return self.fail("the server named nothing to download");
        }
        current.window = started.window.clamp(1, crate::download::DOWNLOAD_WINDOW);
        current.estimate = (started.entries, started.bytes);
        current.phase = Phase::Opening;
        vec![DownloadEffect::StartWriter {
            job: current.id,
            names: started.names,
        }]
    }

    fn read_answered(&mut self, head: Head, data: String) -> Vec<DownloadEffect> {
        let Some(current) = self.job.as_mut() else {
            return Vec::new();
        };
        let Some((job, seq)) = read_id(&head.id).filter(|(job, _)| *job == current.id) else {
            return Vec::new();
        };
        let Phase::Reading { in_flight, .. } = &mut current.phase else {
            return Vec::new();
        };
        let Some(index) = in_flight.iter().position(|read| read.seq == seq) else {
            return Vec::new();
        };
        in_flight.remove(index);
        if let Some(error) = head.error {
            // Only these two leave the session alive; every other error ended
            // it, and a cancel would just be answered with `download_not_found`.
            current.server_ended =
                !matches!(error.code.as_str(), "download_busy" | "invalid_params");
            return self.fail(&error.message);
        }
        let Some(result) = head.result else {
            return self.fail("the server's answer could not be read");
        };
        if result.done {
            current.server_ended = true;
            current.phase = Phase::Saving;
        }
        vec![DownloadEffect::Apply { job, seq, data }]
    }

    /// The user asked to stop. Saving cannot be interrupted: the stage is
    /// being renamed into place.
    pub(crate) fn cancel(&mut self, job: JobId) -> Vec<DownloadEffect> {
        let Some(current) = self.job_mut(job) else {
            return Vec::new();
        };
        if matches!(current.phase, Phase::Saving) {
            return Vec::new();
        }
        let cancel = current.cancel_request();
        self.orphan_begin();
        self.finish_job(
            cancel
                .into_iter()
                .chain([DownloadEffect::Notice {
                    failed: true,
                    title: "Download cancelled".into(),
                    body: "Nothing was saved".into(),
                }])
                .collect(),
        )
    }

    /// The job's connection is gone; the server ends its sessions on a
    /// disconnect, so no cancel is sent.
    pub(crate) fn endpoint_lost(
        &mut self,
        endpoint_id: &ClientEndpointId,
        generation: u64,
    ) -> Vec<DownloadEffect> {
        self.orphans
            .retain(|orphan| &orphan.endpoint_id != endpoint_id || orphan.generation != generation);
        // Why not while saving: every byte already arrived, so the worker's
        // publish decides the outcome, and failing here would announce
        // "Nothing was saved" over files the worker is putting in place.
        let lost = self.job.as_ref().is_some_and(|job| {
            &job.request.endpoint_id == endpoint_id
                && job.request.generation == generation
                && !matches!(job.phase, Phase::Saving)
        });
        if !lost {
            return Vec::new();
        }
        self.finish_job(vec![failed("the connection to the server was lost")])
    }

    pub(crate) fn tick(&mut self, now: Instant) -> Vec<DownloadEffect> {
        if self.next_deadline().is_none_or(|deadline| deadline > now) {
            return Vec::new();
        }
        self.orphan_begin();
        self.fail("the server did not answer in time")
    }

    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        match &self.job.as_ref()?.phase {
            Phase::Beginning { deadline, .. } => Some(*deadline),
            Phase::Reading { in_flight, .. } => in_flight.iter().map(|read| read.deadline).min(),
            Phase::Preparing | Phase::Opening | Phase::Saving => None,
        }
    }

    pub(crate) fn progress(&self) -> Option<UploadProgressView> {
        let job = self.job.as_ref()?;
        let (entries, bytes) = job.estimate;
        let counts = if job.sent_entries <= entries && job.sent_bytes <= bytes {
            format!(
                "{}/{} · {:.1}/{}",
                job.sent_entries,
                entries,
                job.sent_bytes as f64 / 1_000_000.0,
                format_bytes(bytes)
            )
        } else {
            // The tree changed since begin counted it, so the total is stale.
            format!(
                "{} · {}",
                plural(job.sent_entries, "item"),
                format_bytes(job.sent_bytes)
            )
        };
        let (title, body, cancellable) = match job.phase {
            Phase::Preparing | Phase::Beginning { .. } | Phase::Opening => {
                ("Preparing download", "click to cancel".to_owned(), true)
            }
            Phase::Reading { .. } => ("Downloading", format!("{counts} — click to cancel"), true),
            Phase::Saving => ("Saving…", counts, false),
        };
        Some(UploadProgressView {
            job: job.id,
            title: title.into(),
            body,
            cancellable,
        })
    }
}

#[cfg(test)]
mod tests;
