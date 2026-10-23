//! Turns what the user typed in the download dialog into a folder path (design
//! §4.5). Pure: the home folder and the path syntax are arguments, so every
//! rule runs on any host.

use std::path::{Path, PathBuf};

/// How this client writes absolute paths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PathSyntax {
    Posix,
    Windows,
}

impl PathSyntax {
    pub(crate) fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Posix
        }
    }
}

/// The folder `input` names. Why no `~user`: resolving another account's home
/// needs the password database, and a typo would save into the wrong place.
pub(crate) fn resolve(input: &str, home: &Path, syntax: PathSyntax) -> Result<PathBuf, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("enter a folder".to_owned());
    }
    if input.chars().any(char::is_control) {
        return Err("the folder contains control characters".to_owned());
    }
    if let Some(rest) = home_relative(input, syntax) {
        return Ok(join_under_home(home, rest, syntax));
    }
    if is_absolute(input, syntax) {
        return Ok(PathBuf::from(input));
    }
    Err("enter an absolute folder".to_owned())
}

/// What follows `~`, when `input` starts at the home folder.
fn home_relative(input: &str, syntax: PathSyntax) -> Option<&str> {
    let rest = input.strip_prefix('~')?;
    match rest.chars().next() {
        None => Some(""),
        Some('/') => Some(&rest[1..]),
        Some('\\') if syntax == PathSyntax::Windows => Some(&rest[1..]),
        Some(_) => None,
    }
}

fn join_under_home(home: &Path, rest: &str, syntax: PathSyntax) -> PathBuf {
    let separators: &[char] = match syntax {
        PathSyntax::Posix => &['/'],
        PathSyntax::Windows => &['/', '\\'],
    };
    let parts = rest
        .split(separators)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    match syntax {
        PathSyntax::Posix => parts
            .iter()
            .fold(home.to_path_buf(), |dir, part| dir.join(part)),
        // Why build a string: `Path::join` on a Unix host would put `/` into a
        // `C:\` path, and the syntax is a choice, not the host's.
        PathSyntax::Windows => {
            let mut text = home
                .to_string_lossy()
                .trim_end_matches(['/', '\\'])
                .to_owned();
            for part in parts {
                text.push('\\');
                text.push_str(part);
            }
            PathBuf::from(text)
        }
    }
}

fn is_absolute(input: &str, syntax: PathSyntax) -> bool {
    match syntax {
        PathSyntax::Posix => input.starts_with('/'),
        PathSyntax::Windows => {
            let bytes = input.as_bytes();
            let drive = bytes.len() >= 3
                && bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && matches!(bytes[2], b'\\' | b'/');
            // Why more than `\\`: `\\server` alone names no share.
            let share = input.starts_with("\\\\") && input[2..].contains('\\');
            drive || share
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const POSIX_HOME: &str = "/home/me";
    const WINDOWS_HOME: &str = r"C:\Users\me";

    fn posix(input: &str) -> Result<PathBuf, String> {
        resolve(input, Path::new(POSIX_HOME), PathSyntax::Posix)
    }

    fn windows(input: &str) -> Result<PathBuf, String> {
        resolve(input, Path::new(WINDOWS_HOME), PathSyntax::Windows)
    }

    #[test]
    fn a_tilde_names_the_home_folder() {
        assert_eq!(posix("~"), Ok(PathBuf::from("/home/me")));
        assert_eq!(posix("~/"), Ok(PathBuf::from("/home/me")));
    }

    #[test]
    fn a_tilde_path_is_joined_under_home() {
        assert_eq!(
            posix("~/Downloads"),
            Ok(PathBuf::from("/home/me/Downloads"))
        );
        assert_eq!(posix("~//a///b/"), Ok(PathBuf::from("/home/me/a/b")));
    }

    #[test]
    fn a_tilde_with_a_user_name_is_not_expanded() {
        assert_eq!(posix("~root/x"), Err("enter an absolute folder".to_owned()));
        assert_eq!(posix("~x"), Err("enter an absolute folder".to_owned()));
    }

    #[test]
    fn a_backslash_after_the_tilde_only_means_a_separator_in_windows_syntax() {
        assert_eq!(windows(r"~\x"), Ok(PathBuf::from(r"C:\Users\me\x")));
        assert_eq!(windows("~/x"), Ok(PathBuf::from(r"C:\Users\me\x")));
        assert_eq!(windows("~"), Ok(PathBuf::from(r"C:\Users\me")));
        assert!(posix(r"~\x").is_err());
    }

    #[test]
    fn surrounding_whitespace_is_dropped() {
        assert_eq!(posix("  /data/dl \n"), Ok(PathBuf::from("/data/dl")));
        assert_eq!(posix("  ~/dl  "), Ok(PathBuf::from("/home/me/dl")));
    }

    #[test]
    fn an_empty_input_asks_for_a_folder() {
        assert_eq!(posix(""), Err("enter a folder".to_owned()));
        assert_eq!(posix("   "), Err("enter a folder".to_owned()));
    }

    #[test]
    fn a_relative_folder_is_refused() {
        for input in ["Downloads", "./x", "../x", "a/b"] {
            assert_eq!(
                posix(input),
                Err("enter an absolute folder".to_owned()),
                "{input}"
            );
        }
    }

    #[test]
    fn control_characters_are_refused() {
        assert!(posix("/tmp/a\u{1b}b").is_err());
        assert!(posix("/tmp/a\0b").is_err());
    }

    #[test]
    fn windows_syntax_accepts_drive_and_share_paths_and_nothing_else() {
        assert_eq!(windows(r"D:\dl"), Ok(PathBuf::from(r"D:\dl")));
        assert_eq!(windows("d:/dl"), Ok(PathBuf::from("d:/dl")));
        assert_eq!(
            windows(r"\\server\share\dl"),
            Ok(PathBuf::from(r"\\server\share\dl"))
        );
        for input in [r"\\server", r"\dl", "C:", "C:dl", "/dl", "dl"] {
            assert_eq!(
                windows(input),
                Err("enter an absolute folder".to_owned()),
                "{input}"
            );
        }
    }

    #[test]
    fn posix_syntax_refuses_a_drive_path() {
        assert!(posix(r"C:\dl").is_err());
    }
}
