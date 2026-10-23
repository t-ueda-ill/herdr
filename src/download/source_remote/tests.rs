use std::path::{Path, PathBuf};

use super::*;
use crate::container::{ContainerLaunch, ContainerRef, ContainerShell};
use crate::download::frame::{Limits, Packer};
use crate::download::sender_script::SENDER_SCRIPT;
use crate::noninteractive_process::fake_program::FakeProgram;
use crate::offhost::launcher::{
    connection_not_shared, script_argv, EnvironmentLauncher, LocalShellLauncher, Transfer,
};
use crate::offhost::PaneEnvironment;
use crate::ssh_host::SshLaunch;
use crate::upload::test_support::{posix_shells, TempDir};

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|index| (index * 31 % 251) as u8).collect()
}

fn fast() -> RemoteOptions {
    RemoteOptions {
        tick: Duration::from_millis(5),
        grace: Duration::from_millis(300),
        walk: Duration::from_secs(10),
        ..RemoteOptions::default()
    }
}

fn paths_of(paths: &[&Path]) -> Vec<String> {
    paths
        .iter()
        .map(|path| path.to_str().unwrap().to_owned())
        .collect()
}

/// The real sender script under `shell`.
fn real(shell: &[String], paths: &[&Path], options: RemoteOptions) -> RemoteSource {
    RemoteSource::new(
        Box::new(LocalShellLauncher::new(
            shell.to_vec(),
            SENDER_SCRIPT,
            "download_failed",
        )),
        paths_of(paths),
        options,
    )
}

/// A stand-in helper. Its arguments after the limits are `args`.
fn fake(script: &'static str, args: &[&Path], options: RemoteOptions) -> RemoteSource {
    RemoteSource::new(
        Box::new(
            LocalShellLauncher::new(vec!["/bin/sh".to_owned()], script, "download_failed")
                .with_budget(Duration::from_secs(1)),
        ),
        paths_of(args),
        options,
    )
}

/// Every frame up to `End`, or the failure that ended the stream.
fn drain(source: &mut RemoteSource) -> Result<Vec<Frame>, Failure> {
    let control = Control::default();
    let mut frames = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        match source.next(Duration::from_millis(50), &control)? {
            Some(Frame::End) => {
                frames.push(Frame::End);
                return Ok(frames);
            }
            Some(frame) => frames.push(frame),
            None => {}
        }
    }
    panic!("the stream did not end");
}

fn process_exists(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if done() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out waiting for {what}");
}

