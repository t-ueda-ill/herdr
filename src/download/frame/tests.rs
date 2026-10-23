use base64::Engine as _;

use super::*;
use crate::download::{DOWNLOAD_CHUNK_BYTES, MAX_DOWNLOAD_REPLY_JSON};

fn names(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn small(chunk_bytes: usize) -> Limits {
    Limits::new(chunk_bytes)
}

fn packer(top: &[&str]) -> Packer {
    Packer::new(names(top), Limits::new(512 * 1024))
}

fn dir(rel: &str) -> Frame {
    Frame::Entry {
        rel: rel.as_bytes().to_vec(),
        header: PaneUploadEntryHeader {
            kind: PaneUploadEntryKind::Directory,
            mode: 0o755,
            mtime: Some(7),
            size: None,
            link_target: None,
        },
        link_target: None,
    }
}

fn file(rel: &str, size: u64) -> Frame {
    Frame::Entry {
        rel: rel.as_bytes().to_vec(),
        header: PaneUploadEntryHeader {
            kind: PaneUploadEntryKind::File,
            mode: 0o644,
            mtime: None,
            size: Some(size),
            link_target: None,
        },
        link_target: None,
    }
}

fn link(rel: &str, target: &[u8]) -> Frame {
    Frame::Entry {
        rel: rel.as_bytes().to_vec(),
        header: PaneUploadEntryHeader {
            kind: PaneUploadEntryKind::Symlink,
            mode: 0o777,
            mtime: None,
            size: None,
            link_target: None,
        },
        link_target: Some(target.to_vec()),
    }
}

fn data(bytes: &[u8]) -> Frame {
    Frame::Data(bytes.to_vec())
}

fn skipped(rel: &str, reason: SkipReason) -> Frame {
    Frame::Skipped {
        rel: rel.as_bytes().to_vec(),
        reason,
    }
}

fn file_end() -> Frame {
    Frame::FileEnd(Ok(()))
}

/// Answers until one is `done`, an error comes, or `limit` answers passed.
fn answers(packer: &mut Packer, frames: Vec<Frame>) -> Result<Vec<PaneDownloadChunk>, Failure> {
    let mut frames = frames.into_iter();
    let mut out = Vec::new();
    for seq in 0..1000 {
        let chunk = packer.fill(seq, &mut || Ok(frames.next()))?;
        let done = chunk.done;
        out.push(chunk);
        if done {
            return Ok(out);
        }
    }
    panic!("never finished");
}

fn decoded(record: &PaneDownloadRecord) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(&record.data)
        .unwrap()
}

fn all_records(chunks: &[PaneDownloadChunk]) -> Vec<&PaneDownloadRecord> {
    chunks.iter().flat_map(|chunk| &chunk.records).collect()
}

fn code(result: Result<Vec<PaneDownloadChunk>, Failure>) -> &'static str {
    result.unwrap_err().0
}

fn warnings(chunks: &[PaneDownloadChunk]) -> Vec<String> {
    chunks
        .iter()
        .flat_map(|chunk| chunk.warnings.clone())
        .collect()
}

fn paths(chunks: &[PaneDownloadChunk]) -> Vec<&str> {
    all_records(chunks)
        .into_iter()
        .map(|record| record.path.as_str())
        .collect()
}

#[test]
fn a_file_larger_than_the_chunk_is_split_with_continuous_offsets_and_one_header() {
    let mut packer = Packer::new(names(&["f"]), small(4));

    let chunks = answers(
        &mut packer,
        vec![file("f", 10), data(b"0123456789"), file_end(), Frame::End],
    )
    .unwrap();

    let records = all_records(&chunks);
    assert_eq!(
        records.iter().map(|r| decoded(r)).collect::<Vec<_>>(),
        [b"0123".to_vec(), b"4567".to_vec(), b"89".to_vec()]
    );
    assert_eq!(
        records.iter().map(|r| r.offset).collect::<Vec<_>>(),
        [0, 4, 8]
    );
    assert_eq!(
        records.iter().map(|r| r.last).collect::<Vec<_>>(),
        [false, false, true]
    );
    assert_eq!(
        records
            .iter()
            .map(|r| r.entry.is_some())
            .collect::<Vec<_>>(),
        [true, false, false]
    );
    assert!(chunks
        .iter()
        .all(|chunk| chunk.records.iter().map(|r| r.data.len()).sum::<usize>() <= 8));
}

