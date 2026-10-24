//! Shell functions for the machine where an agent runs when that machine is
//! not the one running herdr (ssh, containers). They announce `claude` and
//! `codex` through a private OSC so the herdr in front of the pane can list the
//! agent and send notifications. The byte contract lives in `pane/osc.rs`.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemoteShell {
    Bash,
    Zsh,
    Fish,
    /// POSIX sh, dash and ash: no subshell-safe signal handling, no aliases array.
    Sh,
}

impl RemoteShell {
    pub(crate) const LABELS: &'static str = "bash, zsh, fish, sh";

    pub(crate) fn parse(label: &str) -> Option<Self> {
        match label {
            "bash" => Some(Self::Bash),
            "zsh" => Some(Self::Zsh),
            "fish" => Some(Self::Fish),
            "sh" | "dash" | "ash" => Some(Self::Sh),
            _ => None,
        }
    }

    /// Picks the shell from `$SHELL`, because the snippet is meant to be
    /// appended to the rc file of the shell the user is typing in.
    pub(crate) fn from_shell_env(shell: Option<&str>) -> Self {
        shell
            .and_then(|path| path.rsplit('/').next())
            .and_then(Self::parse)
            .unwrap_or(Self::Bash)
    }
}

/// Shared by the bash, zsh and sh bodies: one text, because the launcher
/// passes a single POSIX body and branches on the running shell here.
///
/// The cwd is reported as OSC 7 on `/dev/tty` and not on stdout, so a prompt
/// command substitution cannot swallow it. Only `%` is percent-encoded: that is
/// the one byte `parse_file_uri_cwd` rejects, and every other byte of a path is
/// carried raw, which avoids an external command per prompt.
/// Why `eval` for the array forms: dash parses the whole body, and `name=(...)`
/// is a syntax error there even in a branch that never runs.
pub(crate) fn injected_cwd_integration_posix() -> String {
    r#"# herdr shell integration: reports the working directory to the herdr in front of this shell.
__herdr_host=${HOSTNAME:-}
[ -n "$__herdr_host" ] || __herdr_host=$(hostname 2>/dev/null) || __herdr_host=
__herdr_osc7() {
  __herdr_rest=$PWD __herdr_path=
  while case $__herdr_rest in *"%"*) true ;; *) false ;; esac; do
    __herdr_path=$__herdr_path${__herdr_rest%%"%"*}%25
    __herdr_rest=${__herdr_rest#*"%"}
  done
  __herdr_path=$__herdr_path$__herdr_rest
  if [ -n "${TMUX:-}" ]; then
    { printf '\033Ptmux;\033\033]7;file://%s%s\007\033\\' "$__herdr_host" "$__herdr_path" >/dev/tty; } 2>/dev/null || :
  else
    { printf '\033]7;file://%s%s\007' "$__herdr_host" "$__herdr_path" >/dev/tty; } 2>/dev/null || :
  fi
  return 0
}
if [ -n "${BASH_VERSION:-}" ]; then
  case "$(declare -p PROMPT_COMMAND 2>/dev/null)" in
    "declare -a"*) eval 'PROMPT_COMMAND=(__herdr_osc7 "${PROMPT_COMMAND[@]}")' ;;
    *) PROMPT_COMMAND="__herdr_osc7${PROMPT_COMMAND:+;$PROMPT_COMMAND}" ;;
  esac
elif [ -n "${ZSH_VERSION:-}" ]; then
  eval 'precmd_functions+=(__herdr_osc7); chpwd_functions+=(__herdr_osc7)'
else
  # A prompt cannot call a function in dash and ash, so `cd` reports instead.
  cd() { command cd "$@" || return; __herdr_osc7; }
fi
__herdr_osc7
"#
    .to_owned()
}

