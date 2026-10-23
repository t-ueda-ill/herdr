use super::*;
use crate::api::schema::{
    ErrorBody, ErrorResponse, PaneDownloadChunk, PaneDownloadStarted, PaneLocation,
    PaneLocationKind, SuccessResponse,
};

const NOW_GENERATION: u64 = 3;

fn host() -> PaneLocation {
    PaneLocation {
        kind: PaneLocationKind::Host,
        ssh_host: None,
        container: None,
    }
}

fn request() -> DownloadRequest {
    DownloadRequest {
        endpoint_id: ClientEndpointId::Local,
        generation: NOW_GENERATION,
        tab: 7,
        pane_id: Some("p1".into()),
        location: host(),
        paths: vec!["/srv/a.txt".into(), "/srv/d".into()],
        max_bytes: 128 * 1024,
    }
}

fn success(id: &str, result: ResponseResult) -> String {
    serde_json::to_string(&SuccessResponse {
        id: id.into(),
        result,
    })
    .unwrap()
}

fn error(id: &str, code: &str, message: &str) -> String {
    serde_json::to_string(&ErrorResponse {
        id: id.into(),
        error: ErrorBody {
            code: code.into(),
            message: message.into(),
        },
    })
    .unwrap()
}

fn started_with(location: PaneLocation, window: u32) -> ResponseResult {
    ResponseResult::PaneDownloadStarted(PaneDownloadStarted {
        download_id: "dl-1".into(),
        location,
        names: vec!["a.txt".into(), "d".into()],
        entries: 120,
        bytes: 80_000_000,
        chunk_bytes: 128 * 1024,
        window,
    })
}

fn chunk(seq: u64, done: bool, entries: u64, bytes: u64) -> ResponseResult {
    ResponseResult::PaneDownloadChunk(PaneDownloadChunk {
        seq,
        records: Vec::new(),
        done,
        sent_entries: entries,
        sent_bytes: bytes,
        warnings: Vec::new(),
    })
}

fn read_id(job: JobId, seq: u64) -> String {
    format!("download:{job}:r{seq}")
}

fn requests(effects: &[DownloadEffect]) -> Vec<Request> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            DownloadEffect::Send { data, .. } => serde_json::from_str(data).ok(),
            _ => None,
        })
        .collect()
}

fn methods(effects: &[DownloadEffect]) -> Vec<String> {
    requests(effects)
        .iter()
        .map(|request| crate::api::api_method_name(&request.method).to_owned())
        .collect()
}

fn read_seqs(effects: &[DownloadEffect]) -> Vec<u64> {
    requests(effects)
        .into_iter()
        .filter_map(|request| match request.method {
            Method::PaneDownloadRead(params) => Some(params.seq),
            _ => None,
        })
        .collect()
}

fn notices(effects: &[DownloadEffect]) -> Vec<(bool, String, String)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            DownloadEffect::Notice {
                failed,
                title,
                body,
            } => Some((*failed, title.clone(), body.clone())),
            _ => None,
        })
        .collect()
}

fn stops(effects: &[DownloadEffect]) -> usize {
    effects
        .iter()
        .filter(|effect| matches!(effect, DownloadEffect::StopWorker { .. }))
        .count()
}

fn applies(effects: &[DownloadEffect]) -> Vec<u64> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            DownloadEffect::Apply { seq, .. } => Some(*seq),
            _ => None,
        })
        .collect()
}

/// A job whose begin was answered and whose stage the worker opened: reads 0
/// and 1 are in flight.
fn reading(downloads: &mut Downloads, now: Instant) -> JobId {
    let job = downloads.start(request());
    downloads.prepared(job, Ok(()), now);
    downloads.response(
        &ClientEndpointId::Local,
        NOW_GENERATION,
        success("download:1:begin", started_with(host(), 2)),
    );
    downloads.opened(job, now);
    job
}

