//! The fixed POSIX `sh` programs run inside a pane's environment.
//!
//! Why not interpolate: every variable part (paths, revisions, flags) arrives
//! as a positional argument, so a hostile file name can never become code.
//! Why only POSIX `sh` features: Alpine images ship BusyBox, which has no
//! `find -printf`, `stat -c`, or GNU `ls` extensions.
//! Output is NUL-separated tokens: names may contain any byte but NUL, and a
//! leading status token lets the parser tell "missing" from "empty" without
//! reading stderr.

/// Entries the listing script prints. The listing is sorted server-side, so a
/// larger directory is approximate: Why not print everything: a directory of a
/// million names would exceed the output cap before the deadline.
pub(crate) const LIST_SCRIPT_ENTRY_CAP: usize = 5_000;

/// Bytes of each `git ls-files` / `find` stream the index script forwards.
pub(crate) const INDEX_STREAM_BYTE_CAP: usize = 16 * 1024 * 1024;

/// Why 8 MiB + 1: one byte past the text limit proves the file is too large
/// without shipping all of it.
pub(crate) const READ_SCRIPT_BYTE_CAP: u64 = crate::files::MAX_TEXT_BYTES + 1;

/// First record of every script's output. Why: the remote login shell may print
/// a banner (`~/.bashrc`, MOTD) before the command runs, and the parser skips
/// everything up to this marker.
pub(crate) const OUTPUT_MARKER: &str = "HERDR-OFFHOST-1";

/// Enters `$1` (`$HOME`, then `/`, when empty) and reports its physical path
/// and the login directory, which is the pane's root when it never reported one.
const ENTER_DIRECTORY: &str = r#"
printf 'HERDR-OFFHOST-1\0'
if [ -z "$1" ]; then
  cd -P -- "${HOME:-/}" 2>/dev/null || cd -P -- / 2>/dev/null || { printf 'eacces\0'; exit 0; }
elif [ ! -e "$1" ]; then
  printf 'enoent\0'; exit 0
elif [ ! -d "$1" ]; then
  printf 'notdir\0'; exit 0
else
  cd -P -- "$1" 2>/dev/null || { printf 'eacces\0'; exit 0; }
fi
cwd=$(pwd -P)
home=$(cd -P -- "${HOME:-/}" 2>/dev/null && pwd -P || printf /)
printf 'ok\0%s\0%s\0' "$cwd" "$home"
"#;

