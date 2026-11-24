//! Pane recording: `herdr --log <dir>` writes what each pane's process prints,
//! and with `--log-input` what herdr sends it, to gzip-compressed asciicast
//! files under `<dir>/<session>/`.
//!
//! Not to be confused with `herdr.log` / `herdr-server.log`, the diagnostic
//! log in [`crate::logging`].

mod cast;
pub(crate) mod launch;
mod sink;
mod writer;

use std::path::PathBuf;
use std::sync::OnceLock;

pub(crate) use writer::PaneRecorder;
pub(crate) use writer::Recorder;
use writer::RecorderConfig;

/// Bytes waiting for the disk across all panes before new bytes are dropped.
const QUEUE_LIMIT_BYTES: usize = 32 * 1024 * 1024;
/// The part of that one pane may hold, so 16 flooding panes still leave room.
const PANE_QUEUE_LIMIT_BYTES: usize = 2 * 1024 * 1024;

/// A session's recording choice, as the server remembers it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct RecordingSettings {
    /// The folder `--log` named; each session records into a folder below it.
    pub dir: PathBuf,
    /// Whether bytes herdr sends to panes are recorded too.
    pub input: bool,
}

/// One pane's recording as the server sees it now.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PaneRecordingStatus {
    /// The file being written; `None` until it is created or when it could not be.
    pub path: Option<PathBuf>,
    pub input: bool,
    /// Bytes left out because the disk could not keep up.
    pub dropped_bytes: u64,
    /// Why recording stopped or never started for this pane.
    pub error: Option<String>,
}

static RECORDER: OnceLock<Recorder> = OnceLock::new();

/// Settles the session's choice at server start and starts recording when it
/// is on. Call once, before the first pane opens.
pub(crate) fn start_server(request: Option<launch::RecordingRequest>) -> std::io::Result<()> {
    // Why fail the start instead of recording anyway: the remembered choice is
    // what later launches compare against and what a restart resumes, so a
    // server that cannot save it would record differently from what it reports.
    let settings = launch::establish(request, &crate::session::data_dir()).map_err(|err| {
        tracing::error!(%err, "could not remember the session's pane recording choice");
        std::io::Error::new(
            err.kind(),
            format!("cannot remember the session's pane recording choice: {err}"),
        )
    })?;
    let Some(settings) = settings else {
        return Ok(());
    };
    let session = session_folder_name();
    let config = RecorderConfig {
        dir: settings.dir.join(&session),
        session,
        input: settings.input,
        part_limit_bytes: sink::DEFAULT_PART_LIMIT_BYTES,
        queue_limit_bytes: QUEUE_LIMIT_BYTES,
        pane_queue_limit_bytes: PANE_QUEUE_LIMIT_BYTES,
    };
    let dir = config.dir.clone();
    let recorder = Recorder::spawn(config).map_err(|err| {
        tracing::error!(%err, "could not start pane recording");
        std::io::Error::new(err.kind(), format!("cannot start pane recording: {err}"))
    })?;
    tracing::info!(dir = %dir.display(), input = settings.input, "recording panes");
    let _ = RECORDER.set(recorder);
    Ok(())
}

/// The folder below `--log` this session records into.
pub(crate) fn session_folder_name() -> String {
    crate::session::active_name().unwrap_or_else(|| crate::session::DEFAULT_SESSION_NAME.to_owned())
}

/// A recorder for a pane runtime that is opening, or `None` when the session
/// does not record.
pub(crate) fn open_pane(
    pane_id: u32,
    cols: u16,
    rows: u16,
    on_failure: impl Fn() + Send + Sync + 'static,
) -> Option<PaneRecorder> {
    RECORDER.get().map(|recorder| {
        recorder.pane_with_failure_notice(pane_id, cols, rows, Some(Box::new(on_failure)))
    })
}

#[cfg(test)]
pub(crate) fn test_recorder(session_dir: PathBuf, input: bool) -> Recorder {
    Recorder::spawn(RecorderConfig {
        dir: session_dir,
        session: "test".into(),
        input,
        part_limit_bytes: sink::DEFAULT_PART_LIMIT_BYTES,
        queue_limit_bytes: QUEUE_LIMIT_BYTES,
        pane_queue_limit_bytes: PANE_QUEUE_LIMIT_BYTES,
    })
    .expect("recording thread starts")
}

/// Finishes every open recording file. Call when the server stops.
pub(crate) fn shutdown() {
    if let Some(recorder) = RECORDER.get() {
        recorder.shutdown();
    }
}
