use super::harness::*;
use std::os::unix::fs::PermissionsExt;

/// Stands in for the ssh client: logs its arguments and becomes a local
/// `/bin/sh`, so an ssh pane really runs a shell without a network.
///
/// `ssh -G` is herdr's `RemoteCommand` lookup, not a pane: it prints nothing
/// and is not logged, so the log keeps one line per pane.
const SSH_SHIM: &str = r#"#!/bin/sh
[ "$1" = -G ] && exit 0
printf '%s\n' "$*" >> "$HERDR_TEST_SSH_LOG"
exec /bin/sh
"#;

const SSH_CONFIG: &str = "\
Include conf.d/*
Host prod
  HostName prod.example.com
  User deploy
Host * !bastion
  ServerAliveInterval 30
Host lab web-?
";

struct SshTest {
    base: PathBuf,
    home: PathBuf,
    config_home: PathBuf,
    runtime_dir: PathBuf,
    socket_path: PathBuf,
    bin_dir: PathBuf,
    log: PathBuf,
}

impl SshTest {
    fn new() -> Self {
        let base = unique_test_dir();
        let runtime_dir = base.join("runtime");
        let home = base.join("home");
        let ssh_dir = home.join(".ssh");
        fs::create_dir_all(ssh_dir.join("conf.d")).unwrap();
        fs::write(ssh_dir.join("config"), SSH_CONFIG).unwrap();
        fs::write(
            ssh_dir.join("conf.d").join("work"),
            "Host work\n  HostName work.example.com\n",
        )
        .unwrap();
        let bin_dir = base.join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let program = bin_dir.join("ssh");
        fs::write(&program, SSH_SHIM).unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            config_home: base.join("config"),
            socket_path: runtime_dir.join("herdr.sock"),
            log: base.join("ssh.log"),
            runtime_dir,
            home,
            bin_dir,
            base,
        }
    }

    fn start_server_with_path(&self, path: &str) -> SpawnedHerdr {
        let herdr = spawn_herdr_with_env(
            &self.config_home,
            &self.runtime_dir,
            &self.socket_path,
            Some(Path::new(path)),
            // Why integration off: the log has one line per ssh call, and the
            // launcher's multi-line script would split it.
            "onboarding = false\n[offhost]\nshell_integration = false\n",
            &[("HOME", &self.home), ("HERDR_TEST_SSH_LOG", &self.log)],
        );
        wait_for_socket(&self.socket_path, Duration::from_secs(5));
        herdr
    }

    fn start_server(&self) -> SpawnedHerdr {
        let inherited = std::env::var("PATH").unwrap_or_default();
        self.start_server_with_path(&format!("{}:{inherited}", self.bin_dir.display()))
    }

    fn cli(&self, args: &[&str]) -> std::process::Output {
        run_cli(&self.socket_path, args)
    }

    fn cli_json(&self, args: &[&str]) -> serde_json::Value {
        run_cli_json(&self.socket_path, args)
    }

    fn calls(&self) -> Vec<String> {
        fs::read_to_string(&self.log)
            .map(|log| log.lines().map(str::to_owned).collect())
            .unwrap_or_default()
    }

    /// The shim logs from the pane process, after the API response.
    fn wait_for_calls(&self, count: usize) -> Vec<String> {
        assert!(
            wait_until(Duration::from_secs(5), Duration::from_millis(25), || {
                self.calls().len() >= count
            }),
            "ssh was started {} times, expected {count}",
            self.calls().len()
        );
        self.calls()
    }

    fn session_file(&self) -> PathBuf {
        self.config_home.join(app_dir_name()).join("session.json")
    }

    fn finish(self, herdr: SpawnedHerdr) {
        cleanup_spawned_herdr(herdr, self.base);
    }
}