fn read_pid(path: &Path) -> u32 {
    wait_until("the helper to write its pid", || {
        std::fs::read_to_string(path).is_ok_and(|text| text.trim().parse::<u32>().is_ok())
    });
    std::fs::read_to_string(path)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

#[test]
fn a_tree_streams_through_the_packer_with_every_shell() {
    for shell in posix_shells() {
        let dir = TempDir::new("remote-tree");
        let root = dir.join("t");
        std::fs::create_dir_all(root.join("d")).unwrap();
        std::fs::write(root.join("d/x"), pattern(200_000)).unwrap();
        std::fs::write(root.join("empty"), b"").unwrap();
        std::os::unix::fs::symlink("d/x", root.join("link")).unwrap();
        let mut source = real(&shell, &[&root], fast());

        let began = source.begin().unwrap();
        let control = Control::default();
        let mut packer = Packer::new(vec!["t".into()], Limits::new(64 * 1024));
        let mut records = Vec::new();
        let mut seq = 0;
        let warnings = loop {
            let mut next = || source.next(Duration::from_millis(200), &control);
            let chunk = packer.fill(seq, &mut next).unwrap();
            seq += 1;
            records.extend(chunk.records);
            if chunk.done {
                break chunk.warnings;
            }
        };

        assert_eq!(
            began,
            Began {
                entries: 5,
                bytes: 200_000
            },
            "{shell:?}"
        );
        assert!(warnings.is_empty(), "{warnings:?}");
        let mut entries: Vec<_> = records
            .iter()
            .filter(|record| record.entry.is_some())
            .map(|record| record.path.as_str())
            .collect();
        // Why sorted: `find` lists a folder in the filesystem's order.
        entries.sort_unstable();
        assert_eq!(
            entries,
            ["t", "t/d", "t/d/x", "t/empty", "t/link"],
            "{shell:?}"
        );
        let body: Vec<u8> = records
            .iter()
            .filter(|record| record.path == "t/d/x")
            .flat_map(|record| {
                use base64::Engine as _;
                base64::engine::general_purpose::STANDARD
                    .decode(&record.data)
                    .unwrap()
            })
            .collect();
        assert!(body == pattern(200_000));
    }
}

#[test]
fn a_selection_the_helper_cannot_find_fails_begin_with_not_found() {
    let dir = TempDir::new("remote-missing");
    let mut source = real(&["/bin/sh".to_owned()], &[&dir.join("nope")], fast());

    let error = source.begin().unwrap_err();

    assert_eq!(error.0, "not_found");
}

#[test]
fn a_selection_over_the_limits_fails_begin_with_download_too_large() {
    let dir = TempDir::new("remote-big");
    std::fs::write(dir.join("f"), pattern(1000)).unwrap();
    let options = RemoteOptions {
        max_bytes: 10,
        ..fast()
    };
    let mut source = real(&["/bin/sh".to_owned()], &[&dir.join("f")], options);

    assert_eq!(source.begin().unwrap_err().0, "download_too_large");
}

#[test]
fn the_helper_is_not_told_to_go_until_the_first_read() {
    let dir = TempDir::new("remote-go");
    let marker = dir.join("went");
    let mut source = fake(
        r#"printf 'HERDR-OFFHOST-1\0est\0%s\0%s\0' 1 1
IFS= read -r go || exit 0
[ "$go" = G ] || exit 0
: > "$4"
printf 'end\0'"#,
        &[&marker],
        fast(),
    );

    source.begin().unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert!(!marker.exists(), "the helper started before the first read");

    let frames = drain(&mut source).unwrap();

    assert_eq!(frames, [Frame::End]);
    assert!(marker.exists());
}

#[test]
fn the_wait_for_the_first_read_is_not_a_stall() {
    let mut source = fake(
        r#"printf 'HERDR-OFFHOST-1\0est\0%s\0%s\0' 1 1
IFS= read -r go || exit 0
printf 'end\0'"#,
        &[],
        RemoteOptions {
            stall: Duration::from_millis(150),
            ..fast()
        },
    );
    source.begin().unwrap();
    std::thread::sleep(Duration::from_millis(500));

    assert_eq!(drain(&mut source).unwrap(), [Frame::End]);
}

#[test]
fn a_helper_that_goes_silent_fails_the_stream_as_stalled_and_is_stopped() {
    let dir = TempDir::new("remote-stall");
    let pidfile = dir.join("pid");
    let mut source = fake(
        // Ignores TERM, so only the KILL after the grace periods stops it.
        r#"echo $$ > "$4"
trap '' TERM
printf 'HERDR-OFFHOST-1\0est\0%s\0%s\0' 1 5
IFS= read -r go || exit 0
printf 'f\0a\0%s\0' '5 644 - t'
while :; do sleep 1; done"#,
        &[&pidfile],
        RemoteOptions {
            stall: Duration::from_millis(300),
            ..fast()
        },
    );
    source.begin().unwrap();
    let pid = read_pid(&pidfile);

    let error = drain(&mut source).unwrap_err();

    assert_eq!(
        error,
        ("download_failed", "the source stopped sending".into())
    );
    source.abort();
    wait_until("the helper to be killed", || !process_exists(pid));
}

#[test]
fn a_full_queue_holds_the_helper_back_without_counting_as_a_stall() {
    let dir = TempDir::new("remote-backpressure");
    std::fs::write(dir.join("big"), pattern(4 * 1024 * 1024)).unwrap();
    let options = RemoteOptions {
        queue: 2,
        stall: Duration::from_millis(150),
        ..fast()
    };
    let mut source = real(&["/bin/sh".to_owned()], &[&dir.join("big")], options);
    source.begin().unwrap();
    // Nothing reads, so the queue fills and the pipe behind it stops the helper.
    let control = Control::default();
    let first = source.next(Duration::from_millis(100), &control).unwrap();
    assert!(first.is_some());
    std::thread::sleep(Duration::from_millis(700));

    let mut frames = vec![first.unwrap()];
    frames.extend(drain(&mut source).unwrap());

    let body: Vec<u8> = frames
        .iter()
        .flat_map(|frame| match frame {
            Frame::Data(bytes) => bytes.clone(),
            _ => Vec::new(),
        })
        .collect();
    assert!(body == pattern(4 * 1024 * 1024));
}

#[test]
fn a_helper_that_dies_mid_stream_is_reported_with_its_status_and_stderr() {
    let mut source = fake(
        r#"printf 'HERDR-OFFHOST-1\0est\0%s\0%s\0' 1 1
IFS= read -r go || exit 0
printf 'd\0a\0%s\0' '0 755 - t'
echo boom >&2
exit 3"#,
        &[],
        fast(),
    );
    source.begin().unwrap();

    let error = drain(&mut source).unwrap_err();

    assert_eq!(error.0, "download_failed");
    assert!(
        error.1.contains("Some(3)") && error.1.contains("boom"),
        "{error:?}"
    );
}

#[test]
fn a_helper_that_exits_cleanly_without_the_end_word_is_an_incomplete_stream() {
    let mut source = fake(
        r#"printf 'HERDR-OFFHOST-1\0est\0%s\0%s\0' 1 1
IFS= read -r go || exit 0
printf 'd\0a\0%s\0' '0 755 - t'
exit 0"#,
        &[],
        fast(),
    );
    source.begin().unwrap();

    let error = drain(&mut source).unwrap_err();

    assert_eq!(error.0, "download_failed");
    assert!(
        error.1.contains("before the stream was complete"),
        "{error:?}"
    );
}

#[test]
fn output_that_breaks_the_grammar_fails_the_stream() {
    let mut source = fake(
        r#"printf 'HERDR-OFFHOST-1\0est\0%s\0%s\0' 1 1
IFS= read -r go || exit 0
printf 'bogus\0'
sleep 5"#,
        &[],
        fast(),
    );
    source.begin().unwrap();

    let error = drain(&mut source).unwrap_err();

    assert_eq!(error.0, "download_failed");
    assert!(error.1.contains("not understood"), "{error:?}");
}

#[test]
fn stderr_after_a_clean_finish_becomes_a_warning_before_the_end() {
    let mut source = fake(
        r#"printf 'HERDR-OFFHOST-1\0est\0%s\0%s\0' 1 1
IFS= read -r go || exit 0
echo 'Warning: something odd' >&2
printf 'end\0'"#,
        &[],
        fast(),
    );
    source.begin().unwrap();

    let frames = drain(&mut source).unwrap();

    assert_eq!(frames.len(), 2);
    assert!(
        matches!(&frames[0], Frame::WalkWarning(text) if text.contains("something odd")),
        "{frames:?}"
    );
    assert_eq!(frames[1], Frame::End);
}

#[test]
fn a_nonzero_exit_after_the_end_word_fails_the_download() {
    let mut source = fake(
        r#"printf 'HERDR-OFFHOST-1\0est\0%s\0%s\0' 1 1
IFS= read -r go || exit 0
printf 'end\0'
exit 1"#,
        &[],
        fast(),
    );
    source.begin().unwrap();

    assert_eq!(drain(&mut source).unwrap_err().0, "download_failed");
}

#[test]
fn a_walk_that_never_answers_fails_begin_and_stops_the_helper() {
    let dir = TempDir::new("remote-walk");
    let pidfile = dir.join("pid");
    let mut source = fake(
        r#"echo $$ > "$4"
printf 'HERDR-OFFHOST-1\0'
sleep 600"#,
        &[&pidfile],
        RemoteOptions {
            walk: Duration::from_millis(300),
            ..fast()
        },
    );
    // The launcher's budget is a second, so give the walk less than that in total.
    let started = Instant::now();

    let error = source.begin().unwrap_err();

    assert_eq!(
        error,
        (
            "download_failed",
            "the folders took too long to scan".into()
        )
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    let pid = read_pid(&pidfile);
    wait_until("the helper to be stopped", || !process_exists(pid));
}

#[test]
fn a_helper_that_ends_before_the_estimate_fails_begin_with_its_status() {
    let mut source = fake(
        r#"echo 'sh: nothing here' >&2
exit 127"#,
        &[],
        fast(),
    );

    let error = source.begin().unwrap_err();

    assert!(error.1.contains("Some(127)"), "{error:?}");
}

#[test]
fn aborting_returns_at_once_and_stops_a_helper_that_ignores_eof_and_term() {
    let dir = TempDir::new("remote-abort");
    let pidfile = dir.join("pid");
    let mut source = fake(
        r#"echo $$ > "$4"
trap '' TERM HUP
printf 'HERDR-OFFHOST-1\0est\0%s\0%s\0' 1 1
while :; do sleep 1; done"#,
        &[&pidfile],
        RemoteOptions {
            grace: Duration::from_secs(1),
            ..fast()
        },
    );
    source.begin().unwrap();
    let pid = read_pid(&pidfile);
    assert!(process_exists(pid));

    let started = Instant::now();
    source.abort();

    assert!(started.elapsed() < Duration::from_millis(500));
    wait_until("the helper to be killed", || !process_exists(pid));
    assert_eq!(
        source.next(Duration::ZERO, &Control::default()),
        Err(cancelled())
    );
}

#[test]
fn dropping_the_source_stops_the_helper() {
    let dir = TempDir::new("remote-drop");
    let pidfile = dir.join("pid");
    let mut source = fake(
        r#"echo $$ > "$4"
printf 'HERDR-OFFHOST-1\0est\0%s\0%s\0' 1 1
IFS= read -r go
sleep 600"#,
        &[&pidfile],
        fast(),
    );
    source.begin().unwrap();
    let pid = read_pid(&pidfile);

    drop(source);

    wait_until("the helper to stop", || !process_exists(pid));
}

#[test]
fn a_cancel_during_a_wait_is_noticed_within_a_tick() {
    let mut source = fake(
        r#"printf 'HERDR-OFFHOST-1\0est\0%s\0%s\0' 1 1
IFS= read -r go
sleep 600"#,
        &[],
        fast(),
    );
    source.begin().unwrap();
    let control = Arc::new(Control::default());
    let flag = Arc::clone(&control);
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        flag.cancel();
    });

    let result = source.next(Duration::from_secs(30), &control);

    canceller.join().unwrap();
    assert_eq!(result, Err(cancelled()));
}

