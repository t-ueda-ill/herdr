use std::sync::mpsc;
use std::time::Duration;

use base64::Engine as _;

use super::*;
use crate::api::schema::{
    PaneDownloadRecord, PaneUploadEntryHeader, PaneUploadEntryKind, SuccessResponse,
};
use crate::upload::test_support::TempDir;

fn destination(input: &str, default_dir: &str, home: Option<&std::path::Path>) -> Destination {
    Destination {
        input: input.into(),
        default_dir: default_dir.into(),
        home: home.map(std::path::Path::to_path_buf),
        syntax: PathSyntax::Posix,
    }
}

fn start(destination: Destination) -> (WorkerHandle, mpsc::Receiver<WorkerEvent>) {
    let (tx, rx) = mpsc::channel();
    let handle = spawn(5, destination, move |event| tx.send(event).is_ok()).unwrap();
    (handle, rx)
}

fn next(rx: &mpsc::Receiver<WorkerEvent>) -> WorkerEvent {
    rx.recv_timeout(Duration::from_secs(5))
        .expect("worker event")
}

fn file_record(path: &str, body: &[u8]) -> PaneDownloadRecord {
    PaneDownloadRecord {
        path: path.into(),
        entry: Some(PaneUploadEntryHeader {
            kind: PaneUploadEntryKind::File,
            mode: 0o644,
            mtime: Some(1_700_000_000),
            size: Some(body.len() as u64),
            link_target: None,
        }),
        offset: 0,
        data: base64::engine::general_purpose::STANDARD.encode(body),
        last: true,
    }
}

fn answer(seq: u64, records: Vec<PaneDownloadRecord>, done: bool) -> String {
    serde_json::to_string(&SuccessResponse {
        id: format!("download:5:r{seq}"),
        result: ResponseResult::PaneDownloadChunk(PaneDownloadChunk {
            seq,
            records,
            done,
            sent_entries: u64::from(!done),
            sent_bytes: 3,
            warnings: if done {
                vec!["server warning".into()]
            } else {
                Vec::new()
            },
        }),
    })
    .unwrap()
}

fn stages(dir: &std::path::Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| name.starts_with(".herdr-upload-"))
        .collect()
}

#[test]
fn a_folder_that_exists_is_prepared_as_typed() {
    let dir = TempDir::new("dl-worker-prepare");
    let (_handle, rx) = start(destination(dir.path().to_str().unwrap(), "", None));

    assert_eq!(
        next(&rx),
        WorkerEvent::Prepared {
            job: 5,
            result: Ok(())
        }
    );
}

#[test]
fn a_tilde_is_expanded_under_the_home_folder() {
    let dir = TempDir::new("dl-worker-home");
    std::fs::create_dir(dir.join("saves")).unwrap();
    let (_handle, rx) = start(destination("~/saves", "", Some(dir.path())));

    assert_eq!(
        next(&rx),
        WorkerEvent::Prepared {
            job: 5,
            result: Ok(())
        }
    );
}

#[test]
fn a_tilde_without_a_known_home_is_refused() {
    let (_handle, rx) = start(destination("~/saves", "", None));

    let WorkerEvent::Prepared {
        result: Err(message),
        ..
    } = next(&rx)
    else {
        panic!("a refusal");
    };
    assert!(message.contains("home folder"), "{message}");
}

#[test]
fn a_missing_folder_that_was_typed_is_not_created() {
    let dir = TempDir::new("dl-worker-missing");
    let typed = dir.join("typo");
    let (_handle, rx) = start(destination(typed.to_str().unwrap(), "", None));

    let WorkerEvent::Prepared {
        result: Err(message),
        ..
    } = next(&rx)
    else {
        panic!("a refusal");
    };
    assert_eq!(message, format!("folder not found: {}", typed.display()));
    assert!(!typed.exists());
}

#[test]
fn the_default_folder_is_created_one_level_deep_when_missing() {
    let dir = TempDir::new("dl-worker-default");
    let downloads = dir.join("Downloads");
    let text = downloads.to_str().unwrap();
    let (_handle, rx) = start(destination(text, text, None));

    assert_eq!(
        next(&rx),
        WorkerEvent::Prepared {
            job: 5,
            result: Ok(())
        }
    );
    assert!(downloads.is_dir());

    let deeper = dir.join("a").join("Downloads");
    let text = deeper.to_str().unwrap();
    let (_handle, rx) = start(destination(text, text, None));
    assert!(matches!(
        next(&rx),
        WorkerEvent::Prepared { result: Err(_), .. }
    ));
}

#[test]
fn a_file_where_a_folder_is_wanted_is_refused() {
    let dir = TempDir::new("dl-worker-file");
    std::fs::write(dir.join("f"), b"x").unwrap();
    let typed = dir.join("f");
    let (_handle, rx) = start(destination(typed.to_str().unwrap(), "", None));

    let WorkerEvent::Prepared {
        result: Err(message),
        ..
    } = next(&rx)
    else {
        panic!("a refusal");
    };
    assert_eq!(message, format!("not a folder: {}", typed.display()));
}

