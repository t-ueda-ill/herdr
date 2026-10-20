use std::collections::VecDeque;
use std::path::Path;

use serde_json::json;

use super::*;
use crate::upload::test_support::TempDir;

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn parse_resolves_relative_paths_from_the_current_directory_and_defaults_to_fail() {
    let parsed = parse(&args(&["w1-p1", "a.txt", "/abs/b"]), Path::new("/work")).unwrap();

    assert_eq!(
        parsed,
        UploadArgs {
            pane_id: "w1-p1".into(),
            paths: vec![PathBuf::from("/work/a.txt"), PathBuf::from("/abs/b")],
            destination: None,
            collision: PaneUploadCollision::Fail,
            json: false,
        }
    );
}

#[test]
fn parse_reads_the_destination_conflict_policy_and_json_flag() {
    let parsed = parse(
        &args(&[
            "p",
            "x",
            "--dest",
            "/srv/in",
            "--on-conflict",
            "rename",
            "--json",
        ]),
        Path::new("/"),
    )
    .unwrap();

    assert_eq!(parsed.destination.as_deref(), Some("/srv/in"));
    assert_eq!(parsed.collision, PaneUploadCollision::Rename);
    assert!(parsed.json);
}

#[test]
fn parse_rejects_a_missing_path_or_an_unknown_policy() {
    assert!(parse(&args(&["p"]), Path::new("/")).is_err());
    assert!(parse(&args(&["p", "x", "--on-conflict", "merge"]), Path::new("/")).is_err());
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

fn started() -> serde_json::Value {
    json!({"id": "b", "result": {
        "type": "pane_upload_started", "upload_id": "up-1",
        "location": {"kind": "container", "container": "c"},
        "destination": "/work", "conflicts": [], "chunk_bytes": 4, "window": 2
    }})
}

fn progress() -> serde_json::Value {
    json!({"id": "w", "result": {"type": "pane_upload_progress", "received_entries": 1, "received_bytes": 1}})
}

fn error(code: &str) -> serde_json::Value {
    json!({"id": "x", "error": {"code": code, "message": "nope"}})
}

fn local_tree() -> (TempDir, UploadArgs) {
    let dir = TempDir::new("cli-upload");
    std::fs::create_dir(dir.join("t")).unwrap();
    std::fs::write(dir.join("t/f"), b"0123456789").unwrap();
    let args = UploadArgs {
        pane_id: "p".into(),
        paths: vec![dir.join("t")],
        destination: None,
        collision: PaneUploadCollision::Fail,
        json: false,
    };
    (dir, args)
}

fn run_scripted(server: &mut Scripted, args: &UploadArgs, interrupted: bool) -> Outcome {
    let flag = AtomicBool::new(interrupted);
    upload(server, args, &flag, &mut |_| {}).unwrap()
}

#[test]
fn writes_go_one_at_a_time_in_offset_order_with_the_servers_chunk_size() {
    let (_dir, args) = local_tree();
    let finished = json!({"id": "f", "result": {
        "type": "pane_upload_finished", "location": {"kind": "container"},
        "destination": "/work", "paths": ["/work/t"], "warnings": []
    }});
    let mut server = Scripted::new(vec![
        started(),
        progress(),
        progress(),
        progress(),
        progress(),
        finished,
    ]);

    let outcome = run_scripted(&mut server, &args, false);

    assert_eq!(exit_code(&outcome), 0);
    let Method::PaneUploadBegin(begin) = &server.sent[0].0.method else {
        panic!()
    };
    assert_eq!(begin.names, ["t"]);
    assert_eq!((begin.entries, begin.bytes), (2, 10));
    assert!(server.sent[0].1, "only begin checks the protocol");
    assert!(server.sent[1..].iter().all(|(_, first)| !first));
    let offsets: Vec<(String, u64)> = server.sent[1..5]
        .iter()
        .map(|(request, _)| match &request.method {
            Method::PaneUploadWrite(write) => (write.path.clone(), write.offset),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        offsets,
        [
            ("t".into(), 0),
            ("t/f".into(), 0),
            ("t/f".into(), 4),
            ("t/f".into(), 8)
        ]
    );
    assert_eq!(server.methods().last(), Some(&"pane.upload.finish"));
}

#[test]
fn a_conflict_exits_3_and_cancels_the_upload() {
    let (_dir, args) = local_tree();
    let mut server = Scripted::new(vec![
        started(),
        progress(),
        progress(),
        progress(),
        progress(),
        error("upload_conflict"),
    ]);

    let outcome = run_scripted(&mut server, &args, false);

    assert_eq!(exit_code(&outcome), CONFLICT_EXIT);
    assert_eq!(server.methods().last(), Some(&"pane.upload.cancel"));
}

#[test]
fn a_failed_write_stops_sending_and_cancels() {
    let (_dir, args) = local_tree();
    let mut server = Scripted::new(vec![started(), error("upload_failed")]);

    let outcome = run_scripted(&mut server, &args, false);

    assert_eq!(exit_code(&outcome), 1);
    assert_eq!(
        server.methods(),
        [
            "pane.upload.begin",
            "pane.upload.write",
            "pane.upload.cancel"
        ]
    );
}

#[test]
fn an_interrupt_cancels_before_the_next_write() {
    let (_dir, args) = local_tree();
    let mut server = Scripted::new(vec![started()]);

    let outcome = run_scripted(&mut server, &args, true);

    assert_eq!(outcome, Outcome::Interrupted);
    assert_eq!(
        server.methods(),
        ["pane.upload.begin", "pane.upload.cancel"]
    );
}

#[test]
fn a_refused_begin_sends_nothing_else() {
    let (_dir, args) = local_tree();
    let mut server = Scripted::new(vec![error("ssh_unavailable")]);

    let outcome = run_scripted(&mut server, &args, false);

    assert_eq!(
        outcome,
        Outcome::Failed("ssh_unavailable".into(), "nope".into())
    );
    assert_eq!(server.methods(), ["pane.upload.begin"]);
}

#[test]
fn a_missing_local_path_fails_before_contacting_the_server() {
    let dir = TempDir::new("cli-missing");
    let args = UploadArgs {
        pane_id: "p".into(),
        paths: vec![dir.join("nope")],
        destination: None,
        collision: PaneUploadCollision::Fail,
        json: false,
    };
    let mut server = Scripted::new(Vec::new());

    let outcome = run_scripted(&mut server, &args, false);

    assert!(matches!(outcome, Outcome::Local(_)));
    assert!(server.sent.is_empty());
}
