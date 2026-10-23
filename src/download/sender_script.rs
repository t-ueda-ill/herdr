//! The POSIX `sh` half of a download out of a container or over ssh, and the
//! parser for what it prints. One helper process per download walks the
//! selection, reports an estimate, waits for `G`, then streams every entry.
//!
//! Arguments: `$1` = [`EMIT_SCRIPT`], `$2` = entry limit, `$3` = byte limit,
//! `$4...` = absolute paths without a trailing `/`. Stdout is
//! `HERDR-OFFHOST-1\0` and NUL-terminated tokens:
//!
//! ```text
//! walk:   missing <path> | eacces <path> | special <path> | nozero | changed
//!         (any of these ends the helper)
//!         big                      the limits were exceeded
//!         est <entries> <bytes>    then the helper waits for `G\n` on stdin
//! stream: d <rel> <meta>           directory
//!         l <rel> <target>         symlink, not followed
//!         f <rel> <meta>           file: exactly <size> body bytes, then e
//!         e <status> <meta2>       the file's trailer
//!         s <rel> <reason>         left out: special | eacces | link
//!         w <name>                 `find` could not read part of this selection
//!         end
//! meta = "<size> <mode in octal> <mtime|-> <change token>"
//! ```
//!
//! Everything the helper prints is untrusted: it runs where the files are, and
//! that may be a broken or hostile environment.

use crate::api::schema::{PaneUploadEntryHeader, PaneUploadEntryKind};
use crate::offhost::tokens::{Grammar, Item, Malformed, Shape, TokenReader};
use crate::upload::path::Failure;

use super::frame::{Changed, Frame, SkipReason};
use super::notice_text;

