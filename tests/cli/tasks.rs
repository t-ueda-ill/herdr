use std::fs;
use std::time::Duration;

use super::harness::*;

/// Starts a fake `pi` that answers prompts through `pane report-agent`.
fn start_fake_agent() -> (SpawnedHerdr, std::path::PathBuf, std::path::PathBuf, String) {
    use std::os::unix::fs::PermissionsExt;

    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let socket_path = runtime_dir.join("herdr.sock");
    let bin = base.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let fake_pi = bin.join("pi");
    fs::write(
        &fake_pi,
        format!(
            "#!/bin/sh\nexport HERDR_AGENT=pi\nreport() {{ '{0}' pane report-agent \"$HERDR_PANE_ID\" --source custom:fake-pi --agent pi --state \"$1\" >/dev/null; }}\nreport idle\nwhile IFS= read -r prompt; do\n  case \"$prompt\" in\n    \"stay silent\") continue ;;\n    \"ask first\") report blocked; continue ;;\n  esac\n  report working\n  sleep 0.3\n  report idle\ndone\n",
            env!("CARGO_BIN_EXE_herdr"),
        ),
    )
    .unwrap();
    fs::set_permissions(&fake_pi, fs::Permissions::from_mode(0o755)).unwrap();

    let herdr = spawn_herdr_with_path(&config_home, &runtime_dir, &socket_path, Some(&bin));
    wait_for_socket(&socket_path, Duration::from_secs(5));
    let created = run_cli_json(
        &socket_path,
        &["workspace", "create", "--cwd", base.to_str().unwrap()],
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    let started = run_cli_json(
        &socket_path,
        &[
            "agent",
            "start",
            "main",
            "--kind",
            "pi",
            "--pane",
            &pane_id,
            "--timeout",
            "8000",
        ],
    );
    assert_eq!(started["result"]["type"], "agent_started");
    (herdr, base, socket_path, pane_id)
}

fn report(socket_path: &std::path::Path, pane_id: &str, state: &str) {
    let output = run_cli(
        socket_path,
        &[
            "pane",
            "report-agent",
            pane_id,
            "--source",
            "custom:fake-pi",
            "--agent",
            "pi",
            "--state",
            state,
        ],
    );
    assert!(output.status.success(), "report {state} failed");
}

fn stdout_json(output: &std::process::Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
        panic!(
            "stdout is not JSON: {} / stderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

#[test]
fn a_task_follows_the_agent_turn_from_submit_to_completed() {
    let (herdr, base, socket_path, _pane_id) = start_fake_agent();

    let done = run_cli(
        &socket_path,
        &[
            "task",
            "submit",
            "main",
            "Review this diff",
            "--wait",
            "--timeout",
            "5000",
        ],
    );

    assert!(
        done.status.success(),
        "{}",
        String::from_utf8_lossy(&done.stderr)
    );
    let task = &stdout_json(&done)["result"]["task"];
    assert_eq!(task["state"], "completed");
    assert_eq!(task["location"]["id"], "local");
    assert!(task["evidence"].is_string());

    let list = run_cli_json(&socket_path, &["task", "list", "--location", "local"]);
    assert_eq!(list["result"]["tasks"][0]["task_id"], task["task_id"]);
    let elsewhere = run_cli_json(&socket_path, &["task", "list", "--location", "ssh:devbox"]);
    assert_eq!(elsewhere["result"]["tasks"], serde_json::json!([]));

    cleanup_spawned_herdr(herdr, base);
}

#[test]
fn a_task_for_a_working_agent_waits_and_can_be_cancelled() {
    let (herdr, base, socket_path, pane_id) = start_fake_agent();
    report(&socket_path, &pane_id, "working");

    let queued = run_cli_json(&socket_path, &["task", "submit", "main", "first"]);
    let first = queued["result"]["task"]["task_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(queued["result"]["task"]["state"], "queued");
    let second = run_cli_json(&socket_path, &["task", "submit", "main", "second"]);
    let second = second["result"]["task"]["task_id"]
        .as_str()
        .unwrap()
        .to_string();

    let cancelled = run_cli_json(&socket_path, &["task", "cancel", &second]);
    assert_eq!(cancelled["result"]["task"]["state"], "cancelled");

    report(&socket_path, &pane_id, "idle");
    let waited = run_cli(&socket_path, &["task", "wait", &first, "--timeout", "5000"]);
    assert!(
        waited.status.success(),
        "{}",
        String::from_utf8_lossy(&waited.stderr)
    );
    assert_eq!(stdout_json(&waited)["result"]["task"]["state"], "completed");

    let refused = run_cli(&socket_path, &["task", "cancel", &first]);
    assert_eq!(refused.status.code(), Some(1));
    let refused: serde_json::Value = serde_json::from_slice(&refused.stderr).unwrap();
    assert_eq!(refused["error"]["code"], "task_already_sent");

    cleanup_spawned_herdr(herdr, base);
}

#[test]
fn a_task_that_needs_an_answer_stops_a_wait_that_asks_for_it() {
    let (herdr, base, socket_path, _pane_id) = start_fake_agent();

    let asked = run_cli(
        &socket_path,
        &[
            "task",
            "submit",
            "main",
            "ask first",
            "--wait",
            "--until",
            "needs_input",
            "--timeout",
            "5000",
        ],
    );

    assert!(
        asked.status.success(),
        "{}",
        String::from_utf8_lossy(&asked.stderr)
    );
    assert_eq!(
        stdout_json(&asked)["result"]["task"]["state"],
        "needs_input"
    );

    cleanup_spawned_herdr(herdr, base);
}

#[test]
fn a_task_without_any_reaction_stalls_and_fails_the_wait() {
    let (herdr, base, socket_path, _pane_id) = start_fake_agent();

    let silent = run_cli(
        &socket_path,
        &[
            "task",
            "submit",
            "main",
            "stay silent",
            "--wait",
            "--timeout",
            "9000",
        ],
    );

    assert_eq!(silent.status.code(), Some(1));
    let task = &stdout_json(&silent)["result"]["task"];
    assert_eq!(task["state"], "stalled");
    assert!(task["reason"].is_string());

    cleanup_spawned_herdr(herdr, base);
}