/// Fish counterpart of [`injected_cwd_integration_posix`]. The host is read
/// once, and the path is encoded with `string` so no process starts per `cd`.
/// Why `$HOSTNAME` before fish's own `$hostname`: the POSIX body prefers it,
/// and both bodies must name the same host for one machine.
pub(crate) fn injected_cwd_integration_fish() -> String {
    r#"# herdr shell integration: reports the working directory to the herdr in front of this shell.
if test -n "$HOSTNAME"
    set -g __herdr_host $HOSTNAME
else
    set -g __herdr_host $hostname
end
function __herdr_osc7 --on-variable PWD
    isatty stdin; or return 0
    set -l path (string replace -a '%' '%25' -- $PWD)
    if set -q TMUX
        printf '\033Ptmux;\033\033]7;file://%s%s\007\033\\' $__herdr_host $path >/dev/tty 2>/dev/null
    else
        printf '\033]7;file://%s%s\007' $__herdr_host $path >/dev/tty 2>/dev/null
    end
    return 0
end
__herdr_osc7
"#
    .to_owned()
}

/// Agents the generated functions wrap.
const WRAPPED_AGENTS: [&str; 2] = ["claude", "codex"];

pub(crate) fn remote_shell_function(shell: RemoteShell) -> String {
    match shell {
        RemoteShell::Bash => {
            posix_function(|agent| format!("${{BASH_ALIASES[{agent}]-}}"), SUBSHELL_RUN)
        }
        RemoteShell::Zsh => posix_function(|agent| format!("${{aliases[{agent}]-}}"), SUBSHELL_RUN),
        RemoteShell::Sh => posix_function(|_| String::new(), TRAP_RUN),
        RemoteShell::Fish => fish_function(),
    }
}

/// Exit is announced by a subshell EXIT trap, which also fires when the agent
/// dies of a signal; a plain statement after the call would be skipped when an
/// interactive shell abandons the command list on SIGINT. INT/TERM are not
/// trapped, because Ctrl-C that only cancels an agent action must not release.
const SUBSHELL_RUN: &str = r#"__herdr_agent_run() {
  local __herdr_kind=$1 __herdr_alias=$2 __herdr_token
  shift 2
  __herdr_token="$$-${RANDOM:-0}${RANDOM:-0}"
  __herdr_agent_osc "herdr-agent;v=1;start;$__herdr_token;$__herdr_kind"
  (
    trap '__herdr_agent_osc "herdr-agent;v=1;exit;$__herdr_token"' EXIT
    if [ -n "$__herdr_alias" ]; then
      eval "$__herdr_alias \"\$@\""
    else
      command "$__herdr_kind" "$@"
    fi
  )
}
"#;

/// Why not the subshell above: dash and ash kill the subshell on Ctrl-C and
/// leave the agent detached from the prompt. Trapping INT keeps this shell
/// alive across the child's SIGINT, so the exit marker is written after it.
const TRAP_RUN: &str = r#"__herdr_agent_run() {
  local __herdr_kind=$1 __herdr_token __herdr_rc=0
  shift 2
  __herdr_seq=$(( ${__herdr_seq:-0} + 1 ))
  __herdr_token="$$-$(date +%s)-$__herdr_seq"
  __herdr_agent_osc "herdr-agent;v=1;start;$__herdr_token;$__herdr_kind"
  trap ':' INT
  command "$__herdr_kind" "$@" || __herdr_rc=$?
  trap - INT
  __herdr_agent_osc "herdr-agent;v=1;exit;$__herdr_token"
  return "$__herdr_rc"
}
"#;

fn posix_function(alias_lookup: impl Fn(&str) -> String, run: &str) -> String {
    let setup: String = WRAPPED_AGENTS
        .iter()
        .map(|agent| {
            // The alias is captured before it is removed, because a leftover
            // alias would be expanded inside the function definition below and
            // `command claude` would then run the alias target twice or fail.
            let capture = alias_lookup(agent);
            let capture = if capture.is_empty() {
                String::new()
            } else {
                format!("__herdr_alias_{agent}=\"{capture}\"\n")
            };
            format!(
                "{capture}unalias {agent} 2>/dev/null || :\n{agent}() {{ __herdr_agent_run {agent} \"${{__herdr_alias_{agent}-}}\" \"$@\"; }}\n"
            )
        })
        .collect();
    format!(
        r#"# herdr remote-shell integration: lets a herdr in front of this shell list
# and notify for claude/codex started here. Needs a controlling terminal.
__herdr_agent_osc() {{
  if [ -n "${{TMUX:-}}" ]; then
    {{ printf '\033Ptmux;\033\033]43737;%s\007\033\\' "$1" >/dev/tty; }} 2>/dev/null || :
  else
    {{ printf '\033]43737;%s\007' "$1" >/dev/tty; }} 2>/dev/null || :
  fi
}}
{run}{setup}"#
    )
}

