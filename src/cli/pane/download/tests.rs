use std::collections::VecDeque;
use std::path::Path;

use serde_json::json;

use super::*;
use crate::api::schema::{PaneDownloadRecord, PaneUploadEntryHeader, PaneUploadEntryKind};
use crate::upload::test_support::TempDir;

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn parse_keeps_pane_paths_as_given_and_defaults_to_the_current_directory_and_rename() {
    let parsed = parse(
        &args(&["w1-p1", "/etc/hosts", "/var/log"]),
        Path::new("/work"),
    )
    .unwrap();

    assert_eq!(
        parsed,
        DownloadArgs {
            pane_id: "w1-p1".into(),
            paths: vec!["/etc/hosts".into(), "/var/log".into()],
            destination: PathBuf::from("/work"),
            collision: PaneUploadCollision::Rename,
            json: false,
        }
    );
}

#[test]
fn parse_resolves_a_relative_destination_from_the_current_directory() {
    let parsed = parse(
        &args(&[
            "p",
            "/x",
            "--dest",
            "out",
            "--on-conflict",
            "fail",
            "--json",
        ]),
        Path::new("/work"),
    )
    .unwrap();

    assert_eq!(parsed.destination, PathBuf::from("/work/out"));
    assert_eq!(parsed.collision, PaneUploadCollision::Fail);
    assert!(parsed.json);
    let absolute = parse(&args(&["p", "/x", "--dest", "/srv/in"]), Path::new("/work")).unwrap();
    assert_eq!(absolute.destination, PathBuf::from("/srv/in"));
}

#[test]
fn parse_rejects_a_missing_path_or_an_unknown_policy() {
    assert!(parse(&args(&["p"]), Path::new("/")).is_err());
    assert!(parse(
        &args(&["p", "/x", "--on-conflict", "merge"]),
        Path::new("/")
    )
    .is_err());
}

#[test]
fn the_command_is_a_valid_clap_grammar() {
    command().debug_assert();
}

/// Answers each request with the next scripted response and records them.
struct Scripted {
    answers: VecDeque<serde_json::Value>,
    sent: Vec<(Request, bool)>,
}

impl Scripted {
    fn new(answers: Vec<serde_json::Value>) -> Self {
        Self {
            answers: answers.into(),
            sent: Vec::new(),
        }
    }

    fn methods(&self) -> Vec<&'static str> {
        self.sent
            .iter()
            .map(|(request, _)| request.method.operation().name)
            .collect()
    }
}

impl Server for Scripted {
    fn request(&mut self, request: &Request, first: bool) -> std::io::Result<serde_json::Value> {
        self.sent.push((request.clone(), first));
        Ok(self
            .answers
            .pop_front()
            .unwrap_or_else(|| json!({"id": request.id, "result": {"type": "ok"}})))
    }
}

fn started(names: &[&str]) -> serde_json::Value {
    json!({"id": "b", "result": {
        "type": "pane_download_started", "download_id": "dl-1",
        "location": {"kind": "container", "container": "c"},
        "names": names, "entries": 2, "bytes": 6, "chunk_bytes": 524288, "window": 2
    }})
}

fn chunk(
    seq: u64,
    records: Vec<PaneDownloadRecord>,
    done: bool,
    warnings: &[&str],
) -> serde_json::Value {
    json!({"id": "r", "result": {
        "type": "pane_download_chunk", "seq": seq,
        "records": records, "done": done,
        "sent_entries": 0, "sent_bytes": 0, "warnings": warnings
    }})
}

fn error(code: &str) -> serde_json::Value {
    json!({"id": "x", "error": {"code": code, "message": "nope"}})
}

fn header(kind: PaneUploadEntryKind, size: Option<u64>) -> Option<PaneUploadEntryHeader> {
    Some(PaneUploadEntryHeader {
        kind,
        mode: if size.is_some() { 0o644 } else { 0o755 },
        mtime: None,
        size,
        link_target: None,
    })
}

fn directory(path: &str) -> PaneDownloadRecord {
    PaneDownloadRecord {
        path: path.into(),
        entry: header(PaneUploadEntryKind::Directory, None),
        offset: 0,
        data: String::new(),
        last: true,
    }
}

/// `abc` at offset 0, then `def`: a 6 byte file in two records.
fn file_records(path: &str) -> (PaneDownloadRecord, PaneDownloadRecord) {
    (
        PaneDownloadRecord {
            path: path.into(),
            entry: header(PaneUploadEntryKind::File, Some(6)),
            offset: 0,
            data: "YWJj".into(),
            last: false,
        },
        PaneDownloadRecord {
            path: path.into(),
            entry: None,
            offset: 3,
            data: "ZGVm".into(),
            last: true,
        },
    )
}

fn call(dest: &TempDir, collision: PaneUploadCollision) -> DownloadArgs {
    DownloadArgs {
        pane_id: "p".into(),
        paths: vec!["/srv/t".into()],
        destination: dest.path().to_path_buf(),
        collision,
        json: false,
    }
}

