use super::*;
use crate::api::schema::{
    ErrorBody, ErrorResponse, PaneLocation, PaneLocationKind, PaneUploadFinished,
    PaneUploadProgress, PaneUploadStarted, SuccessResponse,
};

const LIMITS: UploadLimits = UploadLimits {
    confirm_bytes: 1000,
    confirm_entries: 10,
    max_bytes: 10_000,
    max_entries: 100,
};

fn drop_request() -> DropRequest {
    DropRequest {
        endpoint_id: ClientEndpointId::Local,
        generation: 3,
        target: UploadTarget::Pane("p1".into()),
        trailing_space: true,
        chunk_cap: crate::upload::UPLOAD_CHUNK_BYTES,
        can_confirm: true,
    }
}

fn summary(entries: u64, bytes: u64) -> ScanSummary {
    ScanSummary {
        names: vec!["a b.txt".into()],
        entries,
        bytes,
        skipped_special: 0,
        warnings: Vec::new(),
    }
}

fn location() -> PaneLocation {
    PaneLocation {
        kind: PaneLocationKind::Ssh,
        ssh_host: Some("box".into()),
        container: None,
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

fn started(window: u32, chunk_bytes: u64) -> ResponseResult {
    ResponseResult::PaneUploadStarted(PaneUploadStarted {
        upload_id: "up-1".into(),
        location: location(),
        destination: "/home/u".into(),
        conflicts: vec!["a b.txt".into()],
        chunk_bytes,
        window,
    })
}

fn progress() -> ResponseResult {
    ResponseResult::PaneUploadProgress(PaneUploadProgress {
        received_entries: 1,
        received_bytes: 1,
    })
}

fn sent_methods(effects: &[UploadEffect]) -> Vec<String> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            UploadEffect::Send { data, .. } => {
                let request: Request = serde_json::from_str(data).unwrap();
                Some(crate::api::api_method_name(&request.method).to_owned())
            }
            _ => None,
        })
        .collect()
}

fn write(n: u64, completes_entry: bool) -> WriteOut {
    WriteOut {
        request_id: format!("upload:1:w{n}"),
        data: format!("write-{n}"),
        bytes: 10,
        completes_entry,
    }
}

/// A job that has begun and is ready to send writes.
fn sending(uploads: &mut Uploads, now: Instant) -> JobId {
    let job = uploads.start(drop_request());
    uploads.checked(job, true);
    uploads.scanned(job, Ok(summary(2, 20)), &LIMITS, now);
    uploads.response(
        &ClientEndpointId::Local,
        3,
        &success("upload:1:begin", started(2, 512 * 1024)),
        now,
    );
    job
}

#[test]
fn a_small_drop_begins_writes_finishes_with_rename_and_pastes_quoted_paths() {
    let now = Instant::now();
    let mut uploads = Uploads::default();
    let job = uploads.start(drop_request());
    assert!(uploads.checked(job, true).is_empty());

    let begin = uploads.scanned(job, Ok(summary(2, 20)), &LIMITS, now);
    assert_eq!(sent_methods(&begin), ["pane.upload.begin"]);
    let UploadEffect::Send { data, .. } = &begin[0] else {
        panic!("begin is sent");
    };
    let request: Request = serde_json::from_str(data).unwrap();
    let Method::PaneUploadBegin(params) = request.method else {
        panic!("begin params");
    };
    assert_eq!(params.pane_id.as_deref(), Some("p1"));
    assert_eq!((params.entries, params.bytes), (2, 20));

    let start = uploads.response(
        &ClientEndpointId::Local,
        3,
        &success("upload:1:begin", started(2, 512 * 1024)),
        now,
    );
    assert_eq!(
        start,
        [UploadEffect::StartWriter {
            job,
            upload_id: "up-1".into(),
            chunk_bytes: 512 * 1024,
            window: 2,
        }]
    );

    for n in 0..2 {
        let sent = uploads.outgoing(job, write(n, true), now);
        assert!(
            matches!(&sent[..], [UploadEffect::Send { data, .. }] if *data == format!("write-{n}"))
        );
    }
    assert!(uploads.writes_done(job, now).is_empty());
    assert_eq!(
        uploads.response(
            &ClientEndpointId::Local,
            3,
            &success("upload:1:w0", progress()),
            now
        ),
        [UploadEffect::Credit { job }]
    );
    let last = uploads.response(
        &ClientEndpointId::Local,
        3,
        &success("upload:1:w1", progress()),
        now,
    );
    assert_eq!(last[0], UploadEffect::Credit { job });
    assert_eq!(sent_methods(&last), ["pane.upload.finish"]);
    let UploadEffect::Send { data, .. } = &last[1] else {
        panic!("finish is sent");
    };
    assert!(data.contains("\"collision\":\"rename\""), "{data}");

    let done = uploads.response(
        &ClientEndpointId::Local,
        3,
        &success(
            "upload:1:finish",
            ResponseResult::PaneUploadFinished(PaneUploadFinished {
                location: location(),
                destination: "/home/u".into(),
                paths: vec!["/home/u/a b (1).txt".into()],
                warnings: Vec::new(),
            }),
        ),
        now,
    );
    assert_eq!(done[0], UploadEffect::StopWorker { job });
    assert_eq!(
        done[1],
        UploadEffect::Paste {
            endpoint_id: ClientEndpointId::Local,
            generation: 3,
            target: UploadTarget::Pane("p1".into()),
            text: "'/home/u/a b (1).txt' ".into(),
        }
    );
    assert!(
        matches!(&done[2], UploadEffect::Notice { failed: false, body, .. } if body.contains("/home/u"))
    );
    assert!(!uploads.busy());
}