#[test]
fn consecutive_data_frames_of_one_answer_become_one_record() {
    let mut packer = packer(&["f"]);

    let chunks = answers(
        &mut packer,
        vec![
            file("f", 6),
            data(b"ab"),
            data(b"cd"),
            data(b"ef"),
            file_end(),
            Frame::End,
        ],
    )
    .unwrap();

    let records = all_records(&chunks);
    assert_eq!(records.len(), 1);
    assert_eq!(decoded(records[0]), b"abcdef");
    assert!(records[0].last);
    assert_eq!(records[0].entry.as_ref().unwrap().size, Some(6));
}

#[test]
fn an_empty_file_is_one_record_with_header_and_last() {
    let mut packer = packer(&["e"]);

    let chunks = answers(&mut packer, vec![file("e", 0), file_end(), Frame::End]).unwrap();

    let records = all_records(&chunks);
    assert_eq!(records.len(), 1);
    assert!(records[0].last && records[0].entry.is_some() && records[0].data.is_empty());
}

#[test]
fn directories_and_links_are_single_final_records_with_clean_headers() {
    let mut packer = packer(&["d"]);

    let chunks = answers(
        &mut packer,
        vec![dir("d"), link("d/l", b"../x"), Frame::End],
    )
    .unwrap();

    let records = all_records(&chunks);
    assert!(records.iter().all(|r| r.last && r.offset == 0));
    let entry = records[1].entry.as_ref().unwrap();
    assert_eq!(entry.kind, PaneUploadEntryKind::Symlink);
    assert_eq!(entry.link_target.as_deref(), Some("../x"));
    assert_eq!(records[0].entry.as_ref().unwrap().link_target, None);
}

#[test]
fn modes_are_reduced_to_permission_bits() {
    let mut packer = packer(&["d"]);
    let mut sticky = dir("d");
    if let Frame::Entry { header, .. } = &mut sticky {
        header.mode = 0o1777;
    }

    let chunks = answers(&mut packer, vec![sticky, Frame::End]).unwrap();

    assert_eq!(all_records(&chunks)[0].entry.as_ref().unwrap().mode, 0o777);
}

#[test]
fn the_record_count_closes_an_answer() {
    let mut limits = small(1024);
    limits.max_records = 3;
    let mut packer = Packer::new(names(&["d"]), limits);
    let mut frames = vec![dir("d")];
    frames.extend((0..6).map(|i| dir(&format!("d/{i}"))));
    frames.push(Frame::End);

    let chunks = answers(&mut packer, frames).unwrap();

    assert_eq!(
        chunks.iter().map(|c| c.records.len()).collect::<Vec<_>>(),
        [3, 3, 1, 0]
    );
}

#[test]
fn the_name_budget_closes_an_answer_after_the_record_that_reaches_it() {
    let mut limits = small(1024);
    limits.max_meta_bytes = 15;
    let mut packer = Packer::new(names(&["top"]), limits);
    let frames = vec![
        dir("top"),
        dir("top/aaaaaaaa"),
        dir("top/bbbbbbbb"),
        dir("top/cccccccc"),
        Frame::End,
    ];

    let chunks = answers(&mut packer, frames).unwrap();

    assert_eq!(
        chunks.iter().map(|c| c.records.len()).collect::<Vec<_>>(),
        [2, 2, 0]
    );
}