/// Run by `find -exec sh -c` once per batch of entries; `$1` is the parent the
/// reported paths are relative to, the rest are the paths `find` found.
///
/// Why this is an argument of [`SENDER_SCRIPT`] and not written into it: the
/// body of `find -exec sh -c '...' sh {} +` inside the script would be a quote
/// inside a quote. `command_argv` quotes arguments correctly, so passing the
/// text as one removes the nesting.
/// Why `ls -lnd` and awk instead of `stat`: POSIX has no stat(1). BusyBox can
/// be built without it and macOS has `stat -f`, so it only supplies the mtime,
/// by whichever form the begin step found to work.
/// Why a single trailing `exit 0`: `find -exec ... +` reports failure when any
/// batch exits nonzero, and the last test in the loop must not be mistaken for
/// an unreadable folder.
/// Why pad with `/dev/zero` and cut with `head -c`: the stream must carry
/// exactly `size` bytes whatever the file does meanwhile, or the framing
/// slips. The trailer says whether it changed.
/// Why fd 3 for `cat`'s status: a pipeline hides it, and dash has no pipefail.
/// A read that fails halfway (EIO, a dropped NFS mount) leaves size and mtime
/// as they were, so only that status finds the zeros that fill the rest.
/// Why skip the pipeline for an empty file: BSD `head -c 0` is an error.
/// Why `L` for a name without `/` in its path: a link selected on its own is
/// followed (its target is saved), while links inside folders stay links.
/// Why the trailing dot after `readlink`: `$(...)` strips every trailing
/// newline, which would send a different target. The server skips the link
/// when a newline remains.
pub(crate) const EMIT_SCRIPT: &str = r##"exec 4>&1
nl='
'
bits() {
  b=0
  case $1 in r??) b=4 ;; esac
  case $1 in ?w?) b=$((b+2)) ;; esac
  case $1 in ??[xst]) b=$((b+1)) ;; esac
}
meta() {
  path=$1 deref=$2
  l=$(ls -lnd"$deref" -- "$path" 2>/dev/null) || return 1
  set -f; set -- $l; set +f
  [ $# -ge 5 ] || return 1
  perm=$1 size=$5 tok="$1 $2 $3 $4 $5 $6 $7 $8"
  case $size in ''|*[!0-9]*) return 1 ;; esac
  a=${perm#?}; a=${a%"${a#?????????}"}
  u=${a%??????} g=${a#???} g=${g%???} o=${a#??????}
  bits "$u"; ub=$b; bits "$g"; gb=$b; bits "$o"; ob=$b
  case $mt in
    c) v=$(stat ${deref:+-L} -c %Y -- "$path" 2>/dev/null) ;;
    f) v=$(stat ${deref:+-L} -f %m -- "$path" 2>/dev/null) ;;
    *) v=- ;;
  esac
  case $v in ''|*[!0-9]*) v=- ;; esac
  printf '%s %s%s%s %s %s' "$size" "$ub" "$gb" "$ob" "$v" "$tok"
}
parent=$1; shift
for f do
  [ -z "$stop" ] || [ -d "$stop" ] || exit 0
  rel=${f#"$parent"/}
  deref=
  case $rel in */*) ;; *) deref=L ;; esac
  if [ -L "$f" ] && [ -z "$deref" ]; then
    if t=$(readlink -- "$f" 2>/dev/null && echo .) && t=${t%.} && t=${t%"$nl"} && [ -n "$t" ]
    then printf 'l\0%s\0%s\0' "$rel" "$t"
    else printf 's\0%s\0link\0' "$rel"
    fi
  elif [ -d "$f" ]; then
    if m=$(meta "$f" "$deref"); then printf 'd\0%s\0%s\0' "$rel" "$m"
    else printf 's\0%s\0eacces\0' "$rel"
    fi
  elif [ -f "$f" ]; then
    { [ -r "$f" ] && m=$(meta "$f" "$deref"); } || { printf 's\0%s\0eacces\0' "$rel"; continue; }
    size=${m%% *}
    printf 'f\0%s\0%s\0' "$rel" "$m"
    if [ "$size" -gt 0 ]; then
      st=$( { { cat -- "$f" 2>/dev/null; printf %s "$?" >&3; cat /dev/zero 2>/dev/null; } \
              | head -c "$size" >&4; } 3>&1 )
    else
      st=0
    fi
    printf 'e\0%s\0%s\0' "$st" "$(meta "$f" "$deref" 2>/dev/null)"
  else
    printf 's\0%s\0special\0' "$rel"
  fi
done
exit 0
"##;

/// Why `unset BLOCK_SIZE LS_BLOCK_SIZE TIME_STYLE` and `LC_ALL=C`: GNU `ls -l`
/// honours them (the size can read `1.2K`), and the date columns change with
/// the style and the locale.
/// Why `ls -lnd` and awk for the estimate: see [`EMIT_SCRIPT`]. Names with a
/// newline split a line, which only skews this estimate.
/// Why `find -H`: a selected link is followed and nothing below it is. The
/// estimate counts such a link as the link itself: it is only an estimate, and
/// the server enforces its limits on what is actually sent.
/// Why awk prints nothing when nothing was listed: every selected path
/// vanished after the checks above, which is `changed`, not "empty".
/// Why `IFS= read -r go` needs the newline: `read` waits for one until EOF.
/// Why wait for `G` at all: the estimate is the progress card's denominator,
/// and a client that cannot stage the download can cancel before any file is
/// read.
/// Why `[ -x ]` for a folder: `find` cannot enter one with read but no search
/// permission.
/// Why a watchdog after `G`: a container runtime does not pass the client's
/// death on to the process and keeps draining its output, and over ssh with no
/// terminal nothing sends SIGHUP, so the stream would run to the end with
/// nobody listening. It reads stdin to its end, which is what a cancel closes,
/// then removes the `stop` folder and ends this shell; each batch of
/// [`EMIT_SCRIPT`] leaves when the folder is gone. Why a folder and not a
/// signal: the helper has no process group of its own to signal safely. A
/// `cat` already blocked on a path that turned into a FIFO is not reached.
/// Why the watchdog's output goes to `/dev/null`: a copy of the pipe held
/// open would keep the reader waiting after `end`.
/// Why `5<&0`: a background command reads `/dev/null` for stdin unless it
/// names the descriptor.
/// Why no watchdog without `mktemp`: the stream then ends the old way.
pub(crate) const SENDER_SCRIPT: &str = r##"unset BLOCK_SIZE LS_BLOCK_SIZE TIME_STYLE CLICOLOR_FORCE; LC_ALL=C; export LC_ALL
printf 'HERDR-OFFHOST-1\0'
emit=$1 me=$2 mb=$3; shift 3
[ -c /dev/zero ] || { printf 'nozero\0'; exit 0; }
for p do
  [ -e "$p" ] || { printf 'missing\0%s\0' "$p"; exit 0; }
  if [ ! -d "$p" ] && [ ! -f "$p" ]; then printf 'special\0%s\0' "$p"; exit 0; fi
  [ -r "$p" ] || { printf 'eacces\0%s\0' "$p"; exit 0; }
  if [ -d "$p" ] && [ ! -x "$p" ]; then printf 'eacces\0%s\0' "$p"; exit 0; fi
done
if stat -c %Y / >/dev/null 2>&1; then mt=c
elif stat -f %m / >/dev/null 2>&1; then mt=f
else mt=-
fi
export mt
est=$(find -H "$@" -exec ls -lnd -- {} + 2>/dev/null | awk -v me="$me" -v mb="$mb" '
  { n++; if (substr($1, 1, 1) == "-") b += $5; if (n > me || b > mb) { print "big"; exit } }
  END { if (n > 0 && n <= me && b <= mb) printf "%.0f %.0f", n, b }')
case $est in
  big) printf 'big\0'; exit 0 ;;
  ''|*[!0-9\ ]*) printf 'changed\0'; exit 0 ;;
esac
printf 'est\0%s\0%s\0' "${est% *}" "${est#* }"
IFS= read -r go || exit 0
[ "$go" = G ] || exit 0
stop=$(mktemp -d 2>/dev/null) || stop=
export stop
if [ -n "$stop" ]; then
  trap 'rm -rf "$stop"' EXIT
  main=$$
  exec 5<&0
  ( cat <&5; rm -rf "$stop"; kill "$main" ) >/dev/null 2>&1 &
fi
for p do
  parent=${p%/*}
  find -H "$p" -exec sh -c "$emit" sh "$parent" {} + 2>/dev/null || printf 'w\0%s\0' "${p##*/}"