#[test]
fn finish_waits_until_every_write_is_answered() {
    let now = Instant::now();
    let mut uploads = Uploads::default();
    let job = sending(&mut uploads, now);
    uploads.outgoing(job, write(0, true), now);

    assert!(uploads.writes_done(job, now).is_empty());
    let answered = uploads.response(
        &ClientEndpointId::Local,
        3,
        &success("upload:1:w0", progress()),
        now,
    );

    assert_eq!(sent_methods(&answered), ["pane.upload.finish"]);
}

#[test]
fn a_paste_that_is_not_a_drop_ends_the_job_without_a_notice() {
    let mut uploads = Uploads::default();
    let job = uploads.start(drop_request());

    assert_eq!(
        uploads.checked(job, false),
        [UploadEffect::StopWorker { job }]
    );
    assert!(!uploads.busy());
}

#[test]
fn drops_past_a_confirm_value_ask_and_declining_sends_nothing() {
    let now = Instant::now();
    let mut uploads = Uploads::default();
    let job = uploads.start(drop_request());
    uploads.checked(job, true);

    let ask = uploads.scanned(job, Ok(summary(11, 20)), &LIMITS, now);
    assert!(
        matches!(&ask[..], [UploadEffect::Confirm { job: asked, detail, .. }]
        if *asked == job && detail.contains("11 items"))
    );
    assert!(uploads.progress().is_none());

    assert_eq!(
        uploads.confirmed(job, false, now),
        [UploadEffect::StopWorker { job }]
    );
    assert!(!uploads.busy());
}

#[test]
fn accepting_the_confirmation_begins_the_upload() {
    let now = Instant::now();
    let mut uploads = Uploads::default();
    let job = uploads.start(drop_request());
    uploads.checked(job, true);
    uploads.scanned(job, Ok(summary(11, 20)), &LIMITS, now);

    assert_eq!(
        sent_methods(&uploads.confirmed(job, true, now)),
        ["pane.upload.begin"]
    );
}

#[test]
fn a_direct_attach_refuses_what_a_shell_would_confirm() {
    let now = Instant::now();
    let mut uploads = Uploads::default();
    let job = uploads.start(DropRequest {
        can_confirm: false,
        target: UploadTarget::Direct {
            terminal_id: "t1".into(),
            bracketed: true,
        },
        ..drop_request()
    });
    uploads.checked(job, true);

    let effects = uploads.scanned(job, Ok(summary(11, 20)), &LIMITS, now);

    assert!(
        matches!(&effects[..], [UploadEffect::StopWorker { .. }, UploadEffect::Notice { failed: true, body, .. }]
        if body.contains("herdr pane upload"))
    );
}

