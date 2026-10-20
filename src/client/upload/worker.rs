//! The thread that touches local files for one drop: it checks the paths
//! exist, walks them, then reads and encodes one chunk per credit (design
//! §3.4). Why a thread: `/mnt/c` over 9P or a Windows share can take seconds
//! per stat, and the client loop must keep drawing and reading keys.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

use crate::api::schema::{Method, Request};
use crate::upload::source::{scan_limited, DuplicateNames, Scan, ScanError, Writes};

use super::transfer::{JobId, ScanSummary, WriteOut};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorkerCommand {
    Start {
        upload_id: String,
        chunk_bytes: usize,
        window: u32,
    },
    Credit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorkerEvent {
    Checked {
        job: JobId,
        is_drop: bool,
    },
    Scanned {
        job: JobId,
        result: Result<ScanSummary, ScanError>,
    },
    Outgoing {
        job: JobId,
        write: WriteOut,
    },
    WritesDone {
        job: JobId,
    },
    Failed {
        job: JobId,
        message: String,
    },
}

/// Dropping the handle stops the worker at its next wait.
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
    paths: Vec<PathBuf>,
    max_entries: u64,
    duplicates: DuplicateNames,
    emit: impl Fn(WorkerEvent) -> bool + Send + 'static,
) -> std::io::Result<WorkerHandle> {
    let (commands, receiver) = std::sync::mpsc::channel();
    crate::thread_spawn::spawn_named("herdr-upload-read", move || {
        run(job, &paths, max_entries, duplicates, &emit, &receiver);
    })?;
    Ok(WorkerHandle { job, commands })
}

fn summary_of(scan: &Scan) -> ScanSummary {
    ScanSummary {
        names: scan.names.clone(),
        entries: scan.entries.len() as u64,
        bytes: scan.bytes,
        skipped_special: scan.skipped_special,
        warnings: scan.warnings.clone(),
    }
}

/// Decoded length of standard padded base64.
fn decoded_len(data: &str) -> u64 {
    let padding = data.bytes().rev().take_while(|byte| *byte == b'=').count();
    ((data.len() / 4) * 3).saturating_sub(padding) as u64
}