done
printf 'end\0'
"##;

/// The most body bytes in one [`Frame::Data`].
const DATA_FRAME_BYTES: usize = 64 * 1024;

pub(crate) struct SenderGrammar;

impl Grammar for SenderGrammar {
    const SCRIPT: &'static str = "sender";

    fn shape(keyword: &[u8]) -> Option<Shape> {
        Some(match keyword {
            b"nozero" | b"changed" | b"big" | b"end" => Shape::fields(0),
            b"missing" | b"eacces" | b"special" | b"w" => Shape::fields(1),
            b"est" | b"d" | b"l" | b"s" | b"e" => Shape::fields(2),
            b"f" => Shape {
                arity: 2,
                body: Some(file_body_length),
            },
            _ => return None,
        })
    }
}

fn malformed(message: impl Into<String>) -> Malformed {
    Malformed(message.into())
}

/// Digits only: `u64::from_str` also accepts a leading `+`.
fn strict_u64(field: &[u8], what: &str) -> Result<u64, Malformed> {
    if field.is_empty() || !field.iter().all(u8::is_ascii_digit) {
        return Err(malformed(format!("{what} is not a number")));
    }
    std::str::from_utf8(field)
        .ok()
        .and_then(|digits| digits.parse().ok())
        .ok_or_else(|| malformed(format!("{what} is too large")))
}

/// What a `meta` field says about an entry.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Meta {
    size: u64,
    mode: u32,
    mtime: Option<i64>,
}

fn parse_meta(field: &[u8]) -> Result<Meta, Malformed> {
    let mut parts = field.splitn(4, |byte| *byte == b' ');
    let (Some(size), Some(mode), Some(mtime), Some(_token)) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(malformed("an entry's attributes have too few fields"));
    };
    let size = strict_u64(size, "size")?;
    if mode.is_empty() || mode.len() > 6 || !mode.iter().all(|byte| (b'0'..=b'7').contains(byte)) {
        return Err(malformed("mode is not an octal number"));
    }
    let mode = std::str::from_utf8(mode)
        .ok()
        .and_then(|digits| u32::from_str_radix(digits, 8).ok())
        .ok_or_else(|| malformed("mode is not an octal number"))?;
    let mtime = if mtime == b"-" {
        None
    } else {
        Some(
            i64::try_from(strict_u64(mtime, "mtime")?)
                .map_err(|_| malformed("mtime is too large"))?,
        )
    };
    Ok(Meta {
        size,
        // Why mask: the session refuses anything above 0o777, and setuid
        // bits do not belong on a downloaded copy.
        mode: mode & 0o777,
        mtime,
    })
}