fn answer(downloads: &mut Downloads, data: String) -> Vec<DownloadEffect> {
    downloads.response(&ClientEndpointId::Local, NOW_GENERATION, data)
}

#[test]
fn a_download_prepares_begins_opens_and_then_keeps_two_reads_in_flight() {
    let now = Instant::now();
    let mut downloads = Downloads::default();
    let job = downloads.start(request());
    assert!(downloads.busy());

    let begin = downloads.prepared(job, Ok(()), now);
    assert_eq!(methods(&begin), ["pane.download.begin"]);
    let Method::PaneDownloadBegin(params) = requests(&begin).remove(0).method else {
        panic!("begin params");
    };
    assert_eq!(params.pane_id.as_deref(), Some("p1"));
    assert_eq!(params.paths, ["/srv/a.txt", "/srv/d"]);
    assert_eq!(params.max_bytes, Some(128 * 1024));

    let opening = answer(
        &mut downloads,
        success("download:1:begin", started_with(host(), 2)),
    );
    assert_eq!(
        opening,
        vec![DownloadEffect::StartWriter {
            job,
            names: vec!["a.txt".into(), "d".into()]
        }]
    );

    let first_reads = downloads.opened(job, now);
    assert_eq!(read_seqs(&first_reads), [0, 1]);

    let apply = answer(
        &mut downloads,
        success(&read_id(job, 0), chunk(0, false, 1, 10)),
    );
    assert_eq!(applies(&apply), [0]);
    assert!(
        read_seqs(&apply).is_empty(),
        "the next read waits for the apply"
    );

    let next = downloads.applied(job, 0, 1, 10, now);
    assert_eq!(read_seqs(&next), [2]);
}

#[test]
fn an_empty_heartbeat_answer_changes_no_counts_and_asks_for_the_next_read() {
    let now = Instant::now();
    let mut downloads = Downloads::default();
    let job = reading(&mut downloads, now);
    answer(
        &mut downloads,
        success(&read_id(job, 0), chunk(0, false, 0, 0)),
    );
    let before = downloads.progress().unwrap();

    let next = downloads.applied(job, 0, 0, 0, now);

    assert_eq!(read_seqs(&next), [2]);
    assert_eq!(downloads.progress().unwrap(), before);
}

#[test]
fn the_done_answer_moves_to_saving_and_publishing_ends_the_job_with_notices() {
    let now = Instant::now();
    let mut downloads = Downloads::default();
    let job = reading(&mut downloads, now);

    let done = answer(
        &mut downloads,
        success(&read_id(job, 0), chunk(0, true, 3, 30)),
    );

    assert_eq!(applies(&done), [0]);
    let card = downloads.progress().unwrap();
    assert_eq!(card.title, "Saving…");
    assert!(!card.cancellable);

    let finished = downloads.published(
        job,
        "/home/u/Downloads".into(),
        vec![
            "/home/u/Downloads/a.txt".into(),
            "/home/u/Downloads/d".into(),
        ],
        vec!["skipped d/x: it collides".into()],
    );
    assert_eq!(stops(&finished), 1);
    assert_eq!(
        notices(&finished),
        [(
            false,
            "Downloaded".to_owned(),
            "2 items to /home/u/Downloads · 1 warning: skipped d/x: it collides".to_owned()
        )]
    );
    assert!(finished.contains(&DownloadEffect::TabNotice {
        tab: 7,
        text: "downloaded 2 items to /home/u/Downloads".into()
    }));
    assert!(!downloads.busy());
}

#[test]
fn answers_after_the_done_one_are_dropped_without_a_cancel_or_a_second_ending() {
    let now = Instant::now();
    let mut downloads = Downloads::default();
    let job = reading(&mut downloads, now);
    answer(
        &mut downloads,
        success(&read_id(job, 0), chunk(0, true, 1, 1)),
    );

    let late = answer(
        &mut downloads,
        error(&read_id(job, 1), "download_not_found", "no such download"),
    );

    assert!(late.is_empty());
    let finished = downloads.published(job, "/d".into(), vec!["/d/a".into()], Vec::new());
    assert!(!notices(&finished)[0].0, "published, not failed");
}

