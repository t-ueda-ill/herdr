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
    let mut sections = 0usize;
    let mut clean_sections = 0usize;
    while let Some(token) = iter.next() {
        match *token {
            b"/trunc" => cut = true,
            b"/cached" | b"/others" | b"/find" => {
                section = Some(token);
                sections += 1;
            }
            // Zero only: a producer killed by `timeout` or failing on a
            // permission error left a partial stream.
            b"/status" => {
                if iter.next().copied() == Some(b"0".as_slice()) {
                    clean_sections += 1;
                }
            }
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
    let mut truncated = !ended || cut || clean_sections < sections;
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

/// What `stat_script` found; every field the environment could not give is `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedStat {
    pub(crate) kind: DirectoryEntryKind,
    pub(crate) mode: Option<u32>,
    pub(crate) uid: Option<String>,
    pub(crate) size: Option<u64>,
    pub(crate) mtime_seconds: Option<u64>,
    pub(crate) link_target: Option<String>,
    pub(crate) children: Option<u64>,
    pub(crate) children_limited: bool,
}

pub(crate) fn parse_stat(output: &[u8]) -> Result<ParsedStat, ParseFailure> {
    let all = tokens(after_marker(output)?);
    let mut iter = all.iter().copied();
    let first = iter
        .next()
        .ok_or(ParseFailure::Malformed("empty script output"))?;
    if let Some(status) = status_of(first) {
        return Err(ParseFailure::Script(status));
    }
    if first != b"ok" {
        return Err(ParseFailure::Malformed("missing status record"));
    }
    let mut next = || {
        iter.next()
            .ok_or(ParseFailure::Malformed("short stat record"))
    };
    let kind = match next()? {
        b"l" => DirectoryEntryKind::Symlink,
        b"d" => DirectoryEntryKind::Dir,
        b"f" => DirectoryEntryKind::File,
        _ => DirectoryEntryKind::Other,
    };
    let line = String::from_utf8_lossy(next()?).into_owned();
    let number = |token: &[u8]| std::str::from_utf8(token).ok()?.parse::<u64>().ok();
    let mtime_seconds = number(next()?);
    let link_target = Some(next()?)
        .filter(|token| !token.is_empty())
        .and_then(text);
    let children = number(next()?);
    let children_limited = next()? == b"1";
    // `mode links uid gid size …`; a device shows `major, minor` for size.
    let mut fields = line.split_whitespace();
    let mode = fields.next().and_then(mode_bits);
    let uid = fields
        .nth(1)
        .filter(|uid| uid.bytes().all(|byte| byte.is_ascii_digit()))
        .map(str::to_owned);
    let size = (kind == DirectoryEntryKind::File)
        .then(|| fields.nth(1).and_then(|size| size.parse().ok()))
        .flatten();
    Ok(ParsedStat {
        kind,
        mode,
        uid,
        size,
        mtime_seconds,
        link_target,
        children,
        children_limited,
    })
}

/// `-rwsr-x--T` to `0o4750`. macOS appends `@`/`+` and GNU appends `.`/`+`
/// after the ten mode characters, so only those are read.
fn mode_bits(field: &str) -> Option<u32> {
    let chars: Vec<char> = field.chars().take(10).collect();
    if chars.len() != 10 {
        return None;
    }
    let mut mode = 0;
    for (index, ch) in chars[1..].iter().enumerate() {
        let bit = 1 << (8 - index);
        let execute = index % 3 == 2;
        match (ch, execute) {
            ('-', _) => {}
            ('r', false) if index % 3 == 0 => mode |= bit,
            ('w', false) if index % 3 == 1 => mode |= bit,
            ('x', true) => mode |= bit,
            ('s', true) | ('t', true) => mode |= bit | special_bit(index),
            ('S', true) | ('T', true) => mode |= special_bit(index),
            _ => return None,
        }
    }
    Some(mode)
}

/// setuid, setgid or sticky, by which execute column carries it.
fn special_bit(index: usize) -> u32 {
    match index {
        2 => 0o4000,
        5 => 0o2000,
        _ => 0o1000,
    }
}

#[cfg(test)]
mod stat_tests {
    use super::*;

    fn record(line: &str, kind: &str) -> Vec<u8> {
        format!("HERDR-OFFHOST-1\0ok\0{kind}\0{line}\0\0\0\0\0").into_bytes()
    }

    #[test]
    fn mode_columns_carry_setuid_setgid_and_sticky_and_ignore_platform_suffixes() {
        for (line, mode) in [
            ("-rw-r--r--  1 1000 1000 5 Oct 27 14:21 /a", 0o644),
            ("-rwsr-x--T@ 1 1000 1000 5 Oct 27 14:21 /a", 0o5750),
            ("drwxrwsrwt+ 2 0 0 4096 Oct 27 14:21 /a", 0o3777),
            ("-rwSr-Sr-x. 1 0 0 5 Oct 27 14:21 /a", 0o6645),
        ] {
            let parsed = parse_stat(&record(line, "f")).unwrap();
            assert_eq!(parsed.mode, Some(mode), "{line}");
            assert_eq!(
                parsed.uid.as_deref(),
                Some(line.split_whitespace().nth(2).unwrap())
            );
        }
    }

    #[test]
    fn a_device_has_no_size_and_unreadable_ls_output_leaves_fields_empty() {
        let device =
            parse_stat(&record("crw-rw-rw- 1 0 0 1, 3 Oct 27 14:21 /dev/null", "o")).unwrap();
        let garbled = parse_stat(&record("ls: cannot access", "f")).unwrap();

        assert_eq!(device.size, None);
        assert_eq!(device.mode, Some(0o666));
        assert_eq!(garbled.mode, None);
        assert_eq!(garbled.uid, None);
        assert_eq!(garbled.size, None);
    }
}
