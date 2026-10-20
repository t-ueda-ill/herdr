//! The POSIX `sh` half of an upload into a container or over ssh: one process
//! per upload reads headers and bodies from stdin, assembles them in a stage
//! beside the destination, and publishes or rolls back at `C`.
//!
//! Stdin, one header per line (paths are relative, validated, and free of
//! control characters):
//!
//! ```text
//! Q <name>                          a name whose existence to report
//! Q.                                end of the names
//! D <path>                          create a directory
//! L <path> NL <target>              create a symlink
//! F <size> <mode> <stamp|-> <path>  followed by exactly <size> body bytes
//! M <mode> <stamp|-> <path>         attributes of a nested directory
//! T <mode> <stamp|-> <name>         attributes of a top-level directory, applied after publishing
//! C fail|rename|replace             publish
//! EOF                               abort: remove the stage
//! ```
//!
//! Stdout is `HERDR-OFFHOST-1\0` and NUL-terminated tokens; see [`Token`].

/// Why `dd bs=<remaining> count=1` for bodies: it is one read(2) of at most the
/// remaining bytes, so it can never consume the next header. BusyBox `head -c`
/// reads ahead through stdio and would. Why not tar: minimal images lack it,
/// and it cannot report which entry failed.
/// Why `mkdir` with a server-made token instead of `mktemp -d`: minimal images
/// may lack mktemp, and `mkdir` already fails when the name exists.
/// Why the stage beside the destination: publishing is then a rename on one
/// filesystem, never a copy.
/// Why `trap ''` during commit: originals moved aside by `replace` sit in
/// `$stage/o`, and the abort trap would delete them. A commit is renames only,
/// so it always runs to publication or rollback.
/// Why chmod before every removal: an uploaded read-only directory would make
/// `rm -rf` fail and leak the stage.
/// Why top-level attributes after publishing: moving a directory into another
/// parent needs write access to the directory itself (its `..` entry).
/// Why `${got:-0}`: an empty `wc` result is an arithmetic syntax error in dash,
/// which would exit without reporting.
/// Why `fresh` before every entry and noclobber for files: where the target
/// filesystem folds case or Unicode normalization, `a` may resolve to an
/// uploaded symlink `A`; following it would write or chmod outside the stage,
/// and `ln -s` onto a directory would link inside it. `true`, not `:`, because
/// a failed redirection on a special builtin exits dash without a report.
/// Why `s` after every slow commit step: the client times out on silence, not
/// on a total that cannot account for forks per directory.
/// Why `o` becomes `x` before `committed`, and the stage is wiped only after
/// the paths: a client that stops waiting once every path is reported may
/// kill a cleanup midway, and the stale sweep then removes the rest instead of
/// warning about kept originals.
/// Why recount the file: `||` sees only `wc`'s status, and tee keeps
/// forwarding on ENOSPC, so a short file would otherwise be reported ok.
pub(crate) const RECEIVER_SCRIPT: &str = r#"printf 'HERDR-OFFHOST-1\0'
if [ -z "$1" ]; then cd -P -- "${HOME:-/}" 2>/dev/null || { printf 'eacces\0'; exit 0; }
elif [ ! -e "$1" ]; then printf 'enoent\0'; exit 0
elif [ ! -d "$1" ]; then printf 'notdir\0'; exit 0
else cd -P -- "$1" 2>/dev/null || { printf 'eacces\0'; exit 0; }; fi
dest=$(pwd -P)
d=$dest; [ "$d" = / ] && d=
[ -w . ] || { printf 'eacces\0'; exit 0; }
umask 077
stage=$d/.herdr-upload-$2
mkdir -m 700 -- "$stage" 2>/dev/null || { printf 'stagefail\0'; exit 0; }
wipe() { chmod -R u+w -- "$1" 2>/dev/null; rm -rf -- "$1"; }
cleanup() { wipe "$stage"; }
rearm() { trap 'cleanup; exit 1' HUP INT TERM PIPE; }
rearm
mkdir -- "$stage/i" "$stage/o" "$stage/t" || { cleanup; printf 'stagefail\0'; exit 0; }
uid=$(id -u)
for x in "$d"/.herdr-upload-*; do
  [ "$x" = "$stage" ] && continue
  [ -d "$x" ] && [ ! -L "$x" ] || continue
  [ -n "$(find "$x" -prune -user "$uid" -mtime +0 2>/dev/null)" ] || continue
  [ -z "$(ls -A -- "$x/o" 2>/dev/null)" ] || { printf 'w\0leftover\0%s\0' "$x"; continue; }
  wipe "$x"