fn run(
    job: JobId,
    paths: &[PathBuf],
    max_entries: u64,
    duplicates: DuplicateNames,
    emit: &dyn Fn(WorkerEvent) -> bool,
    commands: &Receiver<WorkerCommand>,
) {
    let is_drop = paths
        .iter()
        .all(|path| std::fs::symlink_metadata(path).is_ok());
    if !emit(WorkerEvent::Checked { job, is_drop }) || !is_drop {
        return;
    }
    let scan = match scan_limited(paths, duplicates, max_entries) {
        Ok(scan) => scan,
        Err(error) => {
            emit(WorkerEvent::Scanned {
                job,
                result: Err(error),
            });
            return;
        }
    };
    if !emit(WorkerEvent::Scanned {
        job,
        result: Ok(summary_of(&scan)),
    }) {
        return;
    }
    let Ok(WorkerCommand::Start {
        upload_id,
        chunk_bytes,
        window,
    }) = commands.recv()
    else {
        return;
    };
    let mut credits = window.max(1);
    for (index, write) in Writes::new(&scan, upload_id, chunk_bytes).enumerate() {
        while credits == 0 {
            match commands.recv() {
                Ok(WorkerCommand::Credit) => credits += 1,
                Ok(WorkerCommand::Start { .. }) => {}
                Err(_) => return,
            }
        }
        let write = match write {
            Ok(write) => write,
            Err(message) => {
                emit(WorkerEvent::Failed { job, message });
                return;
            }
        };
        let bytes = decoded_len(&write.data);
        let completes_entry = write.last;
        let request_id = format!("upload:{job}:w{index}");
        let request = Request {
            id: request_id.clone(),
            method: Method::PaneUploadWrite(write),
        };
        let Ok(data) = serde_json::to_string(&request) else {
            emit(WorkerEvent::Failed {
                job,
                message: "a write could not be encoded".into(),
            });
            return;
        };
        credits -= 1;
        if !emit(WorkerEvent::Outgoing {
            job,
            write: WriteOut {
                request_id,
                data,
                bytes,
                completes_entry,
            },
        }) {
            return;
        }
    }
    emit(WorkerEvent::WritesDone { job });
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::upload::test_support::TempDir;
    use std::sync::mpsc;
    use std::time::Duration;

    fn start(paths: Vec<PathBuf>) -> (WorkerHandle, mpsc::Receiver<WorkerEvent>) {
        let (tx, rx) = mpsc::channel();
        let handle = spawn(9, paths, 100, DuplicateNames::Rename, move |event| {
            tx.send(event).is_ok()
        })
        .unwrap();
        (handle, rx)
    }

    fn next(rx: &mpsc::Receiver<WorkerEvent>) -> WorkerEvent {
        rx.recv_timeout(Duration::from_secs(5))
            .expect("worker event")
    }

    #[test]
    fn a_missing_path_makes_the_paste_plain_text() {
        let dir = TempDir::new("worker-missing");
        std::fs::write(dir.join("a"), b"").unwrap();
        let (_handle, rx) = start(vec![dir.join("a"), dir.join("nope")]);

        assert_eq!(
            next(&rx),
            WorkerEvent::Checked {
                job: 9,
                is_drop: false
            }
        );
    }

    #[test]
    fn writes_are_prepared_only_while_credits_remain() {
        let dir = TempDir::new("worker-credits");
        std::fs::write(dir.join("f"), vec![7u8; 10]).unwrap();
        let (handle, rx) = start(vec![dir.join("f")]);

        assert_eq!(
            next(&rx),
            WorkerEvent::Checked {
                job: 9,
                is_drop: true
            }
        );
        let WorkerEvent::Scanned {
            result: Ok(summary),
            ..
        } = next(&rx)
        else {
            panic!("scan summary");
        };
        assert_eq!(
            (summary.names, summary.entries, summary.bytes),
            (vec!["f".to_owned()], 1, 10)
        );

        handle.send(WorkerCommand::Start {
            upload_id: "up".into(),
            chunk_bytes: 4,
            window: 1,
        });
        let WorkerEvent::Outgoing { write: first, .. } = next(&rx) else {
            panic!("first write");
        };
        assert_eq!(
            (
                first.request_id.as_str(),
                first.bytes,
                first.completes_entry
            ),
            ("upload:9:w0", 4, false)
        );
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());

        for expected in [(4, false), (2, true)] {
            handle.send(WorkerCommand::Credit);
            let WorkerEvent::Outgoing { write, .. } = next(&rx) else {
                panic!("next write");
            };
            assert_eq!((write.bytes, write.completes_entry), expected);
            let request: Request = serde_json::from_str(&write.data).unwrap();
            assert!(matches!(request.method, Method::PaneUploadWrite(_)));
        }
        handle.send(WorkerCommand::Credit);
        assert_eq!(next(&rx), WorkerEvent::WritesDone { job: 9 });
    }

    #[test]
    fn dropping_the_handle_stops_a_waiting_worker() {
        let dir = TempDir::new("worker-stop");
        std::fs::write(dir.join("f"), b"x").unwrap();
        let (handle, rx) = start(vec![dir.join("f")]);
        next(&rx);
        next(&rx);

        drop(handle);

        assert!(matches!(
            rx.recv_timeout(Duration::from_secs(5)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        ));
    }

    #[test]
    fn base64_lengths_decode_to_the_chunk_size() {
        assert_eq!(decoded_len(""), 0);
        assert_eq!(decoded_len("YQ=="), 1);
        assert_eq!(decoded_len("YWI="), 2);
        assert_eq!(decoded_len("YWJj"), 3);
    }
}