fn fish_function() -> String {
    let wrappers: String = WRAPPED_AGENTS
        .iter()
        .map(|agent| format!("function {agent}\n    __herdr_agent_run {agent} $argv\nend\n"))
        .collect();
    // Why not an EXIT trap as in bash/zsh: fish has no subshell to hang one
    // on, and fish abandons the running function when the agent dies of
    // SIGINT, so a killed agent leaves its claim until `pane release-agent`.
    format!(
        r#"# herdr remote-shell integration: lets a herdr in front of this shell list
# and notify for claude/codex started here. Needs a controlling terminal.
# Limit: an agent killed by a signal is not announced as exited.
function __herdr_agent_osc
    isatty stdin; or return 0
    if set -q TMUX
        printf '\033Ptmux;\033\033]43737;%s\007\033\\' $argv[1] >/dev/tty 2>/dev/null
    else
        printf '\033]43737;%s\007' $argv[1] >/dev/tty 2>/dev/null
    end
    return 0
end
function __herdr_agent_run
    set -l kind $argv[1]
    set -l token (random)(random)
    __herdr_agent_osc "herdr-agent;v=1;start;$fish_pid-$token;$kind"
    command $kind $argv[2..-1]
    set -l code $status
    __herdr_agent_osc "herdr-agent;v=1;exit;$fish_pid-$token"
    return $code
end
{wrappers}"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::io::Write;
    #[cfg(unix)]
    use std::process::{Command, Stdio};

    #[cfg(unix)]
    fn syntax_check(shell: &str, flag: &str, source: &str) {
        let Ok(mut child) = Command::new(shell)
            .args([flag, "/dev/stdin"])
            .stdin(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        else {
            eprintln!("skipping: {shell} is not installed");
            return;
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(source.as_bytes());
        }
        let output = child.wait_with_output().expect("shell exits");
        assert!(
            output.status.success(),
            "{shell} rejected the function: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(unix)]
    #[test]
    fn sh_function_is_valid_dash() {
        syntax_check("dash", "-n", &remote_shell_function(RemoteShell::Sh));
    }

    #[cfg(unix)]
    /// Sources the function in `shell` without a controlling terminal, so the
    /// OSC writes fail the way they do for a cron job or a pipe.
    fn run_without_tty(shell: &str, flavor: RemoteShell, body: &str) -> Option<String> {
        let dir =
            std::env::temp_dir().join(format!("herdr-remote-shell-{}-{shell}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let fake = dir.join("fake-agent");
        std::fs::write(&fake, "#!/bin/sh\necho \"ran:$1\"\n").expect("fake agent");
        std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .expect("chmod");
        let function = dir.join("function.sh");
        std::fs::write(&function, remote_shell_function(flavor)).expect("function file");
        let script = body
            .replace("FUNCTION", &function.display().to_string())
            .replace("FAKE", &fake.display().to_string());
        let output = Command::new("setsid")
            .args(["-w", shell, "-c", &script])
            .stdin(Stdio::null())
            .output()
            .ok()?;
        let _ = std::fs::remove_dir_all(&dir);
        Some(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    #[cfg(unix)]
    #[test]
    fn bash_function_runs_an_alias_target_that_is_not_on_path() {
        let script = "shopt -s expand_aliases; alias claude=FAKE; . FUNCTION; claude hi";
        if let Some(out) = run_without_tty("bash", RemoteShell::Bash, script) {
            assert_eq!(out.trim(), "ran:hi");
        }
    }

    #[cfg(unix)]
    #[test]
    fn zsh_function_runs_an_alias_target_that_is_not_on_path() {
        let script = "alias claude=FAKE; . FUNCTION; claude hi";
        if Command::new("zsh").arg("--version").output().is_err() {
            return;
        }
        // macOS has zsh but no setsid, so the run cannot detach from the tty.
        if let Some(out) = run_without_tty("zsh", RemoteShell::Zsh, script) {
            assert_eq!(out.trim(), "ran:hi");
        }
    }

    #[cfg(unix)]
    #[test]
    fn bash_function_survives_set_e_without_a_controlling_terminal() {
        let script = "set -e; . FUNCTION; PATH=$(dirname FAKE):$PATH; ln -sf FAKE $(dirname FAKE)/claude; claude hi; echo reached";
        if let Some(out) = run_without_tty("bash", RemoteShell::Bash, script) {
            assert_eq!(out.lines().collect::<Vec<_>>(), ["ran:hi", "reached"]);
        }
    }

    #[cfg(unix)]
    #[test]
    fn sh_function_survives_set_e_without_a_controlling_terminal() {
        let script = "set -e; . FUNCTION; PATH=$(dirname FAKE):$PATH; ln -sf FAKE $(dirname FAKE)/claude; claude hi; echo reached";
        if let Some(out) = run_without_tty("dash", RemoteShell::Sh, script) {
            assert_eq!(out.lines().collect::<Vec<_>>(), ["ran:hi", "reached"]);
        }
    }

    #[cfg(unix)]
    #[test]
    fn bash_function_is_valid_bash() {
        syntax_check("bash", "-n", &remote_shell_function(RemoteShell::Bash));
    }

    #[cfg(unix)]
    #[test]
    fn zsh_function_is_valid_zsh() {
        syntax_check("zsh", "-n", &remote_shell_function(RemoteShell::Zsh));
    }

    #[cfg(unix)]
    #[test]
    fn fish_function_is_valid_fish() {
        syntax_check("fish", "-n", &remote_shell_function(RemoteShell::Fish));
    }

    #[test]
    fn every_shell_wraps_claude_and_codex() {
        for shell in [
            RemoteShell::Bash,
            RemoteShell::Zsh,
            RemoteShell::Fish,
            RemoteShell::Sh,
        ] {
            let text = remote_shell_function(shell);
            assert!(text.contains("claude"), "{shell:?}");
            assert!(text.contains("codex"), "{shell:?}");
        }
    }

    #[test]
    fn function_announces_start_and_exit_in_the_pane_osc_contract() {
        for shell in [RemoteShell::Bash, RemoteShell::Fish] {
            let text = remote_shell_function(shell);
            assert!(text.contains("43737;"));
            assert!(text.contains("herdr-agent;v=1;start;"));
            assert!(text.contains("herdr-agent;v=1;exit;"));
            assert!(text.contains("Ptmux;"), "tmux passthrough: {shell:?}");
        }
    }

    #[test]
    fn exit_is_announced_from_an_exit_trap_and_not_from_signal_traps() {
        let text = remote_shell_function(RemoteShell::Bash);

        assert!(text.contains("EXIT"));
        assert!(!text.contains("INT"));
        assert!(!text.contains("TERM"));
    }

    #[test]
    fn shell_is_chosen_from_the_environment_with_a_bash_fallback() {
        assert_eq!(
            RemoteShell::from_shell_env(Some("/usr/bin/zsh")),
            RemoteShell::Zsh
        );
        assert_eq!(
            RemoteShell::from_shell_env(Some("/usr/bin/fish")),
            RemoteShell::Fish
        );
        assert_eq!(
            RemoteShell::from_shell_env(Some("/bin/dash")),
            RemoteShell::Sh
        );
        assert_eq!(
            RemoteShell::from_shell_env(Some("/bin/csh")),
            RemoteShell::Bash
        );
        assert_eq!(RemoteShell::from_shell_env(None), RemoteShell::Bash);
    }

    #[test]
    fn unknown_shell_label_is_rejected() {
        assert_eq!(RemoteShell::parse("csh"), None);
    }
}