#[test]
fn uploads_past_a_maximum_or_the_scan_limit_are_refused() {
    let now = Instant::now();
    for result in [Ok(summary(101, 0)), Err(ScanError::TooManyEntries(100))] {
        let mut uploads = Uploads::default();
        let job = uploads.start(drop_request());
        uploads.checked(job, true);

        let effects = uploads.scanned(job, result, &LIMITS, now);

        assert!(
            matches!(&effects[..], [UploadEffect::StopWorker { .. }, UploadEffect::Notice { failed: true, body, .. }]
            if body.contains("ui.upload_max_entries")),
            "{effects:?}"
        );
    }
}

#[test]
fn answers_from_another_connection_are_ignored() {
    let now = Instant::now();
    let mut uploads = Uploads::default();
    let job = uploads.start(drop_request());
    uploads.checked(job, true);
    uploads.scanned(job, Ok(summary(1, 1)), &LIMITS, now);
    let answer = success("upload:1:begin", started(2, 1024));

    assert!(uploads
        .response(&ClientEndpointId::Local, 4, &answer, now)
        .is_empty());
    assert!(uploads
        .response(&ClientEndpointId::Local, 3, "not json", now)
        .is_empty());
    assert!(!uploads
        .response(&ClientEndpointId::Local, 3, &answer, now)
        .is_empty());
}

#[test]
fn the_chunk_size_is_capped_for_the_connection_and_the_window_clamped() {
    let now = Instant::now();
    let mut uploads = Uploads::default();
    let job = uploads.start(DropRequest {
        chunk_cap: REMOTE_CHUNK_BYTES,
        ..drop_request()
    });
    uploads.checked(job, true);
    uploads.scanned(job, Ok(summary(1, 1)), &LIMITS, now);

    let effects = uploads.response(
        &ClientEndpointId::Local,
        3,
        &success("upload:1:begin", started(9, 512 * 1024)),
        now,
    );

    assert!(
        matches!(&effects[..], [UploadEffect::StartWriter { chunk_bytes, window: 2, .. }]
        if *chunk_bytes == REMOTE_CHUNK_BYTES)
    );
}

#[test]
fn a_refused_begin_reports_the_server_message_without_cancelling() {
    let now = Instant::now();
    let mut uploads = Uploads::default();
    let job = uploads.start(drop_request());
    uploads.checked(job, true);
    uploads.scanned(job, Ok(summary(1, 1)), &LIMITS, now);

    let effects = uploads.response(
        &ClientEndpointId::Local,
        3,
        &error("upload:1:begin", "ssh_unavailable", "cannot upload to box"),
        now,
    );

    assert!(sent_methods(&effects).is_empty());
    assert!(
        matches!(&effects[..], [UploadEffect::StopWorker { .. }, UploadEffect::Notice { failed: true, body, .. }]
        if body == "cannot upload to box")
    );
}

#[test]
fn a_refused_write_cancels_the_session() {
    let now = Instant::now();
    let mut uploads = Uploads::default();
    let job = sending(&mut uploads, now);
    uploads.outgoing(job, write(0, false), now);

    let effects = uploads.response(
        &ClientEndpointId::Local,
        3,
        &error("upload:1:w0", "upload_failed", "disk full"),
        now,
    );

    assert_eq!(sent_methods(&effects), ["pane.upload.cancel"]);
    assert!(effects.contains(&UploadEffect::StopWorker { job }));
    assert!(!uploads.busy());
}

#[test]
fn cancelling_while_sending_stops_but_publishing_cannot_be_cancelled() {
    let now = Instant::now();
    let mut uploads = Uploads::default();
    let job = sending(&mut uploads, now);
    let effects = uploads.cancel(job);
    assert_eq!(sent_methods(&effects), ["pane.upload.cancel"]);
    assert!(!uploads.busy());

    let mut uploads = Uploads::default();
    let job = sending(&mut uploads, now);
    uploads.writes_done(job, now);
    assert!(!uploads.progress().unwrap().cancellable);
    assert!(uploads.cancel(job).is_empty());
    assert!(uploads.busy());
}

#[test]
fn an_unanswered_request_times_out_and_cancels() {
    let now = Instant::now();
    let mut uploads = Uploads::default();
    let job = sending(&mut uploads, now);
    uploads.outgoing(job, write(0, false), now);
    assert_eq!(uploads.next_deadline(), Some(now + REQUEST_TIMEOUT));

    assert!(uploads.tick(now + Duration::from_secs(59)).is_empty());
    let effects = uploads.tick(now + REQUEST_TIMEOUT);

    assert_eq!(sent_methods(&effects), ["pane.upload.cancel"]);
    assert!(!uploads.busy());
}