fn run_scripted(server: &mut Scripted, args: &DownloadArgs, interrupted: bool) -> Outcome {
    let flag = AtomicBool::new(interrupted);
    download(server, args, &flag).unwrap()
}

fn leftovers(dest: &TempDir) -> Vec<String> {
    std::fs::read_dir(dest.path())
        .unwrap()
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.starts_with(crate::upload::STAGE_PREFIX))
        .collect()
}

fn happy_script() -> Vec<serde_json::Value> {
    let (head, tail) = file_records("t/f");
    vec![
        started(&["t"]),
        chunk(0, vec![directory("t"), head], false, &[]),
        chunk(1, Vec::new(), false, &[]),
        chunk(2, vec![tail], false, &[]),
        chunk(3, Vec::new(), true, &["skipped t/x: special file"]),
    ]
}

#[test]
fn reads_go_one_at_a_time_in_order_and_the_tree_is_published_at_the_end() {
    let dest = TempDir::new("cli-dl-happy");
    let mut server = Scripted::new(happy_script());

    let outcome = run_scripted(
        &mut server,
        &call(&dest, PaneUploadCollision::Rename),
        false,
    );

    assert_eq!(exit_code(&outcome), 0);
    assert_eq!(
        server.methods(),
        [
            "pane.download.begin",
            "pane.download.read",
            "pane.download.read",
            "pane.download.read",
            "pane.download.read",
        ]
    );
    let seqs: Vec<u64> = server.sent[1..]
        .iter()
        .map(|(request, _)| match &request.method {
            Method::PaneDownloadRead(read) => read.seq,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(seqs, [0, 1, 2, 3]);
    assert!(server.sent[0].1 && server.sent[1..].iter().all(|(_, first)| !first));
    let Method::PaneDownloadBegin(begin) = &server.sent[0].0.method else {
        panic!()
    };
    assert_eq!(begin.paths, ["/srv/t"]);
    assert_eq!(begin.pane_id.as_deref(), Some("p"));
    assert_eq!(
        begin.max_bytes,
        Some(crate::download::DOWNLOAD_CHUNK_BYTES as u64)
    );
    assert_eq!(std::fs::read(dest.join("t/f")).unwrap(), b"abcdef");
    let Outcome::Finished {
        paths, warnings, ..
    } = outcome
    else {
        panic!()
    };
    assert_eq!(paths, [dest.join("t").to_str().unwrap()]);
    assert_eq!(warnings, ["skipped t/x: special file"]);
    assert!(leftovers(&dest).is_empty());
}

#[test]
fn a_second_download_of_the_same_name_is_numbered_by_default() {
    let dest = TempDir::new("cli-dl-twice");
    for _ in 0..2 {
        let mut server = Scripted::new(happy_script());
        run_scripted(
            &mut server,
            &call(&dest, PaneUploadCollision::Rename),
            false,
        );
    }

    assert!(dest.join("t/f").exists() && dest.join("t (1)/f").exists());
}

#[test]
fn a_conflict_under_fail_exits_3_publishes_nothing_and_leaves_no_stage() {
    let dest = TempDir::new("cli-dl-conflict");
    std::fs::create_dir(dest.join("t")).unwrap();
    let mut server = Scripted::new(happy_script());

    let outcome = run_scripted(&mut server, &call(&dest, PaneUploadCollision::Fail), false);

    assert_eq!(outcome, Outcome::Conflict(vec!["t".into()]));
    assert_eq!(exit_code(&outcome), CONFLICT_EXIT);
    assert!(!dest.join("t/f").exists());
    assert!(leftovers(&dest).is_empty());
    assert!(
        !server.methods().contains(&"pane.download.cancel"),
        "the server's session ended with `done`"
    );
}

#[test]
fn an_interrupt_cancels_before_the_next_read_and_removes_the_stage() {
    let dest = TempDir::new("cli-dl-interrupt");
    let mut server = Scripted::new(vec![started(&["t"])]);

    let outcome = run_scripted(&mut server, &call(&dest, PaneUploadCollision::Rename), true);

    assert_eq!(outcome, Outcome::Interrupted);
    assert_eq!(exit_code(&outcome), 130);
    assert_eq!(
        server.methods(),
        ["pane.download.begin", "pane.download.cancel"]
    );
    assert!(leftovers(&dest).is_empty());
}

#[test]
fn an_error_that_ends_the_session_is_not_followed_by_a_cancel() {
    let dest = TempDir::new("cli-dl-ended");
    let (head, _) = file_records("t/f");
    let mut server = Scripted::new(vec![
        started(&["t"]),
        chunk(0, vec![directory("t"), head], false, &[]),
        error("download_source_changed"),
    ]);

    let outcome = run_scripted(
        &mut server,
        &call(&dest, PaneUploadCollision::Rename),
        false,
    );

    assert_eq!(
        outcome,
        Outcome::Failed("download_source_changed".into(), "nope".into())
    );
    assert_eq!(
        server.methods(),
        [
            "pane.download.begin",
            "pane.download.read",
            "pane.download.read"
        ]
    );
    assert!(leftovers(&dest).is_empty());
    assert!(!dest.join("t").exists());
}

#[test]
fn an_error_that_leaves_the_session_open_is_followed_by_a_cancel() {
    for code in ["download_busy", "invalid_params"] {
        let dest = TempDir::new("cli-dl-open");
        let mut server = Scripted::new(vec![started(&["t"]), error(code)]);

        let outcome = run_scripted(
            &mut server,
            &call(&dest, PaneUploadCollision::Rename),
            false,
        );

        assert_eq!(exit_code(&outcome), 1);
        assert_eq!(
            server.methods().last(),
            Some(&"pane.download.cancel"),
            "{code}"
        );
        assert!(leftovers(&dest).is_empty());
    }
}

#[test]
fn a_record_that_breaks_the_order_cancels_and_removes_the_stage() {
    let dest = TempDir::new("cli-dl-broken");
    let (head, tail) = file_records("t/f");
    let mut server = Scripted::new(vec![
        started(&["t"]),
        // The file arrives before its parent directory.
        chunk(0, vec![head, tail], false, &[]),
    ]);

    let outcome = run_scripted(
        &mut server,
        &call(&dest, PaneUploadCollision::Rename),
        false,
    );

    let Outcome::Failed(code, message) = &outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(code, "download_failed");
    assert!(!message.to_lowercase().contains("upload"), "{message}");
    assert_eq!(server.methods().last(), Some(&"pane.download.cancel"));
    assert!(leftovers(&dest).is_empty());
}

#[test]
fn a_refused_begin_sends_nothing_else() {
    let dest = TempDir::new("cli-dl-refused");
    let mut server = Scripted::new(vec![error("not_found")]);

    let outcome = run_scripted(
        &mut server,
        &call(&dest, PaneUploadCollision::Rename),
        false,
    );

    assert_eq!(outcome, Outcome::Failed("not_found".into(), "nope".into()));
    assert_eq!(server.methods(), ["pane.download.begin"]);
}

#[test]
fn a_bad_selection_or_destination_fails_before_contacting_the_server() {
    let dest = TempDir::new("cli-dl-local");
    let mut same_name = call(&dest, PaneUploadCollision::Rename);
    same_name.paths = vec!["/a/Makefile".into(), "/b/makefile".into()];
    let mut missing = call(&dest, PaneUploadCollision::Rename);
    missing.destination = dest.join("nope");
    let mut file = call(&dest, PaneUploadCollision::Rename);
    std::fs::write(dest.join("plain"), b"x").unwrap();
    file.destination = dest.join("plain");

    for args in [same_name, missing, file] {
        let mut server = Scripted::new(Vec::new());

        let outcome = run_scripted(&mut server, &args, false);

        assert!(matches!(outcome, Outcome::Local(_)), "{outcome:?}");
        assert!(server.sent.is_empty());
    }
}

#[test]
fn what_is_printed_never_carries_a_control_character_and_json_has_the_documented_keys() {
    let finished = Outcome::Finished {
        destination: "/dl".into(),
        paths: vec!["/dl/a\u{1b}[31mb".into()],
        warnings: vec!["skipped \u{1b}]0;x\u{7}".into()],
    };

    let plain = render(&finished, false);
    let json = render(&finished, true);

    assert_eq!(plain.stdout, "/dl/a\\u{1b}[31mb\n");
    assert!(plain
        .stderr
        .starts_with("warning: skipped \\u{1b}]0;x\\u{7}"));
    let value: serde_json::Value = serde_json::from_str(json.stdout.trim()).unwrap();
    assert_eq!(value["destination"], "/dl");
    assert_eq!(value["paths"][0], "/dl/a\u{1b}[31mb");
    assert!(value["warnings"].is_array());
    assert!(![&plain.stdout, &plain.stderr, &json.stderr]
        .iter()
        .any(|text| text.contains('\u{1b}')));
}

/// Answers the begin, then loses every later connection except the cancel.
struct Unreachable {
    sent: Vec<&'static str>,
}

impl Server for Unreachable {
    fn request(&mut self, request: &Request, _first: bool) -> std::io::Result<serde_json::Value> {
        let name = request.method.operation().name;
        self.sent.push(name);
        match name {
            "pane.download.begin" => Ok(started(&["t"])),
            "pane.download.cancel" => Ok(json!({"id": request.id, "result": {"type": "ok"}})),
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                "server went away",
            )),
        }
    }
}

#[test]
fn a_read_that_cannot_reach_the_server_removes_the_stage_and_tries_a_cancel() {
    let dest = TempDir::new("cli-dl-unreachable");
    let mut server = Unreachable { sent: Vec::new() };
    let flag = AtomicBool::new(false);

    let result = download(
        &mut server,
        &call(&dest, PaneUploadCollision::Rename),
        &flag,
    );

    assert!(result.is_err());
    assert!(leftovers(&dest).is_empty());
    assert_eq!(
        server.sent,
        [
            "pane.download.begin",
            "pane.download.read",
            "pane.download.cancel"
        ]
    );
}