/// Sets `repo` to the nearest ancestor holding a `.git` file (worktree or
/// submodule) or a `.git/HEAD`, without running git so a plain directory costs
/// no process. Why not any `.git` entry: a stray empty directory of that name
/// would make every directory below it look like a broken repository. Skips `.git` itself: git refuses to work there.
const FIND_REPOSITORY: &str = r#"
trust=$2
repo=
case "$cwd/" in
  */.git/*) ;;
  *)
    d=$cwd
    while :; do
      if [ -f "$d/.git" ] || [ -e "$d/.git/HEAD" ]; then repo=$d; break; fi
      [ "$d" = / ] && break
      d=${d%/*}
      [ -n "$d" ] || d=/
    done
    ;;
esac
g() {
  if [ "$trust" = 1 ]; then
    git -c "safe.directory=$repo" -C "$repo" "$@"
  else
    git -C "$repo" "$@"
  fi
}
"#;

/// Sets `gitstate` to `git` (with `top`), `giterr` (with `gitmsg`), or leaves
/// it empty outside a repository.
/// Why stdout and stderr are captured apart: git prints advice and warnings to
/// stderr that must not become part of the path.
const GIT_TOP: &str = r#"
gitstate=
if [ -n "$repo" ]; then
  if ! command -v git >/dev/null 2>&1; then
    gitstate=giterr
    gitmsg="git not installed"
  elif top=$(g rev-parse --show-toplevel 2>/dev/null) && [ -n "$top" ]; then
    gitstate=git
  else
    gitstate=giterr
    gitmsg=$(g rev-parse --show-toplevel 2>&1 >/dev/null | head -n 1)
  fi
fi
"#;

/// `$1` directory (empty: the login directory), `$2` `1` to trust the repository.
///
/// Tokens: `ok`, physical path, login directory, then per entry `e kind target name`
/// (kind `d f l o`; target `d f o x` for links, else empty), `total n`, and
/// one of `nogit`, `giterr message`, or `git prefix <ignored paths…> rc status`.
pub(crate) fn list_script() -> String {
    let cap = LIST_SCRIPT_ENTRY_CAP;
    let mut script = String::new();
    script.push_str(ENTER_DIRECTORY);
    script.push_str(FIND_REPOSITORY);
    script.push_str(GIT_TOP);
    script.push_str(&format!(
        r#"
n=0
for f in .[!.]* ..?* *; do
  [ -e "$f" ] || [ -L "$f" ] || continue
  [ "$f" = .git ] && continue
  n=$((n+1))
  [ "$n" -gt {cap} ] && continue
  t=
  if [ -L "$f" ]; then
    k=l
    if [ -d "$f" ]; then t=d; elif [ -f "$f" ]; then t=f; elif [ -e "$f" ]; then t=o; else t=x; fi
  elif [ -d "$f" ]; then k=d
  elif [ -f "$f" ]; then k=f
  else k=o
  fi
  printf 'e\0%s\0%s\0%s\0' "$k" "$t" "$f"
done
printf 'total\0%s\0' "$n"
if [ -z "$gitstate" ]; then
  printf 'nogit\0'
elif [ "$gitstate" = giterr ]; then
  printf 'giterr\0%s\0' "$gitmsg"
else
  # Why `base`: the root repository `/` would turn `"$top"/*` into `//*`.
  base=${{top%/}}
  case "$cwd" in
    "$top"|"$base"/*)
      rel=${{cwd#"$base"}}
      rel=${{rel#/}}
      [ -n "$rel" ] && rel=$rel/
      repo=$top
      printf 'git\0%s\0' "$rel"
      n=0
      {{
        for f in .[!.]* ..?* *; do
          [ -e "$f" ] || [ -L "$f" ] || continue
          [ "$f" = .git ] && continue
          n=$((n+1))
          [ "$n" -gt {cap} ] && continue
          printf '%s\0' "$rel$f"
        done
      }} | g check-ignore -z --stdin
      printf 'rc\0%s\0' "$?"
      ;;
    *) printf 'nogit\0' ;;
  esac
fi
"#
    ));
    script
}

/// `$1` directory (empty: the login directory), `$2` `1` to trust the repository,
/// `$3` seconds `find` may run (empty: unbounded).
///
/// Tokens: `ok`, physical path, login directory, one of `nogit`, `giterr message` or
/// `git top`, then a stream: `/cached … /others … /end` for git, else
/// `/find … /end`. Why `/`-prefixed markers: git and `find` print relative
/// paths, so no path can equal a marker.
pub(crate) fn index_script() -> String {
    let cap = INDEX_STREAM_BYTE_CAP;
    let mut script = String::new();
    script.push_str(ENTER_DIRECTORY);
    script.push_str(FIND_REPOSITORY);
    script.push_str(GIT_TOP);
    script.push_str(&format!(
        r#"
if [ "$gitstate" = git ]; then
  repo=$top
  printf 'git\0%s\0' "$top"
  printf '/cached\0'
  g ls-files -z --stage --cached | head -c {cap}
  printf '\0/others\0'
  g ls-files -z --others --exclude-standard | head -c {cap}
  printf '\0/end\0'
  exit 0
elif [ "$gitstate" = giterr ]; then
  printf 'giterr\0%s\0' "$gitmsg"
else
  printf 'nogit\0'
fi
# Why not walk these: `/` and the kernel's trees hold millions of paths, and
# `find` keeps running inside the environment after the host gave up.
case "$cwd" in
  /|/proc|/proc/*|/sys|/sys/*|/dev|/dev/*)
    printf '/find\0\0/trunc\0/end\0'
    exit 0 ;;
esac
printf '/find\0'
if [ -n "$3" ] && command -v timeout >/dev/null 2>&1; then
  timeout "$3" find . -name .git -prune -o -type f -exec printf '%s\0' {{}} + | head -c {cap}
else
  find . -name .git -prune -o -type f -exec printf '%s\0' {{}} + | head -c {cap}
fi
printf '\0/end\0'
"#
    ));
    script
}

/// `$1` file, `$2` the revision the caller already holds (may be empty).
///
/// Tokens: `enoent`, `notfile`, `eacces` or `toolarge`; else `unchanged rev`
/// or `content rev` followed by the file bytes to the end of the output.
/// Why check size before content: an oversized file is rejected without
/// transferring it, over a possibly slow ssh link.
pub(crate) fn read_script() -> String {
    let cap = READ_SCRIPT_BYTE_CAP;
    let limit = crate::files::MAX_TEXT_BYTES;
    format!(
        r#"
printf 'HERDR-OFFHOST-1\0'
if [ ! -e "$1" ]; then printf 'enoent\0'; exit 0; fi
if [ ! -f "$1" ]; then printf 'notfile\0'; exit 0; fi
if [ ! -r "$1" ]; then printf 'eacces\0'; exit 0; fi
size=$(wc -c < "$1" | tr -d ' ')
if [ "$size" -gt {limit} ]; then printf 'toolarge\0'; exit 0; fi
set -- "$1" "$2" $(cksum < "$1")
rev="cksum:$3:$4"
if [ "$2" = "$rev" ]; then printf 'unchanged\0%s\0' "$rev"; exit 0; fi
printf 'content\0%s\0' "$rev"
head -c {cap} "$1"
"#
    )
}
