use std::process::Stdio;

use super::*;
use crate::container::ContainerLaunch;
use crate::container::{ContainerRef, ContainerShell};
use crate::noninteractive_process::fake_program::FakeProgram;
use crate::noninteractive_process::spawn_streaming;
use crate::offhost::launcher::{
    script_argv, EnvironmentLauncher, LocalShellLauncher, OffhostLauncher, Transfer,
};
use crate::offhost::PaneEnvironment;
use crate::ssh_host::SshLaunch;
use crate::upload::receiver_script::RECEIVER_SCRIPT;
use crate::upload::sink_tests;
use crate::upload::test_support::TempDir;
use std::ffi::OsString;
use std::path::PathBuf;

pub(crate) fn local_sink(shell: &[String], dest: &str, control: Arc<Control>) -> RemoteSink {
    RemoteSink::new(
        Box::new(LocalShellLauncher::new(
            shell.to_vec(),
            RECEIVER_SCRIPT,
            "upload_failed",
        )),
        dest.to_owned(),
        crate::upload::stage_token(),
        control,
        RemoteTimeouts::default(),
    )
}

fn factory(dest: &str) -> Box<dyn UploadSink> {
    Box::new(local_sink(&["/bin/sh".to_owned()], dest, Arc::default()))
}

sink_tests::sink_contract!(factory);

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

fn script_argv_for_upload(
    env: &PaneEnvironment,
    args: &[&str],
) -> Result<(&'static str, Vec<OsString>), Failure> {
    script_argv(env, Transfer::UPLOAD, RECEIVER_SCRIPT, args)
}

fn strings(argv: Vec<OsString>) -> Vec<String> {
    argv.into_iter()
        .map(|arg| arg.into_string().unwrap())
        .collect()
}

#[test]
fn a_container_receiver_runs_through_docker_exec_with_stdin_as_the_pane_user() {
    let (program, argv) = script_argv_for_upload(&container(None), &["/work", "tok"]).unwrap();
    let argv = strings(argv);

    assert_eq!(program, "docker");
    assert_eq!(argv[..5], ["exec", "-i", "-u", "dev", "cid"]);
    assert_eq!(argv[5..7], ["sh", "-c"]);
    assert_eq!(argv[argv.len() - 3..], ["sh", "/work", "tok"]);
}

#[test]
fn an_ssh_receiver_requires_the_panes_shared_connection() {
    let env = PaneEnvironment::Ssh(ssh_launch(None));

    let error = script_argv_for_upload(&env, &["", "tok"]).unwrap_err();

    assert_eq!(error.0, "ssh_unavailable");
    assert!(
        error.1.contains("not shared (ControlMaster unavailable)"),
        "{}",
        error.1
    );
    assert!(error.1.contains("Reopen the ssh pane"), "{}", error.1);
}

#[test]
fn a_container_on_an_ssh_host_without_a_control_path_is_refused_instead_of_logging_in() {
    let error =
        script_argv_for_upload(&container(Some(ssh_launch(None))), &["", "tok"]).unwrap_err();

    assert_eq!(error.0, "ssh_unavailable");
    assert!(error.1.contains("deploy@devbox"));
}

#[test]
fn an_ssh_receiver_uses_the_control_path_in_batch_mode() {
    let env = PaneEnvironment::Ssh(ssh_launch(Some("/tmp/ctl-%C")));

    let (program, argv) = script_argv_for_upload(&env, &["/srv", "tok"]).unwrap();
    let argv = strings(argv);

    assert_eq!(program, "ssh");
    assert_eq!(argv[..2], ["-S", "/tmp/ctl-%C"]);
    assert!(argv.iter().any(|arg| arg == "BatchMode=yes"));
    assert!(argv.last().unwrap().ends_with("'sh' '/srv' 'tok'"));
}