#[test]
fn losing_the_connection_fails_only_the_job_on_it() {
    let now = Instant::now();
    let mut uploads = Uploads::default();
    let job = sending(&mut uploads, now);

    assert!(uploads
        .endpoint_lost(&ClientEndpointId::Local, 2)
        .is_empty());
    let effects = uploads.endpoint_lost(&ClientEndpointId::Local, 3);

    assert_eq!(effects[0], UploadEffect::StopWorker { job });
    assert!(sent_methods(&effects).is_empty());
}

#[test]
fn losing_the_connection_while_asking_closes_the_dialog() {
    let now = Instant::now();
    let mut uploads = Uploads::default();
    let job = uploads.start(drop_request());
    uploads.checked(job, true);
    uploads.scanned(job, Ok(summary(11, 20)), &LIMITS, now);

    let effects = uploads.endpoint_lost(&ClientEndpointId::Local, 3);

    assert_eq!(
        &effects[..2],
        [
            UploadEffect::StopWorker { job },
            UploadEffect::CloseConfirm { job }
        ]
    );
}

#[test]
fn progress_counts_answered_entries_and_bytes() {
    let now = Instant::now();
    let mut uploads = Uploads::default();
    let job = sending(&mut uploads, now);
    uploads.outgoing(job, write(0, true), now);
    uploads.response(
        &ClientEndpointId::Local,
        3,
        &success("upload:1:w0", progress()),
        now,
    );

    let view = uploads.progress().unwrap();

    assert_eq!(view.job, job);
    assert_eq!(view.title, "Uploading");
    assert_eq!(view.body, "1/2 · 0.0/0.0 MB — click to cancel");
    assert!(view.cancellable);
}

fn workbench_request() -> DropRequest {
    DropRequest {
        target: UploadTarget::Workbench(WorkbenchTarget {
            tab: 7,
            destination: "/srv/app".into(),
            pane_id: Some("p1".into()),
            location: location(),
        }),
        trailing_space: false,
        ..drop_request()
    }
}

/// A workbench job whose begin was answered with `conflicts`.
fn asking(uploads: &mut Uploads, conflicts: &[&str], now: Instant) -> (JobId, Vec<UploadEffect>) {
    let job = uploads.start(workbench_request());
    uploads.checked(job, true);
    uploads.scanned(job, Ok(summary(2, 20)), &LIMITS, now);
    let effects = uploads.response(
        &ClientEndpointId::Local,
        3,
        &success(
            "upload:1:begin",
            ResponseResult::PaneUploadStarted(PaneUploadStarted {
                conflicts: conflicts.iter().map(|name| (*name).to_owned()).collect(),
                ..match started(2, 512 * 1024) {
                    ResponseResult::PaneUploadStarted(started) => started,
                    _ => unreachable!(),
                }
            }),
        ),
        now,
    );
    (job, effects)
}

fn finish_collision(effects: &[UploadEffect]) -> Option<PaneUploadCollision> {
    effects.iter().find_map(|effect| match effect {
        UploadEffect::Send { data, .. } => {
            let request: Request = serde_json::from_str(data).ok()?;
            match request.method {
                Method::PaneUploadFinish(params) => Some(params.collision),
                _ => None,
            }
        }
        _ => None,
    })
}

#[test]
fn a_tree_drop_begins_into_the_selected_folder_without_the_threshold_dialog() {
    let now = Instant::now();
    let mut uploads = Uploads::default();
    let job = uploads.start(workbench_request());
    uploads.checked(job, true);

    // Past the confirm value: the tree always asks after begin instead.
    let begin = uploads.scanned(job, Ok(summary(11, 20)), &LIMITS, now);

    assert_eq!(sent_methods(&begin), ["pane.upload.begin"]);
    let UploadEffect::Send { data, .. } = &begin[0] else {
        panic!("begin is sent");
    };
    let request: Request = serde_json::from_str(data).unwrap();
    let Method::PaneUploadBegin(params) = request.method else {
        panic!("begin params");
    };
    assert_eq!(params.pane_id.as_deref(), Some("p1"));
    assert_eq!(params.destination.as_deref(), Some("/srv/app"));
}

