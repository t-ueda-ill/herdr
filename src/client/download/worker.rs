//! The thread that touches this machine's files for one download: it prepares
//! the folder, creates the stage, writes each read answer's records, and
//! publishes at the end (design §4.4). Why a thread: base64 decoding and disk
//! writes of up to 1.3 MB per answer would stall keys and drawing on the
//! client loop.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

use crate::api::schema::{PaneDownloadChunk, PaneUploadCollision, ResponseResult};
use crate::client::upload::transfer::JobId;
use crate::download::local::LocalWriter;
use crate::upload::sink::Committed;

use super::destination::{resolve, PathSyntax};

/// Where the download goes, as the dialog left it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Destination {
    /// The text of the dialog.
    pub(crate) input: String,
    /// The dialog's initial text. Only this folder is created when missing.
    pub(crate) default_dir: String,
    /// The user's home folder, if it is known and absolute.
    pub(crate) home: Option<PathBuf>,
    pub(crate) syntax: PathSyntax,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorkerCommand {
    /// Creates the stage for the names the server will send.
    Open { names: Vec<String> },
    /// Writes one read answer, as the server's JSON.
    Apply { seq: u64, data: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorkerEvent {
    Prepared {
        job: JobId,
        result: Result<(), String>,
    },
    Opened {
        job: JobId,
    },
    Applied {
        job: JobId,
        seq: u64,
        entries: u64,
        bytes: u64,
    },
    Published {
        job: JobId,
        destination: String,
        paths: Vec<String>,
        warnings: Vec<String>,
    },
    Failed {
        job: JobId,
        message: String,
    },
}

/// Dropping the handle stops the worker at its next wait and removes the stage.
pub(crate) struct WorkerHandle {
    pub(crate) job: JobId,
    commands: Sender<WorkerCommand>,
}

impl WorkerHandle {
    pub(crate) fn send(&self, command: WorkerCommand) {
        let _ = self.commands.send(command);
    }
}

pub(crate) fn spawn(
    job: JobId,
    destination: Destination,
    emit: impl Fn(WorkerEvent) -> bool + Send + 'static,
) -> std::io::Result<WorkerHandle> {
    let (commands, receiver) = std::sync::mpsc::channel();
    crate::thread_spawn::spawn_named("herdr-download-write", move || {
        run(job, &destination, &emit, &receiver);
    })?;
    Ok(WorkerHandle { job, commands })
}

/// The folder the download goes into, created if it is the default one.
fn prepare(destination: &Destination) -> Result<PathBuf, String> {
    let wants_home = destination.input.trim_start().starts_with('~');
    let home = match &destination.home {
        Some(home) if home.is_absolute() => home.clone(),
        _ if wants_home => return Err("cannot find your home folder; enter a full path".into()),
        _ => PathBuf::new(),
    };
    let folder = resolve(&destination.input, &home, destination.syntax)?;
    let is_default = !destination.default_dir.trim().is_empty()
        && resolve(&destination.default_dir, &home, destination.syntax).as_ref() == Ok(&folder);
    match std::fs::metadata(&folder) {
        Ok(metadata) if metadata.is_dir() => Ok(folder),
        Ok(_) => Err(format!("not a folder: {}", folder.display())),
        // Why only the default: a mistyped path should not leave a new folder.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && is_default => {
            std::fs::create_dir(&folder)
                .map_err(|error| format!("cannot create {}: {error}", folder.display()))?;
            Ok(folder)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(format!("folder not found: {}", folder.display()))
        }
        Err(error) => Err(format!("cannot use {}: {error}", folder.display())),
    }
}

fn chunk_of(seq: u64, data: &str) -> Result<PaneDownloadChunk, String> {
    let value = serde_json::from_str::<serde_json::Value>(data)
        .map_err(|error| format!("the server's answer could not be read: {error}"))?;
    match crate::upload::reply::result_of(value) {
        Ok(ResponseResult::PaneDownloadChunk(chunk)) if chunk.seq == seq => Ok(chunk),
        Ok(ResponseResult::PaneDownloadChunk(chunk)) => Err(format!(
            "the server answered read {} where {seq} was expected",
            chunk.seq
        )),
        Ok(other) => Err(format!("unexpected answer to read: {other:?}")),
        Err((_, message)) => Err(message),
    }
}

fn run(
    job: JobId,
    destination: &Destination,
    emit: &dyn Fn(WorkerEvent) -> bool,
    commands: &Receiver<WorkerCommand>,
) {
    let folder = match prepare(destination) {
        Ok(folder) => folder,
        Err(message) => {
            emit(WorkerEvent::Prepared {
                job,
                result: Err(message),
            });
            return;
        }
    };
    if !emit(WorkerEvent::Prepared {
        job,
        result: Ok(()),
    }) {
        return;
    }
    let mut writer: Option<LocalWriter> = None;
    // Why a closure: every way out must remove the stage, and the failures are
    // many.
    let fail = |writer: Option<LocalWriter>, message: String| {
        if let Some(writer) = writer {
            writer.abort();
        }
        emit(WorkerEvent::Failed { job, message });
    };
    while let Ok(command) = commands.recv() {
        match command {
            WorkerCommand::Open { names } => {
                match LocalWriter::open(folder.clone(), &names) {
                    Ok((opened, _kept)) => writer = Some(opened),
                    Err((_, message)) => return fail(None, message),
                }
                if !emit(WorkerEvent::Opened { job }) {
                    break;
                }
            }
            WorkerCommand::Apply { seq, data } => {
                let Some(mut open) = writer.take() else {
                    return fail(None, "the download has no stage".into());
                };
                let chunk = match chunk_of(seq, &data) {
                    Ok(chunk) => chunk,
                    Err(message) => return fail(Some(open), message),
                };
                for record in &chunk.records {
                    if let Err((_, message)) = open.apply(record) {
                        return fail(Some(open), message);
                    }
                }
                if !emit(WorkerEvent::Applied {
                    job,
                    seq,
                    entries: chunk.sent_entries,
                    bytes: chunk.sent_bytes,
                }) {
                    open.abort();
                    return;
                }
                if !chunk.done {
                    writer = Some(open);
                    continue;
                }
                match open.publish(PaneUploadCollision::Rename) {
                    Ok(Committed::Published { paths, warnings }) => {
                        emit(WorkerEvent::Published {
                            job,
                            destination: crate::platform::file_path_display(
                                &std::fs::canonicalize(&folder).unwrap_or(folder),
                            ),
                            paths: paths.into_iter().map(|(_, path)| path).collect(),
                            warnings: chunk.warnings.into_iter().chain(warnings).collect(),
                        });
                    }
                    // Rename never meets a conflict; the sink removed the stage.
                    Ok(Committed::Conflict(_)) => {
                        emit(WorkerEvent::Failed {
                            job,
                            message: "a name appeared while publishing".into(),
                        });
                    }
                    Err((_, message)) => {
                        emit(WorkerEvent::Failed { job, message });
                    }
                }
                return;
            }
        }
    }
    // The handle was dropped: the download ended elsewhere.
    if let Some(writer) = writer {
        writer.abort();
    }
}

#[cfg(all(test, unix))]
mod tests;
