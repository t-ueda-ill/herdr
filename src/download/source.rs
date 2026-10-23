//! Where a download's frames come from. A source does its blocking work
//! (filesystem reads, a helper process) on its own thread and hands frames to
//! the session worker through a bounded queue, so a read that never returns
//! costs a thread but never stops the worker from answering or cancelling.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::upload::path::Failure;
use crate::upload::worker::Control;

use super::frame::Frame;

/// What a walk found before any byte moves. Estimates only: the stream may
/// differ when the tree changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Began {
    pub(crate) entries: u64,
    pub(crate) bytes: u64,
}

pub(crate) trait DownloadSource: Send {
    /// Walks the paths: estimates, or why the download cannot start.
    fn begin(&mut self) -> Result<Began, Failure>;
    /// The next frame, waiting at most `wait` in short slices that check the
    /// cancel flag. `Ok(None)`: nothing yet.
    fn next(&mut self, wait: Duration, control: &Control) -> Result<Option<Frame>, Failure>;
    /// Stops the source. Best effort and never blocks on the source's I/O.
    fn abort(&mut self);
}

/// What the source thread queues: a frame, or why the stream cannot go on.
pub(crate) type Queued = Result<Frame, Failure>;

pub(crate) fn cancelled() -> Failure {
    ("download_cancelled", "the download was cancelled".into())
}

fn stalled_failure() -> Failure {
    ("download_failed", "the source stopped sending".into())
}

/// The clock behind the stall rule, shared by both ends of the queue.
struct Progress {
    base: Instant,
    /// Milliseconds after `base` of the last sign of life.
    last_ms: AtomicU64,
    /// The producer is waiting for room in a full queue, which only the
    /// consumer can give.
    blocked_on_full: AtomicBool,
}

impl Progress {
    fn touch(&self) {
        let ms = u64::try_from(self.base.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.last_ms.store(ms, Ordering::Release);
    }

    fn quiet_for(&self) -> Duration {
        let last = Duration::from_millis(self.last_ms.load(Ordering::Acquire));
        self.base.elapsed().saturating_sub(last)
    }
}

/// The source thread's end of the queue.
pub(crate) struct FrameSender {
    sender: SyncSender<Queued>,
    progress: Arc<Progress>,
}

/// The consumer is gone, so the thread should end.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Closed;

impl FrameSender {
    /// Records that the source made progress (bytes were read).
    pub(crate) fn touch(&self) {
        self.progress.touch();
    }

    /// Queues `item`, waiting while the queue is full. Neither the wait for
    /// room nor the send counts as the source stalling: a full queue means the
    /// client is not reading, which the session's read deadline measures.
    pub(crate) fn send(&self, item: Queued) -> Result<(), Closed> {
        self.progress.touch();
        let item = match self.sender.try_send(item) {
            Ok(()) => return Ok(()),
            Err(TrySendError::Disconnected(_)) => return Err(Closed),
            Err(TrySendError::Full(item)) => item,
        };
        self.progress.blocked_on_full.store(true, Ordering::Release);
        let sent = self.sender.send(item);
        // Why touch before clearing the flag: the other order leaves a moment
        // where the consumer sees an unblocked source whose last sign of life
        // predates the whole wait for room, and calls it stalled.
        self.progress.touch();
        self.progress
            .blocked_on_full
            .store(false, Ordering::Release);
        sent.map_err(|_| Closed)
    }
}

/// The worker's end of the queue.
pub(crate) struct FrameFeed {
    receiver: Receiver<Queued>,
    progress: Arc<Progress>,
    tick: Duration,
    stall: Duration,
}

/// A queue holding `capacity` frames. `stall`: how long the source may go
/// without reading anything before the stream fails. `tick`: how often a wait
/// looks at the cancel flag.
pub(crate) fn frame_queue(
    capacity: usize,
    stall: Duration,
    tick: Duration,
) -> (FrameSender, FrameFeed) {
    let (sender, receiver) = mpsc::sync_channel(capacity);
    let progress = Arc::new(Progress {
        base: Instant::now(),
        last_ms: AtomicU64::new(0),
        blocked_on_full: AtomicBool::new(false),
    });
    (
        FrameSender {
            sender,
            progress: Arc::clone(&progress),
        },
        FrameFeed {
            receiver,
            progress,
            tick,
            stall,
        },
    )
}

impl FrameFeed {
    /// Restarts the stall clock from now, for a source that was idle by design
    /// (waiting to be told to go) until this moment.
    pub(crate) fn touch(&self) {
        self.progress.touch();
    }

    /// The source has been waiting on its own I/O for longer than `stall`.
    /// Never true while the producer waits for room in a full queue.
    pub(crate) fn stalled(&self) -> bool {
        !self.progress.blocked_on_full.load(Ordering::Acquire)
            && self.progress.quiet_for() > self.stall
    }

