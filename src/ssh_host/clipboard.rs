//! Copies bridged clipboard images to the host an ssh pane is logged in to,
//! over the pane's own multiplexed connection, so the pasted path names a
//! file the remote shell can open.

use std::ffi::OsString;
use std::fmt;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::clipboard_stage_script::{staged_path_from_output, CLEANUP_SCRIPT, STAGE_SCRIPT};
use crate::noninteractive_process::{run_with_deadline, RunError};
use crate::ssh_host::launch::shell_quote;

/// Budget for one side-channel run, matching the container backend.
const SSH_SIDE_CHANNEL_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub(crate) enum SshSideChannelError {
    SshNotFound,
    Timeout,
    Failed { status: Option<i32>, stderr: String },
    InvalidOutput(String),
}

impl fmt::Display for SshSideChannelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SshNotFound => write!(f, "ssh executable not found"),
            Self::Timeout => write!(f, "ssh did not respond in time"),
            Self::Failed { status, stderr } => match status {
                Some(code) => write!(f, "ssh exited with status {code}: {stderr}"),
                None => write!(f, "ssh was terminated: {stderr}"),
            },
            Self::InvalidOutput(detail) => write!(f, "unexpected ssh output: {detail}"),
        }
    }
}

impl std::error::Error for SshSideChannelError {}

/// Options shared by every side-channel run, before the destination.
///
/// Why `ControlMaster=no` and `BatchMode`: the pane owns the master and its
/// interactive login, so a side channel only joins it and never prompts.
/// Why not `-F`: the user's own config must resolve `host` exactly as it did
/// for the pane, or `%C` in the control path would name another socket.
/// Why the remaining options: like scp(1), a file transfer must not allocate
/// a tty (stdin carries raw bytes), run the config's `RemoteCommand` (ssh
/// refuses a second command), or set up forwardings and local commands.
fn side_channel_argv(host: &str, control_path: &Path) -> Vec<OsString> {
    let mut argv: Vec<OsString> = vec!["-S".into(), control_path.as_os_str().to_owned()];
    for option in [
        "ControlMaster=no",
        "BatchMode=yes",
        "ConnectTimeout=5",
        "RequestTTY=no",
        "RemoteCommand=none",
        "ClearAllForwardings=yes",
        "PermitLocalCommand=no",
        "ForwardX11=no",
        "ForwardAgent=no",
    ] {
        argv.push("-o".into());
        argv.push(option.into());
    }
    argv.push(host.into());
    argv
}

/// `ssh <side channel options> <host> "sh -c '<script>' sh '<file_name>'"`.
///
/// Why one pre-quoted command: ssh joins its remaining arguments with spaces
/// and hands the result to the remote login shell, which would otherwise
/// split the script at `;`, `&&`, and `>`.
fn stage_argv(host: &str, control_path: &Path, file_name: &str) -> Vec<OsString> {
    let mut argv = side_channel_argv(host, control_path);
    argv.push(
        format!(
            "sh -c {} sh {}",
            shell_quote(STAGE_SCRIPT),
            shell_quote(file_name)
        )
        .into(),
    );
    argv
}

/// `ssh <side channel options> <host> "sh -c '<cleanup script>' sh '<path>'…"`, one
/// run per host. Why `rmdir` and not `rm -rf`: each staged image owns a fresh
/// directory, and `rmdir` can only remove it once empty.
fn cleanup_argv(host: &str, control_path: &Path, paths: &[String]) -> Vec<OsString> {
    // Why `sh -c` and not a bare `rm; rmdir` line: the remote login shell may
    // not be a POSIX shell, so redirections and `;` are only safe inside `sh`.
    let mut command = format!("sh -c {} sh", shell_quote(CLEANUP_SCRIPT));
    for path in paths {
        command.push(' ');
        command.push_str(&shell_quote(path));
    }
    let mut argv = side_channel_argv(host, control_path);
    argv.push(command.into());
    argv
}

/// Streams `data` to the pane's ssh host and returns the remote path.
pub(crate) fn stage_image(
    host: &str,
    control_path: &Path,
    file_name: &str,
    data: Vec<u8>,
) -> Result<String, SshSideChannelError> {
    stage_image_with(
        "ssh",
        host,
        control_path,
        file_name,
        data,
        SSH_SIDE_CHANNEL_TIMEOUT,
    )
}