#[test]
fn a_tree_drop_onto_the_servers_host_is_refused_past_a_maximum() {
    let now = Instant::now();
    let mut uploads = Uploads::default();
    let job = uploads.start(DropRequest {
        target: UploadTarget::Workbench(WorkbenchTarget {
            tab: 7,
            destination: "/srv/app".into(),
            pane_id: None,
            location: location(),
        }),
        trailing_space: false,
        ..drop_request()
    });
    uploads.checked(job, true);

    let effects = uploads.scanned(job, Ok(summary(1, 10_001)), &LIMITS, now);

    assert!(
        effects.iter().any(
            |effect| matches!(effect, UploadEffect::Notice { failed: true, body, .. }
            if body.contains("ui.upload_max_bytes"))
        ),
        "{effects:?}"
    );
    assert!(sent_methods(&effects).is_empty());
}

#[test]
fn a_tree_drop_cancels_when_the_pane_is_no_longer_in_the_tabs_environment() {
    let now = Instant::now();
    let mut uploads = Uploads::default();
    let job = uploads.start(DropRequest {
        target: UploadTarget::Workbench(WorkbenchTarget {
            tab: 7,
            destination: "/srv/app".into(),
            pane_id: Some("p1".into()),
            location: PaneLocation {
                kind: PaneLocationKind::Container,
                ssh_host: None,
                container: Some("app".into()),
            },
        }),
        trailing_space: false,
        ..drop_request()
    });
    uploads.checked(job, true);
    uploads.scanned(job, Ok(summary(2, 20)), &LIMITS, now);
    let host = PaneLocation {
        kind: PaneLocationKind::Host,
        ssh_host: None,
        container: None,
    };

    let effects = uploads.response(
        &ClientEndpointId::Local,
        3,
        &success(
            "upload:1:begin",
            ResponseResult::PaneUploadStarted(PaneUploadStarted {
                location: host,
                ..match started(2, 512 * 1024) {
                    ResponseResult::PaneUploadStarted(started) => started,
                    _ => unreachable!(),
                }
            }),
        ),
        now,
    );

    assert_eq!(sent_methods(&effects), ["pane.upload.cancel"]);
    assert!(effects.iter().any(|effect| matches!(effect,
        UploadEffect::TabNotice { tab: 7, text } if text.contains("no longer in app"))));
    assert!(effects
        .iter()
        .any(|effect| matches!(effect, UploadEffect::Notice { failed: true, .. })));
    assert!(!effects.iter().any(|effect| matches!(
        effect,
        UploadEffect::Ask { .. } | UploadEffect::StartWriter { .. }
    )));
    assert!(!uploads.busy());
}

#[test]
fn a_tree_drop_asks_before_writing_and_shows_the_destination_and_conflicts() {
    let now = Instant::now();
    let mut uploads = Uploads::default();

    let (job, effects) = asking(&mut uploads, &["a b.txt"], now);

    assert_eq!(
        effects,
        [UploadEffect::Ask {
            job,
            tab: 7,
            destination: "/srv/app".into(),
            summary: "2 items · 0.0 MB".into(),
            conflicts: vec!["a b.txt".into()],
            note: None,
        }]
    );
    assert!(uploads.progress().is_none());
    assert!(uploads.next_deadline().is_none());
}

#[test]
fn each_tree_choice_writes_then_finishes_with_its_collision_policy() {
    for collision in [
        PaneUploadCollision::Fail,
        PaneUploadCollision::Rename,
        PaneUploadCollision::Replace,
    ] {
        let now = Instant::now();
        let mut uploads = Uploads::default();
        let (job, _) = asking(&mut uploads, &[], now);

        let start = uploads.chosen(job, Some(collision), now);
        assert!(matches!(&start[..], [UploadEffect::StartWriter { .. }]));
        uploads.outgoing(job, write(0, true), now);
        uploads.writes_done(job, now);
        let finish = uploads.response(
            &ClientEndpointId::Local,
            3,
            &success("upload:1:w0", progress()),
            now,
        );

        assert_eq!(finish_collision(&finish), Some(collision), "{collision:?}");
    }
}