#[test]
fn a_session_ending_error_fails_once_without_a_cancel_and_later_answers_are_dropped() {
    let now = Instant::now();
    let mut downloads = Downloads::default();
    let job = reading(&mut downloads, now);

    let failed = answer(
        &mut downloads,
        error(
            &read_id(job, 0),
            "download_source_changed",
            "a.txt changed while downloading",
        ),
    );

    assert!(methods(&failed).is_empty(), "the server already ended it");
    assert_eq!(stops(&failed), 1);
    assert_eq!(
        notices(&failed),
        [(
            true,
            "Download failed".to_owned(),
            "a.txt changed while downloading. Nothing was saved.".to_owned()
        )]
    );
    let late = answer(
        &mut downloads,
        error(&read_id(job, 1), "download_cancelled", "cancelled"),
    );
    assert!(late.is_empty());
}

#[test]
fn a_busy_or_invalid_read_fails_and_cancels_because_the_session_survives() {
    for code in ["download_busy", "invalid_params"] {
        let now = Instant::now();
        let mut downloads = Downloads::default();
        let job = reading(&mut downloads, now);

        let failed = answer(
            &mut downloads,
            error(&read_id(job, 0), code, "too many reads"),
        );

        assert_eq!(methods(&failed), ["pane.download.cancel"], "{code}");
        assert_eq!(stops(&failed), 1);
    }
}

#[test]
fn a_refused_begin_fails_without_a_cancel() {
    let now = Instant::now();
    let mut downloads = Downloads::default();
    let job = downloads.start(request());
    downloads.prepared(job, Ok(()), now);

    let failed = answer(
        &mut downloads,
        error("download:1:begin", "not_found", "/srv/a.txt does not exist"),
    );

    assert!(methods(&failed).is_empty());
    assert_eq!(
        notices(&failed)[0].2,
        "/srv/a.txt does not exist. Nothing was saved."
    );
    assert!(!downloads.busy());
}

#[test]
fn a_destination_that_cannot_be_used_fails_before_begin_is_sent() {
    let now = Instant::now();
    let mut downloads = Downloads::default();
    let job = downloads.start(request());

    let failed = downloads.prepared(job, Err("folder not found: /nope".into()), now);

    assert!(methods(&failed).is_empty());
    assert_eq!(
        notices(&failed)[0].2,
        "folder not found: /nope. Nothing was saved."
    );
    assert!(!downloads.busy());
}

#[test]
fn a_pane_that_moved_elsewhere_cancels_the_session_and_says_so() {
    let now = Instant::now();
    let mut downloads = Downloads::default();
    let job = downloads.start(request());
    downloads.prepared(job, Ok(()), now);
    let elsewhere = PaneLocation {
        kind: PaneLocationKind::Ssh,
        ssh_host: Some("box".into()),
        container: None,
    };

    let failed = answer(
        &mut downloads,
        success("download:1:begin", started_with(elsewhere, 2)),
    );

    assert_eq!(methods(&failed), ["pane.download.cancel"]);
    assert_eq!(
        notices(&failed)[0].2,
        "the pane now runs somewhere else; reopen the files view. Nothing was saved."
    );
}

#[test]
fn cancelling_before_begin_is_answered_cancels_the_session_that_begin_opens() {
    let now = Instant::now();
    let mut downloads = Downloads::default();
    let job = downloads.start(request());
    downloads.prepared(job, Ok(()), now);

    let cancelled = downloads.cancel(job);
    assert!(
        methods(&cancelled).is_empty(),
        "there is no id to cancel yet"
    );
    assert_eq!(notices(&cancelled)[0].1, "Download cancelled");
    assert!(!downloads.busy());

    let late = answer(
        &mut downloads,
        success("download:1:begin", started_with(host(), 2)),
    );
    let Method::PaneDownloadCancel(params) = requests(&late).remove(0).method else {
        panic!("cancel of the late session");
    };
    assert_eq!(params.download_id, "dl-1");
}

