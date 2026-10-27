//! The lines of the details band: which template applies to the selection and
//! the value of each token. Pure, so render only draws what this returns.
use super::model::Investigation;
use crate::api::schema::{DirectoryEntry, DirectoryEntryKind, FileMatch, FileStat};
use crate::config::{DetailsTemplates, DetailsToken};

/// What the band describes, with whatever the listing and `files.stat` know.
struct Subject<'a> {
    path: &'a str,
    entry: Option<&'a DirectoryEntry>,
    stat: Option<&'a FileStat>,
    /// Entries of a loaded directory listing, when `path` is one.
    listed_children: Option<u64>,
}

/// `rows` is the most the band can show; a search hit narrows its context
/// until the hit line fits.
pub(super) fn details_lines(
    tab: &Investigation,
    templates: &DetailsTemplates,
    rows: usize,
) -> Vec<String> {
    if !tab.selected_files.is_empty() {
        return templates
            .multi
            .render(|token| clean(&multi_value(tab, token)));
    }
    if tab.content_search {
        let Some(found) = tab.matches.get(tab.selected_match) else {
            return Vec::new();
        };
        let subject = subject(tab, &found.path);
        let mut lines = Vec::new();
        for radius in (0..=CONTEXT_RADIUS).rev() {
            lines = templates.match_template.render(|token| {
                match_value(found, token, radius).unwrap_or_else(|| clean(&value(&subject, token)))
            });
            if lines.len() <= rows {
                break;
            }
        }
        return lines;
    }
    let Some(path) = tab.details_subject() else {
        return Vec::new();
    };
    let subject = subject(tab, &path);
    let is_dir = tab
        .browser
        .selected_entry()
        .filter(|entry| entry.path == path)
        .is_none_or(|entry| entry.is_dir);
    let template = if is_dir {
        &templates.directory
    } else {
        &templates.file
    };
    template.render(|token| clean(&value(&subject, token)))
}

/// Lines the server sends on each side of a hit.
const CONTEXT_RADIUS: usize = 2;

fn subject<'a>(tab: &'a Investigation, path: &'a str) -> Subject<'a> {
    let stat = tab
        .stat
        .stat
        .as_ref()
        .filter(|stat| tab.stat.subject.as_deref() == Some(path) && stat.path == path);
    Subject {
        path,
        entry: tab.browser.entry_for_path(path),
        stat,
        listed_children: tab.browser.listed_entry_count(path),
    }
}

fn value(subject: &Subject<'_>, token: DetailsToken) -> String {
    let stat = subject.stat;
    let entry = subject.entry;
    let size = stat
        .and_then(|stat| stat.size)
        .or_else(|| entry.and_then(|entry| entry.size));
    let mtime = stat
        .and_then(|stat| stat.mtime)
        .or_else(|| entry.and_then(|entry| entry.mtime));
    let kind = stat
        .and_then(|stat| stat.kind)
        .or_else(|| entry.map(|entry| entry.kind));
    match token {
        DetailsToken::Name => name(subject.path).to_owned(),
        DetailsToken::Path => subject.path.to_owned(),
        DetailsToken::Kind => kind.map(kind_name).unwrap_or_default().to_owned(),
        DetailsToken::Size => size.map(human_size).unwrap_or_default(),
        DetailsToken::SizeBytes => size.map(|size| size.to_string()).unwrap_or_default(),
        DetailsToken::Mtime => mtime.and_then(local_minutes).unwrap_or_default(),
        DetailsToken::MtimeIso => mtime.and_then(utc_rfc3339).unwrap_or_default(),
        DetailsToken::Hidden => flag(name(subject.path).starts_with('.'), "hidden"),
        DetailsToken::Ignored => flag(entry.and_then(|e| e.ignored) == Some(true), "ignored"),
        DetailsToken::LinkTarget => stat
            .and_then(|stat| stat.link_target.clone())
            .unwrap_or_default(),
        DetailsToken::Mode => stat
            .and_then(|stat| stat.mode)
            .map(mode_string)
            .unwrap_or_default(),
        DetailsToken::Owner => stat.and_then(|stat| stat.owner.clone()).unwrap_or_default(),
        DetailsToken::Children => match stat.and_then(|stat| stat.children) {
            Some(count) => items(count, stat.is_some_and(|stat| stat.children_limited)),
            None => subject
                .listed_children
                .map(|count| items(count, false))
                .unwrap_or_default(),
        },
        DetailsToken::Count
        | DetailsToken::TotalSize
        | DetailsToken::Line
        | DetailsToken::Context => String::new(),
    }
}

fn match_value(found: &FileMatch, token: DetailsToken, radius: usize) -> Option<String> {
    Some(match token {
        DetailsToken::Line => found.line.to_string(),
        DetailsToken::Context => context(found, radius),
        _ => return None,
    })
}

fn multi_value(tab: &Investigation, token: DetailsToken) -> String {
    match token {
        DetailsToken::Count => tab.selected_files.len().to_string(),
        DetailsToken::TotalSize => human_size(
            tab.selected_files
                .iter()
                .filter_map(|path| tab.browser.entry_for_path(path))
                .filter(|entry| entry.kind == DirectoryEntryKind::File)
                .filter_map(|entry| entry.size)
                .sum(),
        ),
        _ => String::new(),
    }
}