#[test]
fn the_launcher_streams_through_the_docker_cli_it_was_given() {
    let docker = FakeProgram::new(
        "docker",
        "receiver",
        // Drop docker's own arguments and run the receiver the way exec would.
        r#"while [ "$1" != "-c" ]; do shift; done; shift; exec /bin/sh -c "$@""#,
    );
    let dest = TempDir::new("launcher");
    let launcher = EnvironmentLauncher {
        env: container(None),
        docker: docker.program(),
        ssh: PathBuf::from("/nonexistent/ssh"),
        script: RECEIVER_SCRIPT,
        transfer: Transfer::UPLOAD,
    };
    let mut sink = RemoteSink::new(
        Box::new(launcher),
        dest.str().to_owned(),
        crate::upload::stage_token(),
        Arc::default(),
        RemoteTimeouts::default(),
    );

    sink.begin(&sink_tests::names(&["a"])).unwrap();
    sink_tests::file(&mut sink, "a", b"hello", 0o644, None);
    let committed = sink.commit(PaneUploadCollision::Fail, &[], &[]).unwrap();

    assert_eq!(sink_tests::published(committed).len(), 1);
    assert_eq!(std::fs::read(dest.join("a")).unwrap(), b"hello");
}

#[test]
fn a_receiver_that_cannot_start_reports_the_classified_failure() {
    let docker = FakeProgram::new(
        "docker",
        "gone",
        "echo 'Error response from daemon: No such container: cid' >&2; exit 1",
    );
    let launcher = EnvironmentLauncher {
        env: container(None),
        docker: docker.program(),
        ssh: PathBuf::from("/nonexistent/ssh"),
        script: RECEIVER_SCRIPT,
        transfer: Transfer::UPLOAD,
    };
    let mut sink = RemoteSink::new(
        Box::new(launcher),
        "/work".into(),
        crate::upload::stage_token(),
        Arc::default(),
        RemoteTimeouts::default(),
    );

    let error = sink.begin(&sink_tests::names(&["a"])).unwrap_err();

    assert_eq!(error.0, "container_not_found");
}

#[test]
fn a_receiver_that_stops_reading_is_killed_by_the_watchdog() {
    // Reads the queries, answers begin, then never reads stdin again.
    let docker = FakeProgram::new(
        "docker",
        "stuck",
        r#"printf 'HERDR-OFFHOST-1\0ready\0/x\0qend\0'; exec sleep 600"#,
    );
    let launcher = EnvironmentLauncher {
        env: container(None),
        docker: docker.program(),
        ssh: PathBuf::from("/nonexistent/ssh"),
        script: RECEIVER_SCRIPT,
        transfer: Transfer::UPLOAD,
    };
    let mut sink = RemoteSink::new(
        Box::new(launcher),
        "/x".into(),
        crate::upload::stage_token(),
        Arc::default(),
        RemoteTimeouts {
            stall: Duration::from_millis(300),
            grace: Duration::from_millis(300),
        },
    );
    sink.begin(&sink_tests::names(&["big"])).unwrap();
    sink.start(&EntryStart::File {
        path: crate::upload::path::RelPath::test_new("big"),
        size: 8 * 1024 * 1024,
        mode: 0o644,
        mtime: None,
    })
    .ok();
    let started = Instant::now();

    let result = sink.data(&vec![0u8; 8 * 1024 * 1024]);

    assert!(result.is_err());
    assert!(started.elapsed() < Duration::from_secs(10));
}

/// Runs a stand-in receiver script with a chosen begin budget.
struct ScriptLauncher {
    script: &'static str,
    budget: Duration,
}

impl OffhostLauncher for ScriptLauncher {
    fn launch(&self, args: &[&str]) -> Result<StreamingChild, Failure> {
        let mut argv = vec!["-c".to_owned(), self.script.to_owned(), "sh".to_owned()];
        argv.extend(args.iter().map(|arg| (*arg).to_owned()));
        spawn_streaming(std::path::Path::new("/bin/sh"), &argv)
            .map_err(|error| upload_failed(format!("{error:?}")))
    }

    fn classify(&self, status: Option<i32>, stderr: &str) -> Failure {
        upload_failed(format!("receiver exited with {status:?}: {stderr}"))
    }

    fn timed_out(&self) -> Failure {
        upload_failed("timed out")
    }

    fn begin_budget(&self) -> Duration {
        self.budget
    }
}

fn scripted_sink(script: &'static str, budget: Duration, control: Arc<Control>) -> RemoteSink {
    RemoteSink::new(
        Box::new(ScriptLauncher { script, budget }),
        "/x".into(),
        crate::upload::stage_token(),
        control,
        RemoteTimeouts {
            stall: Duration::from_millis(300),
            grace: Duration::from_millis(200),
        },
    )
}

