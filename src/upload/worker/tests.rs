use std::sync::mpsc;

use base64::Engine as _;

use super::*;
use crate::api::schema::{PaneLocationKind, PaneUploadEntryKind};
use crate::upload::session::Declared;
use crate::upload::sink_host::HostSink;
use crate::upload::test_support::TempDir;

type Answers = mpsc::Receiver<Result<ResponseResult, Failure>>;

struct Harness {
    jobs: mpsc::SyncSender<UploadJob>,
    answers: Answers,
    reply_to: mpsc::Sender<Result<ResponseResult, Failure>>,
    control: Arc<Control>,
    thread: std::thread::JoinHandle<()>,
}

fn start(dest: &TempDir, names: &[&str], timeouts: Timeouts) -> Harness {
    let (jobs, rx) = mpsc::sync_channel(3);
    let control = Arc::new(Control::default());
    let worker = Worker {
        id: "up-1".into(),
        session: Session::new(Declared {
            names: names.iter().map(|name| (*name).to_owned()).collect(),
            entries: 10,
            bytes: 1_000_000,
        })
        .unwrap(),
        sink: Box::new(HostSink::new(
            dest.path().to_path_buf(),
            crate::upload::stage_token(),
        )),
        control: control.clone(),
        location: PaneLocation {
            kind: PaneLocationKind::Host,
            ssh_host: None,
            container: None,
        },
        jobs: rx,
        timeouts,
    };
    let thread = std::thread::spawn(move || worker.run());
    let (reply_to, answers) = mpsc::channel();
    let harness = Harness {
        jobs,
        answers,
        reply_to,
        control,
        thread,
    };
    harness.send(UploadJob::Begin);
    harness
}

impl Harness {
    fn send(&self, job: impl FnOnce(Reply) -> UploadJob) {
        let reply_to = self.reply_to.clone();
        let reply: Reply = Box::new(move |answer| {
            let _ = reply_to.send(answer);
        });
        self.jobs.send(job(reply)).unwrap();
    }

    fn answer(&self) -> Result<ResponseResult, Failure> {
        self.answers
            .recv_timeout(Duration::from_secs(10))
            .expect("the worker should answer")
    }

    fn write(
        &self,
        path: &str,
        entry: Option<PaneUploadEntryHeader>,
        offset: u64,
        data: &[u8],
        last: bool,
    ) {
        let job = WriteJob {
            path: path.into(),
            entry,
            offset,
            data: base64::engine::general_purpose::STANDARD.encode(data),
            last,
        };
        self.send(|reply| UploadJob::Write(job, reply));
    }
}

fn file_header(size: u64) -> Option<PaneUploadEntryHeader> {
    Some(PaneUploadEntryHeader {
        kind: PaneUploadEntryKind::File,
        mode: 0o644,
        mtime: None,
        size: Some(size),
        link_target: None,
    })
}

fn code(answer: Result<ResponseResult, Failure>) -> &'static str {
    match answer {
        Ok(_) => "ok",
        Err((code, _)) => code,
    }
}

#[test]
fn begin_then_chunked_writes_then_finish_publishes_and_reports_paths_in_name_order() {
    let dest = TempDir::new("worker-flow");
    let harness = start(&dest, &["b", "a"], Timeouts::default());

    let started = harness.answer().unwrap();
    harness.write("b", file_header(6), 0, b"hel", false);
    let progress = harness.answer().unwrap();
    harness.write("b", None, 3, b"lo!", true);
    harness.answer().unwrap();
    harness.write("a", file_header(0), 0, b"", true);
    harness.answer().unwrap();
    harness.send(|reply| UploadJob::Finish(PaneUploadCollision::Fail, reply));
    let finished = harness.answer().unwrap();

    let ResponseResult::PaneUploadStarted(started) = started else {
        panic!("{started:?}")
    };
    assert_eq!(started.upload_id, "up-1");
    assert_eq!(started.window, UPLOAD_WINDOW);
    assert_eq!(
        progress,
        ResponseResult::PaneUploadProgress(crate::api::schema::PaneUploadProgress {
            received_entries: 1,
            received_bytes: 3
        })
    );
    let ResponseResult::PaneUploadFinished(finished) = finished else {
        panic!("{finished:?}")
    };
    let root = std::fs::canonicalize(dest.path()).unwrap();
    assert_eq!(
        finished.paths,
        [
            root.join("b").to_str().unwrap().to_owned(),
            root.join("a").to_str().unwrap().to_owned()
        ]
    );
    assert_eq!(std::fs::read(dest.join("b")).unwrap(), b"hello!");
    harness.thread.join().unwrap();
}

