use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Mutex;

use super::*;
use crate::api::schema::{PaneLocationKind, PaneUploadEntryHeader, PaneUploadEntryKind};
use crate::download::source::Began;

type Answers = mpsc::Receiver<Result<ResponseResult, Failure>>;

/// A source that plays back what a test queues and is silent after that,
/// as a stalled filesystem would be.
#[derive(Default)]
struct Script {
    frames: Mutex<VecDeque<Result<Frame, Failure>>>,
    aborted: AtomicBool,
    begin_failure: Mutex<Option<Failure>>,
    /// How long stopping takes, as a helper process that needs its grace does.
    abort_delay: Mutex<Duration>,
    /// How long each frame takes to arrive, as a helper that starts
    /// processes per entry does.
    frame_delay: Mutex<Duration>,
}

struct FakeSource(Arc<Script>);

impl DownloadSource for FakeSource {
    fn begin(&mut self) -> Result<Began, Failure> {
        if let Some(failure) = self.0.begin_failure.lock().unwrap().take() {
            return Err(failure);
        }
        Ok(Began {
            entries: 3,
            bytes: 9,
        })
    }

    fn next(&mut self, wait: Duration, control: &Control) -> Result<Option<Frame>, Failure> {
        if control.is_cancelled() {
            return Err(cancelled());
        }
        if let Some(item) = self.0.frames.lock().unwrap().pop_front() {
            std::thread::sleep(*self.0.frame_delay.lock().unwrap());
            return item.map(Some);
        }
        std::thread::sleep(wait.min(Duration::from_millis(5)));
        Ok(None)
    }

    fn abort(&mut self) {
        std::thread::sleep(*self.0.abort_delay.lock().unwrap());
        self.0.aborted.store(true, Ordering::Release);
    }
}

fn dir(rel: &str) -> Frame {
    Frame::Entry {
        rel: rel.as_bytes().to_vec(),
        header: PaneUploadEntryHeader {
            kind: PaneUploadEntryKind::Directory,
            mode: 0o755,
            mtime: None,
            size: None,
            link_target: None,
        },
        link_target: None,
    }
}

fn fast() -> Timeouts {
    Timeouts {
        tick: Duration::from_millis(5),
        heartbeat: Duration::from_millis(60),
        first_read: Duration::from_secs(10),
        between_reads: Duration::from_secs(10),
        ..Timeouts::default()
    }
}

struct Harness {
    jobs: mpsc::SyncSender<DownloadJob>,
    answers: Answers,
    reply_to: mpsc::Sender<Result<ResponseResult, Failure>>,
    control: Arc<Control>,
    script: Arc<Script>,
    thread: std::thread::JoinHandle<()>,
}

fn start(frames: Vec<Frame>, limits: Limits, timeouts: Timeouts) -> Harness {
    let script = Arc::new(Script::default());
    script
        .frames
        .lock()
        .unwrap()
        .extend(frames.into_iter().map(Ok));
    start_with(script, limits, timeouts, true)
}

fn start_with(script: Arc<Script>, limits: Limits, timeouts: Timeouts, begin: bool) -> Harness {
    let (jobs, rx) = mpsc::sync_channel(4);
    let control = Arc::new(Control::default());
    let worker = Worker::new(Setup {
        id: "dl-1".into(),
        source: Box::new(FakeSource(script.clone())),
        names: vec!["a".into()],
        limits,
        control: control.clone(),
        location: PaneLocation {
            kind: PaneLocationKind::Host,
            ssh_host: None,
            container: None,
        },
        jobs: rx,
        timeouts,
    });
    let thread = std::thread::spawn(move || worker.run());
    let (reply_to, answers) = mpsc::channel();
    let harness = Harness {
        jobs,
        answers,
        reply_to,
        control,
        script,
        thread,
    };
    if begin {
        harness.send(DownloadJob::Begin);
    }
    harness
}

impl Harness {
    fn send(&self, job: impl FnOnce(Reply) -> DownloadJob) {
        let reply_to = self.reply_to.clone();
        let reply: Reply = Box::new(move |answer| {
            let _ = reply_to.send(answer);
        });
        self.jobs.send(job(reply)).unwrap();
    }

    fn read(&self, seq: u64) {
        self.send(|reply| DownloadJob::Read { seq, reply });
    }

    fn answer(&self) -> Result<ResponseResult, Failure> {
        self.answers
            .recv_timeout(Duration::from_secs(10))
            .expect("the worker should answer")
    }

    fn chunk(&self) -> PaneDownloadChunk {
        match self.answer() {
            Ok(ResponseResult::PaneDownloadChunk(chunk)) => chunk,
            other => panic!("expected a chunk, got {other:?}"),
        }
    }

    fn join(self) -> Arc<Script> {
        drop(self.jobs);
        self.thread.join().unwrap();
        self.script
    }
}