#[test]
fn a_commit_that_keeps_reporting_steps_may_outlast_its_budget() {
    // Ten steps 150 ms apart: 1.5 s in all, each well inside the 400 ms budget.
    let mut sink = scripted_sink(
        r#"printf 'HERDR-OFFHOST-1\0ready\0/x\0qend\0'
while IFS= read -r h; do case $h in 'C '*) break ;; esac; done
i=0; while [ $i -lt 10 ]; do sleep 0.15; printf 's\0'; i=$((i+1)); done
printf 'committed\0p\0a\0/x/a\0'"#,
        Duration::from_millis(400),
        Arc::default(),
    );
    sink.begin(&sink_tests::names(&["a"])).unwrap();

    let committed = sink.commit(PaneUploadCollision::Fail, &[], &[]).unwrap();

    assert_eq!(
        sink_tests::published(committed),
        [("a".to_owned(), "/x/a".to_owned())]
    );
}

#[test]
fn a_commit_that_falls_silent_for_its_budget_fails() {
    let mut sink = scripted_sink(
        r#"printf 'HERDR-OFFHOST-1\0ready\0/x\0qend\0'
while IFS= read -r h; do case $h in 'C '*) break ;; esac; done
printf 's\0'; exec sleep 600"#,
        Duration::from_millis(400),
        Arc::default(),
    );
    sink.begin(&sink_tests::names(&["a"])).unwrap();
    let started = Instant::now();

    let error = sink
        .commit(PaneUploadCollision::Fail, &[], &[])
        .unwrap_err();

    assert!(error.1.contains("did not finish in time"), "{}", error.1);
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[test]
fn a_commit_whose_attribute_lines_are_never_read_fails_within_the_stall_timeout() {
    let control = Arc::new(Control::default());
    let mut sink = scripted_sink(
        r#"printf 'HERDR-OFFHOST-1\0ready\0/x\0qend\0'; exec sleep 600"#,
        Duration::from_secs(600),
        control.clone(),
    );
    sink.begin(&sink_tests::names(&["t"])).unwrap();
    // Far more than a pipe buffer of `M` lines.
    let nested: Vec<_> = (0..4_000)
        .map(|i| sink_tests::attrs(&format!("t/{i:0>100}"), 0o755, None))
        .collect();
    control.set_phase(PHASE_COMMITTING);
    let started = Instant::now();

    let result = sink.commit(PaneUploadCollision::Fail, &nested, &[]);

    assert!(result.is_err());
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[test]
fn cancelling_during_a_commit_leaves_the_receiver_to_finish_publishing() {
    let control = Arc::new(Control::default());
    let mut sink = scripted_sink(
        r#"printf 'HERDR-OFFHOST-1\0ready\0/x\0qend\0'
while IFS= read -r h; do case $h in 'C '*) break ;; esac; done
sleep 0.6; printf 'committed\0p\0a\0/x/a\0'"#,
        Duration::from_secs(10),
        control.clone(),
    );
    sink.begin(&sink_tests::names(&["a"])).unwrap();
    control.set_phase(PHASE_COMMITTING);
    control.cancel();

    let committed = sink.commit(PaneUploadCollision::Fail, &[], &[]).unwrap();

    assert_eq!(sink_tests::published(committed).len(), 1);
}