#[test]
fn the_data_budget_counts_all_files_in_an_answer() {
    let mut packer = Packer::new(names(&["a", "b"]), small(5));

    let chunks = answers(
        &mut packer,
        vec![
            file("a", 3),
            data(b"aaa"),
            file_end(),
            file("b", 3),
            data(b"bbb"),
            file_end(),
            Frame::End,
        ],
    )
    .unwrap();

    assert!(chunks.iter().all(|chunk| {
        chunk
            .records
            .iter()
            .map(|r| decoded(r).len())
            .sum::<usize>()
            <= 5
    }));
    let joined: Vec<u8> = all_records(&chunks)
        .iter()
        .flat_map(|r| decoded(r))
        .collect();
    assert_eq!(joined, b"aaabbb");
}

#[test]
fn nothing_ready_makes_an_empty_answer_that_is_not_done() {
    let mut packer = packer(&["f"]);

    let chunk = packer.fill(0, &mut || Ok(None)).unwrap();

    assert!(chunk.records.is_empty() && !chunk.done && chunk.warnings.is_empty());
    assert_eq!(chunk.seq, 0);
}

#[test]
fn done_comes_in_an_answer_after_the_records_and_carries_the_warnings() {
    let mut packer = packer(&["d"]);

    let chunks = answers(
        &mut packer,
        vec![
            dir("d"),
            Frame::WalkWarning("cannot read d/x".into()),
            Frame::End,
        ],
    )
    .unwrap();

    assert_eq!(chunks.len(), 2);
    assert!(!chunks[0].done && chunks[0].warnings.is_empty() && chunks[0].records.len() == 1);
    assert!(chunks[1].done && chunks[1].records.is_empty());
    assert_eq!(chunks[1].warnings, ["cannot read d/x"]);
}

#[test]
fn an_end_with_nothing_gathered_is_done_at_once_and_a_later_read_is_refused() {
    let mut packer = packer(&["d"]);
    let first = packer
        .fill(0, &mut || Ok(Some(dir("d"))))
        .unwrap_or_else(|_| unreachable!());
    assert!(!first.done);
    let mut frames = vec![Frame::End].into_iter();

    let last = packer.fill(1, &mut || Ok(frames.next())).unwrap();

    assert!(last.done);
    assert_eq!(
        packer.fill(2, &mut || Ok(None)).unwrap_err().0,
        "download_not_found"
    );
}

#[test]
fn counters_are_cumulative_over_answers() {
    let mut packer = Packer::new(names(&["f", "g"]), small(2));

    let chunks = answers(
        &mut packer,
        vec![
            file("f", 3),
            data(b"abc"),
            file_end(),
            file("g", 1),
            data(b"z"),
            file_end(),
            Frame::End,
        ],
    )
    .unwrap();

    let last = chunks.last().unwrap();
    assert_eq!((last.sent_entries, last.sent_bytes), (2, 4));
    assert!(chunks
        .windows(2)
        .all(|pair| pair[0].sent_bytes <= pair[1].sent_bytes));
}

#[test]
fn warnings_are_cut_at_the_budget_with_a_count_of_the_rest() {
    let mut packer = packer(&["d"]);
    let mut frames = vec![dir("d")];
    frames.extend((0..400).map(|i| Frame::WalkWarning(format!("{i}:{}", "w".repeat(200)))));
    frames.push(Frame::End);

    let chunks = answers(&mut packer, frames).unwrap();

    let all = warnings(&chunks);
    let total: usize = all.iter().map(String::len).sum();
    assert!(total <= MAX_WARNING_BYTES, "{total}");
    let more = all.last().unwrap();
    assert!(
        more.starts_with("and ") && more.ends_with(" more"),
        "{more}"
    );
    let counted: usize = more[4..more.len() - 5].parse().unwrap();
    assert_eq!(counted + all.len() - 1, 400);
}