done
printf 'ready\0%s\0' "$dest"
n=0
fail() { cleanup; printf 'err\0%s\0%s\0' "$n" "$1"; exit 0; }
undone() { cleanup; printf 'err\0commit\0%s\0' "$1"; exit 0; }
fresh() {
  case $1 in */*) up=$stage/i/${1%/*}; [ -d "$up" ] && [ ! -L "$up" ] || return 1 ;; esac
  [ ! -e "$stage/i/$1" ] && [ ! -L "$stage/i/$1" ]
}
taken() { [ -e "$d/$1" ] || [ -L "$d/$1" ] || [ -e "$stage/i/$1" ] || [ -L "$stage/i/$1" ]; }
pick() {
  stem=$1 ext=
  if [ "$2" = 0 ]; then case $1 in ?*.*) stem=${1%.*} ext=.${1##*.} ;; esac; fi
  i=1
  while [ "$i" -le 9999 ]; do
    taken "$stem ($i)$ext" || { fin="$stem ($i)$ext"; return 0; }
    i=$((i+1))
  done
  return 1
}
rollback() {
  j=$1 bad=0
  while [ "$j" -ge 1 ]; do
    eval "nm=\$n_$j fn=\$f_$j mk=\$m_$j rk=\$r_$j"
    if [ -n "$mk" ]; then mv -- "$d/$fn" "$stage/i/$nm" || bad=1; eval "m_$j="; fi
    if [ -n "$rk" ]; then mv -- "$stage/o/$j" "$d/$nm" || bad=1; eval "r_$j="; fi
    printf 's\0'
    j=$((j-1))
  done
  [ "$bad" = 0 ] || { printf 'rbfail\0%s\0' "$stage"; exit 0; }
}
commit() {
  trap '' HUP INT TERM PIPE
  case $1 in fail|rename|replace) ;; *) fail header ;; esac
  k=0
  for s in "$stage/i"/* "$stage/i"/.[!.]* "$stage/i"/..?*; do
    [ -e "$s" ] || [ -L "$s" ] || continue
    k=$((k+1)); nm=${s##*/}; eval "n_$k=\$nm m_$k= r_$k= f_$k="
  done
  K=$k
  if [ "$1" = fail ]; then
    hit=0 k=1
    while [ "$k" -le "$K" ]; do
      eval "nm=\$n_$k"
      if [ -e "$d/$nm" ] || [ -L "$d/$nm" ]; then printf 'c\0%s\0' "$nm"; hit=1; fi
      k=$((k+1))
    done
    if [ "$hit" = 1 ]; then printf 'conflict\0'; rearm; return 1; fi
  fi
  k=1
  while [ "$k" -le "$K" ]; do
    eval "nm=\$n_$k"
    s=$stage/i/$nm fin=$nm
    if [ -e "$d/$nm" ] || [ -L "$d/$nm" ]; then
      case $1 in
      rename)
        if [ -L "$s" ] || [ -d "$s" ]; then whole=1; else whole=0; fi
        pick "$nm" "$whole" || { rollback "$k"; undone noname; } ;;
      replace)
        mv -- "$d/$nm" "$stage/o/$k" || { rollback "$k"; undone replace; }
        eval "r_$k=1" ;;
      fail)
        rollback "$k"; printf 'c\0%s\0conflict\0' "$nm"; rearm; return 1 ;;
      esac
    fi
    eval "f_$k=\$fin"
    mv -- "$s" "$d/$fin" || { rollback "$k"; undone publish; }
    eval "m_$k=1"
    printf 's\0'
    k=$((k+1))
  done
  k=1
  while [ "$k" -le "$K" ]; do
    eval "nm=\$n_$k fn=\$f_$k"
    if [ -f "$stage/t/$nm" ] && IFS=' ' read -r mode st < "$stage/t/$nm"; then
      chmod "$mode" "$d/$fn" 2>/dev/null || printf 'w\0chmod\0%s\0' "$fn"
      [ "$st" = - ] || TZ=UTC0 touch -c -t "$st" "$d/$fn" 2>/dev/null || printf 'w\0mtime\0%s\0' "$fn"
      printf 's\0'
    fi
    k=$((k+1))
  done
  mv -- "$stage/o" "$stage/x" 2>/dev/null
  printf 'committed\0'
  k=1
  while [ "$k" -le "$K" ]; do
    eval "nm=\$n_$k fn=\$f_$k"
    printf 'p\0%s\0%s\0' "$nm" "$d/$fn"
    k=$((k+1))
  done
  wipe "$stage"
  return 0
}
while IFS= read -r h; do
  case $h in
  'Q.') printf 'qend\0' ;;
  'Q '*) q=${h#Q }; if [ -e "$d/$q" ] || [ -L "$d/$q" ]; then printf 'c\0%s\0' "$q"; fi ;;
  'D '*) n=$((n+1)); p=${h#D }; fresh "$p" || fail mkdir
    mkdir -- "$stage/i/$p" || fail mkdir; printf 'ok\0%s\0' "$n" ;;
  'L '*) n=$((n+1)); p=${h#L }; IFS= read -r t || fail eof; fresh "$p" || fail symlink
    ln -s -- "$t" "$stage/i/$p" || fail symlink; [ -L "$stage/i/$p" ] || fail symlink
    printf 'ok\0%s\0' "$n" ;;
  'F '*) n=$((n+1)); r=${h#F }
    size=${r%% *}; r=${r#* }; mode=${r%% *}; r=${r#* }; st=${r%% *}; p=${r#* }
    f=$stage/i/$p
    fresh "$p" || fail create
    set -C; true > "$f"; c=$?; set +C
    [ "$c" = 0 ] && [ -f "$f" ] && [ ! -L "$f" ] || fail create
    left=$size
    while [ "$left" -gt 0 ]; do
      b=$left; [ "$b" -gt 1048576 ] && b=1048576
      got=$(dd bs="$b" count=1 2>/dev/null | tee -a "$f" | wc -c) || fail write
      got=$(( ${got:-0} + 0 ))
      [ "$got" -gt 0 ] || fail eof
      left=$((left-got))
    done
    have=$(wc -c < "$f")
    [ "$(( ${have:-0} + 0 ))" -eq "$size" ] || fail short
    chmod "$mode" "$f" || fail chmod
    [ "$st" = - ] || TZ=UTC0 touch -c -t "$st" "$f" 2>/dev/null || printf 'w\0mtime\0%s\0' "$p"
    printf 'ok\0%s\0' "$n" ;;
  'M '*) r=${h#M }; mode=${r%% *}; r=${r#* }; st=${r%% *}; p=${r#* }
    [ -d "$stage/i/$p" ] && [ ! -L "$stage/i/$p" ] || fail chmod
    chmod "$mode" "$stage/i/$p" || fail chmod
    [ "$st" = - ] || TZ=UTC0 touch -c -t "$st" "$stage/i/$p" 2>/dev/null || printf 'w\0mtime\0%s\0' "$p"
    printf 's\0' ;;
  'T '*) r=${h#T }; mode=${r%% *}; r=${r#* }; st=${r%% *}; p=${r#* }
    printf '%s %s\n' "$mode" "$st" > "$stage/t/$p" || fail attr ;;
  'C '*) if commit "${h#C }"; then exit 0; fi ;;
  *) fail header ;;
  esac
