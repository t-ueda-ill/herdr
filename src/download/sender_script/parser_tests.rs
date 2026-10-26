use super::*;

const START: &[u8] = b"HERDR-OFFHOST-1\0";
const META: &str = "5 644 1700000000 -rw-r--r-- 1 0 0 5 Oct 23 12:00";

fn stream(parts: &[&[u8]]) -> Vec<u8> {
    let mut bytes = START.to_vec();
    bytes.extend_from_slice(b"est\x003\x0042\0");
    for part in parts {
        bytes.extend_from_slice(part);
    }
    bytes
}

fn file(rel: &str, meta: &str, body: &[u8], status: &str, meta2: &str) -> Vec<u8> {
    let mut bytes = format!("f\0{rel}\0{meta}\0").into_bytes();
    bytes.extend_from_slice(body);
    bytes.extend_from_slice(format!("e\0{status}\0{meta2}\0").as_bytes());
    bytes
}

fn parse(bytes: &[u8]) -> Result<Vec<Event>, Failure> {
    SenderParser::default().feed(bytes)
}

fn frames(events: Vec<Event>) -> Vec<Frame> {
    events
        .into_iter()
        .filter_map(|event| match event {
            Event::Frame(frame) => Some(frame),
            Event::Estimate { .. } => None,
        })
        .collect()
}

fn code(result: Result<Vec<Event>, Failure>) -> &'static str {
    result.unwrap_err().0
}

#[test]
fn the_estimate_comes_first_and_carries_both_numbers() {
    let events = parse(&stream(&[])).unwrap();

    assert_eq!(
        events,
        [Event::Estimate {
            entries: 3,
            bytes: 42
        }]
    );
}

#[test]
fn a_walk_word_ends_the_download_with_the_error_it_stands_for() {
    for (word, expected) in [
        ("missing\0/a\0", "not_found"),
        ("eacces\0/a\0", "permission_denied"),
        ("special\0/a\0", "invalid_params"),
        ("nozero\0", "download_failed"),
        ("changed\0", "download_source_changed"),
        ("big\0", "download_too_large"),
    ] {
        let mut bytes = START.to_vec();
        bytes.extend_from_slice(word.as_bytes());

        assert_eq!(code(parse(&bytes)), expected, "{word}");
    }
}

#[test]
fn a_path_in_a_walk_error_is_shown_without_control_characters() {
    let mut bytes = START.to_vec();
    bytes.extend_from_slice(b"missing\0/a\x1b[31m\0");

    let (_, message) = parse(&bytes).unwrap_err();

    assert!(!message.contains('\u{1b}'), "{message:?}");
    assert!(message.contains("\\u{1b}"), "{message:?}");
}

#[test]
fn words_out_of_place_fail_the_download() {
    let mut unterminated_file = stream(&[]);
    unterminated_file.extend_from_slice(format!("f\0a\0{META}\0hello").as_bytes());
    unterminated_file.extend_from_slice(b"end\0");
    let mut end_before_estimate = START.to_vec();
    end_before_estimate.extend_from_slice(b"end\0");
    for (label, bytes) in [
        ("stream word before the estimate", end_before_estimate),
        ("walk word after the estimate", stream(&[b"big\0"])),
        ("second estimate", stream(&[b"est\x001\x001\0"])),
        ("unknown word", stream(&[b"x\0a\0b\0"])),
        ("file without its trailer", unterminated_file),
        ("trailer without a file", stream(&[b"e\x000\0x\0"])),
        (
            "entry after the end",
            stream(&[b"end\0", b"d\0a\x000 755 - t\0"]),
        ),
    ] {
        assert_eq!(code(parse(&bytes)), "download_failed", "{label}");
    }
}

#[test]
fn entries_become_frames_with_their_attributes() {
    let events = parse(&stream(&[
        b"d\0sub\x000 755 1700000000 drwxr-xr-x 2 0 0 4096 Oct 23 12:00\0",
        b"l\0sub/ln\0../x\0",
        &file("sub/a", META, b"hello", "0", META),
        b"end\0",
    ]))
    .unwrap();

    let frames = frames(events);
    assert_eq!(
        frames[0],
        Frame::Entry {
            rel: b"sub".to_vec(),
            header: PaneUploadEntryHeader {
                kind: PaneUploadEntryKind::Directory,
                mode: 0o755,
                mtime: Some(1_700_000_000),
                size: None,
                link_target: None,
            },
            link_target: None,
        }
    );
    assert_eq!(
        frames[1],
        Frame::Entry {
            rel: b"sub/ln".to_vec(),
            header: PaneUploadEntryHeader {
                kind: PaneUploadEntryKind::Symlink,
                mode: 0o777,
                mtime: None,
                size: None,
                link_target: None,
            },
            link_target: Some(b"../x".to_vec()),
        }
    );
    assert_eq!(
        frames[2],
        Frame::Entry {
            rel: b"sub/a".to_vec(),
            header: PaneUploadEntryHeader {
                kind: PaneUploadEntryKind::File,
                mode: 0o644,
                mtime: Some(1_700_000_000),
                size: Some(5),
                link_target: None,
            },
            link_target: None,
        }
    );
    assert_eq!(frames[3], Frame::Data(b"hello".to_vec()));
    assert_eq!(frames[4], Frame::FileEnd(Ok(())));
    assert_eq!(frames[5], Frame::End);
}