#[test]
fn cancelling_the_tree_dialog_cancels_the_begun_session() {
    let now = Instant::now();
    let mut uploads = Uploads::default();
    let (job, _) = asking(&mut uploads, &["a b.txt"], now);

    let effects = uploads.chosen(job, None, now);

    assert_eq!(sent_methods(&effects), ["pane.upload.cancel"]);
    assert!(!uploads.busy());
}

#[test]
fn a_conflict_that_appears_before_publishing_asks_again_and_finishes_with_the_new_choice() {
    let now = Instant::now();
    let mut uploads = Uploads::default();
    let (job, _) = asking(&mut uploads, &[], now);
    uploads.chosen(job, Some(PaneUploadCollision::Fail), now);
    uploads.outgoing(job, write(0, true), now);
    uploads.writes_done(job, now);
    uploads.response(
        &ClientEndpointId::Local,
        3,
        &success("upload:1:w0", progress()),
        now,
    );

    let ask = uploads.response(
        &ClientEndpointId::Local,
        3,
        &error(
            "upload:1:finish",
            "upload_conflict",
            "already exists in the destination: a b.txt",
        ),
        now,
    );

    assert!(sent_methods(&ask).is_empty(), "the stage is kept");
    assert!(
        matches!(&ask[..], [UploadEffect::Ask { job: asked, note: Some(note), .. }]
        if *asked == job && note.contains("a b.txt"))
    );
    let retry = uploads.chosen(job, Some(PaneUploadCollision::Replace), now);
    assert_eq!(sent_methods(&retry), ["pane.upload.finish"]);
    assert_eq!(finish_collision(&retry), Some(PaneUploadCollision::Replace));
}

#[test]
fn a_finished_tree_upload_refreshes_the_folder_instead_of_pasting() {
    let now = Instant::now();
    let mut uploads = Uploads::default();
    let (job, _) = asking(&mut uploads, &[], now);
    uploads.chosen(job, Some(PaneUploadCollision::Fail), now);
    uploads.outgoing(job, write(0, true), now);
    uploads.writes_done(job, now);
    uploads.response(
        &ClientEndpointId::Local,
        3,
        &success("upload:1:w0", progress()),
        now,
    );

    let done = uploads.response(
        &ClientEndpointId::Local,
        3,
        &success(
            "upload:1:finish",
            ResponseResult::PaneUploadFinished(PaneUploadFinished {
                location: location(),
                destination: "/real/srv/app".into(),
                paths: vec!["/real/srv/app/a b.txt".into()],
                warnings: Vec::new(),
            }),
        ),
        now,
    );

    assert!(!done
        .iter()
        .any(|effect| matches!(effect, UploadEffect::Paste { .. })));
    assert!(done.contains(&UploadEffect::WorkbenchDone {
        tab: 7,
        destination: "/srv/app".into(),
        names: vec!["a b.txt".into()],
    }));
    assert!(done.iter().any(|effect| matches!(effect,
        UploadEffect::TabNotice { tab: 7, text } if text == "uploaded 1 item to /srv/app")));
}

#[test]
fn a_tree_drop_of_missing_paths_says_so_in_the_tab() {
    let mut uploads = Uploads::default();
    let job = uploads.start(workbench_request());

    let effects = uploads.checked(job, false);

    assert!(effects.iter().any(|effect| matches!(effect,
        UploadEffect::TabNotice { tab: 7, text } if text.contains("not found"))));
}

#[test]
fn tree_failures_also_reach_the_tab_notice() {
    let now = Instant::now();
    let mut uploads = Uploads::default();
    let job = uploads.start(workbench_request());
    uploads.checked(job, true);

    let effects = uploads.scanned(job, Ok(summary(101, 0)), &LIMITS, now);

    assert!(effects.iter().any(|effect| matches!(effect,
        UploadEffect::TabNotice { tab: 7, text } if text.starts_with("upload failed: "))));
}

#[test]
fn losing_the_connection_while_the_tree_asks_closes_its_dialog() {
    let now = Instant::now();
    let mut uploads = Uploads::default();
    let (job, _) = asking(&mut uploads, &[], now);

    let effects = uploads.endpoint_lost(&ClientEndpointId::Local, 3);

    assert!(effects.contains(&UploadEffect::CloseConfirm { job }));
}