#[test]
fn a_source_that_was_not_begun_or_begun_twice_refuses() {
    let mut source = fake("exit 0", &[], fast());
    assert!(source.next(Duration::ZERO, &Control::default()).is_err());
    let _ = source.begin();

    assert_eq!(source.begin().unwrap_err().0, "download_failed");
}

fn container(ssh: Option<SshLaunch>) -> PaneEnvironment {
    PaneEnvironment::Container(Box::new(ContainerLaunch {
        reference: ContainerRef::Name("scratch".into()),
        canonical: "scratch".into(),
        id: "cid".into(),
        workdir: None,
        start_dir: None,
        user: Some("dev".into()),
        shell: ContainerShell::BashOrSh,
        ssh,
    }))
}

fn ssh_launch(control_path: Option<&str>) -> SshLaunch {
    SshLaunch::new("deploy@devbox".into(), control_path.map(Into::into)).unwrap()
}

fn strings(argv: Vec<std::ffi::OsString>) -> Vec<String> {
    argv.into_iter()
        .map(|arg| arg.into_string().unwrap())
        .collect()
}

#[test]
fn a_container_helper_runs_through_docker_exec_with_stdin_as_the_pane_user() {
    let args = ["limit", "limit", "/work/a"];
    let (program, argv) =
        script_argv(&container(None), Transfer::DOWNLOAD, SENDER_SCRIPT, &args).unwrap();
    let argv = strings(argv);

    assert_eq!(program, "docker");
    assert_eq!(argv[..5], ["exec", "-i", "-u", "dev", "cid"]);
    assert_eq!(argv[5..8], ["sh", "-c", SENDER_SCRIPT]);
    assert_eq!(argv[argv.len() - 4..], ["sh", "limit", "limit", "/work/a"]);
}