fn stage_image_with(
    program: impl AsRef<Path>,
    host: &str,
    control_path: &Path,
    file_name: &str,
    data: Vec<u8>,
    timeout: Duration,
) -> Result<String, SshSideChannelError> {
    let argv = stage_argv(host, control_path, file_name);
    let output = run_ssh(
        program.as_ref(),
        &argv,
        Some(data),
        Instant::now() + timeout,
    )?;
    staged_path_from_output(&output, file_name).map_err(SshSideChannelError::InvalidOutput)
}

/// Removes staged files. Callers ignore the error: the master may be gone.
pub(crate) fn remove_images(
    host: &str,
    control_path: &Path,
    paths: &[String],
) -> Result<(), SshSideChannelError> {
    remove_images_with("ssh", host, control_path, paths, SSH_SIDE_CHANNEL_TIMEOUT)
}

fn remove_images_with(
    program: impl AsRef<Path>,
    host: &str,
    control_path: &Path,
    paths: &[String],
    timeout: Duration,
) -> Result<(), SshSideChannelError> {
    if paths.is_empty() {
        return Ok(());
    }
    let argv = cleanup_argv(host, control_path, paths);
    run_ssh(program.as_ref(), &argv, None, Instant::now() + timeout).map(|_| ())
}

/// Stdout of a successful run.
fn run_ssh(
    program: &Path,
    argv: &[OsString],
    stdin: Option<Vec<u8>>,
    deadline: Instant,
) -> Result<String, SshSideChannelError> {
    let output = run_with_deadline(program, argv, stdin, deadline).map_err(|err| match err {
        RunError::NotFound => SshSideChannelError::SshNotFound,
        RunError::Timeout => {
            tracing::warn!(?program, "ssh side channel timed out");
            SshSideChannelError::Timeout
        }
        RunError::Failed { status, detail } => SshSideChannelError::Failed {
            status,
            stderr: detail,
        },
    })?;
    if output.status != Some(0) {
        return Err(SshSideChannelError::Failed {
            status: output.status,
            stderr: output.stderr.trim().to_owned(),
        });
    }
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAME: &str = "client-7-clipboard-123-0.png";
    const CONTROL: &str = "/tmp/hssh-1000/0123abcd-%C";

    fn strings(argv: Vec<OsString>) -> Vec<String> {
        argv.into_iter()
            .map(|arg| arg.into_string().unwrap())
            .collect()
    }

    const SIDE_CHANNEL: [&str; 20] = [
        "-S",
        CONTROL,
        "-o",
        "ControlMaster=no",
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=5",
        "-o",
        "RequestTTY=no",
        "-o",
        "RemoteCommand=none",
        "-o",
        "ClearAllForwardings=yes",
        "-o",
        "PermitLocalCommand=no",
        "-o",
        "ForwardX11=no",
        "-o",
        "ForwardAgent=no",
    ];

    fn expected(host: &str, command: &str) -> Vec<String> {
        SIDE_CHANNEL
            .iter()
            .copied()
            .chain([host, command])
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn stage_argv_joins_the_pane_master_and_runs_the_shared_script() {
        let argv = strings(stage_argv("deploy@prod", Path::new(CONTROL), NAME));

        assert_eq!(
            argv,
            expected(
                "deploy@prod",
                &format!("sh -c '{STAGE_SCRIPT}' sh '{NAME}'")
            )
        );
    }

    #[test]
    fn cleanup_argv_removes_every_path_of_a_host_in_one_run() {
        let paths = vec![
            "/tmp/herdr-clipboard-image-aB3dE9/a.png".to_owned(),
            "/tmp/herdr-clipboard-image-Zz09Qq/b.jpg".to_owned(),
        ];

        assert_eq!(
            strings(cleanup_argv("prod", Path::new(CONTROL), &paths)),
            expected(
                "prod",
                &format!(
                    "sh -c '{CLEANUP_SCRIPT}' sh '/tmp/herdr-clipboard-image-aB3dE9/a.png' \
                     '/tmp/herdr-clipboard-image-Zz09Qq/b.jpg'"
                )
            )
        );
    }

    /// Opt-in: `cargo nextest run --run-ignored only -E 'test(/ssh_host::clipboard::tests::live/)'`.
    mod live {
        use super::super::*;
        use crate::clipboard_stage_script::STAGE_DIR_PREFIX;
        use std::path::PathBuf;
        use std::process::{Command, Output};

        const ALIAS: &str = "herdr-clip-live";
        const UID: u32 = 1234;

        fn run(program: &str, args: &[&str]) -> Output {
            Command::new(program).args(args).output().unwrap()
        }

        fn ok(program: &str, args: &[&str]) -> String {
            let output = run(program, args);
            assert!(
                output.status.success(),
                "{program} {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout).into_owned()
        }

        /// A throwaway sshd container plus a private ssh config, key, and
        /// control socket. Nothing touches the user's own `~/.ssh`.
        struct LiveHost {
            container: String,
            dir: PathBuf,
            socket_dir: PathBuf,
            config: PathBuf,
            control: PathBuf,
            real_ssh: String,
        }

        impl LiveHost {
            fn ssh(&self, args: &[&str]) -> Output {
                let mut full = vec!["-F", self.config.to_str().unwrap()];
                full.extend_from_slice(args);
                run(&self.real_ssh, &full)
            }

            fn stop_master(&self) -> Output {
                self.ssh(&["-S", self.control.to_str().unwrap(), "-O", "exit", ALIAS])
            }
        }

        impl Drop for LiveHost {
            fn drop(&mut self) {
                let _ = self.stop_master();
                let _ = run("docker", &["rm", "-f", &self.container]);
                let _ = std::fs::remove_dir_all(&self.dir);
                let _ = std::fs::remove_dir_all(&self.socket_dir);
            }
        }

        fn start_live_host() -> LiveHost {
            let dir =
                std::env::temp_dir().join(format!("herdr-ssh-clip-live-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("bin")).unwrap();
            // Why not under `dir`: ssh appends a 17-byte suffix while binding
            // the control socket, and a long TMPDIR would pass the ~104-byte
            // socket path limit.
            let socket_dir = PathBuf::from(format!("/tmp/hcl-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&socket_dir);
            {
                use std::os::unix::fs::DirBuilderExt;
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&socket_dir)
                    .unwrap();
            }
            let key = dir.join("id");
            ok(
                "ssh-keygen",
                &["-q", "-t", "ed25519", "-N", "", "-f", key.to_str().unwrap()],
            );
            let public_key = std::fs::read_to_string(dir.join("id.pub")).unwrap();
            // `*` instead of alpine's locked `!` lets sshd accept the key
            // without enabling passwords.
            let setup = format!(
                "apk add --no-cache openssh-server >/dev/null && ssh-keygen -A >/dev/null && \
                 adduser -D -u {UID} herdr && sed -i 's/^herdr:!/herdr:*/' /etc/shadow && \
                 mkdir -m 700 /home/herdr/.ssh && echo '{}' > /home/herdr/.ssh/authorized_keys && \
                 chown -R herdr /home/herdr/.ssh && \
                 exec /usr/sbin/sshd -D -e -o PasswordAuthentication=no \
                 -o KbdInteractiveAuthentication=no",
                public_key.trim()
            );
            let name = format!("herdr-ssh-clip-live-{}", std::process::id());
            let container = ok(
                "docker",
                &[
                    "run",
                    "-d",
                    "--rm",
                    "--name",
                    &name,
                    "-p",
                    "127.0.0.1::22",
                    "alpine:3.20",
                    "sh",
                    "-c",
                    &setup,
                ],
            )
            .trim()
            .to_owned();
            let port = ok("docker", &["port", &container, "22/tcp"]);
            let port = port
                .lines()
                .next()
                .and_then(|line| line.trim().rsplit(':').next())
                .unwrap()
                .to_owned();
            // No usable identity here: on its own a side channel cannot log
            // in, like a host whose login needs the pane's interactive auth.
            let config = dir.join("config");
            std::fs::write(
                &config,
                format!(
                    "Host {ALIAS}\n  HostName 127.0.0.1\n  Port {port}\n  User herdr\n  \
                     UserKnownHostsFile {known}\n  StrictHostKeyChecking accept-new\n  \
                     IdentitiesOnly yes\n  IdentityAgent none\n  IdentityFile {none}\n  \
                     LogLevel ERROR\n",
                    known = dir.join("known_hosts").display(),
                    none = dir.join("no-such-key").display(),
                ),
            )
            .unwrap();
            let real_ssh = ok("sh", &["-c", "command -v ssh"]).trim().to_owned();
            let host = LiveHost {
                container,
                control: socket_dir.join("%C"),
                dir,
                socket_dir,
                config,
                real_ssh,
            };
            // The pane's own login: `-i` stands in for its interactive auth.
            let login = [
                "-i",
                key.to_str().unwrap(),
                "-S",
                host.control.to_str().unwrap(),
                "-o",
                "ControlMaster=yes",
                "-o",
                "ControlPersist=60",
                "-o",
                "ConnectTimeout=5",
                "-fN",
                ALIAS,
            ];
            let deadline = std::time::Instant::now() + Duration::from_secs(90);
            loop {
                let started = host.ssh(&login);
                if started.status.success() {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "sshd never accepted the master: {}",
                    String::from_utf8_lossy(&started.stderr)
                );
                std::thread::sleep(Duration::from_secs(1));
            }
            host
        }

        fn container_out(host: &LiveHost, args: &[&str]) -> Output {
            let mut full = vec!["exec", host.container.as_str()];
            full.extend_from_slice(args);
            run("docker", &full)
        }

        #[test]
        #[ignore = "requires docker, network access for apk, and OpenSSH; run with --run-ignored"]
        fn stage_image_reaches_the_ssh_host_through_the_pane_master_only() {
            if !run("docker", &["version"]).status.success() {
                return;
            }
            let host = start_live_host();
            // The production argv has no `-F`; this wrapper points the real
            // ssh at the private config so `stage_image` runs unchanged.
            let wrapper = host.dir.join("bin/ssh");
            std::fs::write(
                &wrapper,
                format!(
                    "#!/bin/sh\nexec {} -F {} \"$@\"\n",
                    host.real_ssh,
                    host.config.display()
                ),
            )
            .unwrap();
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            // Safe only because nextest runs each test in its own process.
            let path = std::env::var("PATH").unwrap_or_default();
            std::env::set_var("PATH", format!("{}:{path}", host.dir.join("bin").display()));

            let bytes = (0..=255u8).cycle().take(300_000).collect::<Vec<_>>();
            let file_name = "client-1-clipboard-42-0.png";
            let staged = stage_image(ALIAS, &host.control, file_name, bytes.clone()).unwrap();

            let dir = Path::new(&staged).parent().unwrap().display().to_string();
            assert_eq!(staged, format!("{dir}/{file_name}"));
            assert!(
                dir.starts_with(&format!("/tmp/{STAGE_DIR_PREFIX}")),
                "{dir}"
            );
            let stat = container_out(&host, &["stat", "-c", "%U %a", &staged, &dir]);
            assert_eq!(
                String::from_utf8_lossy(&stat.stdout),
                "herdr 600\nherdr 700\n"
            );
            let read_back = container_out(&host, &["cat", &staged]);
            assert_eq!(read_back.stdout, bytes, "no tty mangled the raw bytes");

            remove_images(ALIAS, &host.control, std::slice::from_ref(&staged)).unwrap();
            let gone = container_out(&host, &["test", "-e", &staged]);
            assert!(!gone.status.success(), "cleanup removes the staged file");
            let dir_gone = container_out(&host, &["test", "-e", &dir]);
            assert!(!dir_gone.status.success(), "cleanup removes the directory");

            assert!(host.stop_master().status.success());
            let err = stage_image(ALIAS, &host.control, file_name, b"x".to_vec()).unwrap_err();
            assert!(
                matches!(
                    err,
                    SshSideChannelError::Failed {
                        status: Some(255),
                        ..
                    }
                ),
                "without the master the side channel cannot log in: {err}"
            );
        }
    }

    #[test]
    fn shell_quote_round_trips_through_a_posix_shell() {
        for text in ["", "/tmp/a b/$HOME", "it's", "`id`", "a\"b", ";rm -rf /"] {
            let output = std::process::Command::new("sh")
                .args(["-c", &format!("printf %s {}", shell_quote(text))])
                .output()
                .unwrap();
            assert_eq!(String::from_utf8_lossy(&output.stdout), text);
        }
    }

    mod runner {
        use super::super::*;
        use super::NAME;
        use crate::clipboard_stage_script::STAGE_DIR_PREFIX;
        use crate::noninteractive_process::fake_program::FakeProgram;

        fn secs(n: u64) -> Duration {
            Duration::from_secs(n)
        }

        fn control() -> &'static Path {
            Path::new(super::CONTROL)
        }

        /// A fake `ssh` that runs the remote command with the host `sh`,
        /// rooted at a fresh temp dir, the way a remote login shell would.
        struct FakeRemote {
            ssh: FakeProgram,
            tmpdir: std::path::PathBuf,
        }

        impl FakeRemote {
            fn new(name: &str) -> Self {
                let tmpdir = std::env::temp_dir()
                    .join(format!("herdr-ssh-clip-{name}-tmp-{}", std::process::id()));
                let _ = std::fs::remove_dir_all(&tmpdir);
                std::fs::create_dir_all(&tmpdir).unwrap();
                // The last argument is the remote command string.
                let script = format!(
                    "for arg; do command=\"$arg\"; done; TMPDIR={} exec sh -c \"$command\"",
                    tmpdir.display()
                );
                Self {
                    ssh: FakeProgram::new("ssh", name, &script),
                    tmpdir,
                }
            }

            fn staged_dirs(&self) -> Vec<std::path::PathBuf> {
                std::fs::read_dir(&self.tmpdir)
                    .unwrap()
                    .map(|entry| entry.unwrap().path())
                    .filter(|path| {
                        path.file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| name.starts_with(STAGE_DIR_PREFIX))
                    })
                    .collect()
            }
        }

        impl Drop for FakeRemote {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.tmpdir);
            }
        }

        #[test]
        fn stage_image_writes_a_private_file_and_returns_the_remote_path() {
            use std::os::unix::fs::PermissionsExt;
            let remote = FakeRemote::new("stage");

            let path = stage_image_with(
                remote.ssh.program(),
                "prod",
                control(),
                NAME,
                b"image-bytes".to_vec(),
                secs(5),
            )
            .unwrap();

            let dirs = remote.staged_dirs();
            assert_eq!(dirs.len(), 1);
            let expected = dirs[0].join(NAME);
            assert_eq!(path, expected.display().to_string());
            assert_eq!(std::fs::read(&expected).unwrap(), b"image-bytes");
            let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&expected), 0o600);
            assert_eq!(mode(&dirs[0]), 0o700);
        }

        #[test]
        fn removing_staged_images_deletes_them_on_the_remote_host() {
            let remote = FakeRemote::new("cleanup");
            let path = stage_image_with(
                remote.ssh.program(),
                "prod",
                control(),
                NAME,
                b"x".to_vec(),
                secs(5),
            )
            .unwrap();

            remove_images_with(
                remote.ssh.program(),
                "prod",
                control(),
                std::slice::from_ref(&path),
                secs(5),
            )
            .unwrap();

            assert!(!Path::new(&path).exists());
            assert!(
                remote.staged_dirs().is_empty(),
                "the per-image directory goes with the file"
            );
        }

        #[test]
        fn stage_image_fails_when_the_pane_master_is_down() {
            let ssh = FakeProgram::new(
                "ssh",
                "master-down",
                "cat >/dev/null; echo 'Permission denied (publickey,password).' >&2; exit 255",
            );

            let err = stage_image_with(ssh.program(), "prod", control(), NAME, vec![1, 2], secs(5))
                .unwrap_err();

            match err {
                SshSideChannelError::Failed { status, stderr } => {
                    assert_eq!(status, Some(255));
                    assert_eq!(stderr, "Permission denied (publickey,password).");
                }
                other => panic!("unexpected {other:?}"),
            }
        }

        #[test]
        fn stage_image_times_out_within_the_deadline() {
            let ssh = FakeProgram::new("ssh", "hang", "exec sleep 30");
            let start = std::time::Instant::now();

            let err = stage_image_with(
                ssh.program(),
                "prod",
                control(),
                NAME,
                vec![0; 1 << 20],
                Duration::from_millis(300),
            )
            .unwrap_err();

            assert!(matches!(err, SshSideChannelError::Timeout));
            assert!(start.elapsed() < secs(5));
        }

        #[test]
        fn stage_image_rejects_output_that_is_not_the_staged_file() {
            let ssh = FakeProgram::new(
                "ssh",
                "garbage",
                "cat >/dev/null; printf '%s\\n%s' /tmp/evil /tmp/x/client-7-clipboard-123-0.png",
            );

            let err = stage_image_with(ssh.program(), "prod", control(), NAME, vec![1], secs(5))
                .unwrap_err();

            assert!(matches!(err, SshSideChannelError::InvalidOutput(_)));
        }

        #[test]
        fn stage_image_reports_a_missing_ssh_executable() {
            let err = stage_image_with(
                "/nonexistent/herdr-ssh",
                "prod",
                control(),
                NAME,
                vec![1],
                secs(5),
            )
            .unwrap_err();

            assert!(matches!(err, SshSideChannelError::SshNotFound));
            assert_eq!(err.to_string(), "ssh executable not found");
        }

        #[test]
        fn remove_images_skips_ssh_when_nothing_was_staged() {
            remove_images_with("/nonexistent/herdr-ssh", "prod", control(), &[], secs(5)).unwrap();
        }
    }
}