/// Sends every write of a `sending` job and answers them, leaving finish sent.
fn finishing(uploads: &mut Uploads, now: Instant) -> JobId {
    let job = sending(uploads, now);
    uploads.writes_done(job, now);
    job
}

fn finished(paths: &[&str]) -> String {
    success(
        "upload:1:finish",
        ResponseResult::PaneUploadFinished(PaneUploadFinished {
            location: location(),
            destination: "/home/u".into(),
            paths: paths.iter().map(|path| (*path).to_owned()).collect(),
            warnings: Vec::new(),
        }),
    )
}

#[test]
fn published_paths_with_control_characters_are_shown_instead_of_pasted() {
    for path in [
        "/home/u/x\u{1b}[201~rm -rf ~/a",
        "/home/u/a\nb",
        "/home/u/\u{9b}c",
    ] {
        let now = Instant::now();
        let mut uploads = Uploads::default();
        finishing(&mut uploads, now);

        let done = uploads.response(&ClientEndpointId::Local, 3, &finished(&[path]), now);

        assert!(
            !done
                .iter()
                .any(|effect| matches!(effect, UploadEffect::Paste { .. })),
            "{path:?}: {done:?}"
        );
        assert!(
            done.iter().any(|effect| matches!(
                effect,
                UploadEffect::Notice { body, .. } if body.contains("not pasted")
            )),
            "{path:?}: {done:?}"
        );
        assert!(!uploads.busy());
    }
}

fn beginning(uploads: &mut Uploads, now: Instant) -> JobId {
    let job = uploads.start(drop_request());
    uploads.checked(job, true);
    uploads.scanned(job, Ok(summary(2, 20)), &LIMITS, now);
    job
}

#[test]
fn a_begin_answered_after_cancelling_is_cancelled_on_the_server() {
    let now = Instant::now();
    let mut uploads = Uploads::default();
    let job = beginning(&mut uploads, now);
    assert!(uploads.progress().unwrap().cancellable);

    let cancelled = uploads.cancel(job);
    assert!(sent_methods(&cancelled).is_empty(), "no session id yet");
    assert!(!uploads.busy());
    let late = uploads.response(
        &ClientEndpointId::Local,
        3,
        &success("upload:1:begin", started(2, 512 * 1024)),
        now,
    );

    assert_eq!(sent_methods(&late), ["pane.upload.cancel"]);
    assert!(late.iter().any(|effect| matches!(
        effect,
        UploadEffect::Send { data, .. } if data.contains("\"up-1\"")
    )));
    // Answered once: a repeat is ignored.
    assert!(uploads
        .response(
            &ClientEndpointId::Local,
            3,
            &success("upload:1:begin", started(2, 512 * 1024)),
            now,
        )
        .is_empty());
}

#[test]
fn a_begin_answered_after_timing_out_is_cancelled_on_the_server() {
    let now = Instant::now();
    let mut uploads = Uploads::default();
    beginning(&mut uploads, now);

    uploads.tick(now + REQUEST_TIMEOUT);
    // A later drop runs meanwhile; the stale answer still finds its session.
    let next = uploads.start(drop_request());
    let late = uploads.response(
        &ClientEndpointId::Local,
        3,
        &success("upload:1:begin", started(2, 512 * 1024)),
        now,
    );

    assert_eq!(sent_methods(&late), ["pane.upload.cancel"]);
    assert!(uploads.busy(), "job {next} is untouched");
}

#[test]
fn a_late_begin_on_a_lost_connection_or_refused_needs_no_cancel() {
    let now = Instant::now();
    let mut uploads = Uploads::default();
    let job = beginning(&mut uploads, now);
    uploads.cancel(job);

    let refused = uploads.response(
        &ClientEndpointId::Local,
        3,
        &error("upload:1:begin", "not_found", "gone"),
        now,
    );
    assert!(sent_methods(&refused).is_empty());

    let job = uploads.start(drop_request());
    uploads.checked(job, true);
    uploads.scanned(job, Ok(summary(2, 20)), &LIMITS, now);
    uploads.cancel(job);
    uploads.endpoint_lost(&ClientEndpointId::Local, 3);
    let late = uploads.response(
        &ClientEndpointId::Local,
        3,
        &success("upload:2:begin", started(2, 512 * 1024)),
        now,
    );
    assert!(sent_methods(&late).is_empty());
}