done
cleanup; printf 'aborted\0'
"#;

use crate::api::schema::PaneUploadCollision;

/// One header line (and, for `L`, its target line) as the script reads it.
pub(crate) fn query_line(name: &str) -> String {
    format!("Q {name}\n")
}

pub(crate) const QUERY_END: &str = "Q.\n";

pub(crate) fn directory_line(path: &str) -> String {
    format!("D {path}\n")
}

pub(crate) fn symlink_lines(path: &str, target: &str) -> String {
    format!("L {path}\n{target}\n")
}

fn stamp_field(mtime: Option<i64>) -> String {
    mtime
        .and_then(super::stamp::touch_stamp)
        .unwrap_or_else(|| "-".to_owned())
}

/// Why `{:04o}`: `chmod 420` would read decimal 0o644 as octal 0420.
pub(crate) fn file_line(path: &str, size: u64, mode: u32, mtime: Option<i64>) -> String {
    format!("F {size} {mode:04o} {} {path}\n", stamp_field(mtime))
}

pub(crate) fn nested_attrs_line(path: &str, mode: u32, mtime: Option<i64>) -> String {
    format!("M {mode:04o} {} {path}\n", stamp_field(mtime))
}

pub(crate) fn top_attrs_line(name: &str, mode: u32, mtime: Option<i64>) -> String {
    format!("T {mode:04o} {} {name}\n", stamp_field(mtime))
}

pub(crate) fn commit_line(collision: PaneUploadCollision) -> Option<&'static str> {
    match collision {
        PaneUploadCollision::Fail => Some("C fail\n"),
        PaneUploadCollision::Rename => Some("C rename\n"),
        PaneUploadCollision::Replace => Some("C replace\n"),
        PaneUploadCollision::Unknown => None,
    }
}