#[test]
fn cancelling_while_a_write_is_blocked_terminates_the_receiver() {
    let docker = FakeProgram::new(
        "docker",
        "blocked",
        r#"printf 'HERDR-OFFHOST-1\0ready\0/x\0qend\0'; exec sleep 600"#,
    );
    let control = Arc::new(Control::default());
    let launcher = EnvironmentLauncher {
        env: container(None),
        docker: docker.program(),
        ssh: PathBuf::from("/nonexistent/ssh"),
        script: RECEIVER_SCRIPT,
        transfer: Transfer::UPLOAD,
    };
    let mut sink = RemoteSink::new(
        Box::new(launcher),
        "/x".into(),
        crate::upload::stage_token(),
        control.clone(),
        RemoteTimeouts {
            stall: Duration::from_secs(600),
            grace: Duration::from_millis(200),
        },
    );
    sink.begin(&sink_tests::names(&["big"])).unwrap();
    let canceller = control.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        canceller.cancel();
    });
    let started = Instant::now();

    let result = sink.data(&vec![0u8; 8 * 1024 * 1024]);

    assert!(result.is_err());
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[test]
fn every_local_shell_passes_a_full_upload_through_the_remote_sink() {
    for shell in crate::upload::test_support::posix_shells() {
        let dest = TempDir::new("remote-shells");
        let mut sink = local_sink(&shell, dest.str(), Arc::default());
        sink.begin(&sink_tests::names(&["d"])).unwrap();
        sink_tests::dir(&mut sink, "d");
        sink_tests::file(&mut sink, "d/x", &vec![1u8; 200_000], 0o640, None);

        let committed = sink.commit(PaneUploadCollision::Fail, &[], &[]).unwrap();

        assert_eq!(sink_tests::published(committed).len(), 1, "{shell:?}");
        assert_eq!(std::fs::read(dest.join("d/x")).unwrap().len(), 200_000);
    }
    let _ = Stdio::null();
}

/// Live check against a real container, for maintainers:
/// `HERDR_TEST_DOCKER_CONTAINER=<id> cargo nextest run real_container`.
#[test]
fn a_real_container_receives_a_tree_through_docker_exec() {
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
    let docker = |args: &[&str]| {
        let output = std::process::Command::new("docker")
            .arg("exec")
            .arg(&id)
            .args(args)
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    };
    let reset = std::process::Command::new("docker")
        .args(["exec", "-u", "0", &id, "sh", "-c"])
        .arg("rm -rf /tmp/herdr-live && mkdir -m 1777 /tmp/herdr-live")
        .status()
        .unwrap();
    assert!(reset.success());
    let upload = |collision| {
        let mut sink = RemoteSink::new(
            Box::new(EnvironmentLauncher::system(
                env.clone(),
                RECEIVER_SCRIPT,
                Transfer::UPLOAD,
            )),
            "/tmp/herdr-live".into(),
            crate::upload::stage_token(),
            Arc::default(),
            RemoteTimeouts::default(),
        );
        sink.begin(&sink_tests::names(&["tree"])).unwrap();
        sink_tests::dir(&mut sink, "tree");
        sink_tests::file(
            &mut sink,
            "tree/run.sh",
            b"#!/bin/sh\necho hi\n",
            0o755,
            Some(1_600_000_000),
        );
        sink_tests::file(
            &mut sink,
            "tree/big",
            &vec![b'x'; 3 * 1024 * 1024],
            0o644,
            None,
        );
        sink_tests::link(&mut sink, "tree/up", "../../etc");
        sink_tests::published(sink.commit(collision, &[], &[]).unwrap())
    };

    let first = upload(PaneUploadCollision::Fail);
    let second = upload(PaneUploadCollision::Rename);

    assert_eq!(first[0].1, "/tmp/herdr-live/tree");
    assert_eq!(second[0].1, "/tmp/herdr-live/tree (1)");
    assert_eq!(docker(&["sh", "/tmp/herdr-live/tree/run.sh"]), "hi");
    assert_eq!(
        docker(&["stat", "-c", "%a %Y", "/tmp/herdr-live/tree/run.sh"]),
        "755 1600000000"
    );
    assert_eq!(
        docker(&["wc", "-c", "/tmp/herdr-live/tree/big"]),
        "3145728 /tmp/herdr-live/tree/big"
    );
    assert_eq!(
        docker(&["readlink", "/tmp/herdr-live/tree/up"]),
        "../../etc"
    );
    assert_eq!(
        docker(&[
            "sh",
            "-c",
            "ls -A /tmp/herdr-live | grep -c herdr-upload || true"
        ]),
        "0"
    );
    let owner = docker(&["stat", "-c", "%U", "/tmp/herdr-live/tree/big"]);
    let user = std::env::var("HERDR_TEST_DOCKER_USER").unwrap_or_else(|_| "root".into());
    assert_eq!(owner, user, "files belong to the exec user (Q11)");
}
