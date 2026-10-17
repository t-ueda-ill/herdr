//! Copies bridged clipboard images into the container a pane shell runs in,
//! so the pasted path names a file the pane's processes can open.

use std::path::Path;
use std::time::{Duration, Instant};

use super::docker::{program_for, run_docker_with, DockerHost};
use super::ContainerListError;
use crate::clipboard_stage_script::{staged_path_from_output, STAGE_SCRIPT};

/// `docker exec -i [-u <user>] <id> sh -c '<script>' sh <file_name>`.
/// No `-t`: stdin carries raw image bytes.
fn stage_argv(container_id: &str, user: Option<&str>, file_name: &str) -> Vec<String> {
    let mut argv = vec!["exec".to_owned(), "-i".to_owned()];
    push_user(&mut argv, user);
    argv.extend(
        [container_id, "sh", "-c", STAGE_SCRIPT, "sh", file_name]
            .into_iter()
            .map(str::to_owned),
    );
    argv
}

/// `docker exec [-u <user>] <id> rm -f <paths…>`.
fn cleanup_argv(container_id: &str, user: Option<&str>, paths: &[String]) -> Vec<String> {
    let mut argv = vec!["exec".to_owned()];
    push_user(&mut argv, user);
    argv.extend([container_id, "rm", "-f"].into_iter().map(str::to_owned));
    argv.extend(paths.iter().cloned());
    argv
}

fn push_user(argv: &mut Vec<String>, user: Option<&str>) {
    // An empty configured user means the container's default user, the same
    // as passing no `-u`.
    if let Some(user) = user.filter(|user| !user.is_empty()) {
        argv.push("-u".to_owned());
        argv.push(user.to_owned());
    }
}

/// Streams `data` into the container and returns the in-container path.
pub(crate) fn stage_image(
    host: &DockerHost,
    container_id: &str,
    user: Option<&str>,
    file_name: &str,
    data: Vec<u8>,
) -> Result<String, ContainerListError> {
    stage_image_with(
        program_for(host),
        host,
        container_id,
        user,
        file_name,
        data,
        host.timeout(),
    )
}

fn stage_image_with(
    program: impl AsRef<Path>,
    host: &DockerHost,
    container_id: &str,
    user: Option<&str>,
    file_name: &str,
    data: Vec<u8>,
    timeout: Duration,
) -> Result<String, ContainerListError> {
    let argv = stage_argv(container_id, user, file_name);
    let args = argv.iter().map(String::as_str).collect::<Vec<_>>();
    let output = run_docker_with(
        program.as_ref(),
        host,
        &args,
        Some(data),
        Instant::now() + timeout,
    )?;
    if output.status != Some(0) {
        return Err(ContainerListError::Failed {
            status: output.status,
            stderr: output.stderr.trim().to_owned(),
        });
    }
    staged_path_from_output(&output.stdout, file_name).map_err(ContainerListError::InvalidOutput)
}

/// Removes staged files. Callers ignore the error: the container may be gone.
pub(crate) fn remove_images(
    host: &DockerHost,
    container_id: &str,
    user: Option<&str>,
    paths: &[String],
) -> Result<(), ContainerListError> {
    remove_images_with(
        program_for(host),
        host,
        container_id,
        user,
        paths,
        host.timeout(),
    )
}