#[test]
fn cancelling_while_reading_sends_a_cancel_and_stops_the_worker() {
    let now = Instant::now();
    let mut downloads = Downloads::default();
    let job = reading(&mut downloads, now);

    let cancelled = downloads.cancel(job);

    assert_eq!(methods(&cancelled), ["pane.download.cancel"]);
    assert_eq!(stops(&cancelled), 1);
    assert_eq!(
        notices(&cancelled),
        [(
            true,
            "Download cancelled".to_owned(),
            "Nothing was saved".to_owned()
        )]
    );
    assert!(cancelled.contains(&DownloadEffect::TabNotice {
        tab: 7,
        text: "download cancelled: Nothing was saved".into()
    }));
}

#[test]
fn saving_cannot_be_cancelled() {
    let now = Instant::now();
    let mut downloads = Downloads::default();
    let job = reading(&mut downloads, now);
    answer(
        &mut downloads,
        success(&read_id(job, 0), chunk(0, true, 1, 1)),
    );

    assert!(downloads.cancel(job).is_empty());
    assert!(downloads.busy());
}

#[test]
fn a_write_failure_cancels_the_session_unless_the_server_already_ended_it() {
    let now = Instant::now();
    let mut downloads = Downloads::default();
    let job = reading(&mut downloads, now);
    let failed = downloads.worker_failed(job, "cannot save a.txt: No space left on device".into());
    assert_eq!(methods(&failed), ["pane.download.cancel"]);
    assert_eq!(stops(&failed), 1);

    let mut downloads = Downloads::default();
    let job = reading(&mut downloads, now);
    answer(
        &mut downloads,
        success(&read_id(job, 0), chunk(0, true, 1, 1)),
    );
    let failed = downloads.worker_failed(job, "cannot publish".into());
    assert!(methods(&failed).is_empty());
    assert_eq!(stops(&failed), 1);
}

#[test]
fn begin_may_take_three_minutes_and_a_read_one() {
    let now = Instant::now();
    let mut downloads = Downloads::default();
    let job = downloads.start(request());
    downloads.prepared(job, Ok(()), now);
    assert_eq!(
        downloads.next_deadline(),
        Some(now + Duration::from_secs(180))
    );
    assert!(downloads.tick(now + Duration::from_secs(179)).is_empty());
    let expired = downloads.tick(now + Duration::from_secs(180));
    assert_eq!(
        notices(&expired)[0].2,
        "the server did not answer in time. Nothing was saved."
    );
    assert!(!downloads.busy());

    let mut downloads = Downloads::default();
    let job = reading(&mut downloads, now);
    assert_eq!(
        downloads.next_deadline(),
        Some(now + Duration::from_secs(60))
    );
    let expired = downloads.tick(now + Duration::from_secs(60));
    assert_eq!(methods(&expired), ["pane.download.cancel"]);
    assert_eq!(stops(&expired), 1);
    assert!(!downloads.busy());
    let _ = job;
}

#[test]
fn losing_the_connection_ends_the_job_without_a_cancel() {
    let now = Instant::now();
    let mut downloads = Downloads::default();
    reading(&mut downloads, now);

    assert!(downloads
        .endpoint_lost(&ClientEndpointId::Local, NOW_GENERATION + 1)
        .is_empty());
    let lost = downloads.endpoint_lost(&ClientEndpointId::Local, NOW_GENERATION);

    assert!(methods(&lost).is_empty());
    assert_eq!(stops(&lost), 1);
    assert_eq!(
        notices(&lost)[0].2,
        "the connection to the server was lost. Nothing was saved."
    );
}

