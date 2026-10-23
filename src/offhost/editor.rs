//! The host-side argv of the overlay pane that edits a file inside a pane's
//! environment. The overlay is interactive, so unlike [`super::exec`] it uses a
//! tty and may prompt (an ssh password, for example).

use crate::container::launch::BASH_OR_SH_PROBE;
use crate::container::ContainerShell;
use crate::ssh_host::launch::{shell_quote, ssh_command_argv};

use super::exec::PaneEnvironment;

/// Run by a POSIX shell with the file as `$1`. Why `eval` and not a plain
/// `${EDITOR:-vi} "$1"`: zsh does not split an unquoted expansion, so
/// `EDITOR='code --wait'` would be one command name there.
/// Why `\$edit_file`: the path must reach the editor as data, never as shell text.
pub(crate) const EDITOR_SCRIPT: &str = r#"edit_file=$1; eval "${EDITOR:-vi} \"\$edit_file\"""#;

/// Same, with the directory to start in as `$1` and the file as `$2`.
const SSH_INNER_SCRIPT: &str =
    r#"[ -n "$1" ] && cd -- "$1" 2>/dev/null; edit_file=$2; eval "${EDITOR:-vi} \"\$edit_file\"""#;

/// Runs under `sh` whatever the account's login shell is, so it needs no
/// knowledge of fish or csh syntax. `$1` is the directory, `$2` the file.
///
/// The user's `EDITOR` lives in their login environment: a sh-family shell
/// runs the whole script itself (`-l` reads the profile files); any other
/// shell only reports `EDITOR`, which the POSIX `sh` then uses.
/// Why not `-i`: an interactive shell would read `.bashrc`-style files that may
/// print banners or start a multiplexer.
const SSH_WRAPPER: &str = r#"
case "${SHELL##*/}" in
  sh|bash|zsh|dash|ash|ksh|mksh|yash)
    exec "$SHELL" -l -c "$0" sh "$1" "$2" ;;
esac
if [ -n "$SHELL" ]; then
  EDITOR=$("$SHELL" -l -c 'printf "%s" "$EDITOR"' </dev/null 2>/dev/null)
  export EDITOR
fi
exec sh -c "$0" sh "$1" "$2"
"#;

/// Runs under `sh` inside a container whose configured shell cannot run POSIX
/// scripts (fish). `$0` is the editor script, `$1` the shell, `$2` `1` when the
/// pane uses a login shell, `$3` the file.
/// Why not run the script in that shell: `docker exec` does not inherit the
/// pane shell's environment, so only the shell's own startup files know `EDITOR`.
const CONTAINER_FALLBACK_WRAPPER: &str = r#"
if [ "$2" = 1 ]; then
  reported=$("$1" -l -c 'printf "%s" "$EDITOR"' </dev/null 2>/dev/null)
else
  reported=$("$1" -c 'printf "%s" "$EDITOR"' </dev/null 2>/dev/null)
fi
if [ -n "$reported" ]; then
  EDITOR=$reported
  export EDITOR
fi
exec sh -c "$0" sh "$3"
"#;

/// Shells whose `-c` accepts the POSIX scripts above.
fn is_sh_family(shell: &str) -> bool {
    let name = shell.rsplit('/').next().unwrap_or(shell);
    matches!(
        name,
        "sh" | "bash" | "zsh" | "dash" | "ash" | "ksh" | "mksh" | "yash"
    )
}