fn remove_images_with(
    program: impl AsRef<Path>,
    host: &DockerHost,
    container_id: &str,
    user: Option<&str>,
    paths: &[String],
    timeout: Duration,
) -> Result<(), ContainerListError> {
    if paths.is_empty() {
        return Ok(());
    }
    let argv = cleanup_argv(container_id, user, paths);
    let args = argv.iter().map(String::as_str).collect::<Vec<_>>();
    let output = run_docker_with(
        program.as_ref(),
        host,
        &args,
        None,
        Instant::now() + timeout,
    )?;
    if output.status != Some(0) {
        return Err(ContainerListError::Failed {
            status: output.status,
            stderr: output.stderr.trim().to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAME: &str = "client-7-clipboard-123-0.png";

    #[test]
    fn stage_argv_streams_stdin_to_the_pane_user_without_a_tty() {
        let argv = stage_argv("abc123", Some("node"), NAME);
        assert_eq!(
            argv,
            [
                "exec",
                "-i",
                "-u",
                "node",
                "abc123",
                "sh",
                "-c",
                STAGE_SCRIPT,
                "sh",
                NAME
            ]
        );
        assert!(!argv.iter().any(|arg| arg == "-t" || arg == "-it"));
    }

    #[test]
    fn stage_argv_uses_the_container_default_user_when_none_or_empty() {
        for user in [None, Some("")] {
            assert_eq!(
                stage_argv("abc123", user, NAME),
                ["exec", "-i", "abc123", "sh", "-c", STAGE_SCRIPT, "sh", NAME]
            );
        }
    }

    #[test]
    fn cleanup_argv_removes_every_path_in_one_exec() {
        let paths = vec!["/tmp/a.png".to_owned(), "/tmp/b.jpg".to_owned()];
        assert_eq!(
            cleanup_argv("abc123", Some("1000:1000"), &paths),
            [
                "exec",
                "-u",
                "1000:1000",
                "abc123",
                "rm",
                "-f",
                "/tmp/a.png",
                "/tmp/b.jpg"
            ]
        );
        assert_eq!(
            cleanup_argv("abc123", None, &paths[..1]),
            ["exec", "abc123", "rm", "-f", "/tmp/a.png"]
        );
    }

    /// Opt-in: `cargo nextest run --run-ignored only -E 'test(/clipboard::tests::live/)'`.
    #[cfg(all(unix, not(target_os = "macos")))]
    mod live {
        use super::super::*;
        use std::process::Command;

        struct LiveContainer(String);

        impl Drop for LiveContainer {
            fn drop(&mut self) {
                let _ = Command::new("docker").args(["rm", "-f", &self.0]).output();
            }
        }

        fn docker_out(args: &[&str]) -> std::process::Output {
            Command::new("docker").args(args).output().unwrap()
        }

        #[test]
        #[ignore = "requires a docker daemon and the alpine image; run with --run-ignored"]
        fn stage_image_writes_a_private_file_owned_by_the_pane_user() {
            if !Command::new("docker")
                .arg("version")
                .output()
                .is_ok_and(|output| output.status.success())
            {
                return;
            }
            let name = format!("herdr-clip-live-{}", std::process::id());
            let started = docker_out(&[
                "run", "-d", "--rm", "--name", &name, "alpine", "sleep", "60",
            ]);
            assert!(
                started.status.success(),
                "docker run failed: {}",
                String::from_utf8_lossy(&started.stderr)
            );
            let container =
                LiveContainer(String::from_utf8_lossy(&started.stdout).trim().to_owned());
            let bytes = (0..=255u8).cycle().take(300_000).collect::<Vec<_>>();
            let file_name = "client-1-clipboard-42-0.png";

            // Alpine ships `guest` (uid 405), so the owner check is not root-only.
            let path = stage_image(
                &DockerHost::Local,
                &container.0,
                Some("guest"),
                file_name,
                bytes.clone(),
            )
            .unwrap();
            assert_eq!(path, format!("/tmp/herdr-clipboard-images-405/{file_name}"));
            let stat = docker_out(&[
                "exec",
                &container.0,
                "stat",
                "-c",
                "%U %a",
                &path,
                "/tmp/herdr-clipboard-images-405",
            ]);
            assert_eq!(
                String::from_utf8_lossy(&stat.stdout),
                "guest 600\nguest 700\n"
            );
            let read_back = docker_out(&["exec", "-u", "guest", &container.0, "cat", &path]);
            assert_eq!(
                read_back.stdout, bytes,
                "the pane user reads the exact bytes"
            );

            let root_path = stage_image(
                &DockerHost::Local,
                &container.0,
                None,
                file_name,
                b"root".to_vec(),
            )
            .unwrap();
            assert_eq!(
                root_path,
                format!("/tmp/herdr-clipboard-images-0/{file_name}")
            );

            remove_images(
                &DockerHost::Local,
                &container.0,
                Some("guest"),
                std::slice::from_ref(&path),
            )
            .unwrap();
            let gone = docker_out(&["exec", &container.0, "test", "-e", &path]);
            assert!(!gone.status.success(), "cleanup removes the staged file");
            drop(container);
            assert!(
                remove_images(&DockerHost::Local, &name, None, &[root_path]).is_err(),
                "cleanup reports a vanished container; callers ignore it"
            );
        }
    }

    #[cfg(unix)]
    mod runner {
        use super::super::*;
        use super::NAME;
        use crate::container::docker::tests::Shim;

        fn secs(n: u64) -> Duration {
            Duration::from_secs(n)
        }

        /// Runs the real stage script with the host `sh`, rooted at a fresh
        /// host temp dir. Executables in `fake_bin` shadow the real ones.
        struct RealScript {
            shim: Shim,
            tmpdir: std::path::PathBuf,
            fake_bin: std::path::PathBuf,
        }

        impl RealScript {
            fn new(name: &str) -> Self {
                let tmpdir = std::env::temp_dir()
                    .join(format!("herdr-clip-{name}-tmp-{}", std::process::id()));
                let fake_bin = std::env::temp_dir()
                    .join(format!("herdr-clip-{name}-bin-{}", std::process::id()));
                for dir in [&tmpdir, &fake_bin] {
                    let _ = std::fs::remove_dir_all(dir);
                    std::fs::create_dir_all(dir).unwrap();
                }
                let script = format!(
                    "shift; while [ \"$1\" != sh ]; do shift; done; shift; \
                     script=\"$2\"; shift 3; PATH={}:$PATH TMPDIR={} \
                     exec sh -c \"$script\" sh \"$1\"",
                    fake_bin.display(),
                    tmpdir.display()
                );
                Self {
                    shim: Shim::new(name, &script),
                    tmpdir,
                    fake_bin,
                }
            }

            fn fake(&self, command: &str, body: &str) {
                use std::os::unix::fs::PermissionsExt;
                let path = self.fake_bin.join(command);
                std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }

            fn staging_dir(&self) -> std::path::PathBuf {
                let uid = unsafe { libc::geteuid() };
                self.tmpdir.join(format!("herdr-clipboard-images-{uid}"))
            }

            fn stage(&self) -> Result<String, ContainerListError> {
                stage_image_with(
                    self.shim.program(),
                    &DockerHost::Local,
                    "abc123",
                    None,
                    NAME,
                    b"image-bytes".to_vec(),
                    secs(5),
                )
            }
        }

        impl Drop for RealScript {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.tmpdir);
                let _ = std::fs::remove_dir_all(&self.fake_bin);
            }
        }

        #[test]
        fn stage_image_returns_the_printed_path_after_consuming_stdin() {
            let script = RealScript::new("stage");
            let path = script.stage().unwrap();
            let expected = script.staging_dir().join(NAME);
            assert_eq!(path, expected.display().to_string());
            assert_eq!(std::fs::read(&expected).unwrap(), b"image-bytes");
        }

        #[test]
        fn stage_image_writes_nothing_when_the_staging_dir_cannot_be_secured() {
            // A directory pre-created by another uid makes `chmod` fail.
            let chmod_denied = RealScript::new("chmod-denied");
            chmod_denied.fake("chmod", "echo 'chmod: Operation not permitted' >&2; exit 1");
            // Something other than a directory in the way makes `mkdir` fail.
            let mkdir_blocked = RealScript::new("mkdir-blocked");
            std::fs::write(mkdir_blocked.staging_dir(), b"not a dir").unwrap();

            for (case, script) in [("chmod", &chmod_denied), ("mkdir", &mkdir_blocked)] {
                match script.stage() {
                    Err(ContainerListError::Failed { status, .. }) => {
                        assert_eq!(status, Some(1), "{case}");
                    }
                    other => panic!("{case}: expected a failed stage, got {other:?}"),
                }
                assert!(
                    !script.staging_dir().join(NAME).exists(),
                    "{case}: the image must not be written"
                );
            }
        }

        #[test]
        fn stage_image_still_succeeds_when_the_stale_sweep_fails() {
            let script = RealScript::new("find-fails");
            script.fake("find", "exit 1");
            let path = script.stage().unwrap();
            assert_eq!(std::fs::read(&path).unwrap(), b"image-bytes");
        }

        #[test]
        fn stage_image_reports_a_container_without_sh() {
            let shim = Shim::new(
                "clip-nosh",
                "cat >/dev/null; echo 'exec: \"sh\": executable file not found' >&2; exit 126",
            );
            let err = stage_image_with(
                shim.program(),
                &DockerHost::Local,
                "abc",
                None,
                NAME,
                vec![1, 2],
                secs(5),
            )
            .unwrap_err();
            match err {
                ContainerListError::Failed { status, stderr } => {
                    assert_eq!(status, Some(126));
                    assert!(stderr.contains("executable file not found"));
                }
                other => panic!("unexpected {other:?}"),
            }
        }

        #[test]
        fn stage_image_rejects_output_that_is_not_the_staged_file() {
            let shim = Shim::new("clip-garbage", "cat >/dev/null; printf %s /tmp/other.png");
            let err = stage_image_with(
                shim.program(),
                &DockerHost::Local,
                "abc",
                None,
                NAME,
                vec![1],
                secs(5),
            )
            .unwrap_err();
            assert!(matches!(err, ContainerListError::InvalidOutput(_)));
        }

        #[test]
        fn stage_image_times_out_within_the_deadline() {
            let shim = Shim::new("clip-hang", "exec sleep 30");
            let start = std::time::Instant::now();
            let err = stage_image_with(
                shim.program(),
                &DockerHost::Local,
                "abc",
                None,
                NAME,
                vec![0; 1 << 20],
                Duration::from_millis(300),
            )
            .unwrap_err();
            assert!(matches!(err, ContainerListError::Timeout));
            assert!(start.elapsed() < secs(5));
        }

        #[test]
        fn remove_images_skips_docker_when_nothing_was_staged() {
            remove_images_with(
                "/nonexistent/herdr-docker",
                &DockerHost::Local,
                "abc",
                None,
                &[],
                secs(5),
            )
            .unwrap();
        }

        #[test]
        fn remove_images_passes_every_path_to_rm() {
            let log =
                std::env::temp_dir().join(format!("herdr-clip-rm-{}.log", std::process::id()));
            let shim = Shim::new("clip-rm", &format!("echo \"$@\" > {}", log.display()));
            let paths = vec!["/tmp/a.png".to_owned(), "/tmp/b.png".to_owned()];
            remove_images_with(
                shim.program(),
                &DockerHost::Local,
                "abc",
                Some("node"),
                &paths,
                secs(5),
            )
            .unwrap();
            let logged = std::fs::read_to_string(&log).unwrap();
            let _ = std::fs::remove_file(&log);
            assert_eq!(
                logged.trim(),
                "exec -u node abc rm -f /tmp/a.png /tmp/b.png"
            );
        }

        fn remote() -> DockerHost {
            DockerHost::Ssh {
                host: "deploy@prod".into(),
                control_path: Some("/tmp/hssh-1/abc-%C".into()),
            }
        }

        /// Fake `ssh` that records argv and runs the remote command string
        /// through `sh`, with `docker` shadowed by a function that records
        /// its arguments and consumes stdin.
        fn remote_docker_shim(name: &str, log: &std::path::Path) -> Shim {
            let script = format!(
                "printf '%s\\n' \"$@\" > {log}.argv; for last; do :; done; \
                 docker() {{ printf '%s\\n' \"$@\" > {log}.docker; cat > {log}.stdin; \
                 printf %s /tmp/herdr-clipboard-images-0/{NAME}; }}; eval \"$last\"",
                log = log.display()
            );
            Shim::named("ssh", name, &script)
        }

        #[test]
        fn stage_image_over_ssh_streams_the_image_into_the_remote_container() {
            let log =
                std::env::temp_dir().join(format!("herdr-clip-ssh-stage-{}", std::process::id()));
            let shim = remote_docker_shim("clip-ssh-stage", &log);

            let path = stage_image_with(
                shim.program(),
                &remote(),
                "abc123",
                Some("node"),
                NAME,
                b"image-bytes".to_vec(),
                secs(5),
            )
            .unwrap();

            let read = |ext: &str| {
                let file = std::path::PathBuf::from(format!("{}.{ext}", log.display()));
                let text = std::fs::read(&file).unwrap_or_default();
                let _ = std::fs::remove_file(file);
                text
            };
            let (argv, docker, stdin) = (read("argv"), read("docker"), read("stdin"));
            let argv = String::from_utf8(argv).unwrap();
            assert_eq!(path, format!("/tmp/herdr-clipboard-images-0/{NAME}"));
            assert_eq!(stdin, b"image-bytes");
            assert!(argv.contains("ControlMaster=auto"), "{argv}");
            assert!(argv.contains("deploy@prod"), "{argv}");
            assert!(!argv.lines().any(|line| line == "-t"), "{argv}");
            // The script survived the remote shell as a single `-c` word.
            let docker = String::from_utf8(docker).unwrap();
            assert_eq!(
                docker,
                format!("exec\n-i\n-u\nnode\nabc123\nsh\n-c\n{STAGE_SCRIPT}\nsh\n{NAME}\n")
            );
        }

        #[test]
        fn remove_images_over_ssh_runs_rm_inside_the_remote_container() {
            let log =
                std::env::temp_dir().join(format!("herdr-clip-ssh-rm-{}", std::process::id()));
            let shim = remote_docker_shim("clip-ssh-rm", &log);
            let paths = vec!["/tmp/a b.png".to_owned()];

            remove_images_with(shim.program(), &remote(), "abc", None, &paths, secs(5)).unwrap();

            let file = std::path::PathBuf::from(format!("{}.docker", log.display()));
            let docker = std::fs::read_to_string(&file).unwrap();
            for ext in ["argv", "docker", "stdin"] {
                let _ = std::fs::remove_file(format!("{}.{ext}", log.display()));
            }
            assert_eq!(docker, "exec\nabc\nrm\n-f\n/tmp/a b.png\n");
        }

        #[test]
        fn clipboard_over_ssh_reports_a_login_that_needs_a_prompt() {
            let shim = Shim::named(
                "ssh",
                "clip-ssh-login",
                "cat >/dev/null; echo 'Permission denied (publickey).' >&2; exit 255",
            );
            let err = stage_image_with(
                shim.program(),
                &remote(),
                "abc",
                None,
                NAME,
                vec![1],
                secs(5),
            )
            .unwrap_err();
            assert!(matches!(
                err,
                ContainerListError::SshLoginRequired { ref host } if host == "deploy@prod"
            ));
        }
    }
}
