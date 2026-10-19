#[path = "directory_bindings.rs"]
pub(crate) mod bindings;

pub(crate) use bindings::command;

use crate::api::schema::{DirectoryEntry, DirectoryEntryKind, ResponseResult, SuccessResponse};

use super::projection::ParseError;

pub(super) fn run_directory_command(args: &[String]) -> std::io::Result<i32> {
    let (request, json) = match bindings::request(args) {
        Ok(parsed) => parsed,
        Err(ParseError::Usage(message)) => {
            eprintln!("{message}");
            return Ok(2);
        }
        Err(ParseError::Io(error)) => {
            if error.kind() == std::io::ErrorKind::InvalidInput {
                eprintln!("{error}");
                return Ok(2);
            }
            return Err(error);
        }
        Err(ParseError::Clap(error)) => return super::print_clap_error(&error),
    };
    let response = super::send_request(&request)?;
    if json || response.get("error").is_some() {
        return super::print_response(&response);
    }
    let parsed: SuccessResponse =
        serde_json::from_value(response.clone()).map_err(std::io::Error::other)?;
    let (lines, notes) = match parsed.result {
        ResponseResult::DirectoryList {
            entries,
            truncated,
            git_error,
            ..
        } => (
            entries.iter().map(format_entry).collect(),
            notes(truncated, git_error),
        ),
        ResponseResult::DirectoryIndex {
            entries,
            truncated,
            git_error,
            ..
        } => (entries, notes(truncated, git_error)),
        _ => return super::print_response(&response),
    };
    for line in lines {
        println!("{line}");
    }
    for note in notes {
        eprintln!("{note}");
    }
    Ok(0)
}

/// `ls -F` style markers, matching the file viewer rows.
fn format_entry(entry: &DirectoryEntry) -> String {
    let marker = match entry.kind {
        DirectoryEntryKind::Dir => "/",
        DirectoryEntryKind::Symlink => "@",
        DirectoryEntryKind::File | DirectoryEntryKind::Other => "",
    };
    format!("{}{marker}", entry.name)
}

fn notes(truncated: bool, git_error: Option<String>) -> Vec<String> {
    let mut notes = Vec::new();
    if truncated {
        notes.push("(truncated)".to_owned());
    }
    if let Some(error) = git_error {
        notes.push(format!("git: {error}"));
    }
    notes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, kind: DirectoryEntryKind) -> DirectoryEntry {
        DirectoryEntry {
            name: name.into(),
            kind,
            target_kind: None,
            size: None,
            mtime: None,
            hidden: false,
            ignored: None,
        }
    }

    #[test]
    fn directory_rows_mark_directories_and_symlinks() {
        assert_eq!(format_entry(&entry("src", DirectoryEntryKind::Dir)), "src/");
        assert_eq!(
            format_entry(&entry("link", DirectoryEntryKind::Symlink)),
            "link@"
        );
        assert_eq!(
            format_entry(&entry("main.rs", DirectoryEntryKind::File)),
            "main.rs"
        );
    }

    #[test]
    fn directory_notes_report_truncation_and_git_errors() {
        assert!(notes(false, None).is_empty());
        assert_eq!(
            notes(true, Some("dubious ownership".into())),
            ["(truncated)", "git: dubious ownership"]
        );
    }
}