#[test]
fn warnings_escape_control_characters_and_are_cut_to_one_line_budget() {
    let mut packer = packer(&["d"]);

    let chunks = answers(
        &mut packer,
        vec![
            dir("d"),
            Frame::WalkWarning("\u{1b}]0;title\u{7}".into()),
            Frame::WalkWarning("x".repeat(5000)),
            Frame::End,
        ],
    )
    .unwrap();

    let all = warnings(&chunks);
    assert_eq!(all[0], "\\u{1b}]0;title\\u{7}");
    assert_eq!(all[1].len(), MAX_WARNING_LINE_BYTES);
}

#[test]
fn a_file_that_changed_fails_with_source_changed_even_after_its_last_record() {
    let mut packer = packer(&["f"]);

    let result = answers(
        &mut packer,
        vec![
            file("f", 2),
            data(b"ab"),
            Frame::FileEnd(Err(Changed("its modification time changed".into()))),
            Frame::End,
        ],
    );

    let failure = result.unwrap_err();
    assert_eq!(failure.0, "download_source_changed");
    assert!(failure.1.contains('f') && failure.1.contains("modification time"));
}

#[test]
fn a_file_that_ends_short_or_runs_long_fails_with_source_changed() {
    let short = answers(
        &mut packer(&["f"]),
        vec![file("f", 4), data(b"ab"), file_end(), Frame::End],
    );
    let long = answers(
        &mut packer(&["f"]),
        vec![file("f", 1), data(b"ab"), file_end(), Frame::End],
    );

    assert_eq!(code(short), "download_source_changed");
    assert_eq!(code(long), "download_source_changed");
}

#[test]
fn skipped_names_drop_their_descendants_data_and_trailers_silently() {
    let long = format!("d/{}", "x/".repeat(2100));
    let bad = |rel: &[u8], kind| Frame::Entry {
        rel: rel.to_vec(),
        header: PaneUploadEntryHeader {
            kind,
            mode: 0o755,
            mtime: None,
            size: (kind == PaneUploadEntryKind::File).then_some(1),
            link_target: None,
        },
        link_target: None,
    };
    let mut packer = packer(&["d"]);
    let frames = vec![
        dir("d"),
        bad(b"d/\xff", PaneUploadEntryKind::Directory),
        bad(b"d/\xff/under", PaneUploadEntryKind::File),
        data(b"x"),
        Frame::FileEnd(Err(Changed("ignored".into()))),
        bad(b"d/with\nnewline", PaneUploadEntryKind::File),
        data(b"x"),
        file_end(),
        bad(b"d/a/../b", PaneUploadEntryKind::Directory),
        bad(long.as_bytes(), PaneUploadEntryKind::Directory),
        file("d/ok", 1),
        data(b"k"),
        file_end(),
        Frame::End,
    ];

    let chunks = answers(&mut packer, frames).unwrap();

    assert_eq!(paths(&chunks), ["d", "d/ok"]);
    let all = warnings(&chunks);
    assert_eq!(all.len(), 4, "one warning per skipped root: {all:?}");
    assert!(all.iter().all(|line| line.starts_with("skipped ")));
    assert!(all.iter().any(|line| line.contains("\\u{a}")));
}

#[test]
fn a_skipped_folder_drops_what_is_below_it_but_not_a_name_that_merely_starts_with_it() {
    let mut packer = packer(&["d"]);

    let chunks = answers(
        &mut packer,
        vec![
            dir("d"),
            skipped("d/a", SkipReason::PermissionDenied),
            dir("d/a/x"),
            file("d/a/x/f", 1),
            data(b"1"),
            file_end(),
            dir("d/ab"),
            file("d/ab/y", 1),
            data(b"2"),
            file_end(),
            Frame::End,
        ],
    )
    .unwrap();

    assert_eq!(paths(&chunks), ["d", "d/ab", "d/ab/y"]);
    assert_eq!(warnings(&chunks), ["skipped d/a: permission denied"]);
}

