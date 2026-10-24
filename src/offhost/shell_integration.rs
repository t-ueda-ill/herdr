//! The `sh -c` launcher that starts a pane's shell on an ssh host or inside a
//! container. It changes into the directory a new pane inherits and, unless
//! integration is off, makes the shell read the cwd reporting from
//! `integration::remote_shell` without editing any file the user owns.

use std::ffi::OsString;
use std::path::Path;

use crate::integration::{injected_integration_fish, injected_integration_posix};

/// Run as `sh -c LAUNCHER sh START_DIR SHELL LOGIN POSIX_BODY FISH_BODY`.
///
/// `$1` directory to enter (empty: stay), `$2` shell (empty: `$SHELL`,
/// `bash-or-sh`: bash when present), `$3` `1` for a login shell, `$4` POSIX
/// integration (empty: none), `$5` fish integration.
///
/// Why a temp directory and not `--rcfile`-style flags alone: zsh, fish and
/// sh have no flag that adds a file after the user's startup files, and the
/// user's own rc files must stay untouched. Each branch removes the directory
/// from inside the shell once its startup has read the files.
/// Why `cd` can fail silently: a directory that vanished falls back to where
/// `docker -w` or the ssh login put the shell, and the pane must still open.
pub(crate) const LAUNCHER: &str = r##"
[ -n "$1" ] && cd -- "$1" 2>/dev/null
shell=$2
login=$3
case "$shell" in
  "") shell=${SHELL:-sh} ;;
  bash-or-sh) if command -v bash >/dev/null 2>&1; then shell=bash; else shell=sh; fi ;;
esac
plain() {
  if [ "$login" = 1 ]; then exec "$shell" -l; fi
  exec "$shell"
}
[ -n "$4$5" ] || plain
d=$(mktemp -d 2>/dev/null) || plain
[ -n "$d" ] && [ -d "$d" ] || plain
printf '%s\n' "$4" >"$d/herdr.sh" 2>/dev/null || { rm -rf "$d"; plain; }
printf '%s\n' "$5" >"$d/herdr.fish" 2>/dev/null || { rm -rf "$d"; plain; }
case "${shell##*/}" in
  bash)
    if [ "$login" = 1 ]; then
      cat >"$d/rc.bash" <<'RC'
[ -r /etc/profile ] && . /etc/profile
for __herdr_f in ~/.bash_profile ~/.bash_login ~/.profile; do
  [ -r "$__herdr_f" ] && { . "$__herdr_f"; break; }
done
RC
    else
      cat >"$d/rc.bash" <<'RC'
[ -r /etc/bash.bashrc ] && . /etc/bash.bashrc
[ -r ~/.bashrc ] && . ~/.bashrc
RC
    fi
    cat >>"$d/rc.bash" <<'RC'
unset __herdr_f
__herdr_d=${BASH_SOURCE[0]%/*}
. "$__herdr_d/herdr.sh"
rm -rf "$__herdr_d"
unset __herdr_d
RC
    exec "$shell" --rcfile "$d/rc.bash" -i ;;
  zsh)
    cat >"$d/.zshenv" <<'RC'
if [ -n "${__HERDR_ZDOTDIR-}" ]; then ZDOTDIR=$__HERDR_ZDOTDIR; else unset ZDOTDIR; fi
unset __HERDR_ZDOTDIR
[ -r "${ZDOTDIR:-$HOME}/.zshenv" ] && . "${ZDOTDIR:-$HOME}/.zshenv"
__herdr_first_prompt() {
  precmd_functions=(${precmd_functions:#__herdr_first_prompt})
  unfunction __herdr_first_prompt
  . "$__HERDR_DIR/herdr.sh"
  rm -rf "$__HERDR_DIR"
  unset __HERDR_DIR
}
precmd_functions=(__herdr_first_prompt $precmd_functions)
RC
    if [ "$login" = 1 ]; then
      __HERDR_DIR=$d __HERDR_ZDOTDIR=${ZDOTDIR-} ZDOTDIR=$d exec "$shell" -l
    fi
    __HERDR_DIR=$d __HERDR_ZDOTDIR=${ZDOTDIR-} ZDOTDIR=$d exec "$shell" ;;
  fish)
    if [ "$login" = 1 ]; then
      exec "$shell" -l -C "source '$d/herdr.fish'; rm -rf '$d'"
    fi
    exec "$shell" -C "source '$d/herdr.fish'; rm -rf '$d'" ;;
  sh|dash|ash)
    cat >"$d/env.sh" <<'RC'
if [ -n "${__HERDR_ENV-}" ]; then ENV=$__HERDR_ENV; else unset ENV; fi
unset __HERDR_ENV
[ -n "${ENV-}" ] && [ -r "$ENV" ] && . "$ENV"
. "$__HERDR_DIR/herdr.sh"
rm -rf "$__HERDR_DIR"
unset __HERDR_DIR
RC
    if [ "$login" = 1 ]; then
      __HERDR_DIR=$d __HERDR_ENV=${ENV-} ENV=$d/env.sh exec "$shell" -l
    fi
    __HERDR_DIR=$d __HERDR_ENV=${ENV-} ENV=$d/env.sh exec "$shell" ;;
  *)
    rm -rf "$d"
    plain ;;
esac
"##;

/// Which shell the launcher starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LauncherShell<'a> {
    /// The account's `$SHELL`, which is what a plain `ssh -t host` runs.
    Account,
    BashOrSh,
    Explicit(&'a str),
}

/// True when a pane needs the launcher instead of its plain argv.
pub(crate) fn needs_launcher(shell_integration: bool, start_dir: Option<&Path>) -> bool {
    shell_integration || start_dir.is_some()
}

/// `sh -c LAUNCHER sh <args>`, the command both launchers run remotely.
pub(crate) fn launcher_argv(
    start_dir: Option<&Path>,
    shell: LauncherShell<'_>,
    login: bool,
    shell_integration: bool,
) -> Vec<OsString> {
    let (posix, fish) = if shell_integration {
        (injected_integration_posix(), injected_integration_fish())
    } else {
        (String::new(), String::new())
    };
    vec![
        "sh".into(),
        "-c".into(),
        LAUNCHER.into(),
        "sh".into(),
        start_dir.map_or_else(OsString::new, |dir| dir.as_os_str().to_owned()),
        match shell {
            LauncherShell::Account => "",
            LauncherShell::BashOrSh => "bash-or-sh",
            LauncherShell::Explicit(shell) => shell,
        }
        .into(),
        if login { "1" } else { "0" }.into(),
        posix.into(),
        fish.into(),
    ]
}

// The tests run the launcher with local POSIX shells and a pty.
#[cfg(all(test, unix))]
mod tests;