/// `cwd` is the pane's own directory, so relative paths in `$EDITOR`'s
/// config and git discovery behave as they do in the pane.
///
/// Containers use the pane's login mode. An ssh pane is `ssh -t host` with no
/// command, which sshd answers with a login shell, so the editor always gets
/// one too.
pub(crate) fn editor_argv(
    env: &PaneEnvironment,
    cwd: Option<&str>,
    login: bool,
    path: &str,
) -> Vec<String> {
    match env {
        PaneEnvironment::Container(launch) => {
            let mut docker = vec!["exec".to_owned(), "-it".to_owned()];
            if let Some(user) = &launch.user {
                docker.extend(["-u".to_owned(), user.clone()]);
            }
            if let Some(cwd) = cwd {
                docker.extend(["-w".to_owned(), cwd.to_owned()]);
            }
            docker.push(launch.id.clone());
            let strs = |parts: &[&str]| -> Vec<String> {
                parts.iter().map(|part| (*part).to_owned()).collect()
            };
            match &launch.shell {
                ContainerShell::Explicit(shell) if is_sh_family(shell) => {
                    docker.push(shell.clone());
                    docker.extend(login.then(|| "-l".to_owned()));
                    docker.extend(strs(&["-c", EDITOR_SCRIPT, "sh", path]));
                }
                // Why a wrapper: a fish label's `-c` cannot run the POSIX script.
                ContainerShell::Explicit(shell) => {
                    docker.extend(strs(&[
                        "sh",
                        "-c",
                        CONTAINER_FALLBACK_WRAPPER,
                        EDITOR_SCRIPT,
                    ]));
                    docker.extend([shell.clone(), if login { "1" } else { "0" }.to_owned()]);
                    docker.push(path.to_owned());
                }
                ContainerShell::BashOrSh => {
                    docker.extend(strs(&["sh", "-c", BASH_OR_SH_PROBE, "sh"]));
                    docker.extend(login.then(|| "-l".to_owned()));
                    docker.extend(strs(&["-c", EDITOR_SCRIPT, "sh", path]));
                }
            }
            match &launch.ssh {
                None => std::iter::once("docker".to_owned()).chain(docker).collect(),
                Some(ssh) => {
                    // Why one quoted string: ssh joins its arguments with spaces
                    // for the remote login shell, which would re-split the script.
                    let command = std::iter::once("docker".to_owned())
                        .chain(docker.iter().map(|arg| shell_quote(arg)))
                        .collect::<Vec<_>>()
                        .join(" ");
                    lossy(ssh_command_argv(ssh, command.into()))
                }
            }
        }
        PaneEnvironment::Ssh(launch) => {
            // `sh -c WRAPPER INNER CWD FILE`: `$0` of the wrapper is the inner script.
            let command = [
                "sh",
                "-c",
                SSH_WRAPPER,
                SSH_INNER_SCRIPT,
                cwd.unwrap_or(""),
                path,
            ]
            .into_iter()
            .map(shell_quote)
            .collect::<Vec<_>>()
            .join(" ");
            lossy(ssh_command_argv(launch, command.into()))
        }
    }
}

