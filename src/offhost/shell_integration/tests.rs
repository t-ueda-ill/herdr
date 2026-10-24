use super::*;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("herdr-launcher-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn installed(shell: &str) -> bool {
    Command::new(shell).arg("--version").output().is_ok()
        || Command::new(shell).args(["-c", "exit 0"]).output().is_ok()
}

fn fake_shell(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, "#!/bin/sh\necho \"pwd=$(pwd) args=$*\"\n").expect("fake shell");
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .expect("chmod");
    path
}

/// Runs the launcher without a terminal and returns stdout.
fn run_launcher(args: &[&str], envs: &[(&str, &str)]) -> String {
    let mut command = Command::new("sh");
    command
        .args(["-c", LAUNCHER, "sh"])
        .args(args)
        .stdin(Stdio::null());
    for (key, value) in envs {
        command.env(key, value);
    }
    let output = command.output().expect("sh runs");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn strings(argv: Vec<OsString>) -> Vec<String> {
    argv.into_iter()
        .map(|arg| arg.into_string().expect("utf-8"))
        .collect()
}

#[test]
fn launcher_enters_the_start_dir_before_the_shell_starts() {
    let dir = scratch("enter");
    let shell = fake_shell(&dir, "fake");
    let out = run_launcher(
        &[dir.to_str().unwrap(), shell.to_str().unwrap(), "0", "", ""],
        &[],
    );
    assert_eq!(out.trim(), format!("pwd={} args=", dir.display()));
}

#[test]
fn launcher_stays_where_it_is_when_the_start_dir_is_missing() {
    let dir = scratch("missing");
    let shell = fake_shell(&dir, "fake");
    let out = run_launcher(
        &["/nonexistent/herdr", shell.to_str().unwrap(), "0", "", ""],
        &[],
    );
    let here = std::env::current_dir().unwrap();
    assert_eq!(out.trim(), format!("pwd={} args=", here.display()));
}

#[test]
fn launcher_stays_where_it_is_without_a_start_dir() {
    let dir = scratch("nostart");
    let shell = fake_shell(&dir, "fake");
    let out = run_launcher(&["", shell.to_str().unwrap(), "0", "", ""], &[]);
    let here = std::env::current_dir().unwrap();
    assert_eq!(out.trim(), format!("pwd={} args=", here.display()));
}

#[test]
fn launcher_passes_the_login_flag_to_the_plain_shell() {
    let dir = scratch("login");
    let shell = fake_shell(&dir, "fake");
    let out = run_launcher(&["", shell.to_str().unwrap(), "1", "", ""], &[]);
    assert!(out.trim().ends_with("args=-l"), "{out}");
}

#[test]
fn launcher_runs_the_account_shell_when_none_is_named() {
    let dir = scratch("account");
    let shell = fake_shell(&dir, "fake");
    let out = run_launcher(
        &["", "", "0", "", ""],
        &[("SHELL", shell.to_str().unwrap())],
    );
    assert!(out.contains("args="), "{out}");
}

#[test]
fn launcher_picks_bash_when_bash_or_sh_and_bash_exists() {
    let dir = scratch("probe");
    fake_shell(&dir, "bash");
    let path = format!("{}:/usr/bin:/bin", dir.display());
    let out = run_launcher(&["", "bash-or-sh", "1", "", ""], &[("PATH", &path)]);
    assert!(out.trim().ends_with("args=-l"), "{out}");
}

#[test]
fn launcher_starts_a_plain_shell_when_mktemp_fails() {
    let dir = scratch("mktemp");
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    fake_shell(&bin, "bash");
    let mktemp = bin.join("mktemp");
    std::fs::write(&mktemp, "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(&mktemp, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let path = format!("{}:/usr/bin:/bin", bin.display());
    let out = run_launcher(
        &["", "bash", "0", "body", "body"],
        &[("PATH", &path), ("TMPDIR", dir.to_str().unwrap())],
    );
    assert_eq!(
        out.trim(),
        format!("pwd={} args=", std::env::current_dir().unwrap().display())
    );
}

#[test]
fn launcher_starts_a_plain_shell_for_a_shell_it_cannot_integrate_with() {
    let dir = scratch("other");
    let tmp = dir.join("tmp");
    std::fs::create_dir_all(&tmp).unwrap();
    let shell = fake_shell(&dir, "ksh");
    let out = run_launcher(
        &["", shell.to_str().unwrap(), "0", "body", "body"],
        &[("TMPDIR", tmp.to_str().unwrap())],
    );
    assert!(out.contains("args="), "{out}");
    assert_eq!(std::fs::read_dir(&tmp).unwrap().count(), 0);
}

#[test]
fn launcher_script_is_valid_posix_sh() {
    for shell in ["sh", "dash", "bash"] {
        if !installed(shell) {
            continue;
        }
        let output = Command::new(shell)
            .args(["-n", "-c", LAUNCHER])
            .output()
            .expect("shell runs");
        assert!(
            output.status.success(),
            "{shell}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn posix_body_is_valid_in_bash_dash_and_zsh() {
    let body = injected_integration_posix();
    for shell in ["bash", "dash", "zsh"] {
        if !installed(shell) {
            continue;
        }
        let output = Command::new(shell)
            .args(["-n", "-c", &body])
            .output()
            .expect("shell runs");
        assert!(
            output.status.success(),
            "{shell}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn fish_body_is_valid_fish() {
    if !installed("fish") {
        return;
    }
    let output = Command::new("fish")
        .args(["-n", "-c", &injected_integration_fish()])
        .output()
        .expect("fish runs");
    assert!(output.status.success());
}

/// Output of an interactive shell started through the launcher on a pty.
/// The pty matters: the reports go to `/dev/tty`, which pipes do not have.
fn run_in_pty(args: &[&str], home: &Path, tmp: &Path, input: &str) -> String {
    run_in_pty_with_script(LAUNCHER, args, home, tmp, input)
}

fn run_in_pty_with_script(
    script: &str,
    args: &[&str],
    home: &Path,
    tmp: &Path,
    input: &str,
) -> String {
    let pair = portable_pty::native_pty_system()
        .openpty(portable_pty::PtySize {
            rows: 24,
            cols: 200,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("pty");
    let mut command = portable_pty::CommandBuilder::new("sh");
    command.args(["-c", script, "sh"]);
    command.args(args);
    command.env_clear();
    command.env("PATH", "/usr/bin:/bin");
    command.env("HOME", home);
    command.env("TMPDIR", tmp);
    command.env("TERM", "dumb");
    command.env("HOSTNAME", "testhost");
    let mut child = pair.slave.spawn_command(command).expect("spawn");
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().expect("reader");
    let mut writer = pair.master.take_writer().expect("writer");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    // The shell reads its input line by line, so each line waits for the
    // prompt before it is typed; a fixed pause is enough for dash and bash.
    for line in input.lines() {
        std::thread::sleep(Duration::from_millis(300));
        writer
            .write_all(format!("{line}\n").as_bytes())
            .expect("write");
    }
    let mut out = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(chunk) => out.extend(chunk),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if matches!(child.try_wait(), Ok(Some(_))) {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let _ = child.kill();
    String::from_utf8_lossy(&out).into_owned()
}

/// Paths carried by OSC 7 reports, in order.
fn osc7_paths(output: &str) -> Vec<String> {
    output
        .split("\x1b]7;file://testhost")
        .skip(1)
        .filter_map(|rest| rest.split('\x07').next())
        .map(str::to_owned)
        .collect()
}

fn pty_report_test(shell: &str, rc_file: &str, rc_body: &str, login: &str) {
    if !installed(shell) {
        eprintln!("skipping: {shell} is not installed");
        return;
    }
    let root = scratch(&format!("pty-{shell}-{login}"));
    let home = root.join("home");
    let tmp = root.join("tmp");
    let tricky = root.join("a b%41");
    for dir in [&home, &tmp, &tricky] {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(home.join(rc_file), rc_body).unwrap();
    let integration = injected_integration_posix();
    let input = format!(
        "cd '{}'\nprintf 'rc=%s\\n' \"$HERDR_TEST_RC\"\nexit",
        tricky.display()
    );
    let out = run_in_pty(
        &[
            root.to_str().unwrap(),
            shell,
            login,
            &integration,
            &injected_integration_fish(),
        ],
        &home,
        &tmp,
        &input,
    );
    let paths = osc7_paths(&out);
    let encoded = format!("{}/a b%2541", root.display());
    assert_eq!(
        paths.first().map(String::as_str),
        Some(root.to_str().unwrap()),
        "start report: {out:?}"
    );
    assert!(paths.contains(&encoded), "{paths:?} in {out:?}");
    assert!(out.contains("rc=yes"), "user rc file was not read: {out:?}");
    assert_eq!(
        std::fs::read_dir(&tmp).unwrap().count(),
        0,
        "the integration directory is removed"
    );
}

#[test]
fn bash_reports_its_directory_after_the_users_rc_file() {
    pty_report_test("bash", ".bashrc", "export HERDR_TEST_RC=yes\n", "0");
}

#[test]
fn login_bash_reads_the_profile_and_reports_its_directory() {
    pty_report_test("bash", ".bash_profile", "export HERDR_TEST_RC=yes\n", "1");
}

#[test]
fn dash_reports_its_directory_after_the_users_env_file() {
    if !installed("dash") {
        return;
    }
    let root = scratch("pty-dash-env");
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    // Why `ENV` through the launcher environment: the launcher keeps the
    // user's `$ENV` file, so it must still be sourced.
    let envfile = home.join("env");
    std::fs::write(&envfile, "HERDR_TEST_RC=yes\n").unwrap();
    let tmp = root.join("tmp");
    std::fs::create_dir_all(&tmp).unwrap();
    let tricky = root.join("a b%41");
    std::fs::create_dir_all(&tricky).unwrap();
    let launcher_env = format!("ENV='{}'; export ENV; ", envfile.display());
    let wrapped = format!("{launcher_env}{LAUNCHER}");
    let integration = injected_integration_posix();
    let pair = (wrapped, integration);
    let out = {
        // Same pty harness, with the launcher text replaced by one that
        // exports `ENV` first.
        run_in_pty_with_script(
            &pair.0,
            &[root.to_str().unwrap(), "dash", "0", &pair.1, ""],
            &home,
            &tmp,
            &format!(
                "cd '{}'\nprintf 'rc=%s\\n' \"$HERDR_TEST_RC\"\nexit",
                tricky.display()
            ),
        )
    };
    let paths = osc7_paths(&out);
    assert_eq!(paths.first().map(String::as_str), root.to_str());
    assert!(
        paths.contains(&format!("{}/a b%2541", root.display())),
        "{out:?}"
    );
    assert!(out.contains("rc=yes"), "{out:?}");
    assert_eq!(std::fs::read_dir(&tmp).unwrap().count(), 0);
}

#[test]
fn bash_keeps_the_users_prompt_command() {
    if !installed("bash") {
        return;
    }
    let root = scratch("pty-bash-pc");
    let home = root.join("home");
    let tmp = root.join("tmp");
    for dir in [&home, &tmp] {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(
        home.join(".bashrc"),
        "PROMPT_COMMAND='echo prompt-command-ran'\n",
    )
    .unwrap();
    let out = run_in_pty(
        &[
            root.to_str().unwrap(),
            "bash",
            "0",
            &injected_integration_posix(),
            "",
        ],
        &home,
        &tmp,
        "true\nexit",
    );
    assert!(out.contains("prompt-command-ran"), "{out:?}");
    assert!(!osc7_paths(&out).is_empty(), "{out:?}");
}

#[test]
fn zsh_reports_its_directory_after_the_users_rc_file() {
    if !installed("zsh") {
        return;
    }
    let root = scratch("pty-zsh");
    let home = root.join("home");
    let tmp = root.join("tmp");
    let tricky = root.join("a b%41");
    for dir in [&home, &tmp, &tricky] {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(home.join(".zshrc"), "export HERDR_TEST_RC=yes\n").unwrap();
    let out = run_in_pty(
        &[
            root.to_str().unwrap(),
            "zsh",
            "0",
            &injected_integration_posix(),
            "",
        ],
        &home,
        &tmp,
        &format!(
            "cd '{}'\nprintf 'rc=%s\\n' \"$HERDR_TEST_RC\"\nexit",
            tricky.display()
        ),
    );
    let paths = osc7_paths(&out);
    assert!(
        paths.contains(&format!("{}/a b%2541", root.display())),
        "{out:?}"
    );
    assert!(out.contains("rc=yes"), "{out:?}");
    assert_eq!(std::fs::read_dir(&tmp).unwrap().count(), 0);
}

#[test]
fn fish_reports_its_directory_after_the_users_config() {
    if !installed("fish") {
        return;
    }
    let root = scratch("pty-fish");
    let home = root.join("home");
    let tmp = root.join("tmp");
    let tricky = root.join("a b%41");
    for dir in [&home, &tmp, &tricky] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let out = run_in_pty(
        &[
            root.to_str().unwrap(),
            "fish",
            "0",
            "",
            &injected_integration_fish(),
        ],
        &home,
        &tmp,
        &format!("cd '{}'\nexit", tricky.display()),
    );
    let paths = osc7_paths(&out);
    assert!(
        paths.contains(&format!("{}/a b%2541", root.display())),
        "{out:?}"
    );
}

#[test]
fn launcher_without_integration_starts_the_plain_shell_in_the_start_dir() {
    let argv = strings(launcher_argv(
        Some(Path::new("/srv/app")),
        LauncherShell::BashOrSh,
        true,
        false,
    ));
    assert_eq!(
        argv,
        [
            "sh",
            "-c",
            LAUNCHER,
            "sh",
            "/srv/app",
            "bash-or-sh",
            "1",
            "",
            ""
        ]
    );
}

#[test]
fn launcher_argv_carries_both_integration_bodies_when_enabled() {
    let argv = strings(launcher_argv(None, LauncherShell::Account, false, true));
    assert_eq!(argv[4..7], ["", "", "0"]);
    assert_eq!(argv[7], injected_integration_posix());
    assert_eq!(argv[8], injected_integration_fish());
}

#[test]
fn launcher_argv_names_an_explicit_shell() {
    let argv = strings(launcher_argv(
        None,
        LauncherShell::Explicit("/bin/zsh"),
        false,
        false,
    ));
    assert_eq!(argv[5], "/bin/zsh");
}

#[test]
fn launcher_is_needed_for_integration_or_a_start_dir_only() {
    assert!(needs_launcher(true, None));
    assert!(needs_launcher(false, Some(Path::new("/x"))));
    assert!(!needs_launcher(false, None));
}

/// Starts the fake agents in an interactive shell from the launcher and
/// returns the pty output. `claude` and `cursor-agent` are executables in
/// `$HOME/bin`.
fn run_fake_agents(shell: &str, input: &str) -> String {
    run_fake_agents_with_rc(shell, "", input)
}

fn run_fake_agents_with_rc(shell: &str, rc: &str, input: &str) -> String {
    run_fake_agents_with(shell, rc, "echo \"ran-$0 $*\"\n", input)
}

/// `fake_body` is the shell script behind the fake `claude` and
/// `cursor-agent`.
fn run_fake_agents_with(shell: &str, rc: &str, fake_body: &str, input: &str) -> String {
    let root = scratch(&format!("agents-{shell}"));
    let home = root.join("home");
    let tmp = root.join("tmp");
    let bin = home.join("bin");
    for dir in [&tmp, &bin] {
        std::fs::create_dir_all(dir).unwrap();
    }
    for name in ["claude", "cursor-agent"] {
        let path = bin.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{fake_body}")).unwrap();
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
    }
    std::fs::write(home.join(".bashrc"), rc).unwrap();
    std::fs::write(home.join(".zshrc"), rc).unwrap();
    run_in_pty(
        &[
            root.to_str().unwrap(),
            shell,
            "0",
            &injected_integration_posix(),
            &injected_integration_fish(),
        ],
        &home,
        &tmp,
        &format!("PATH=$HOME/bin:$PATH\n{input}\nexit"),
    )
}

/// An rc prefix that makes a runaway wrapper recursion fail to fork instead
/// of exhausting the machine.
/// Why threads on Linux and processes elsewhere: Linux counts threads against
/// `ulimit -u`, macOS counts processes. Why `/proc` and not whether `ps -L`
/// works: macOS `ps -L` succeeds too, printing its keyword list.
const FORK_GUARD_RC: &str = "if [ -d /proc/self/task ]; then\n\
  __herdr_n=$(ps -u $(id -u) -L -o lwp= | wc -l)\n\
else\n\
  __herdr_n=$(ps -u $(id -u) -o pid= | wc -l)\n\
fi\n\
ulimit -u $(( __herdr_n + 80 )) 2>/dev/null || :\n";

/// `(verb, token, label)` of each agent claim in order; exit has no label.
fn agent_claims(output: &str) -> Vec<(String, String, String)> {
    output
        .split("\x1b]43737;herdr-agent;v=1;")
        .skip(1)
        .filter_map(|rest| rest.split('\x07').next())
        .map(|claim| {
            let mut fields = claim.split(';');
            (
                fields.next().unwrap_or_default().to_owned(),
                fields.next().unwrap_or_default().to_owned(),
                fields.next().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

fn assert_start_then_exit(output: &str, label: &str) {
    let claims = agent_claims(output);
    assert_eq!(claims.len(), 2, "{output:?}");
    assert_eq!(claims[0].0, "start");
    assert_eq!(claims[0].2, label);
    assert_eq!(claims[1].0, "exit");
    assert_eq!(claims[0].1, claims[1].1, "exit carries the start token");
    assert!(!claims[0].1.is_empty());
}

#[test]
fn bash_announces_a_wrapped_agent_with_its_label_and_still_reports_the_directory() {
    if !installed("bash") {
        return;
    }
    let out = run_fake_agents("bash", "claude");
    assert_start_then_exit(&out, "claude");
    assert!(out.contains("ran-"), "{out:?}");
    assert!(!osc7_paths(&out).is_empty(), "{out:?}");
}

#[test]
fn bash_maps_a_command_that_differs_from_its_label() {
    if !installed("bash") {
        return;
    }
    let out = run_fake_agents("bash", "cursor-agent");
    assert_start_then_exit(&out, "cursor");
}

#[test]
fn dash_announces_a_wrapped_agent_with_its_label_and_still_reports_the_directory() {
    if !installed("dash") {
        return;
    }
    let out = run_fake_agents("dash", "claude");
    assert_start_then_exit(&out, "claude");
    assert!(out.contains("ran-"), "{out:?}");
    assert!(!osc7_paths(&out).is_empty(), "{out:?}");
}

#[test]
fn dash_runs_a_dashed_command_without_announcing_it() {
    if !installed("dash") {
        return;
    }
    let out = run_fake_agents("dash", "cursor-agent");
    assert!(agent_claims(&out).is_empty(), "{out:?}");
    assert!(out.contains("ran-"), "{out:?}");
}

#[test]
fn bash_keeps_the_users_alias_for_a_wrapped_command() {
    if !installed("bash") {
        return;
    }
    let out = run_fake_agents_with_rc(
        "bash",
        "alias claude='$HOME/bin/claude --flag'\n",
        "claude hi",
    );
    assert_start_then_exit(&out, "claude");
    assert!(out.contains("--flag hi"), "{out:?}");
}

fn assert_alias_for_the_wrapped_command_runs_the_real_one(shell: &str, command: &str, label: &str) {
    let rc = format!("{FORK_GUARD_RC}alias {command}='{command} --from-alias'\n");
    let out = run_fake_agents_with(
        shell,
        &rc,
        "echo \"fake $*\"\n",
        &format!("{command} x\necho back-at-prompt-$((1+1))"),
    );
    assert_start_then_exit(&out, label);
    assert!(out.contains("fake --from-alias x"), "{out:?}");
    assert!(out.contains("back-at-prompt-2"), "{out:?}");
}

#[test]
fn bash_runs_the_real_command_when_the_alias_wraps_the_same_name() {
    if !installed("bash") {
        return;
    }
    assert_alias_for_the_wrapped_command_runs_the_real_one("bash", "claude", "claude");
}

#[test]
fn bash_runs_the_real_dashed_command_when_the_alias_wraps_the_same_name() {
    if !installed("bash") {
        return;
    }
    assert_alias_for_the_wrapped_command_runs_the_real_one("bash", "cursor-agent", "cursor");
}

#[test]
fn zsh_runs_the_real_command_when_the_alias_wraps_the_same_name() {
    if !installed("zsh") {
        return;
    }
    assert_alias_for_the_wrapped_command_runs_the_real_one("zsh", "claude", "claude");
}

#[test]
fn zsh_runs_the_real_dashed_command_when_the_alias_wraps_the_same_name() {
    if !installed("zsh") {
        return;
    }
    assert_alias_for_the_wrapped_command_runs_the_real_one("zsh", "cursor-agent", "cursor");
}

fn assert_ctrl_c_ends_the_claim_and_returns_to_the_prompt(shell: &str) {
    let out = run_fake_agents_with(
        shell,
        "",
        "echo fake-started\nsleep 5\n",
        "claude x\n\x03\necho back-at-prompt-$((1+1))",
    );
    assert_start_then_exit(&out, "claude");
    assert!(out.contains("back-at-prompt-2"), "{out:?}");
}

#[test]
fn bash_announces_exit_and_returns_to_the_prompt_after_ctrl_c() {
    if !installed("bash") {
        return;
    }
    assert_ctrl_c_ends_the_claim_and_returns_to_the_prompt("bash");
}

#[test]
fn zsh_announces_exit_and_returns_to_the_prompt_after_ctrl_c() {
    if !installed("zsh") {
        return;
    }
    assert_ctrl_c_ends_the_claim_and_returns_to_the_prompt("zsh");
}