#[test]
fn an_ssh_helper_requires_the_panes_shared_connection_and_says_download() {
    let env = PaneEnvironment::Ssh(ssh_launch(None));

    let error = script_argv(&env, Transfer::DOWNLOAD, SENDER_SCRIPT, &[]).unwrap_err();

    assert_eq!(error.0, "ssh_unavailable");
    assert_eq!(
        error,
        connection_not_shared(Transfer::DOWNLOAD, "deploy@devbox")
    );
    assert!(
        error.1.starts_with("cannot download from deploy@devbox:"),
        "{}",
        error.1
    );
    assert!(error.1.ends_with("then try again."), "{}", error.1);
}

#[test]
fn a_container_on_an_ssh_host_without_a_control_path_is_refused_with_the_download_wording() {
    let env = container(Some(ssh_launch(None)));

    let error = script_argv(&env, Transfer::DOWNLOAD, SENDER_SCRIPT, &[]).unwrap_err();

    assert_eq!(error.0, "ssh_unavailable");
    assert!(
        error.1.contains("cannot download from deploy@devbox"),
        "{}",
        error.1
    );
}

#[test]
fn an_ssh_helper_uses_the_control_path_in_batch_mode() {
    let env = PaneEnvironment::Ssh(ssh_launch(Some("/tmp/ctl-%C")));

    let (program, argv) = script_argv(
        &env,
        Transfer::DOWNLOAD,
        SENDER_SCRIPT,
        &["1", "2", "/srv/x"],
    )
    .unwrap();
    let argv = strings(argv);

    assert_eq!(program, "ssh");
    assert_eq!(argv[..2], ["-S", "/tmp/ctl-%C"]);
    assert!(argv.iter().any(|arg| arg == "BatchMode=yes"));
    assert!(argv.last().unwrap().ends_with("'sh' '1' '2' '/srv/x'"));
}