#[test]
fn answers_are_written_and_the_done_one_publishes_with_every_warning() {
    let dir = TempDir::new("dl-worker-flow");
    let (handle, rx) = start(destination(dir.path().to_str().unwrap(), "", None));
    next(&rx);

    handle.send(WorkerCommand::Open {
        names: vec!["a.txt".into()],
    });
    assert_eq!(next(&rx), WorkerEvent::Opened { job: 5 });
    handle.send(WorkerCommand::Apply {
        seq: 0,
        data: answer(0, vec![file_record("a.txt", b"hi!")], false),
    });
    assert_eq!(
        next(&rx),
        WorkerEvent::Applied {
            job: 5,
            seq: 0,
            entries: 1,
            bytes: 3
        }
    );
    assert!(
        stages(dir.path()).len() == 1,
        "written into the stage first"
    );
    assert!(!dir.join("a.txt").exists());

    handle.send(WorkerCommand::Apply {
        seq: 1,
        data: answer(1, Vec::new(), true),
    });
    assert!(matches!(next(&rx), WorkerEvent::Applied { seq: 1, .. }));
    let WorkerEvent::Published {
        destination,
        paths,
        warnings,
        ..
    } = next(&rx)
    else {
        panic!("published");
    };
    assert_eq!(std::fs::read(dir.join("a.txt")).unwrap(), b"hi!");
    assert_eq!(paths.len(), 1);
    assert!(paths[0].ends_with("a.txt"), "{paths:?}");
    assert!(destination.ends_with(dir.path().file_name().unwrap().to_str().unwrap()));
    assert_eq!(warnings, ["server warning"]);
    assert!(stages(dir.path()).is_empty());
}

#[test]
fn a_name_that_exists_is_published_under_a_numbered_name() {
    let dir = TempDir::new("dl-worker-numbered");
    std::fs::write(dir.join("a.txt"), b"old").unwrap();
    let (handle, rx) = start(destination(dir.path().to_str().unwrap(), "", None));
    next(&rx);
    handle.send(WorkerCommand::Open {
        names: vec!["a.txt".into()],
    });
    next(&rx);

    handle.send(WorkerCommand::Apply {
        seq: 0,
        data: answer(0, vec![file_record("a.txt", b"new")], true),
    });
    next(&rx);
    assert!(matches!(next(&rx), WorkerEvent::Published { .. }));

    assert_eq!(std::fs::read(dir.join("a.txt")).unwrap(), b"old");
    assert_eq!(std::fs::read(dir.join("a (1).txt")).unwrap(), b"new");
}

#[test]
fn a_broken_answer_fails_the_download_and_removes_the_stage() {
    let dir = TempDir::new("dl-worker-broken");
    let (handle, rx) = start(destination(dir.path().to_str().unwrap(), "", None));
    next(&rx);
    handle.send(WorkerCommand::Open {
        names: vec!["a.txt".into()],
    });
    next(&rx);

    handle.send(WorkerCommand::Apply {
        seq: 0,
        data: "not json".into(),
    });

    assert!(matches!(next(&rx), WorkerEvent::Failed { .. }));
    assert!(stages(dir.path()).is_empty());
}

#[test]
fn an_answer_for_the_wrong_read_fails_the_download() {
    let dir = TempDir::new("dl-worker-seq");
    let (handle, rx) = start(destination(dir.path().to_str().unwrap(), "", None));
    next(&rx);
    handle.send(WorkerCommand::Open {
        names: vec!["a.txt".into()],
    });
    next(&rx);

    handle.send(WorkerCommand::Apply {
        seq: 0,
        data: answer(1, Vec::new(), false),
    });

    let WorkerEvent::Failed { message, .. } = next(&rx) else {
        panic!("failure");
    };
    assert!(message.contains("expected"), "{message}");
    assert!(stages(dir.path()).is_empty());
}

#[test]
fn a_record_the_stage_cannot_take_fails_without_the_word_upload() {
    let dir = TempDir::new("dl-worker-record");
    let (handle, rx) = start(destination(dir.path().to_str().unwrap(), "", None));
    next(&rx);
    handle.send(WorkerCommand::Open {
        names: vec!["a.txt".into()],
    });
    next(&rx);

    // A record for a name the server never declared.
    handle.send(WorkerCommand::Apply {
        seq: 0,
        data: answer(0, vec![file_record("b.txt", b"x")], false),
    });

    let WorkerEvent::Failed { message, .. } = next(&rx) else {
        panic!("failure");
    };
    assert!(!message.to_lowercase().contains("upload"), "{message}");
    assert!(stages(dir.path()).is_empty());
}

#[test]
fn dropping_the_handle_removes_the_stage() {
    let dir = TempDir::new("dl-worker-drop");
    let (handle, rx) = start(destination(dir.path().to_str().unwrap(), "", None));
    next(&rx);
    handle.send(WorkerCommand::Open {
        names: vec!["a.txt".into()],
    });
    next(&rx);
    assert_eq!(stages(dir.path()).len(), 1);

    drop(handle);

    // Why no wait: a client that quits right after must not leave it behind.
    assert!(stages(dir.path()).is_empty());
}