/// `grep -C` style: `12:hit` for the matched line, `11-text` around it,
/// at most `radius` lines on each side.
fn context(found: &FileMatch, radius: usize) -> String {
    let Some(context) = &found.context else {
        return format!("{}:{}", found.line, clean(&found.text));
    };
    let before = &context.before[context.before.len().saturating_sub(radius)..];
    let first = found.line.saturating_sub(before.len());
    let before = before
        .iter()
        .enumerate()
        .map(|(offset, text)| format!("{}-{}", first + offset, clean(text)));
    let after = context
        .after
        .iter()
        .take(radius)
        .enumerate()
        .map(|(offset, text)| format!("{}-{}", found.line + 1 + offset, clean(text)));
    before
        .chain(std::iter::once(format!(
            "{}:{}",
            found.line,
            clean(&found.text)
        )))
        .chain(after)
        .collect::<Vec<_>>()
        .join("\n")
}

/// File contents and names may hold control characters that would move the
/// cursor or break the band into extra lines.
fn clean(text: &str) -> String {
    text.chars()
        .map(|ch| if ch == '\t' { ' ' } else { ch })
        .filter(|ch| !ch.is_control())
        .collect()
}

fn name(path: &str) -> &str {
    path.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
}

fn flag(set: bool, word: &str) -> String {
    if set {
        word.to_owned()
    } else {
        String::new()
    }
}

fn kind_name(kind: DirectoryEntryKind) -> &'static str {
    match kind {
        DirectoryEntryKind::File => "file",
        DirectoryEntryKind::Dir => "directory",
        DirectoryEntryKind::Symlink => "symlink",
        DirectoryEntryKind::Other => "other",
    }
}

fn items(count: u64, limited: bool) -> String {
    let more = if limited { "+" } else { "" };
    if count == 1 && !limited {
        "1 item".to_owned()
    } else {
        format!("{count}{more} items")
    }
}

/// Binary units with one decimal: `512 B`, `12.3 KiB`.
pub(super) fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["KiB", "MiB", "GiB", "TiB", "PiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// `rwsr-x--T` from permission bits with setuid, setgid and sticky.
pub(super) fn mode_string(mode: u32) -> String {
    let mut text = String::with_capacity(9);
    for (shift, special, set_char) in [(6, 0o4000, 's'), (3, 0o2000, 's'), (0, 0o1000, 't')] {
        let bits = (mode >> shift) & 0o7;
        text.push(if bits & 0o4 != 0 { 'r' } else { '-' });
        text.push(if bits & 0o2 != 0 { 'w' } else { '-' });
        let execute = bits & 0o1 != 0;
        text.push(match (mode & special != 0, execute) {
            (true, true) => set_char,
            (true, false) => set_char.to_ascii_uppercase(),
            (false, true) => 'x',
            (false, false) => '-',
        });
    }
    text
}

fn local_minutes(millis: u64) -> Option<String> {
    let seconds = i64::try_from(millis / 1000).ok()?;
    let local = crate::platform::local_datetime_at(seconds)?;
    Some(format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        local.year(),
        u8::from(local.month()),
        local.day(),
        local.hour(),
        local.minute()
    ))
}

fn utc_rfc3339(millis: u64) -> Option<String> {
    let seconds = i64::try_from(millis / 1000).ok()?;
    let time = time::OffsetDateTime::from_unix_timestamp(seconds).ok()?;
    time.format(&time::format_description::well_known::Rfc3339)
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::FileMatchContext;

    #[test]
    fn sizes_use_binary_units_with_one_decimal() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1536), "1.5 KiB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MiB");
    }

    #[test]
    fn modes_show_setuid_setgid_and_sticky_in_the_execute_columns() {
        assert_eq!(mode_string(0o755), "rwxr-xr-x");
        assert_eq!(mode_string(0o4750), "rwsr-x---");
        assert_eq!(mode_string(0o2640), "rw-r-S---");
        assert_eq!(mode_string(0o1777), "rwxrwxrwt");
        assert_eq!(mode_string(0o1776), "rwxrwxrwT");
    }

    #[test]
    fn context_reads_like_grep_and_strips_control_characters() {
        let found = FileMatch {
            path: "/a".into(),
            revision: String::new(),
            start: 0,
            end: 1,
            line: 3,
            text: "hit\tline".into(),
            matched: "hit".into(),
            replacement: None,
            context: Some(FileMatchContext {
                before: vec!["one".into(), "two\u{1b}[2J".into()],
                after: vec!["four".into()],
            }),
        };

        assert_eq!(context(&found, 2), "1-one\n2-two[2J\n3:hit line\n4-four");
        assert_eq!(context(&found, 1), "2-two[2J\n3:hit line\n4-four");
        assert_eq!(
            context(
                &FileMatch {
                    context: None,
                    ..found
                },
                2
            ),
            "3:hit line"
        );
    }

    #[test]
    fn children_say_items_and_mark_a_count_that_stopped_at_the_limit() {
        assert_eq!(items(1, false), "1 item");
        assert_eq!(items(3, false), "3 items");
        assert_eq!(items(10_000, true), "10000+ items");
    }

    #[test]
    fn the_root_has_an_empty_name_and_others_their_last_component() {
        assert_eq!(name("/"), "");
        assert_eq!(name("/repo/src/"), "src");
        assert_eq!(name("C:\\work\\a.txt"), "a.txt");
    }

    #[test]
    fn utc_times_are_rfc3339() {
        assert_eq!(
            utc_rfc3339(1_700_000_000_123).as_deref(),
            Some("2023-11-14T22:13:20Z")
        );
    }
}