fn error_code(output: &std::process::Output) -> serde_json::Value {
    let body: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap_or_else(|err| {
        panic!(
            "stderr is not a JSON error ({err}): {}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    body["error"]["code"].clone()
}

/// The pane's ssh multiplexes through a herdr-owned master and lets the
/// user's own config apply.
fn assert_multiplexed_ssh_to(call: &str, destination: &str) {
    let args: Vec<&str> = call.split(' ').collect();
    assert_eq!(&args[..2], ["-C", "-S"], "{call}");
    assert!(
        args[2].starts_with("/tmp/hssh-") && args[2].ends_with("-%C"),
        "the control path must be herdr's shared ssh directory: {call}"
    );
    assert_eq!(
        &args[3..],
        [
            "-o",
            "ControlMaster=auto",
            "-o",
            "ControlPersist=600",
            "-t",
            destination
        ],
        "{call}"
    );
}

fn assert_pane_runs_shell(test: &SshTest, pane_id: &str, marker: &str) {
    let typed = test.cli(&["pane", "run", pane_id, &format!("echo {marker}-$((40+2))")]);
    assert!(
        typed.status.success(),
        "pane run failed: {}",
        String::from_utf8_lossy(&typed.stderr)
    );
    assert!(
        wait_until(Duration::from_secs(5), Duration::from_millis(25), || {
            pane_read_recent_contains(&test.socket_path, pane_id, &format!("{marker}-42"))
        }),
        "pane {pane_id} never evaluated the typed command"
    );
}

fn pane_ssh_in_session(session: &serde_json::Value) -> Vec<serde_json::Value> {
    session["workspaces"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|workspace| workspace["tabs"].as_array().into_iter().flatten())
        .flat_map(|tab| {
            tab["panes"]
                .as_object()
                .into_iter()
                .flat_map(|panes| panes.values())
        })
        .filter_map(|pane| pane.get("ssh").cloned())
        .collect()
}

#[test]
fn workspace_create_with_ssh_runs_ssh_through_herdr_control_master() {
    let test = SshTest::new();
    let herdr = test.start_server();

    let created = test.cli_json(&[
        "workspace",
        "create",
        "--cwd",
        test.base.to_str().unwrap(),
        "--ssh",
        "prod",
    ]);

    assert_eq!(created["result"]["type"], "workspace_created");
    assert_eq!(created["result"]["root_pane"]["ssh_host"], "prod");
    let calls = test.wait_for_calls(1);
    assert_eq!(calls.len(), 1, "{calls:#?}");
    assert_multiplexed_ssh_to(&calls[0], "prod");
    let pane_id = created["result"]["root_pane"]["pane_id"].as_str().unwrap();
    assert_pane_runs_shell(&test, pane_id, "workspace-ssh");

    test.finish(herdr);
}

#[test]
fn pane_split_with_user_prefixed_ssh_host_reports_the_alias() {
    let test = SshTest::new();
    let herdr = test.start_server();
    let created = test.cli_json(&["workspace", "create", "--cwd", test.base.to_str().unwrap()]);
    let host_pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();

    let split = test.cli_json(&[
        "pane",
        "split",
        &host_pane_id,
        "--direction",
        "right",
        "--ssh",
        "admin@work",
    ]);
    let ssh_pane_id = split["result"]["pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();

    let ssh_pane = test.cli_json(&["pane", "get", &ssh_pane_id]);
    assert_eq!(ssh_pane["result"]["pane"]["ssh_host"], "work");
    let host_pane = test.cli_json(&["pane", "get", &host_pane_id]);
    assert!(
        host_pane["result"]["pane"].get("ssh_host").is_none(),
        "the split source stays a host pane: {host_pane}"
    );
    let calls = test.wait_for_calls(1);
    assert_multiplexed_ssh_to(&calls[0], "admin@work");
    assert_pane_runs_shell(&test, &ssh_pane_id, "split-ssh");

    test.finish(herdr);
}

#[test]
fn tab_create_with_unconfigured_or_pattern_host_returns_ssh_host_not_found() {
    let test = SshTest::new();
    let herdr = test.start_server();
    let created = test.cli_json(&["workspace", "create", "--cwd", test.base.to_str().unwrap()]);
    let workspace_id = created["result"]["workspace"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();
    let tabs_before = test.cli_json(&["tab", "list", "--workspace", &workspace_id]);

    // `bastion` only appears negated, so it is not an alias the user configured.
    for host in ["ghost", "bastion"] {
        let rejected = test.cli(&["tab", "create", "--workspace", &workspace_id, "--ssh", host]);

        assert_eq!(rejected.status.code(), Some(1), "{host}");
        assert_eq!(error_code(&rejected), "ssh_host_not_found", "{host}");
    }
    assert_eq!(
        test.cli_json(&["tab", "list", "--workspace", &workspace_id]),
        tabs_before,
        "a failed lookup must not leave a tab behind"
    );
    assert!(
        test.calls().is_empty(),
        "ssh must not start: {:#?}",
        test.calls()
    );

    test.finish(herdr);
}

#[test]
fn ssh_open_rejects_option_shaped_host_before_starting_ssh() {
    let test = SshTest::new();
    let herdr = test.start_server();

    for args in [
        ["--ssh", "-oProxyCommand=sh"].as_slice(),
        ["--ssh=-oProxyCommand=sh"].as_slice(),
    ] {
        let mut command = vec!["workspace", "create"];
        command.extend_from_slice(args);
        let rejected = test.cli(&command);

        assert_eq!(rejected.status.code(), Some(1), "{args:?}");
        assert_eq!(error_code(&rejected), "invalid_ssh_host", "{args:?}");
    }
    assert!(
        test.calls().is_empty(),
        "ssh must not start: {:#?}",
        test.calls()
    );
    assert_eq!(
        test.cli_json(&["workspace", "list"])["result"]["workspaces"],
        serde_json::json!([])
    );

    test.finish(herdr);
}

#[test]
fn ssh_hosts_lists_literal_aliases_through_includes() {
    let test = SshTest::new();
    let herdr = test.start_server();

    let listed = test.cli_json(&["ssh", "hosts", "--json"]);

    assert_eq!(listed["result"]["type"], "ssh_hosts");
    assert_eq!(
        listed["result"]["hosts"],
        serde_json::json!([
            {"alias": "work", "hostname": "work.example.com"},
            {"alias": "prod", "hostname": "prod.example.com", "user": "deploy"},
            {"alias": "lab"},
        ])
    );
    let table = test.cli(&["ssh", "hosts"]);
    assert!(table.status.success());
    let text = String::from_utf8_lossy(&table.stdout);
    assert!(text.starts_with("alias"), "{text}");
    assert!(text.contains("prod   prod.example.com  deploy"), "{text}");
    assert!(test.calls().is_empty(), "listing hosts must not run ssh");

    test.finish(herdr);
}

#[test]
fn ssh_hosts_without_an_ssh_client_reports_ssh_unavailable() {
    let test = SshTest::new();
    let empty_path = test.base.join("empty-path");
    fs::create_dir_all(&empty_path).unwrap();
    let herdr = test.start_server_with_path(empty_path.to_str().unwrap());

    let rejected = test.cli(&["ssh", "hosts"]);

    assert_eq!(rejected.status.code(), Some(1));
    assert_eq!(error_code(&rejected), "ssh_unavailable");

    test.finish(herdr);
}

#[test]
fn ssh_launch_survives_session_snapshot_and_restores_a_host_shell() {
    let test = SshTest::new();
    let mut herdr = test.start_server();
    test.cli_json(&[
        "workspace",
        "create",
        "--cwd",
        test.base.to_str().unwrap(),
        "--ssh",
        "prod",
    ]);
    let control_path = test.wait_for_calls(1)[0]
        .split(' ')
        .nth(2)
        .unwrap()
        .to_string();

    assert!(test.cli(&["server", "stop"]).status.success());
    let pid = herdr.child.process_id();
    assert!(herdr.child.wait().unwrap().success());
    unregister_spawned_herdr_pid(pid);
    drop(herdr);

    let session: serde_json::Value =
        serde_json::from_slice(&fs::read(test.session_file()).unwrap()).unwrap();
    assert_eq!(
        pane_ssh_in_session(&session),
        vec![
            serde_json::json!({"host": "prod", "control_path": control_path, "has_remote_command": false})
        ]
    );

    let restarted = test.start_server();
    let panes = test.cli_json(&["pane", "list"]);
    let restored: Vec<&serde_json::Value> = panes["result"]["panes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|pane| &pane["ssh_host"])
        .collect();
    assert_eq!(
        restored,
        vec!["prod"],
        "the restored pane keeps its ssh host"
    );
    let pane_id = panes["result"]["panes"][0]["pane_id"].as_str().unwrap();
    assert_pane_runs_shell(&test, pane_id, "restored-host");
    assert_eq!(
        test.calls().len(),
        1,
        "cold restore starts a host shell instead of reconnecting: {:#?}",
        test.calls()
    );

    test.finish(restarted);
}