#[test]
fn losing_the_connection_while_saving_leaves_the_outcome_to_the_publish() {
    let now = Instant::now();
    let mut downloads = Downloads::default();
    let job = reading(&mut downloads, now);
    answer(
        &mut downloads,
        success(&read_id(job, 0), chunk(0, true, 1, 1)),
    );

    let lost = downloads.endpoint_lost(&ClientEndpointId::Local, NOW_GENERATION);

    assert!(lost.is_empty(), "every byte already arrived");
    assert!(downloads.busy());
    let finished = downloads.published(job, "/d".into(), vec!["/d/a".into()], Vec::new());
    assert_eq!(notices(&finished)[0].1, "Downloaded");
}

#[test]
fn answers_from_another_connection_or_job_are_ignored() {
    let now = Instant::now();
    let mut downloads = Downloads::default();
    let job = reading(&mut downloads, now);
    let data = success(&read_id(job, 0), chunk(0, false, 1, 1));

    assert!(downloads
        .response(&ClientEndpointId::Local, NOW_GENERATION + 1, data.clone())
        .is_empty());
    assert!(downloads
        .response(
            &ClientEndpointId::Local,
            NOW_GENERATION,
            success(&read_id(job + 1, 0), chunk(0, false, 1, 1)),
        )
        .is_empty());
    assert!(downloads.busy());
}

#[test]
fn the_card_counts_against_the_estimate_and_drops_the_denominator_past_it() {
    let now = Instant::now();
    let mut downloads = Downloads::default();
    let job = downloads.start(request());
    let card = downloads.progress().unwrap();
    assert_eq!(
        (card.title.as_str(), card.body.as_str(), card.cancellable),
        ("Preparing download", "click to cancel", true)
    );
    downloads.prepared(job, Ok(()), now);
    downloads.response(
        &ClientEndpointId::Local,
        NOW_GENERATION,
        success("download:1:begin", started_with(host(), 2)),
    );
    downloads.opened(job, now);

    downloads.applied(job, 0, 3, 12_300_000, now);
    let card = downloads.progress().unwrap();
    assert_eq!(card.title, "Downloading");
    assert_eq!(card.body, "3/120 · 12.3/80.0 MB — click to cancel");

    downloads.applied(job, 1, 130, 85_100_000, now);
    assert_eq!(
        downloads.progress().unwrap().body,
        "130 items · 85.1 MB — click to cancel"
    );
}

#[test]
fn a_hostile_name_cannot_put_control_characters_in_a_notice() {
    let now = Instant::now();
    let mut downloads = Downloads::default();
    let job = reading(&mut downloads, now);
    let failed = answer(
        &mut downloads,
        error(
            &read_id(job, 0),
            "download_failed",
            "bad \u{1b}]0;x\u{7} name",
        ),
    );
    for (_, title, body) in notices(&failed) {
        assert!(!title.chars().any(char::is_control));
        assert!(!body.chars().any(char::is_control), "{body:?}");
    }

    let mut downloads = Downloads::default();
    let job = reading(&mut downloads, now);
    answer(
        &mut downloads,
        success(&read_id(job, 0), chunk(0, true, 1, 1)),
    );
    let finished = downloads.published(
        job,
        "/d\u{1b}x".into(),
        vec!["/d/a".into()],
        vec!["skipped \u{1b}[2J".into()],
    );
    for (_, _, body) in notices(&finished) {
        assert!(!body.chars().any(char::is_control), "{body:?}");
    }
    for effect in &finished {
        if let DownloadEffect::TabNotice { text, .. } = effect {
            assert!(!text.chars().any(char::is_control), "{text:?}");
        }
    }
}

#[test]
fn only_one_download_runs_at_a_time() {
    let mut downloads = Downloads::default();
    assert!(!downloads.busy());
    downloads.start(request());
    assert!(downloads.busy());
}
