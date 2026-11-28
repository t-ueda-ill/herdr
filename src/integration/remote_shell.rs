//! Shell functions for the machine where an agent runs when that machine is
//! not the one running herdr (ssh, containers). They announce agents like `claude` and
//! `codex` through a private OSC so the herdr in front of the pane can list the
//! agent and send notifications. The byte contract lives in `pane/osc.rs`.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemoteShell {
    Bash,
    Zsh,
    Fish,
    /// POSIX sh, dash and ash: no subshell-safe signal handling.
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
///
/// The same hooks report the branch of the directory (see
/// [`POSIX_BRANCH_REPORT`]), so a checkout shows at the next prompt.
pub(crate) fn injected_cwd_integration_posix() -> String {
    format!("{POSIX_CWD_REPORT_HEAD}{POSIX_BRANCH_REPORT}{POSIX_CWD_REPORT_HOOKS}")
}

/// Reports `herdr-git;v=1;<branch>` on the agent OSC: the branch `HEAD`
/// names, or nothing when the directory is outside a repo or `HEAD` is
/// detached, which is what the host shows for a host directory.
///
/// Why only builtins (`[`, `read`, parameter expansion): it runs at every
/// prompt, and a `git` process per prompt would slow every command on a
/// busy host. Why a `.git` file is followed: a linked worktree and a
/// submodule point to their git dir that way, relative to the file.
/// Why the report is sent at every prompt and not only on change: a nested
/// shell may have reported its own branch in the meantime.
/// Why the walk resolves a symlink before going up from it: the logical
/// parent of a symlinked directory is not the checkout it lives in. The
/// subshell that resolves it runs only then; it bypasses the `cd` wrapper and
/// sets `__herdr_resolving` so zsh's `chpwd` hook does not report its path.
/// Why not a symlink right under `/` (macOS `/var` and `/tmp`, merged-usr
/// `/bin`): only the root is above it, so resolving changes nothing that
/// matters, while every prompt below it would start the subshell.
/// Why `git` only for `.invalid`: that is the placeholder a reftable repo
/// keeps in `HEAD`, which no builtin can resolve.
/// Why control characters are dropped here and not only by herdr: they reach
/// the terminal with the report, before herdr can reject them.
/// Why `-f` before every `read`: a FIFO in a repo would make `read` wait for
/// a writer, and the prompt would never come back.
/// Why a bare repo is told by its layout and `core.bare`: it has no `.git`,
/// and the host's discovery (`git/discovery.rs`) uses the same two checks, so
/// both sides name the same repo. The config is read like the host reads it:
/// the first `bare` in `[core]`, names without case, comments cut off.
const POSIX_BRANCH_REPORT: &str = r##"__herdr_is_bare() {
  [ -f "$1/HEAD" ] && [ -d "$1/objects" ] && [ -d "$1/refs" ] && [ -f "$1/config" ] || return 1
  __herdr_core=
  while IFS= read -r __herdr_line || [ -n "$__herdr_line" ]; do
    __herdr_line=${__herdr_line#"${__herdr_line%%[![:space:]]*}"}
    case $__herdr_line in
      ""|"#"*|";"*) continue ;;
      "["*)
        case $__herdr_line in
          [[][Cc][Oo][Rr][Ee][]]*) __herdr_core=1 ;;
          *) __herdr_core= ;;
        esac
        continue ;;
      *=*) ;;
      *) continue ;;
    esac
    [ -n "$__herdr_core" ] || continue
    __herdr_key=${__herdr_line%%=*}
    __herdr_key=${__herdr_key%"${__herdr_key##*[![:space:]]}"}
    case $__herdr_key in
      [Bb][Aa][Rr][Ee]) ;;
      *) continue ;;
    esac
    __herdr_value=${__herdr_line#*=}
    __herdr_value=${__herdr_value%%[#;]*}
    __herdr_value=${__herdr_value#"${__herdr_value%%[![:space:]]*}"}
    __herdr_value=${__herdr_value%"${__herdr_value##*[![:space:]]}"}
    case $__herdr_value in
      [Tt][Rr][Uu][Ee]) return 0 ;;
      *) return 1 ;;
    esac
  done <"$1/config"
  return 1
}
__herdr_git_branch() {
  __herdr_dir=$PWD __herdr_head= __herdr_branch=
  while :; do
    if [ -f "$__herdr_dir/.git/HEAD" ]; then
      __herdr_head=$__herdr_dir/.git/HEAD
      break
    fi
    if [ -f "$__herdr_dir/.git" ]; then
      __herdr_line=
      { IFS= read -r __herdr_line <"$__herdr_dir/.git"; } 2>/dev/null || :
      case $__herdr_line in
        "gitdir: /"*) __herdr_head=${__herdr_line#gitdir: }/HEAD ;;
        "gitdir: "?*) __herdr_head=$__herdr_dir/${__herdr_line#gitdir: }/HEAD ;;
      esac
      break
    fi
    if __herdr_is_bare "$__herdr_dir"; then
      __herdr_head=$__herdr_dir/HEAD
      break
    fi
    [ -n "$__herdr_dir" ] || break
    if [ -h "$__herdr_dir" ] && [ -n "${__herdr_dir%/*}" ]; then
      __herdr_dir=$(__herdr_resolving=1; command cd -P -- "$__herdr_dir" 2>/dev/null && pwd -P) || break
    fi
    __herdr_dir=${__herdr_dir%/*}
  done
  if [ -f "$__herdr_head" ]; then
    __herdr_line=
    { IFS= read -r __herdr_line <"$__herdr_head"; } 2>/dev/null || :
    case $__herdr_line in
      "ref: refs/heads/.invalid")
        __herdr_branch=$(command git symbolic-ref --short -q HEAD 2>/dev/null) || __herdr_branch= ;;
      "ref: refs/heads/"?*) __herdr_branch=${__herdr_line#ref: refs/heads/} ;;
    esac
  fi
  case $__herdr_branch in
    *[[:cntrl:]]*) __herdr_branch= ;;
  esac
  if [ -n "${TMUX:-}" ]; then
    { printf '\033Ptmux;\033\033]43737;herdr-git;v=1;%s\007\033\\' "$__herdr_branch" >/dev/tty; } 2>/dev/null || :
  else
    { printf '\033]43737;herdr-git;v=1;%s\007' "$__herdr_branch" >/dev/tty; } 2>/dev/null || :
  fi
  return 0
}
"##;

const POSIX_CWD_REPORT_HEAD: &str = r#"# herdr shell integration: reports the working directory to the herdr in front of this shell.
__herdr_host=${HOSTNAME:-}
[ -n "$__herdr_host" ] || __herdr_host=$(hostname 2>/dev/null) || __herdr_host=
__herdr_osc7() {
  [ -z "${__herdr_resolving:-}" ] || return 0
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
  __herdr_git_branch
  return 0
}
"#;

const POSIX_CWD_REPORT_HOOKS: &str = r#"if [ -n "${BASH_VERSION:-}" ]; then
  case "$(declare -p PROMPT_COMMAND 2>/dev/null)" in
    "declare -a"*) eval 'PROMPT_COMMAND=(__herdr_osc7 "${PROMPT_COMMAND[@]}")' ;;
    *) PROMPT_COMMAND="__herdr_osc7${PROMPT_COMMAND:+;$PROMPT_COMMAND}" ;;
  esac
elif [ -n "${ZSH_VERSION:-}" ]; then
  eval 'precmd_functions+=(__herdr_osc7); chpwd_functions+=(__herdr_osc7)'
else
  # A prompt cannot call a function in dash and ash, so `cd` reports
  # instead, and `git` reports the branch a checkout leaves behind. The
  # `git` wrapper is defined by `eval` at run time because an alias named
  # `git` would rewrite `git() {` while this `if` is parsed, and it is
  # skipped when an alias or the user's own function already owns the name.
  cd() { command cd "$@" || return; __herdr_osc7; }
  if ! alias git >/dev/null 2>&1; then
    case $(type git 2>/dev/null) in
      *function*) ;;
      *) eval 'git() { command git "$@"; __herdr_rc=$?; __herdr_git_branch; return "$__herdr_rc"; }' ;;
    esac
  fi
fi
__herdr_osc7
"#;

/// Fish counterpart of [`injected_cwd_integration_posix`]. The host is read
/// once, and the path is encoded with `string` so no process starts per `cd`.
/// Why `$HOSTNAME` before fish's own `$hostname`: the POSIX body prefers it,
/// and both bodies must name the same host for one machine.
pub(crate) fn injected_cwd_integration_fish() -> String {
    format!("{FISH_CWD_REPORT}{FISH_BRANCH_REPORT}")
}

const FISH_CWD_REPORT: &str = r#"# herdr shell integration: reports the working directory to the herdr in front of this shell.
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
"#;

/// Fish counterpart of [`POSIX_BRANCH_REPORT`], sent at every prompt.
/// Why `test -r` before each `read`: fish prints its own error for a
/// redirection from a missing file, which `2>/dev/null` does not silence.
/// Why regexes and not `?` globs: fish 4 turns on `qmark-noglob`, where `?`
/// matches only itself.
const FISH_BRANCH_REPORT: &str = r#"function __herdr_is_bare
    set -l dir $argv[1]
    test -f "$dir/HEAD"; and test -d "$dir/objects"; and test -d "$dir/refs"; and test -f "$dir/config"; or return 1
    set -l core
    while read -l line
        set line (string trim -- "$line")
        if test -z "$line"; or string match -qr '^[#;]' -- "$line"
            continue
        end
        if string match -qr '^\[' -- "$line"
            set core
            string match -qir '^\[core\]' -- "$line"; and set core 1
            continue
        end
        test -n "$core"; or continue
        string match -qr '=' -- "$line"; or continue
        set -l parts (string split -m 1 '=' -- "$line")
        string match -qi 'bare' -- (string trim -- "$parts[1]"); or continue
        set -l value (string trim -- (string replace -r '[#;].*$' '' -- "$parts[2]"))
        string match -qi 'true' -- "$value"
        return
    end <"$dir/config"
    return 1
end
function __herdr_git_branch --on-event fish_prompt
    isatty stdin; or return 0
    set -l dir $PWD
    set -l head
    while true
        if test -f "$dir/.git/HEAD"
            set head "$dir/.git/HEAD"
            break
        end
        if test -f "$dir/.git"
            set -l line
            test -r "$dir/.git"; and read -l line <"$dir/.git"
            if string match -qr '^gitdir: /' -- "$line"
                set head (string sub -s 9 -- "$line")/HEAD
            else if string match -qr '^gitdir: .' -- "$line"
                set head "$dir/"(string sub -s 9 -- "$line")/HEAD
            end
            break
        end
        if __herdr_is_bare "$dir"
            set head "$dir/HEAD"
            break
        end
        test -n "$dir"; or break
        if test -L "$dir"; and string match -qr '^/[^/]+/' -- "$dir"
            set dir (builtin realpath -- "$dir" 2>/dev/null); or break
        end
        set dir (string replace -r '/[^/]*$' '' -- "$dir")
    end
    set -l branch
    if test -n "$head"; and test -f "$head"
        read -l line <"$head"
        if test "$line" = 'ref: refs/heads/.invalid'
            set branch (command git symbolic-ref --short -q HEAD 2>/dev/null)
        else if string match -qr '^ref: refs/heads/.' -- "$line"
            set branch (string sub -s 17 -- "$line")
        end
    end
    if string match -qr '[[:cntrl:]]' -- "$branch"
        set branch
    end
    if set -q TMUX
        printf '\033Ptmux;\033\033]43737;herdr-git;v=1;%s\007\033\\' "$branch" >/dev/tty 2>/dev/null
    else
        printf '\033]43737;herdr-git;v=1;%s\007' "$branch" >/dev/tty 2>/dev/null
    end
    return 0
end
"#;

/// Marks every prompt and the start of every command's output with OSC 133,
/// so the herdr in front of the shell can move and copy by command. Shared by
/// bash and zsh; dash and ash run it without effect.
///
/// Why `A` and not `P` for the prompt: `A` also makes the terminal start a
/// fresh line. With `P`, a prompt that follows output without a final newline
/// shares a row with that output, and the row then counts as a prompt row
/// only, which hides the output from a lookup by row.
/// Why nothing for bash before 4.4, dash and ash: they have no hook between
/// the typed command and its output (`PS0`, `preexec`), and a prompt mark
/// alone would turn every output row into a prompt row.
/// Why nothing under tmux: tmux redraws the screen itself, so a mark would
/// land on whatever row tmux last drew and not on the prompt.
/// Why the prompt hook is appended: text other hooks print belongs to the
/// output above the prompt, so the mark has to follow them.
/// Why a newline joins `PROMPT_COMMAND`: a value that ends in `;` would make
/// `;;` with a semicolon.
/// Why `PS0` holds the escape itself: it is printed without running a command,
/// so no function call is possible there.
/// Why zsh marks once while the launcher loads this: the launcher sources it
/// from inside the first `precmd`, and zsh runs the hooks it had when that
/// round began, so the first prompt would stay unmarked.
pub(crate) fn injected_prompt_marks_posix() -> String {
    r#"# herdr shell integration: marks prompts and command output (OSC 133) for the herdr in front of this shell.
if [ -z "${TMUX:-}" ] && [ -z "${__herdr_marks:-}" ]; then
  __herdr_marks=1
  __herdr_prompt_mark() {
    { printf '\033]133;A\007' >/dev/tty; } 2>/dev/null || :
    return 0
  }
  __herdr_output_mark() {
    { printf '\033]133;C\007' >/dev/tty; } 2>/dev/null || :
    return 0
  }
  if [ -n "${BASH_VERSION:-}" ]; then
    case $BASH_VERSION in
      [123].*|4.[0123].*) ;;
      *)
        case "$(declare -p PROMPT_COMMAND 2>/dev/null)" in
          "declare -a"*) eval 'PROMPT_COMMAND+=(__herdr_prompt_mark)' ;;
          *) PROMPT_COMMAND="${PROMPT_COMMAND:+$PROMPT_COMMAND
}__herdr_prompt_mark" ;;
        esac
        PS0='\e]133;C\a'"${PS0-}" ;;
    esac
  elif [ -n "${ZSH_VERSION:-}" ]; then
    eval 'precmd_functions+=(__herdr_prompt_mark); preexec_functions+=(__herdr_output_mark)'
    [ -z "${__HERDR_DIR:-}" ] || __herdr_prompt_mark
  fi
fi
"#
    .to_owned()
}

/// Fish counterpart of [`injected_prompt_marks_posix`].
///
/// Why no check for a fish that sends OSC 133 itself: a second mark for the
/// same prompt changes nothing, and one body then serves every fish version.
pub(crate) fn injected_prompt_marks_fish() -> String {
    r#"# herdr shell integration: marks prompts and command output (OSC 133) for the herdr in front of this shell.
if not set -q TMUX; and not set -q __herdr_marks
    set -g __herdr_marks 1
    function __herdr_prompt_mark --on-event fish_prompt
        printf '\033]133;A\007' >/dev/tty 2>/dev/null
        return 0
    end
    function __herdr_output_mark --on-event fish_preexec
        printf '\033]133;C\007' >/dev/tty 2>/dev/null
        return 0
    end
end
"#
    .to_owned()
}

/// What `herdr integration prompt-marks` prints: the marks alone, for a shell
/// herdr did not start, such as the one on this machine.
pub(crate) fn prompt_marks_manual_install(shell: RemoteShell) -> String {
    match shell {
        RemoteShell::Fish => injected_prompt_marks_fish(),
        _ => injected_prompt_marks_posix(),
    }
}

/// A command the generated functions wrap, and the label sent in the OSC.
/// They differ where the process is not named after the agent: `cursor-agent`
/// is Cursor, `kiro-cli` is Kiro.
pub(crate) struct WrappedAgent {
    pub(crate) command: &'static str,
    pub(crate) label: &'static str,
}

const fn wrapped(command: &'static str, label: &'static str) -> WrappedAgent {
    WrappedAgent { command, label }
}

/// Every agent with a bundled detection manifest. `omp` and `mastracode` are
/// left out because they have no manifest, so a start claim could not be
/// adopted for them.
pub(crate) const WRAPPED_AGENTS: [WrappedAgent; 22] = [
    wrapped("amp", "amp"),
    wrapped("agy", "agy"),
    wrapped("claude", "claude"),
    wrapped("cline", "cline"),
    wrapped("codex", "codex"),
    wrapped("cursor-agent", "cursor"),
    wrapped("devin", "devin"),
    wrapped("droid", "droid"),
    wrapped("gemini", "gemini"),
    wrapped("grok", "grok"),
    wrapped("hermes", "hermes"),
    wrapped("kilo", "kilo"),
    wrapped("kimi", "kimi"),
    wrapped("kiro-cli", "kiro"),
    wrapped("letta", "letta"),
    wrapped("maki", "maki"),
    wrapped("muse", "muse"),
    wrapped("opencode", "opencode"),
    wrapped("pi", "pi"),
    wrapped("qodercli", "qodercli"),
    wrapped("qwen", "qwen"),
    wrapped("copilot", "copilot"),
];

impl WrappedAgent {
    /// dash and ash reject `-` in a function name.
    fn is_posix_function_name(&self) -> bool {
        !self.command.contains('-')
    }

    fn variable_suffix(&self) -> String {
        self.command.replace('-', "_")
    }
}

pub(crate) fn remote_shell_function(shell: RemoteShell) -> String {
    match shell {
        RemoteShell::Bash => posix_function(&subshell_run(false), true),
        RemoteShell::Zsh => posix_function(&subshell_run(true), true),
        RemoteShell::Sh => posix_function(TRAP_RUN, false),
        RemoteShell::Fish => fish_function(),
    }
}

/// What `herdr integration remote-shell` prints: the directory report, the
/// prompt marks and the agent functions, for a shell herdr did not start.
pub(crate) fn remote_shell_manual_install(shell: RemoteShell) -> String {
    match shell {
        RemoteShell::Fish => format!(
            "{}{}{}",
            injected_cwd_integration_fish(),
            injected_prompt_marks_fish(),
            remote_shell_function(shell)
        ),
        _ => format!(
            "{}{}{}",
            injected_cwd_integration_posix(),
            injected_prompt_marks_posix(),
            remote_shell_function(shell)
        ),
    }
}

/// Body the launcher sources in a bash, zsh or sh pane: directory report,
/// prompt marks and agent functions.
pub(crate) fn injected_integration_posix() -> String {
    format!(
        "{}{}{}",
        injected_cwd_integration_posix(),
        injected_prompt_marks_posix(),
        injected_agent_integration_posix()
    )
}

/// Body the launcher sources in a fish pane.
pub(crate) fn injected_integration_fish() -> String {
    format!(
        "{}{}{}",
        injected_cwd_integration_fish(),
        injected_prompt_marks_fish(),
        fish_function()
    )
}

/// Exit is announced by a subshell EXIT trap, which also fires when the agent
/// dies of a signal; a plain statement after the call would be skipped when an
/// interactive shell abandons the command list on SIGINT. TERM is not trapped,
/// and INT is trapped only by zsh (see `subshell_run`), because Ctrl-C that
/// only cancels an agent action must not release.
///
/// Why the wrapper function is unset before the alias is evaluated: an alias
/// such as `alias claude='claude --flag'` names the wrapped command itself, so
/// evaluating it would call this wrapper again and recurse without end. The
/// unset is local to the subshell, so the interactive shell keeps its wrapper.
fn subshell_run(zsh: bool) -> String {
    // Why a no-op handler and not a release: zsh's subshell dies of SIGINT
    // without running its EXIT trap, so the claim would stay. `:` does not
    // announce anything, so a Ctrl-C that only cancels an agent action still
    // does not release; the subshell survives, waits for the agent (which has
    // default traps again in its own process and still gets the Ctrl-C), and
    // leaves normally, which fires the EXIT trap.
    let trap_int = if zsh { "    trap ':' INT\n" } else { "" };
    format!(
        r#"__herdr_agent_run() {{
  local __herdr_cmd=$1 __herdr_label=$2 __herdr_alias=$3 __herdr_token
  shift 3
  __herdr_token="$$-${{RANDOM:-0}}${{RANDOM:-0}}"
  __herdr_agent_osc "herdr-agent;v=1;start;$__herdr_token;$__herdr_label"
  (
    trap '__herdr_agent_osc "herdr-agent;v=1;exit;$__herdr_token"' EXIT
{trap_int}    if [ -n "$__herdr_alias" ]; then
      unset -f -- "$__herdr_cmd"
      eval "$__herdr_alias \"\$@\""
    else
      command "$__herdr_cmd" "$@"
    fi
  )
}}
"#
    )
}

/// Why not the subshell above: dash and ash kill the subshell on Ctrl-C and
/// leave the agent detached from the prompt. Trapping INT keeps this shell
/// alive across the child's SIGINT, so the exit marker is written after it.
///
/// Why a busy flag and not `unset -f` as above: there is no subshell to hold
/// the unset, and the wrapper must survive for the next call. While the flag is
/// set the wrapper runs the real command, which stops an alias that names the
/// wrapped command from recursing.
const TRAP_RUN: &str = r#"__herdr_agent_run() {
  local __herdr_cmd=$1 __herdr_label=$2 __herdr_alias=$3 __herdr_token __herdr_rc=0
  shift 3
  __herdr_seq=$(( ${__herdr_seq:-0} + 1 ))
  __herdr_token="$$-$(date +%s)-$__herdr_seq"
  __herdr_agent_osc "herdr-agent;v=1;start;$__herdr_token;$__herdr_label"
  trap ':' INT
  if [ -n "$__herdr_alias" ]; then
    eval "__herdr_busy_$__herdr_cmd=1"
    eval "$__herdr_alias \"\$@\"" || __herdr_rc=$?
    eval "unset __herdr_busy_$__herdr_cmd"
  else
    command "$__herdr_cmd" "$@" || __herdr_rc=$?
  fi
  trap - INT
  __herdr_agent_osc "herdr-agent;v=1;exit;$__herdr_token"
  return "$__herdr_rc"
}
"#;

const POSIX_OSC: &str = r#"__herdr_agent_osc() {
  if [ -n "${TMUX:-}" ]; then
    { printf '\033Ptmux;\033\033]43737;%s\007\033\\' "$1" >/dev/tty; } 2>/dev/null || :
  else
    { printf '\033]43737;%s\007' "$1" >/dev/tty; } 2>/dev/null || :
  fi
}
"#;

/// Wraps one command: `__herdr_wrap <command> <label> <variable suffix>`.
///
/// Why one function with `eval` and not a definition per command: bash and dash
/// parse a whole `if` before running it, and an alias still in place rewrites
/// the name in `claude() {` while it is parsed. The definition is therefore
/// built after the `unalias`, at run time. The alias is read from the output of
/// `alias NAME` and not from `BASH_ALIASES` or `aliases`, because bash 3.2
/// (macOS `/bin/bash`) has no `BASH_ALIASES` and dash has neither; the value
/// part of that output is a quoted shell word, so `eval` takes it back.
///
/// The alias is captured only when one exists: a second run of this code, for
/// example the injected copy after the same code in the user's rc, finds the
/// aliases already removed and must keep what the first run captured.
/// A function of the same name that herdr did not define is the user's own and
/// stays as it is. Why a marker variable and not a look at the function body:
/// dash cannot print a function body. Why a different lookup per shell: the
/// message of bash's plain `type` is translated by locale, so bash asks
/// `type -t` and zsh asks its `functions` table; only dash and ash, whose
/// `type` is not translated, match the message. The zsh form is `eval`ed
/// because dash cannot parse `${+functions[$1]}`.
const WRAP_HELPER: &str = r#"__herdr_wrap() {
  local __herdr_a __herdr_f=
  if [ -n "${BASH_VERSION-}" ]; then
    [ "$(type -t "$1" 2>/dev/null)" = function ] && __herdr_f=1
  elif [ -n "${ZSH_VERSION-}" ]; then
    eval '(( ${+functions[$1]} )) && __herdr_f=1'
  else
    case $(type "$1" 2>/dev/null) in
      "$1 is a function"*|"$1 is a shell function"*) __herdr_f=1 ;;
    esac
  fi
  if [ -n "$__herdr_f" ]; then
    eval "[ -n \"\${__herdr_fn_$3-}\" ]" || return 0
  fi
  if __herdr_a=$(alias "$1" 2>/dev/null); then
    __herdr_a=${__herdr_a#alias }
    eval "__herdr_alias_$3=${__herdr_a#"$1"=}"
  fi
  unalias "$1" 2>/dev/null || :
  eval "$1() { if [ -n \"\${__herdr_busy_$3-}\" ]; then command $1 \"\$@\"; else __herdr_agent_run $1 $2 \"\${__herdr_alias_$3-}\" \"\$@\"; fi; }"
  eval "__herdr_fn_$3=1"
}
"#;

fn wrap_calls(dashed: bool) -> String {
    WRAPPED_AGENTS
        .iter()
        .filter(|agent| agent.is_posix_function_name() != dashed)
        .map(|agent| {
            format!(
                "__herdr_wrap {} {} {}\n",
                agent.command,
                agent.label,
                agent.variable_suffix()
            )
        })
        .collect()
}

fn posix_function(run: &str, dashed_names: bool) -> String {
    let plain = wrap_calls(false);
    let dashed = if dashed_names {
        wrap_calls(true)
    } else {
        String::new()
    };
    format!(
        "# herdr remote-shell integration: lets a herdr in front of this shell list\n\
# and notify for agents started here. Needs a controlling terminal.\n\
{POSIX_OSC}{run}{WRAP_HELPER}{plain}{dashed}unset -f __herdr_wrap\n"
    )
}

/// Agent functions for the one body that bash, zsh and sh all source.
///
/// Why the dashed names are behind a shell test: dash rejects `-` in a
/// function name, and `eval` of such a definition is an error there.
fn injected_agent_integration_posix() -> String {
    let bash_run = subshell_run(false);
    let zsh_run = subshell_run(true);
    let plain = wrap_calls(false);
    let dashed = wrap_calls(true);
    format!(
        r#"# herdr agent integration: lets the herdr in front of this shell list and notify for agents started here.
{POSIX_OSC}if [ -n "${{BASH_VERSION:-}}" ]; then
{bash_run}elif [ -n "${{ZSH_VERSION:-}}" ]; then
{zsh_run}else
{TRAP_RUN}fi
{WRAP_HELPER}{plain}if [ -n "${{BASH_VERSION:-}}" ] || [ -n "${{ZSH_VERSION:-}}" ]; then
{dashed}fi
unset -f __herdr_wrap
"#
    )
}

fn fish_function() -> String {
    let wrappers: String = WRAPPED_AGENTS
        .iter()
        .map(|agent| {
            format!(
                "if not functions -q {command}; or functions {command} | string match -q '*__herdr_agent_run*'\n    function {command}\n        __herdr_agent_run {command} {label} $argv\n    end\nend\n",
                command = agent.command,
                label = agent.label
            )
        })
        .collect();
    // Why a function that already exists is left alone: fish aliases are
    // functions, so it is the user's alias or function unless its body is ours.
    // Why not an EXIT trap as in bash/zsh: fish has no subshell to hang one
    // on, and fish abandons the running function when the agent dies of
    // SIGINT, so a killed agent leaves its claim until `pane release-agent`.
    format!(
        r#"# herdr remote-shell integration: lets a herdr in front of this shell list
# and notify for agents started here. Needs a controlling terminal.
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
    set -l cmd $argv[1]
    set -l label $argv[2]
    set -l token (random)(random)
    __herdr_agent_osc "herdr-agent;v=1;start;$fish_pid-$token;$label"
    command $cmd $argv[3..-1]
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
    fn wrapped_commands_map_only_to_labels_the_claim_parser_accepts() {
        for agent in &WRAPPED_AGENTS {
            let parsed = crate::detect::parse_agent_label(agent.label);
            let by_command = crate::detect::parse_agent_label(agent.command);
            assert!(parsed.is_some(), "{} -> {}", agent.command, agent.label);
            assert_eq!(parsed, by_command, "{}", agent.command);
            assert_eq!(
                crate::detect::agent_label(parsed.expect("checked")),
                agent.label,
                "{} sends the canonical label",
                agent.command
            );
        }
    }

    #[test]
    fn wrapped_commands_are_the_interactive_executables_of_manifest_agents() {
        #[cfg(not(windows))]
        for agent in &WRAPPED_AGENTS {
            let parsed = crate::detect::parse_agent_label(agent.label).expect("label");
            assert_eq!(
                crate::detect::interactive_agent_executable(parsed),
                agent.command
            );
        }
        let mut commands: Vec<_> = WRAPPED_AGENTS.iter().map(|agent| agent.command).collect();
        commands.sort_unstable();
        commands.dedup();
        assert_eq!(commands.len(), WRAPPED_AGENTS.len());
    }

    #[test]
    fn every_shell_wraps_the_widened_agent_list() {
        for shell in [
            RemoteShell::Bash,
            RemoteShell::Zsh,
            RemoteShell::Fish,
            RemoteShell::Sh,
        ] {
            let text = remote_shell_function(shell);
            for agent in &WRAPPED_AGENTS {
                if shell == RemoteShell::Sh && agent.command.contains('-') {
                    continue;
                }
                assert!(text.contains(agent.command), "{shell:?} {}", agent.command);
            }
        }
    }

    #[test]
    fn sh_function_omits_commands_that_are_not_valid_function_names() {
        let text = remote_shell_function(RemoteShell::Sh);
        assert!(!text.contains("cursor-agent"));
        assert!(!text.contains("kiro-cli"));
        assert!(text.contains("__herdr_wrap copilot copilot copilot"));
    }

    #[cfg(unix)]
    #[test]
    fn manual_install_output_is_valid_and_reports_the_directory() {
        for (shell, name, flag) in [
            (RemoteShell::Bash, "bash", "-n"),
            (RemoteShell::Sh, "dash", "-n"),
            (RemoteShell::Zsh, "zsh", "-n"),
            (RemoteShell::Fish, "fish", "-n"),
        ] {
            let text = remote_shell_manual_install(shell);
            assert!(text.contains("]7;file://"), "{shell:?}");
            assert!(text.contains("herdr-agent;v=1;start;"), "{shell:?}");
            syntax_check(name, flag, &text);
        }
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