#[test]
fn a_link_target_that_is_not_text_or_is_empty_skips_the_link() {
    let mut packer = packer(&["d"]);

    let chunks = answers(
        &mut packer,
        vec![
            dir("d"),
            link("d/bytes", b"\xff\xfe"),
            link("d/empty", b""),
            link("d/newline", b"a\nb"),
            link("d/fine", b"t"),
            Frame::End,
        ],
    )
    .unwrap();

    assert_eq!(paths(&chunks), ["d", "d/fine"]);
    assert_eq!(warnings(&chunks).len(), 3);
}

#[test]
fn a_path_outside_the_requested_names_is_a_failure_not_a_skip() {
    for rel in ["other", "other/x", "dd"] {
        let result = answers(&mut packer(&["d"]), vec![dir("d"), dir(rel), Frame::End]);
        assert_eq!(code(result), "download_failed", "{rel}");
    }
    let skipped_outside = answers(
        &mut packer(&["d"]),
        vec![skipped("zz", SkipReason::Special), Frame::End],
    );
    assert_eq!(code(skipped_outside), "download_failed");
}

#[test]
fn a_child_before_its_parent_is_a_failure() {
    let result = answers(
        &mut packer(&["d"]),
        vec![dir("d"), dir("d/a/b"), Frame::End],
    );

    assert_eq!(code(result), "download_failed");
}

#[test]
fn structure_violations_by_the_source_are_failures() {
    let cases: Vec<Vec<Frame>> = vec![
        vec![data(b"x")],
        vec![file_end()],
        vec![file("f", 1), Frame::End],
        vec![file("f", 1), dir("f/x")],
        vec![file("f", 1), skipped("f/x", SkipReason::Link)],
    ];
    for frames in cases {
        let result = answers(&mut packer(&["f"]), frames);
        assert_eq!(code(result), "download_failed");
    }
    let mut no_size = file("f", 1);
    if let Frame::Entry { header, .. } = &mut no_size {
        header.size = None;
    }
    assert_eq!(
        code(answers(&mut packer(&["f"]), vec![no_size])),
        "download_failed"
    );
}

#[test]
fn a_top_level_name_that_is_skipped_or_never_arrives_fails_with_source_changed() {
    let skipped_top = answers(
        &mut packer(&["a"]),
        vec![skipped("a", SkipReason::Special), Frame::End],
    );
    let missing = answers(&mut packer(&["a", "b"]), vec![dir("a"), Frame::End]);
    let unusable_top = answers(
        &mut packer(&["a"]),
        vec![
            Frame::Entry {
                rel: b"a".to_vec(),
                header: PaneUploadEntryHeader {
                    kind: PaneUploadEntryKind::Symlink,
                    mode: 0o777,
                    mtime: None,
                    size: None,
                    link_target: None,
                },
                link_target: Some(b"\xff".to_vec()),
            },
            Frame::End,
        ],
    );

    assert_eq!(code(skipped_top), "download_source_changed");
    let failure = missing.unwrap_err();
    assert_eq!(failure.0, "download_source_changed");
    assert!(failure.1.contains('b'));
    assert_eq!(code(unusable_top), "download_source_changed");
}

#[test]
fn too_many_entries_or_bytes_fail_with_download_too_large() {
    let mut limits = small(1024);
    limits.max_entries = 2;
    let entries = answers(
        &mut Packer::new(names(&["d"]), limits),
        vec![dir("d"), dir("d/a"), dir("d/b"), Frame::End],
    );
    let mut limits = small(1024);
    limits.max_bytes = 10;
    let bytes = answers(
        &mut Packer::new(names(&["a", "b"]), limits),
        vec![file("a", 6), data(b"123456"), file_end(), file("b", 5)],
    );

    assert_eq!(code(entries), "download_too_large");
    assert_eq!(code(bytes), "download_too_large");
}