/// `fields`: keyword, rel, meta.
fn file_body_length(fields: &[Vec<u8>]) -> Result<u64, Malformed> {
    let meta = fields
        .get(2)
        .ok_or_else(|| malformed("a file has no attributes"))?;
    let size = meta.split(|byte| *byte == b' ').next().unwrap_or_default();
    strict_u64(size, "size")
}

fn lossy(bytes: &[u8]) -> String {
    notice_text(&String::from_utf8_lossy(bytes))
}

fn not_understood(message: &str) -> Failure {
    (
        "download_failed",
        format!("the sender's output was not understood: {message}"),
    )
}

/// What the helper reported before it was told to go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Event {
    Estimate { entries: u64, bytes: u64 },
    Frame(Frame),
}

#[derive(Debug)]
enum Stage {
    Walk,
    Stream,
    /// A file's body has been announced; only its trailer may follow.
    File {
        meta: Vec<u8>,
        left: u64,
    },
    Ended,
    Failed,
}

/// Turns the helper's stdout, in whatever pieces it arrives, into events,
/// holding the helper to its grammar: walk words only before `est`, stream
/// words only after, a trailer after every file body, nothing after `end`.
pub(crate) struct SenderParser {
    reader: TokenReader<SenderGrammar>,
    stage: Stage,
}

impl Default for SenderParser {
    fn default() -> Self {
        Self {
            reader: TokenReader::default(),
            stage: Stage::Walk,
        }
    }
}

impl SenderParser {
    /// `true` once `end` was read.
    #[cfg(test)]
    pub(crate) fn ended(&self) -> bool {
        matches!(self.stage, Stage::Ended)
    }

    pub(crate) fn feed(&mut self, bytes: &[u8]) -> Result<Vec<Event>, Failure> {
        if matches!(self.stage, Stage::Failed) {
            return Err(not_understood("output after a failure"));
        }
        let items = match self.reader.feed(bytes) {
            Ok(items) => items,
            Err(Malformed(message)) => {
                self.stage = Stage::Failed;
                return Err(not_understood(&message));
            }
        };
        let mut events = Vec::new();
        for item in items {
            match self.step(item, &mut events) {
                Ok(()) => {}
                Err(failure) => {
                    self.stage = Stage::Failed;
                    return Err(failure);
                }
            }
        }
        Ok(events)
    }

    fn step(&mut self, item: Item, events: &mut Vec<Event>) -> Result<(), Failure> {
        match item {
            Item::Body(bytes) => {
                let Stage::File { left, .. } = &mut self.stage else {
                    return Err(not_understood("file data outside a file"));
                };
                let len = bytes.len() as u64;
                *left = left
                    .checked_sub(len)
                    .ok_or_else(|| not_understood("a file is longer than it said"))?;
                for chunk in bytes.chunks(DATA_FRAME_BYTES) {
                    events.push(Event::Frame(Frame::Data(chunk.to_vec())));
                }
                Ok(())
            }
            Item::Token(fields) => self.token(&fields, events),
        }
    }