fn lossy(argv: Vec<std::ffi::OsString>) -> Vec<String> {
    argv.into_iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::container::{ContainerLaunch, ContainerRef};
    use crate::ssh_host::SshLaunch;

    fn container(shell: ContainerShell, ssh: Option<SshLaunch>) -> PaneEnvironment {
        PaneEnvironment::Container(Box::new(ContainerLaunch {
            reference: ContainerRef::Name("scratch".into()),
            canonical: "scratch".into(),
            id: "cid".into(),
            workdir: Some("/work".into()),
            start_dir: None,
            user: Some("dev".into()),
            shell,
            ssh,
        }))
    }

    fn ssh() -> SshLaunch {
        SshLaunch::new("deploy@devbox".into(), Some("/tmp/ctl-%C".into())).unwrap()
    }

    #[test]
    fn container_editor_enters_as_the_pane_user_in_the_pane_directory_with_a_tty() {
        let argv = editor_argv(
            &container(ContainerShell::BashOrSh, None),
            Some("/srv/app"),
            false,
            "/srv/app/main.rs",
        );

        assert_eq!(
            argv,
            [
                "docker",
                "exec",
                "-it",
                "-u",
                "dev",
                "-w",
                "/srv/app",
                "cid",
                "sh",
                "-c",
                BASH_OR_SH_PROBE,
                "sh",
                "-c",
                EDITOR_SCRIPT,
                "sh",
                "/srv/app/main.rs"
            ]
        );
    }

    #[test]
    fn container_editor_uses_the_labelled_shell_and_the_pane_login_mode() {
        let argv = editor_argv(
            &container(ContainerShell::Explicit("/bin/zsh".into()), None),
            None,
            true,
            "/f",
        );

        assert_eq!(
            argv,
            [
                "docker",
                "exec",
                "-it",
                "-u",
                "dev",
                "cid",
                "/bin/zsh",
                "-l",
                "-c",
                EDITOR_SCRIPT,
                "sh",
                "/f"
            ]
        );
    }

    #[test]
    fn container_editor_asks_a_non_posix_shell_for_editor_through_a_sh_wrapper() {
        let argv = editor_argv(
            &container(ContainerShell::Explicit("/usr/bin/fish".into()), None),
            None,
            true,
            "/f",
        );

        assert_eq!(
            argv,
            [
                "docker",
                "exec",
                "-it",
                "-u",
                "dev",
                "cid",
                "sh",
                "-c",
                CONTAINER_FALLBACK_WRAPPER,
                EDITOR_SCRIPT,
                "/usr/bin/fish",
                "1",
                "/f"
            ]
        );
    }

    /// Runs what `docker exec` would run inside the container, with a fake
    /// shell that answers like fish: it only knows how to print `EDITOR`.
    #[cfg(unix)]
    fn run_container_with_fake_fish(login: bool, inherited_editor: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let tools =
            std::env::temp_dir().join(format!("herdr-cfish-{}-{login}", std::process::id()));
        std::fs::create_dir_all(&tools).unwrap();
        let fish = tools.join("fish");
        let expected = if login { "-l -c" } else { "-c" };
        std::fs::write(
            &fish,
            format!(
                "#!/bin/sh\ncase \"$*\" in \"{expected} \"*EDITOR*) printf '%s' \"printf '%s|' FISH-EDITOR\" ;; *) echo fish-ran-script; exit 1 ;; esac\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fish, std::fs::Permissions::from_mode(0o755)).unwrap();
        let shell = fish.to_str().unwrap().to_owned();
        let argv = editor_argv(
            &container(ContainerShell::Explicit(shell), None),
            None,
            login,
            "/a b/$(id)",
        );
        let inside = &argv[argv.iter().position(|arg| arg == "cid").unwrap() + 1..];
        let output = std::process::Command::new(&inside[0])
            .args(&inside[1..])
            .env("EDITOR", inherited_editor)
            .output()
            .unwrap();
        let _ = std::fs::remove_dir_all(&tools);
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    #[cfg(unix)]
    #[test]
    fn container_editor_uses_the_editor_a_non_posix_shell_reports_with_the_pane_login_mode() {
        assert_eq!(
            run_container_with_fake_fish(true, "ignored"),
            "FISH-EDITOR|/a b/$(id)|"
        );
        assert_eq!(
            run_container_with_fake_fish(false, "ignored"),
            "FISH-EDITOR|/a b/$(id)|"
        );
    }

    #[cfg(unix)]
    #[test]
    fn container_editor_keeps_the_inherited_editor_when_the_non_posix_shell_reports_none() {
        let tools = std::env::temp_dir().join(format!("herdr-cfish-none-{}", std::process::id()));
        std::fs::create_dir_all(&tools).unwrap();
        let fish = tools.join("fish");
        std::fs::write(&fish, "#!/bin/sh\nexit 1\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fish, std::fs::Permissions::from_mode(0o755)).unwrap();
        let argv = editor_argv(
            &container(
                ContainerShell::Explicit(fish.to_str().unwrap().into()),
                None,
            ),
            None,
            false,
            "/f",
        );
        let inside = &argv[argv.iter().position(|arg| arg == "cid").unwrap() + 1..];
        let output = std::process::Command::new(&inside[0])
            .args(&inside[1..])
            .env("EDITOR", "printf '%s|' INHERITED")
            .output()
            .unwrap();
        let _ = std::fs::remove_dir_all(&tools);

        assert_eq!(String::from_utf8_lossy(&output.stdout), "INHERITED|/f|");
    }

    #[test]
    fn container_editor_on_an_ssh_host_runs_docker_through_ssh_with_a_tty() {
        let argv = editor_argv(
            &container(ContainerShell::Explicit("/bin/sh".into()), Some(ssh())),
            Some("/w"),
            false,
            "/it's",
        );

        assert_eq!(argv[0], "ssh");
        assert!(argv.iter().any(|arg| arg == "-t"));
        assert!(argv.iter().any(|arg| arg == "RemoteCommand=none"));
        assert_eq!(argv[argv.len() - 2], "deploy@devbox");
        assert_eq!(
            argv.last().unwrap(),
            &format!(
                "docker 'exec' '-it' '-u' 'dev' '-w' '/w' 'cid' '/bin/sh' '-c' '{}' 'sh' '/it'\\''s'",
                EDITOR_SCRIPT.replace('\'', r"'\''")
            )
        );
    }

    #[test]
    fn ssh_editor_uses_the_pane_control_path_and_a_tty_without_batch_mode() {
        let argv = editor_argv(&PaneEnvironment::Ssh(ssh()), Some("/w"), false, "/a b");

        assert_eq!(
            argv[..8],
            [
                "ssh",
                "-C",
                "-S",
                "/tmp/ctl-%C",
                "-o",
                "ControlMaster=auto",
                "-o",
                "ControlPersist=600"
            ]
        );
        assert!(argv.iter().any(|arg| arg == "-t"));
        assert!(!argv.iter().any(|arg| arg.contains("BatchMode")));
    }

    #[test]
    fn ssh_editor_is_the_same_whatever_the_host_shell_mode() {
        let env = PaneEnvironment::Ssh(ssh());

        assert_eq!(
            editor_argv(&env, Some("/w"), true, "/f"),
            editor_argv(&env, Some("/w"), false, "/f")
        );
    }

    #[test]
    fn ssh_editor_command_is_single_quoted_words_a_fish_login_shell_can_read() {
        let argv = editor_argv(&PaneEnvironment::Ssh(ssh()), Some("/w d"), false, "/it's");
        let command = argv.last().unwrap();

        assert!(command.starts_with("'sh' '-c' '"), "{command}");
        assert!(command.ends_with("'/w d' '/it'\\''s'"), "{command}");
    }

    /// Runs the remote command string the way the account's login shell
    /// would, with a chosen `$SHELL` and `$EDITOR`.
    #[cfg(unix)]
    fn run_remote(shell: &str, editor: &str, cwd: Option<&str>, path: &str) -> String {
        let argv = editor_argv(&PaneEnvironment::Ssh(ssh()), cwd, false, path);
        let command = argv.last().unwrap();
        let output = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(command)
            .env("SHELL", shell)
            .env("EDITOR", editor)
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    #[cfg(unix)]
    #[test]
    fn ssh_editor_splits_a_multiword_editor_and_passes_the_path_as_one_argument() {
        let out = run_remote("/bin/sh", "printf '%s|' EDITOR-ARG", None, "/a b/$(id)'x");

        assert_eq!(out, "EDITOR-ARG|/a b/$(id)'x|");
    }

    #[cfg(unix)]
    #[test]
    fn ssh_editor_starts_in_the_pane_directory_and_survives_a_missing_one() {
        let dir = std::env::temp_dir();
        let dir = std::fs::canonicalize(dir).unwrap();

        let there = run_remote("/bin/sh", "pwd -P #", Some(dir.to_str().unwrap()), "/f");
        let gone = run_remote("/bin/sh", "printf ran", Some("/nonexistent-herdr"), "/f");

        assert_eq!(there.trim(), dir.to_str().unwrap());
        assert_eq!(gone, "ran");
    }

    #[cfg(unix)]
    #[test]
    fn ssh_editor_reads_editor_from_a_non_posix_login_shell_and_runs_it_under_sh() {
        let tools = std::env::temp_dir().join(format!("herdr-fish-{}", std::process::id()));
        std::fs::create_dir_all(&tools).unwrap();
        let fish = tools.join("fish");
        // Answers the EDITOR query and refuses the POSIX script, like fish.
        std::fs::write(
            &fish,
            "#!/bin/sh\ncase \"$*\" in *EDITOR*) printf '%s' \"printf '%s|' FISH-EDITOR\" ;; *) echo fish-ran-script; exit 1 ;; esac\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fish, std::fs::Permissions::from_mode(0o755)).unwrap();

        let out = run_remote(fish.to_str().unwrap(), "ignored", None, "/f");
        let _ = std::fs::remove_dir_all(&tools);

        assert_eq!(out, "FISH-EDITOR|/f|");
    }
}