#[test]
fn begin_answers_with_the_names_estimates_chunk_size_and_window() {
    let harness = start(Vec::new(), Limits::new(4096), fast());

    let Ok(ResponseResult::PaneDownloadStarted(started)) = harness.answer() else {
        panic!("expected a started answer")
    };

    assert_eq!(started.download_id, "dl-1");
    assert_eq!(started.names, vec!["a".to_owned()]);
    assert_eq!((started.entries, started.bytes), (3, 9));
    assert_eq!(started.chunk_bytes, 4096);
    assert_eq!(started.window, DOWNLOAD_WINDOW);
    harness.control.cancel();
    harness.join();
}

#[test]
fn a_source_that_cannot_begin_fails_the_begin_and_is_stopped() {
    let script = Arc::new(Script::default());
    *script.begin_failure.lock().unwrap() = Some(("not_found", "/x does not exist".into()));
    let harness = start_with(script, Limits::new(4096), fast(), true);

    assert_eq!(
        harness.answer().unwrap_err(),
        ("not_found", "/x does not exist".into())
    );

    let script = harness.join();
    assert!(script.aborted.load(Ordering::Acquire));
}

#[test]
fn reads_are_answered_in_order_and_the_last_answer_is_done_without_records() {
    let harness = start(vec![dir("a"), Frame::End], Limits::new(4096), fast());
    harness.answer().unwrap();

    harness.read(0);
    let first = harness.chunk();
    harness.read(1);
    let last = harness.chunk();

    assert_eq!((first.seq, first.records.len(), first.done), (0, 1, false));
    assert_eq!((last.seq, last.records.len(), last.done), (1, 0, true));
    let script = harness.join();
    assert!(script.aborted.load(Ordering::Acquire));
}

#[test]
fn a_read_with_the_wrong_seq_is_refused_and_the_session_goes_on() {
    let harness = start(vec![dir("a"), Frame::End], Limits::new(4096), fast());
    harness.answer().unwrap();

    harness.read(5);
    assert_eq!(harness.answer().unwrap_err().0, "invalid_params");
    harness.read(0);

    assert_eq!(harness.chunk().seq, 0);
    harness.control.cancel();
    harness.join();
}

#[test]
fn a_silent_source_is_answered_with_a_heartbeat() {
    let harness = start(Vec::new(), Limits::new(4096), fast());
    harness.answer().unwrap();
    let asked = Instant::now();

    harness.read(0);
    let chunk = harness.chunk();

    assert!(chunk.records.is_empty() && !chunk.done);
    assert!(asked.elapsed() >= Duration::from_millis(50));
    harness.control.cancel();
    harness.join();
}

#[test]
fn a_steady_trickle_of_frames_still_ends_the_answer_at_the_heartbeat() {
    let script = Arc::new(Script::default());
    *script.frame_delay.lock().unwrap() = Duration::from_millis(10);
    script
        .frames
        .lock()
        .unwrap()
        .extend(std::iter::once(Ok(dir("a"))).chain((0..200).map(|i| Ok(dir(&format!("a/d{i}"))))));
    let harness = start_with(script, Limits::new(1 << 20), fast(), true);
    harness.answer().unwrap();
    let asked = Instant::now();

    harness.read(0);
    let chunk = harness.chunk();

    assert!(!chunk.records.is_empty() && !chunk.done);
    assert!(asked.elapsed() < Duration::from_millis(500));
    harness.control.cancel();
    harness.join();
}

#[test]
fn a_read_beyond_the_window_is_busy_while_the_others_get_heartbeats() {
    let harness = start(Vec::new(), Limits::new(4096), fast());
    harness.answer().unwrap();

    for seq in 0..=u64::from(DOWNLOAD_WINDOW) {
        harness.read(seq);
    }

    let busy = harness.answer().unwrap_err();
    assert_eq!(busy.0, "download_busy");
    assert_eq!(harness.chunk().seq, 0);
    assert_eq!(harness.chunk().seq, 1);
    harness.control.cancel();
    harness.join();
}

#[test]
fn a_session_with_no_first_read_ends_after_its_deadline() {
    let harness = start(
        Vec::new(),
        Limits::new(4096),
        Timeouts {
            first_read: Duration::from_millis(40),
            ..fast()
        },
    );
    harness.answer().unwrap();

    let script = harness.join();

    assert!(script.aborted.load(Ordering::Acquire));
}

#[test]
fn a_session_with_no_next_read_ends_after_the_between_reads_deadline() {
    let harness = start(
        vec![dir("a")],
        Limits::new(4096),
        Timeouts {
            between_reads: Duration::from_millis(40),
            ..fast()
        },
    );
    harness.answer().unwrap();
    harness.read(0);
    harness.chunk();
    let waited = Instant::now();

    let script = harness.join();

    assert!(script.aborted.load(Ordering::Acquire));
    assert!(waited.elapsed() < Duration::from_secs(5));
}

