#![cfg(all(unix, not(target_os = "macos")))]

pub mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use support::{
    cleanup_test_base, isolate_herdr_test_process, register_runtime_dir,
    register_spawned_herdr_pid, unregister_spawned_herdr_pid, wait_for_socket, wait_until,
};

fn unique_test_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    PathBuf::from(format!("/tmp/hlive-{}-{nanos}", std::process::id()))
}

fn app_dir_name() -> &'static str {
    if cfg!(debug_assertions) {
        "herdr-dev"
    } else {
        "herdr"
    }
}

struct SpawnedHerdr {
    _master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
}

impl Drop for SpawnedHerdr {
    fn drop(&mut self) {
        let pid = self.child.process_id();
        support::stop_spawned_herdr(&mut *self.child);

        if let Some(pid) = pid {
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                let mut status = 0;
                let result =
                    unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) };
                if result == pid as libc::pid_t || result == -1 {
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }

            unregister_spawned_herdr_pid(Some(pid));
        }
    }
}

fn spawn_herdr(config_home: &Path, runtime_dir: &Path, socket_path: &Path) -> SpawnedHerdr {
    fs::create_dir_all(config_home.join(app_dir_name())).unwrap();
    fs::create_dir_all(runtime_dir).unwrap();
    register_runtime_dir(runtime_dir);
    fs::write(
        config_home.join(app_dir_name()).join("config.toml"),
        "onboarding = false\n",
    )
    .unwrap();

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();

    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_herdr"));
    isolate_herdr_test_process(&mut cmd);
    cmd.arg("server");
    cmd.env("XDG_CONFIG_HOME", config_home);
    cmd.env("XDG_RUNTIME_DIR", runtime_dir);
    cmd.env("HERDR_SOCKET_PATH", socket_path);
    cmd.env_remove("HERDR_CLIENT_SOCKET_PATH");
    cmd.env("SHELL", "/bin/sh");
    cmd.env_remove("HERDR_ENV");

    let child = pair.slave.spawn_command(cmd).unwrap();
    register_spawned_herdr_pid(child.process_id());
    SpawnedHerdr {
        _master: pair.master,
        child,
    }
}

fn run_cli(socket_path: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_herdr"))
        .args(args)
        .env("HERDR_SOCKET_PATH", socket_path)
        .output()
        .unwrap()
}

fn run_cli_json(socket_path: &Path, args: &[&str]) -> serde_json::Value {
    let output = run_cli(socket_path, args);
    assert!(
        output.status.success(),
        "command failed: herdr {}\nstderr: {}\nstdout: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn docker(args: &[&str]) -> Output {
    Command::new("docker").args(args).output().unwrap()
}

/// Removes the container even when an assertion fails first.
struct LiveContainer {
    name: String,
    id: String,
}

impl LiveContainer {
    fn run_alpine(name: String) -> Self {
        let started = docker(&[
            "run",
            "-d",
            "--rm",
            "--name",
            &name,
            "--label",
            "herdr.shell=/bin/sh",
            "alpine",
            "sleep",
            "60",
        ]);
        assert!(
            started.status.success(),
            "docker run failed: {}",
            String::from_utf8_lossy(&started.stderr)
        );
        let id = String::from_utf8_lossy(&started.stdout).trim().to_owned();
        Self { name, id }
    }

    /// Docker sets a container's hostname to its short id.
    fn hostname(&self) -> &str {
        &self.id[..12]
    }
}

impl Drop for LiveContainer {
    fn drop(&mut self) {
        let _ = docker(&["rm", "-f", &self.name]);
    }
}

#[test]
#[ignore = "requires a docker daemon; run with --ignored"]
fn workspace_create_with_container_opens_shell_inside_running_container() {
    let docker_installed = Command::new("which")
        .arg("docker")
        .output()
        .is_ok_and(|output| output.status.success());
    if !docker_installed {
        return;
    }
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");
    let container = LiveContainer::run_alpine(format!(
        "herdr-live-{}-{}",
        std::process::id(),
        base.file_name().unwrap().to_string_lossy()
    ));
    let herdr = spawn_herdr(&config_home, &runtime_dir, &socket_path);
    wait_for_socket(&socket_path, Duration::from_secs(5));

    let created = run_cli_json(
        &socket_path,
        &[
            "workspace",
            "create",
            "--cwd",
            base.to_str().unwrap(),
            "--container",
            &container.name,
        ],
    );

    assert_eq!(created["result"]["root_pane"]["container"], container.name);
    let pane_id = created["result"]["root_pane"]["pane_id"].as_str().unwrap();
    let typed = run_cli(
        &socket_path,
        &["pane", "run", pane_id, "echo inside-$(hostname)"],
    );
    assert!(typed.status.success());
    let expected = format!("inside-{}", container.hostname());
    let mut last_read = String::new();
    let inside = wait_until(Duration::from_secs(15), Duration::from_millis(50), || {
        let read = run_cli(
            &socket_path,
            &["pane", "read", pane_id, "--source", "recent"],
        );
        last_read = String::from_utf8_lossy(&read.stdout).into_owned();
        last_read.contains(&expected)
    });
    assert!(
        inside,
        "the pane shell must run inside the container; expected {expected:?}, pane read:\n{last_read}"
    );

    drop(herdr);
    drop(container);
    cleanup_test_base(&base);
}