#[test]
fn a_file_body_that_looks_like_tokens_is_data() {
    let body = b"e\x000\0end\0";
    let meta = format!("{} 600 - tok", body.len());

    let events = parse(&stream(&[&file("a", &meta, body, "0", &meta), b"end\0"])).unwrap();

    let frames = frames(events);
    assert_eq!(frames[1], Frame::Data(body.to_vec()));
    assert_eq!(frames[2], Frame::FileEnd(Ok(())));
    assert_eq!(frames[3], Frame::End);
}

#[test]
fn an_empty_file_has_no_data_frame() {
    let events = parse(&stream(&[&file("a", "0 600 - t", b"", "0", "0 600 - t")])).unwrap();

    let frames = frames(events);
    assert!(matches!(frames[0], Frame::Entry { .. }));
    assert_eq!(frames[1], Frame::FileEnd(Ok(())));
}

#[test]
fn input_cut_anywhere_gives_the_same_frames() {
    let whole = stream(&[
        &file("a", META, b"hello", "0", META),
        b"s\0b\0eacces\0",
        b"w\0top\0",
        b"end\0",
    ]);
    let expected = frames(parse(&whole).unwrap());

    for cut in 1..whole.len() {
        let mut parser = SenderParser::default();
        let mut events = parser.feed(&whole[..cut]).unwrap();
        events.extend(parser.feed(&whole[cut..]).unwrap());
        // Data may be split where the input was; join it back.
        let mut joined: Vec<Frame> = Vec::new();
        for frame in frames(events) {
            match (joined.last_mut(), frame) {
                (Some(Frame::Data(last)), Frame::Data(more)) => last.extend(more),
                (_, frame) => joined.push(frame),
            }
        }
        assert_eq!(joined, expected, "cut at {cut}");
    }
}

#[test]
fn a_trailer_with_a_nonzero_status_or_other_attributes_marks_the_file_changed() {
    for (status, meta2) in [("1", META), ("141", META), ("0", "5 644 1700000001 x")] {
        let events = parse(&stream(&[&file("a", META, b"hello", status, meta2)])).unwrap();

        let frames = frames(events);
        assert!(
            matches!(frames[2], Frame::FileEnd(Err(_))),
            "{status} {meta2}: {frames:?}"
        );
    }
}

#[test]
fn an_unknown_reason_for_leaving_an_entry_out_is_refused() {
    assert_eq!(code(parse(&stream(&[b"s\0a\0whim\0"]))), "download_failed");
    for reason in ["special", "eacces", "link"] {
        let bytes = stream(&[format!("s\0a\0{reason}\0").as_bytes()]);

        assert!(matches!(
            frames(parse(&bytes).unwrap())[0],
            Frame::Skipped { .. }
        ));
    }
}

#[test]
fn numbers_must_be_plain_digits() {
    for bad in ["+1", " 1", "", "18446744073709551616", "1.2K", "-1", "0x1"] {
        let mut bytes = START.to_vec();
        bytes.extend_from_slice(format!("est\0{bad}\x001\0").as_bytes());
        assert_eq!(code(parse(&bytes)), "download_failed", "est {bad:?}");

        let meta = format!("{bad} 644 - t");
        let bytes = stream(&[format!("d\0a\0{meta}\0").as_bytes()]);
        assert_eq!(code(parse(&bytes)), "download_failed", "size {bad:?}");

        let bytes = stream(&[&file("a", &meta, b"", "0", &meta)]);
        assert_eq!(code(parse(&bytes)), "download_failed", "body {bad:?}");

        let bytes = stream(&[&file("a", "0 644 - t", b"", bad, "0 644 - t")]);
        assert_eq!(code(parse(&bytes)), "download_failed", "status {bad:?}");
    }
}

#[test]
fn modes_and_times_must_be_well_formed_and_the_mode_is_masked() {
    for bad in ["8 ", "x ", "9999999", ""] {
        let bytes = stream(&[format!("d\0a\x000 {bad}- t\0").as_bytes()]);
        assert_eq!(code(parse(&bytes)), "download_failed", "mode {bad:?}");
    }
    let bytes = stream(&[b"d\0a\x000 4755 - t\0"]);
    match &frames(parse(&bytes).unwrap())[0] {
        Frame::Entry { header, .. } => assert_eq!(header.mode, 0o755),
        other => panic!("{other:?}"),
    }
    let bytes = stream(&[b"d\0a\x000 755 12x t\0"]);
    assert_eq!(code(parse(&bytes)), "download_failed");
    let bytes = stream(&[b"d\0a\x000 755 -\0"]);
    assert_eq!(code(parse(&bytes)), "download_failed");
}

#[test]
fn a_field_over_the_reader_limit_fails_the_download() {
    let mut bytes = stream(&[b"w\0"]);
    bytes.extend(std::iter::repeat_n(b'x', 70_000));

    assert_eq!(code(parse(&bytes)), "download_failed");
}

#[test]
fn a_parser_that_failed_stays_failed() {
    let mut parser = SenderParser::default();
    assert!(parser.feed(b"HERDR-OFFHOST-1\0bogus\0").is_err());

    assert!(parser.feed(b"est\x001\x001\0").is_err());
}

#[test]
fn ended_is_true_only_after_the_end_word() {
    let mut parser = SenderParser::default();
    parser.feed(&stream(&[])).unwrap();
    assert!(!parser.ended());

    parser.feed(b"end\0").unwrap();
    assert!(parser.ended());
}