#[test]
fn a_rejected_write_leaves_the_session_usable() {
    let dest = TempDir::new("worker-reject");
    let harness = start(&dest, &["a"], Timeouts::default());
    harness.answer().unwrap();

    harness.write("a", file_header(2), 1, b"x", false);
    let rejected = harness.answer();
    harness.write("a", file_header(2), 0, b"ok", true);
    let accepted = harness.answer();

    assert_eq!(code(rejected), "invalid_params");
    assert_eq!(code(accepted), "ok");
}

#[test]
fn bad_base64_is_invalid_params() {
    let dest = TempDir::new("worker-b64");
    let harness = start(&dest, &["a"], Timeouts::default());
    harness.answer().unwrap();
    let job = WriteJob {
        path: "a".into(),
        entry: file_header(1),
        offset: 0,
        data: "***".into(),
        last: true,
    };

    harness.send(|reply| UploadJob::Write(job, reply));

    assert_eq!(code(harness.answer()), "invalid_params");
}

#[test]
fn a_conflict_keeps_the_session_open_for_another_finish() {
    let dest = TempDir::new("worker-conflict");
    std::fs::write(dest.join("a"), b"old").unwrap();
    let harness = start(&dest, &["a"], Timeouts::default());
    harness.answer().unwrap();
    harness.write("a", file_header(3), 0, b"new", true);
    harness.answer().unwrap();

    harness.send(|reply| UploadJob::Finish(PaneUploadCollision::Fail, reply));
    let conflict = harness.answer();
    let phase_after_conflict = harness.control.phase();
    harness.send(|reply| UploadJob::Finish(PaneUploadCollision::Rename, reply));
    let renamed = harness.answer();

    let Err((code, message)) = conflict else {
        panic!("{conflict:?}")
    };
    assert_eq!(code, "upload_conflict");
    assert!(message.contains('a'));
    assert_eq!(phase_after_conflict, PHASE_STAGING);
    assert!(renamed.is_ok());
    assert_eq!(std::fs::read(dest.join("a (1)")).unwrap(), b"new");
}

#[test]
fn cancel_removes_the_stage_and_answers_later_requests_as_cancelled() {
    let dest = TempDir::new("worker-cancel");
    let harness = start(&dest, &["a"], Timeouts::default());
    harness.answer().unwrap();
    harness.write("a", file_header(4), 0, b"ab", false);
    harness.answer().unwrap();

    harness.control.cancel();
    harness.write("a", None, 2, b"cd", true);

    assert_eq!(code(harness.answer()), "upload_cancelled");
    harness.thread.join().unwrap();
    assert_eq!(harness.control.phase(), PHASE_DONE);
    assert!(std::fs::read_dir(dest.path()).unwrap().next().is_none());
}

#[test]
fn an_idle_session_is_abandoned_and_its_stage_removed() {
    let dest = TempDir::new("worker-idle");
    let harness = start(
        &dest,
        &["a"],
        Timeouts {
            first_write: Duration::from_millis(200),
            between_writes: Duration::from_millis(200),
            tick: Duration::from_millis(20),
        },
    );
    harness.answer().unwrap();

    harness.thread.join().unwrap();

    assert_eq!(harness.control.phase(), PHASE_DONE);
    assert!(std::fs::read_dir(dest.path()).unwrap().next().is_none());
}

#[test]
fn a_failed_begin_ends_the_session() {
    let dest = TempDir::new("worker-begin");
    let missing = TempDir::new("worker-missing");
    let path = missing.path().to_path_buf();
    drop(missing);
    let (jobs, rx) = mpsc::sync_channel(3);
    let control = Arc::new(Control::default());
    let worker = Worker {
        id: "up-2".into(),
        session: Session::new(Declared {
            names: vec!["a".into()],
            entries: 1,
            bytes: 0,
        })
        .unwrap(),
        sink: Box::new(HostSink::new(path, crate::upload::stage_token())),
        control: control.clone(),
        location: PaneLocation {
            kind: PaneLocationKind::Host,
            ssh_host: None,
            container: None,
        },
        jobs: rx,
        timeouts: Timeouts::default(),
    };
    let (tx, answers) = mpsc::channel();
    jobs.send(UploadJob::Begin(Box::new(move |answer| {
        let _ = tx.send(answer);
    })))
    .unwrap();

    worker.run();

    assert_eq!(code(answers.recv().unwrap()), "not_found");
    assert_eq!(control.phase(), PHASE_DONE);
    drop(dest);
}