    pub(crate) fn next(
        &mut self,
        wait: Duration,
        control: &Control,
    ) -> Result<Option<Frame>, Failure> {
        let deadline = Instant::now() + wait;
        loop {
            // Why try first: frames already queued are served even when the
            // source has since died or the wait is zero.
            match self.receiver.try_recv() {
                Ok(item) => return item.map(Some),
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err((
                        "download_failed",
                        "the source ended before the stream was complete".into(),
                    ));
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
            if control.is_cancelled() {
                return Err(cancelled());
            }
            if self.stalled() {
                return Err(stalled_failure());
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            match self.receiver.recv_timeout(left.min(self.tick)) {
                Ok(item) => return item.map(Some),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    return Err((
                        "download_failed",
                        "the source ended before the stream was complete".into(),
                    ));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TICK: Duration = Duration::from_millis(5);

    fn queue(capacity: usize, stall_ms: u64) -> (FrameSender, FrameFeed) {
        frame_queue(capacity, Duration::from_millis(stall_ms), TICK)
    }

    #[test]
    fn queued_frames_come_out_in_order_and_an_empty_queue_waits_then_gives_none() {
        let (sender, mut feed) = queue(4, 10_000);
        sender.send(Ok(Frame::WalkWarning("a".into()))).unwrap();
        sender.send(Ok(Frame::End)).unwrap();
        let control = Control::default();

        assert_eq!(
            feed.next(Duration::ZERO, &control),
            Ok(Some(Frame::WalkWarning("a".into())))
        );
        assert_eq!(feed.next(Duration::ZERO, &control), Ok(Some(Frame::End)));
        let started = Instant::now();
        assert_eq!(feed.next(Duration::from_millis(30), &control), Ok(None));
        assert!(started.elapsed() >= Duration::from_millis(30));
    }

    #[test]
    fn a_failure_the_source_queued_is_handed_to_the_worker_as_is() {
        let (sender, mut feed) = queue(4, 10_000);
        sender
            .send(Err(("download_source_changed", "x changed".into())))
            .unwrap();

        assert_eq!(
            feed.next(Duration::ZERO, &Control::default()),
            Err(("download_source_changed", "x changed".into()))
        );
    }

    #[test]
    fn a_source_that_vanished_without_ending_fails_the_stream() {
        let (sender, mut feed) = queue(4, 10_000);
        drop(sender);

        let error = feed.next(Duration::ZERO, &Control::default()).unwrap_err();

        assert_eq!(error.0, "download_failed");
    }

    #[test]
    fn a_cancel_during_a_long_wait_returns_within_a_tick_or_two() {
        let (_sender, mut feed) = queue(4, 10_000);
        let control = Arc::new(Control::default());
        let flag = Arc::clone(&control);
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            flag.cancel();
        });
        let started = Instant::now();

        let result = feed.next(Duration::from_secs(30), &control);

        canceller.join().unwrap();
        assert_eq!(result, Err(cancelled()));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn a_source_silent_for_longer_than_the_stall_fails_the_stream() {
        let (_sender, mut feed) = queue(4, 30);

        let error = feed
            .next(Duration::from_secs(10), &Control::default())
            .unwrap_err();

        assert_eq!(error, stalled_failure());
    }

    #[test]
    fn reading_bytes_keeps_a_slow_source_from_counting_as_stalled() {
        let (sender, feed) = queue(4, 60);
        for _ in 0..4 {
            std::thread::sleep(Duration::from_millis(30));
            sender.touch();
            assert!(!feed.stalled());
        }
        std::thread::sleep(Duration::from_millis(90));
        assert!(feed.stalled());
    }

    #[test]
    fn waiting_for_room_in_a_full_queue_is_not_a_stall() {
        let (sender, feed) = queue(1, 20);
        sender.send(Ok(Frame::End)).unwrap();
        let blocked = std::thread::spawn(move || {
            // Full: this waits until the consumer takes the first frame.
            sender.send(Ok(Frame::End))
        });
        std::thread::sleep(Duration::from_millis(120));

        assert!(!feed.stalled());

        let mut feed = feed;
        assert_eq!(
            feed.next(Duration::ZERO, &Control::default()),
            Ok(Some(Frame::End))
        );
        assert_eq!(blocked.join().unwrap(), Ok(()));
    }

    #[test]
    fn sending_to_a_dropped_feed_tells_the_thread_to_end() {
        let (sender, feed) = queue(1, 10_000);
        drop(feed);

        assert_eq!(sender.send(Ok(Frame::End)), Err(Closed));
    }

    #[test]
    fn a_blocked_send_ends_when_the_feed_is_dropped() {
        let (sender, feed) = queue(1, 10_000);
        sender.send(Ok(Frame::End)).unwrap();
        let blocked = std::thread::spawn(move || sender.send(Ok(Frame::End)));
        std::thread::sleep(Duration::from_millis(30));

        drop(feed);

        assert_eq!(blocked.join().unwrap(), Err(Closed));
    }
}