#[test]
fn skipped_roots_count_toward_the_entry_limit_but_what_is_dropped_below_them_does_not() {
    let mut limits = small(1024);
    limits.max_entries = 4;
    let mut within = Packer::new(names(&["d"]), limits);
    let mut frames = vec![dir("d"), skipped("d/s", SkipReason::PermissionDenied)];
    frames.extend((0..50).map(|i| dir(&format!("d/s/{i}"))));
    frames.extend([skipped("d/t", SkipReason::Special), Frame::End]);
    let mut flood = Packer::new(names(&["d"]), limits);
    let mut endless = std::iter::once(dir("d"))
        .chain((0..).map(|i| skipped(&format!("d/s{i}"), SkipReason::Special)));

    let kept = answers(&mut within, frames);
    let flooded = flood.fill(0, &mut || Ok(endless.next()));

    assert!(kept.is_ok(), "{kept:?}");
    assert_eq!(flooded.unwrap_err().0, "download_too_large");
}

#[test]
fn an_overlong_skipped_path_is_not_remembered_and_its_descendants_are_still_dropped() {
    let long = format!("d/{}", "x".repeat(250) + &"/x".repeat(1950));
    let mut packer = packer(&["d"]);
    let frames = vec![
        dir("d"),
        dir(&long),
        dir(&format!("{long}/below")),
        file("d/ok", 1),
        data(b"k"),
        file_end(),
        Frame::End,
    ];

    let chunks = answers(&mut packer, frames).unwrap();

    assert_eq!(paths(&chunks), ["d", "d/ok"]);
    assert!(packer.skipped.is_empty());
}

#[test]
fn file_contents_survive_the_trip_through_records_byte_for_byte() {
    let content: Vec<u8> = (0..=255u8).cycle().take(10_000).collect();
    let mut packer = Packer::new(names(&["f"]), small(777));
    let mut frames = vec![file("f", content.len() as u64)];
    frames.extend(content.chunks(300).map(data));
    frames.extend([file_end(), Frame::End]);

    let chunks = answers(&mut packer, frames).unwrap();

    let joined: Vec<u8> = all_records(&chunks)
        .iter()
        .flat_map(|r| decoded(r))
        .collect();
    assert_eq!(joined, content);
    let records = all_records(&chunks);
    let mut expected = 0;
    for record in &records {
        assert_eq!(record.offset, expected);
        expected += decoded(record).len() as u64;
    }
    assert!(records.last().unwrap().last);
    assert!(records[..records.len() - 1].iter().all(|r| !r.last));
}

#[test]
fn the_worst_answer_the_budgets_allow_fits_the_reply_cap() {
    // Names of `"` only (JSON doubles each), filling the name budget, and a
    // full chunk of data of the same answer.
    let mut packer = Packer::new(names(&["t"]), Limits::new(DOWNLOAD_CHUNK_BYTES));
    let mut frames = vec![dir("t")];
    frames.extend((0..2000).map(|i| dir(&format!("t/{}", "\"".repeat(124 - i % 2)))));
    frames.push(file("t/big", DOWNLOAD_CHUNK_BYTES as u64));
    frames.push(data(&vec![0xffu8; DOWNLOAD_CHUNK_BYTES]));
    frames.push(file_end());
    frames.push(Frame::End);
    let mut frames = frames.into_iter();

    let chunk = packer.fill(0, &mut || Ok(frames.next())).unwrap();

    assert_eq!(chunk.records.len(), 2002);
    let json = serde_json::to_string(&chunk).unwrap();
    assert!(
        json.len() > 1_300_000,
        "the case must be a heavy one: {}",
        json.len()
    );
    assert!(json.len() <= MAX_DOWNLOAD_REPLY_JSON, "{}", json.len());
}

#[test]
fn the_done_answer_with_the_most_warnings_fits_the_reply_cap() {
    let mut packer = packer(&["t"]);
    let mut frames = vec![dir("t")];
    frames.extend((0..2000).map(|_| Frame::WalkWarning("\u{1}".repeat(100))));
    frames.push(Frame::End);

    let chunks = answers(&mut packer, frames).unwrap();

    let json = serde_json::to_string(chunks.last().unwrap()).unwrap();
    assert!(json.len() <= MAX_DOWNLOAD_REPLY_JSON, "{}", json.len());
    assert!(chunks.last().unwrap().done);
}
