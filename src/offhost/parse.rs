//! Turns the raw NUL-separated output of [`super::scripts`] into API types.
//! Names stay bytes until validated: a lossy conversion would name a file that
//! does not exist, and would hide how many entries were left out.

use std::collections::HashSet;

use crate::api::schema::{DirectoryEntry, DirectoryEntryKind};

pub(crate) const MAX_LIST_ENTRIES: usize = 2_000;
pub(crate) const MAX_INDEX_ENTRIES: usize = 50_000;

/// What the script itself found out about the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScriptStatus {
    NotFound,
    NotDirectory,
    NotFile,
    PermissionDenied,
    TooLarge,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ParseFailure {
    Script(ScriptStatus),
    Malformed(&'static str),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedListing {
    pub(crate) canonical: String,
    /// The login directory, for a pane that never reported its own.
    pub(crate) home: String,
    pub(crate) entries: Vec<DirectoryEntry>,
    pub(crate) truncated: bool,
    pub(crate) omitted: usize,
    pub(crate) unlistable: usize,
    pub(crate) git_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedIndex {
    pub(crate) canonical: String,
    /// `None` when the index came from the `find` fallback.
    pub(crate) git_root: Option<String>,
    pub(crate) entries: Vec<String>,
    pub(crate) truncated: bool,
    pub(crate) git_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ParsedRead {
    Unchanged { revision: String },
    Content { revision: String, bytes: Vec<u8> },
}

/// The output after the first marker record: whatever the remote login shell
/// printed before the script ran (a banner, an MOTD) is dropped.
pub(crate) fn after_marker(output: &[u8]) -> Result<&[u8], ParseFailure> {
    let mut marker = super::scripts::OUTPUT_MARKER.as_bytes().to_vec();
    marker.push(0);
    output
        .windows(marker.len())
        .position(|window| window == marker)
        .map(|start| &output[start + marker.len()..])
        .ok_or(ParseFailure::Malformed("script did not run"))
}

/// Tokens of an output whose records all end in NUL.
fn tokens(output: &[u8]) -> Vec<&[u8]> {
    let mut parts: Vec<&[u8]> = output.split(|byte| *byte == 0).collect();
    // The final record's terminator leaves one empty piece behind.
    if parts.last().is_some_and(|part| part.is_empty()) {
        parts.pop();
    }
    parts
}

fn status_of(token: &[u8]) -> Option<ScriptStatus> {
    Some(match token {
        b"enoent" => ScriptStatus::NotFound,
        b"notdir" => ScriptStatus::NotDirectory,
        b"notfile" => ScriptStatus::NotFile,
        b"eacces" => ScriptStatus::PermissionDenied,
        b"toolarge" => ScriptStatus::TooLarge,
        _ => return None,
    })
}

fn text(token: &[u8]) -> Option<String> {
    std::str::from_utf8(token).ok().map(str::to_owned)
}

/// What git said when it could not be used, phrased for the user.
fn git_error_text(message: &[u8]) -> String {
    let message = String::from_utf8_lossy(message);
    if message.contains("dubious ownership") || message.contains("safe.directory") {
        return "untrusted repository".to_owned();
    }
    let message = message.trim();
    if message.is_empty() {
        "git failed".to_owned()
    } else {
        message.to_owned()
    }
}

/// Reads the `ok`, canonical-path prefix both directory scripts share.
fn directory_prefix<'a>(
    tokens: &'a [&'a [u8]],
) -> Result<(String, String, std::slice::Iter<'a, &'a [u8]>), ParseFailure> {
    let mut iter = tokens.iter();
    let first = iter
        .next()
        .ok_or(ParseFailure::Malformed("empty script output"))?;
    if let Some(status) = status_of(first) {
        return Err(ParseFailure::Script(status));
    }
    if *first != b"ok" {
        return Err(ParseFailure::Malformed("missing status record"));
    }
    let canonical = iter
        .next()
        .and_then(|token| text(token))
        .ok_or(ParseFailure::Malformed("canonical path is not UTF-8"))?;
    let home = iter
        .next()
        .and_then(|token| text(token))
        .ok_or(ParseFailure::Malformed("login directory is not UTF-8"))?;
    Ok((canonical, home, iter))
}

struct Candidate {
    name: String,
    kind: DirectoryEntryKind,
    target_kind: Option<DirectoryEntryKind>,
}

impl Candidate {
    fn directory_like(&self) -> bool {
        self.kind == DirectoryEntryKind::Dir || self.target_kind == Some(DirectoryEntryKind::Dir)
    }
}

fn kind_from(token: &[u8]) -> DirectoryEntryKind {
    match token {
        b"d" => DirectoryEntryKind::Dir,
        b"f" => DirectoryEntryKind::File,
        b"l" => DirectoryEntryKind::Symlink,
        _ => DirectoryEntryKind::Other,
    }
}

fn target_kind_from(token: &[u8]) -> Option<DirectoryEntryKind> {
    match token {
        b"d" => Some(DirectoryEntryKind::Dir),
        b"f" => Some(DirectoryEntryKind::File),
        b"o" => Some(DirectoryEntryKind::Other),
        _ => None,
    }
}

pub(crate) fn parse_listing(
    output: &[u8],
    max_entries: usize,
) -> Result<ParsedListing, ParseFailure> {
    let all = tokens(after_marker(output)?);
    let (canonical, home, mut iter) = directory_prefix(&all)?;
    let mut candidates = Vec::new();
    let mut unlistable = 0usize;
    let mut printed = 0usize;
    let mut total = None;
    let mut rest: Vec<&[u8]> = Vec::new();
    while let Some(token) = iter.next() {
        match *token {
            b"e" => {
                let (Some(kind), Some(target), Some(name)) =
                    (iter.next(), iter.next(), iter.next())
                else {
                    return Err(ParseFailure::Malformed("truncated entry record"));
                };
                printed += 1;
                // Why not a lossy name: the client joins names into the paths
                // it sends back, and a replaced byte names a file that is not there.
                match text(name) {
                    Some(name) => candidates.push(Candidate {
                        name,
                        kind: kind_from(kind),
                        target_kind: target_kind_from(target),
                    }),
                    None => unlistable += 1,
                }
            }
            b"total" => {
                total = iter
                    .next()
                    .and_then(|token| text(token))
                    .and_then(|count| count.parse::<usize>().ok());
                rest = iter.by_ref().copied().collect();
                break;
            }
            _ => return Err(ParseFailure::Malformed("unexpected record in listing")),
        }
    }
    let total = total.ok_or(ParseFailure::Malformed("listing has no total"))?;
    let unprinted = total.saturating_sub(printed);

    let (ignored, git_error) = parse_ignored(&rest);

    // Directories (and links to them) first, then case-insensitive names with
    // the exact name as a tie-break, so the order matches the host listing.
    candidates.sort_by_cached_key(|candidate| {
        (
            !candidate.directory_like(),
            candidate.name.to_lowercase(),
            candidate.name.clone(),
        )
    });
    let omitted = (candidates.len() + unprinted).saturating_sub(max_entries);
    candidates.truncate(max_entries);
    let prefix = ignored.as_ref().map(|state| state.prefix.as_str());
    let entries = candidates
        .into_iter()
        .map(|candidate| DirectoryEntry {
            hidden: candidate.name.starts_with('.'),
            ignored: ignored.as_ref().map(|state| {
                state
                    .paths
                    .contains(&format!("{}{}", prefix.unwrap_or(""), candidate.name))
            }),
            name: candidate.name,
            kind: candidate.kind,
            target_kind: candidate.target_kind,
            size: None,
            mtime: None,
        })
        .collect();
    Ok(ParsedListing {
        canonical,
        home,
        entries,
        truncated: omitted > 0,
        omitted,
        unlistable,
        git_error,
    })
}

struct IgnoredState {
    prefix: String,
    paths: HashSet<String>,
}

/// `(ignored paths, git_error)` from the listing script's git section.
fn parse_ignored(rest: &[&[u8]]) -> (Option<IgnoredState>, Option<String>) {
    match rest {
        [b"nogit", ..] | [] => (None, None),
        [b"giterr", message, ..] => (None, Some(git_error_text(message))),
        [b"git", prefix, middle @ .., b"rc", status] => {
            let status = std::str::from_utf8(status)
                .ok()
                .and_then(|status| status.parse::<i32>().ok());
            // check-ignore exits 1 when nothing matched, which is not a failure.
            if !matches!(status, Some(0 | 1)) {
                return (
                    None,
                    Some(match status {
                        Some(status) => format!("git check-ignore exited with status {status}"),
                        None => "git check-ignore did not finish".to_owned(),
                    }),
                );
            }
            let Some(prefix) = text(prefix) else {
                return (None, None);
            };
            let paths = middle
                .iter()
                .filter_map(|path| text(path))
                .collect::<HashSet<_>>();
            (Some(IgnoredState { prefix, paths }), None)
        }
        _ => (None, Some("git output was not understood".to_owned())),
    }
}

/// `<mode> <object> <stage>\t<path>`; gitlinks are submodules, which the index excludes.
fn staged_file_path(record: &str) -> Option<&str> {
    let (meta, path) = record.split_once('\t')?;
    (meta.split(' ').next() != Some("160000")).then_some(path)
}

pub(crate) fn parse_index(output: &[u8], max_entries: usize) -> Result<ParsedIndex, ParseFailure> {
    let all = tokens(after_marker(output)?);
    let (canonical, _home, mut iter) = directory_prefix(&all)?;
    let mut git_root = None;
    let mut git_error = None;
    match iter.next().copied() {
        Some(b"nogit") => {}
        Some(b"giterr") => {
            git_error = Some(git_error_text(iter.next().copied().unwrap_or_default()));
        }
        Some(b"git") => {
            git_root = Some(
                iter.next()
                    .and_then(|token| text(token))
                    .ok_or(ParseFailure::Malformed("git top level is not UTF-8"))?,
            );
        }
        _ => return Err(ParseFailure::Malformed("missing git record")),
    }

    let mut cached: Vec<&[u8]> = Vec::new();
    let mut others: Vec<&[u8]> = Vec::new();
    let mut found: Vec<&[u8]> = Vec::new();
    let mut section: Option<&[u8]> = None;
    let mut ended = false;
    let mut cut = false;
    for token in iter {
        match *token {
            b"/trunc" => cut = true,
            b"/cached" | b"/others" | b"/find" => section = Some(token),
            b"/end" => {
                ended = true;
                break;
            }
            // The script pads each stream with a NUL so a record cut by the
            // byte cap cannot swallow the next marker.
            [] => {}
            _ => match section {
                Some(b"/cached") => cached.push(token),
                Some(b"/others") => others.push(token),
                Some(b"/find") => found.push(token),
                _ => return Err(ParseFailure::Malformed("path before a section marker")),
            },
        }
    }
    let mut truncated = !ended || cut;
    // A stream that reached the script's byte cap was cut mid-record.
    for stream in [&mut cached, &mut others, &mut found] {
        let bytes: usize = stream.iter().map(|record| record.len() + 1).sum();
        if bytes + 1 >= super::scripts::INDEX_STREAM_BYTE_CAP {
            stream.pop();
            truncated = true;
        }
    }

    let mut files: Vec<String> = if git_root.is_some() {
        cached
            .into_iter()
            .filter_map(text)
            .filter_map(|record| staged_file_path(&record).map(str::to_owned))
            // Nested repositories are reported as `dir/`, never their files.
            .chain(
                others
                    .into_iter()
                    .filter_map(text)
                    .filter(|path| !path.ends_with('/')),
            )
            .collect()
    } else {
        found
            .into_iter()
            .filter_map(text)
            .map(|path| path.strip_prefix("./").map(str::to_owned).unwrap_or(path))
            .collect()
    };
    files.sort_unstable();
    // Unmerged paths appear once per conflict stage.
    files.dedup();
    if files.len() > max_entries {
        files.truncate(max_entries);
        truncated = true;
    }
    Ok(ParsedIndex {
        canonical,
        git_root,
        entries: files,
        truncated,
        git_error,
    })
}

pub(crate) fn parse_read(output: &[u8]) -> Result<ParsedRead, ParseFailure> {
    let output = after_marker(output)?;
    let first_end = output
        .iter()
        .position(|byte| *byte == 0)
        .ok_or(ParseFailure::Malformed("empty script output"))?;
    let first = &output[..first_end];
    if let Some(status) = status_of(first) {
        return Err(ParseFailure::Script(status));
    }
    let after = &output[first_end + 1..];
    let revision_end = after
        .iter()
        .position(|byte| *byte == 0)
        .ok_or(ParseFailure::Malformed("missing revision"))?;
    let revision = text(&after[..revision_end]).ok_or(ParseFailure::Malformed("bad revision"))?;
    match first {
        b"unchanged" => Ok(ParsedRead::Unchanged { revision }),
        b"content" => Ok(ParsedRead::Content {
            revision,
            bytes: after[revision_end + 1..].to_vec(),
        }),
        _ => Err(ParseFailure::Malformed("unknown read record")),
    }
}