/// What the receiver reported. Names and paths stay as received; the remote
/// filesystem may hold names that are not UTF-8, which are reported lossily.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Token {
    /// The destination does not exist, is not a directory, or cannot be written.
    NotFound,
    NotDirectory,
    PermissionDenied,
    /// The stage directory could not be created.
    StageFailed,
    Ready(String),
    /// A name that exists in the destination.
    Exists(String),
    QueryEnd,
    /// Entry number `n` (1-based) was written.
    Ok(u64),
    /// Entry number (or `commit`) and what failed. The stage is gone.
    Error(String, String),
    Warning(String, String),
    /// `collision = fail` found the preceding `Exists` names; the stage stays.
    Conflict,
    /// Rollback could not restore everything; the stage at this path keeps the rest.
    RollbackFailed(String),
    /// One slow commit step finished (a directory's attributes, a rename).
    Step,
    Committed,
    Published(String, String),
    Aborted,
}

fn arity(keyword: &[u8]) -> Option<usize> {
    Some(match keyword {
        b"enoent" | b"notdir" | b"eacces" | b"stagefail" | b"qend" | b"conflict" | b"committed"
        | b"aborted" | b"s" => 0,
        b"ready" | b"c" | b"ok" | b"rbfail" => 1,
        b"err" | b"w" | b"p" => 2,
        _ => return None,
    })
}

/// Splits the receiver's stdout into tokens as bytes arrive.
#[derive(Debug, Default)]
pub(crate) struct TokenReader {
    pending: Vec<u8>,
    fields: Vec<Vec<u8>>,
    marker_seen: bool,
    /// Bytes buffered before the marker, bounded: a login banner is dropped.
    preamble: usize,
}

/// A login banner longer than this means the script never started.
const MAX_PREAMBLE_BYTES: usize = 64 * 1024;
const MAX_FIELD_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Malformed(pub(crate) String);

impl TokenReader {
    pub(crate) fn feed(&mut self, bytes: &[u8]) -> Result<Vec<Token>, Malformed> {
        let mut tokens = Vec::new();
        for &byte in bytes {
            if byte != 0 {
                self.pending.push(byte);
                let limit = if self.marker_seen {
                    MAX_FIELD_BYTES
                } else {
                    MAX_PREAMBLE_BYTES
                };
                if self.pending.len() > limit {
                    return Err(Malformed("receiver output field is too long".into()));
                }
                continue;
            }
            let field = std::mem::take(&mut self.pending);
            if !self.marker_seen {
                // Whatever a login shell printed first ends with the marker.
                let marker = crate::offhost::scripts::OUTPUT_MARKER.as_bytes();
                if field.ends_with(marker) {
                    self.marker_seen = true;
                } else {
                    self.preamble += field.len() + 1;
                    if self.preamble > MAX_PREAMBLE_BYTES {
                        return Err(Malformed("receiver did not start".into()));
                    }
                }
                continue;
            }
            self.fields.push(field);
            let keyword = &self.fields[0];
            let Some(arity) = arity(keyword) else {
                return Err(Malformed(format!(
                    "unknown receiver token {:?}",
                    String::from_utf8_lossy(keyword)
                )));
            };
            if self.fields.len() == arity + 1 {
                let fields = std::mem::take(&mut self.fields);
                tokens.push(token(fields));
            }
        }
        Ok(tokens)
    }
}

fn token(fields: Vec<Vec<u8>>) -> Token {
    let mut text = fields
        .into_iter()
        .map(|field| String::from_utf8_lossy(&field).into_owned());
    let keyword = text.next().unwrap_or_default();
    let mut arg = || text.next().unwrap_or_default();
    match keyword.as_str() {
        "enoent" => Token::NotFound,
        "notdir" => Token::NotDirectory,
        "eacces" => Token::PermissionDenied,
        "stagefail" => Token::StageFailed,
        "qend" => Token::QueryEnd,
        "conflict" => Token::Conflict,
        "committed" => Token::Committed,
        "s" => Token::Step,
        "aborted" => Token::Aborted,
        "ready" => Token::Ready(arg()),
        "c" => Token::Exists(arg()),
        "ok" => Token::Ok(arg().parse().unwrap_or(0)),
        "rbfail" => Token::RollbackFailed(arg()),
        "err" => {
            let what = arg();
            Token::Error(what, arg())
        }
        "w" => {
            let what = arg();
            Token::Warning(what, arg())
        }
        _ => {
            let name = arg();
            Token::Published(name, arg())
        }
    }
}

#[cfg(all(test, unix))]
pub(crate) mod tests;