#[test]
fn the_launcher_streams_through_the_docker_cli_it_was_given() {
    let docker = FakeProgram::new(
        "docker",
        "sender",
        // Drop docker's own arguments and run the helper the way exec would.
        r#"while [ "$1" != "-c" ]; do shift; done; shift; exec /bin/sh -c "$@""#,
    );
    let dir = TempDir::new("remote-docker");
    std::fs::write(dir.join("f"), b"hello").unwrap();
    let launcher = EnvironmentLauncher {
        env: container(None),
        docker: docker.program(),
        ssh: PathBuf::from("/nonexistent/ssh"),
        script: SENDER_SCRIPT,
        transfer: Transfer::DOWNLOAD,
    };
    let mut source = RemoteSource::new(Box::new(launcher), paths_of(&[&dir.join("f")]), fast());

    let began = source.begin().unwrap();
    let frames = drain(&mut source).unwrap();

    assert_eq!(
        began,
        Began {
            entries: 1,
            bytes: 5
        }
    );
    assert!(frames.contains(&Frame::Data(b"hello".to_vec())));
}

#[test]
fn a_helper_that_cannot_start_reports_the_classified_failure() {
    let docker = FakeProgram::new(
        "docker",
        "gone",
        "echo 'Error response from daemon: No such container: cid' >&2; exit 1",
    );
    let launcher = EnvironmentLauncher {
        env: container(None),
        docker: docker.program(),
        ssh: PathBuf::from("/nonexistent/ssh"),
        script: SENDER_SCRIPT,
        transfer: Transfer::DOWNLOAD,
    };
    let mut source = RemoteSource::new(Box::new(launcher), vec!["/work".into()], fast());

    let error = source.begin().unwrap_err();

    assert_eq!(error.0, "container_not_found");
}

/// Live check against a real container, for maintainers:
/// `HERDR_TEST_DOCKER_CONTAINER=<id> cargo nextest run real_container`.
#[test]
fn a_real_container_streams_a_tree_through_docker_exec() {
    let Ok(id) = std::env::var("HERDR_TEST_DOCKER_CONTAINER") else {
        eprintln!("skipping: HERDR_TEST_DOCKER_CONTAINER is not set");
        return;
    };
    let env = PaneEnvironment::Container(Box::new(ContainerLaunch {
        reference: ContainerRef::Name(id.clone()),
        canonical: id.clone(),
        id: id.clone(),
        workdir: None,
        start_dir: None,
        user: std::env::var("HERDR_TEST_DOCKER_USER").ok(),
        shell: ContainerShell::BashOrSh,
        ssh: None,
    }));
    let setup = std::process::Command::new("docker")
        .args(["exec", &id, "sh", "-c"])
        .arg(
            "rm -rf /tmp/herdr-live-dl && mkdir -p /tmp/herdr-live-dl/tree/d && \
             head -c 200000 /dev/zero > /tmp/herdr-live-dl/tree/d/x && \
             ln -s d/x /tmp/herdr-live-dl/tree/link && chmod 777 /tmp/herdr-live-dl",
        )
        .status()
        .unwrap();
    assert!(setup.success());
    let launcher = EnvironmentLauncher::system(env, SENDER_SCRIPT, Transfer::DOWNLOAD);
    let mut source = RemoteSource::new(
        Box::new(launcher),
        vec!["/tmp/herdr-live-dl/tree".into()],
        fast(),
    );

    let began = source.begin().unwrap();
    let frames = drain(&mut source).unwrap();

    assert_eq!(began.entries, 4);
    let sent: usize = frames
        .iter()
        .map(|frame| match frame {
            Frame::Data(bytes) => bytes.len(),
            _ => 0,
        })
        .sum();
    assert_eq!(sent, 200_000);
    assert!(frames.iter().any(
        |frame| matches!(frame, Frame::Entry { link_target: Some(target), .. } if target == b"d/x")
    ));
}