    fn token(&mut self, fields: &[Vec<u8>], events: &mut Vec<Event>) -> Result<(), Failure> {
        let keyword = fields[0].as_slice();
        let arg = |index: usize| fields.get(index).map(Vec::as_slice).unwrap_or_default();
        match (&self.stage, keyword) {
            (Stage::Walk, b"missing") => {
                Err(("not_found", format!("{} does not exist", lossy(arg(1)))))
            }
            (Stage::Walk, b"eacces") => Err((
                "permission_denied",
                format!("{} cannot be read", lossy(arg(1))),
            )),
            (Stage::Walk, b"special") => Err((
                "invalid_params",
                format!("{} is not a file or folder", lossy(arg(1))),
            )),
            (Stage::Walk, b"nozero") => Err((
                "download_failed",
                "this environment has no /dev/zero, which downloads need".into(),
            )),
            (Stage::Walk, b"changed") => Err((
                "download_source_changed",
                "the selection changed while it was being scanned".into(),
            )),
            (Stage::Walk, b"big") => Err((
                "download_too_large",
                "the selection is larger than a download may be".into(),
            )),
            (Stage::Walk, b"est") => {
                let entries = strict_u64(arg(1), "the entry count").map_err(failure)?;
                let bytes = strict_u64(arg(2), "the byte count").map_err(failure)?;
                events.push(Event::Estimate { entries, bytes });
                self.stage = Stage::Stream;
                Ok(())
            }
            (Stage::Stream, b"d") => {
                let meta = parse_meta(arg(2)).map_err(failure)?;
                events.push(entry(arg(1), PaneUploadEntryKind::Directory, &meta, None));
                Ok(())
            }
            (Stage::Stream, b"l") => {
                // Why no attributes: the helper does not read a link's own, and
                // a link is never followed, so 0o777 and no mtime say so.
                let header = PaneUploadEntryHeader {
                    kind: PaneUploadEntryKind::Symlink,
                    mode: 0o777,
                    mtime: None,
                    size: None,
                    link_target: None,
                };
                events.push(Event::Frame(Frame::Entry {
                    rel: arg(1).to_vec(),
                    header,
                    link_target: Some(arg(2).to_vec()),
                }));
                Ok(())
            }
            (Stage::Stream, b"f") => {
                let meta = parse_meta(arg(2)).map_err(failure)?;
                events.push(entry(
                    arg(1),
                    PaneUploadEntryKind::File,
                    &meta,
                    Some(meta.size),
                ));
                self.stage = Stage::File {
                    meta: arg(2).to_vec(),
                    left: meta.size,
                };
                Ok(())
            }
            (Stage::File { meta, left }, b"e") => {
                if *left != 0 {
                    return Err(not_understood("a file ended before its data did"));
                }
                let status = strict_u64(arg(1), "a file's status").map_err(failure)?;
                let verdict = if status == 0 && arg(2) == meta.as_slice() {
                    Ok(())
                } else if status != 0 {
                    Err(Changed(format!(
                        "a file could not be read to its end or changed while it was read (status {status})"
                    )))
                } else {
                    Err(Changed(
                        "a file changed while it was being downloaded".into(),
                    ))
                };
                events.push(Event::Frame(Frame::FileEnd(verdict)));
                self.stage = Stage::Stream;
                Ok(())
            }
            (Stage::Stream, b"s") => {
                let reason = match arg(2) {
                    b"special" => SkipReason::Special,
                    b"eacces" => SkipReason::PermissionDenied,
                    b"link" => SkipReason::Link,
                    _ => return Err(not_understood("an unknown reason for leaving an entry out")),
                };
                events.push(Event::Frame(Frame::Skipped {
                    rel: arg(1).to_vec(),
                    reason,
                }));
                Ok(())
            }
            (Stage::Stream, b"w") => {
                events.push(Event::Frame(Frame::WalkWarning(format!(
                    "some of {} could not be read",
                    lossy(arg(1))
                ))));
                Ok(())
            }
            (Stage::Stream, b"end") => {
                events.push(Event::Frame(Frame::End));
                self.stage = Stage::Ended;
                Ok(())
            }
            (Stage::Ended, _) => Err(not_understood("output after the end")),
            _ => Err(not_understood(&format!(
                "unexpected {:?} here",
                String::from_utf8_lossy(keyword)
            ))),
        }
    }
}

fn failure(Malformed(message): Malformed) -> Failure {
    not_understood(&message)
}

fn entry(rel: &[u8], kind: PaneUploadEntryKind, meta: &Meta, size: Option<u64>) -> Event {
    Event::Frame(Frame::Entry {
        rel: rel.to_vec(),
        header: PaneUploadEntryHeader {
            kind,
            mode: meta.mode,
            mtime: meta.mtime,
            size,
            link_target: None,
        },
        link_target: None,
    })
}

#[cfg(test)]
mod parser_tests;
#[cfg(all(test, unix))]
mod shell_tests;