#[test]
fn a_cancel_during_a_wait_for_the_source_stops_it_within_a_tick_or_two() {
    let harness = start(
        Vec::new(),
        Limits::new(4096),
        Timeouts {
            heartbeat: Duration::from_secs(30),
            ..fast()
        },
    );
    harness.answer().unwrap();
    harness.read(0);
    std::thread::sleep(Duration::from_millis(30));
    let cancelled_at = Instant::now();

    harness.control.cancel();

    assert_eq!(harness.answer().unwrap_err().0, "download_cancelled");
    let script = harness.join();
    assert!(script.aborted.load(Ordering::Acquire));
    assert!(cancelled_at.elapsed() < Duration::from_secs(2));
}

#[test]
fn dropping_the_registry_handle_cancels_even_when_the_source_never_looks_at_the_flag() {
    let harness = start(
        Vec::new(),
        Limits::new(4096),
        Timeouts {
            heartbeat: Duration::from_secs(30),
            ..fast()
        },
    );
    harness.answer().unwrap();
    harness.read(0);
    std::thread::sleep(Duration::from_millis(30));
    let Harness {
        jobs,
        answers,
        script,
        thread,
        ..
    } = harness;

    drop(jobs);

    assert_eq!(
        answers
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap_err()
            .0,
        "download_cancelled"
    );
    thread.join().unwrap();
    assert!(script.aborted.load(Ordering::Acquire));
}

#[test]
fn reads_queued_behind_the_finishing_answer_find_the_download_gone() {
    let harness = start(vec![dir("a"), Frame::End], Limits::new(4096), fast());
    harness.answer().unwrap();
    harness.read(0);
    harness.chunk();

    harness.read(1);
    harness.read(2);

    assert!(harness.chunk().done);
    assert_eq!(harness.answer().unwrap_err().0, "download_not_found");
    harness.join();
}

#[test]
fn a_finished_session_answers_queued_reads_and_frees_its_slot_before_its_source_stops() {
    let script = Arc::new(Script::default());
    script
        .frames
        .lock()
        .unwrap()
        .extend([Ok(dir("a")), Ok(Frame::End)]);
    *script.abort_delay.lock().unwrap() = Duration::from_millis(600);
    let harness = start_with(script, Limits::new(4096), fast(), true);
    harness.answer().unwrap();
    harness.read(0);
    harness.chunk();

    harness.read(1);
    harness.read(2);

    assert!(harness.chunk().done);
    let late = harness
        .answers
        .recv_timeout(Duration::from_millis(300))
        .expect("a queued read should not wait for the source to stop");
    assert_eq!(late.unwrap_err().0, "download_not_found");
    assert_eq!(harness.control.phase(), PHASE_DONE);
    assert!(!harness.script.aborted.load(Ordering::Acquire));
    let script = harness.join();
    assert!(script.aborted.load(Ordering::Acquire));
}

#[test]
fn a_failure_from_the_source_fails_the_read_and_ends_the_session() {
    let script = Arc::new(Script::default());
    script
        .frames
        .lock()
        .unwrap()
        .push_back(Err(("download_source_changed", "x changed".into())));
    let harness = start_with(script, Limits::new(4096), fast(), true);
    harness.answer().unwrap();

    harness.read(0);

    assert_eq!(harness.answer().unwrap_err().0, "download_source_changed");
    let script = harness.join();
    assert!(script.aborted.load(Ordering::Acquire));
}

#[test]
fn an_answer_too_big_for_one_frame_is_not_sent_and_ends_the_session() {
    // A budget deliberately wrong: the packer is told chunks may be larger
    // than any answer may be.
    let size = crate::download::MAX_DOWNLOAD_REPLY_JSON;
    let limits = Limits::new(size);
    let header = Frame::Entry {
        rel: b"a".to_vec(),
        header: PaneUploadEntryHeader {
            kind: PaneUploadEntryKind::File,
            mode: 0o644,
            mtime: None,
            size: Some(size as u64),
            link_target: None,
        },
        link_target: None,
    };
    let mut frames = vec![header];
    let mut left = size;
    while left > 0 {
        let take = left.min(64 * 1024);
        frames.push(Frame::Data(vec![0xAB; take]));
        left -= take;
    }
    frames.push(Frame::FileEnd(Ok(())));
    frames.push(Frame::End);
    let harness = start(frames, limits, fast());
    harness.answer().unwrap();

    harness.read(0);

    let failure = harness.answer().unwrap_err();
    assert_eq!(
        failure,
        (
            "download_failed",
            "an answer did not fit in one frame".into()
        )
    );
    harness.join();
}

#[test]
fn a_second_begin_is_refused_and_does_not_disturb_the_session() {
    let harness = start(Vec::new(), Limits::new(4096), fast());
    harness.answer().unwrap();

    harness.send(DownloadJob::Begin);

    assert_eq!(harness.answer().unwrap_err().0, "invalid_params");
    harness.control.cancel();
    harness.join();
}
