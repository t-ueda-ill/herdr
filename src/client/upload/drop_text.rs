//! Deciding, from text alone, whether a paste looks like a file drop (spec Q7,
//! Q8, Q17). Nothing here touches the filesystem: existence is checked later,
//! off the client loop, only for text that already parsed as absolute paths.

use std::path::PathBuf;

use crate::platform::DropPathSyntax;

/// Begin accepts at most this many top-level names.
pub(crate) const MAX_DROP_TOKENS: usize = crate::upload::path::MAX_NAMES;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DropTokens {
    pub(crate) tokens: Vec<String>,
    /// The text ended with a space, which terminals add after a dropped path;
    /// the pasted remote paths keep it so the prompt looks the same.
    pub(crate) trailing_space: bool,
}

/// Splits drop text into path tokens, or `None` when it cannot be one.
pub(crate) fn split_drop_text(text: &str, policy: DropPathSyntax) -> Option<DropTokens> {
    let text = text.trim_end_matches(['\r', '\n']);
    if text.trim().is_empty() {
        return None;
    }
    let trailing_space = text.ends_with([' ', '\t']);
    let mut tokens = Vec::new();
    // Why a lone `\r` too: VTE terminals and xterm send pasted newlines as CR.
    for line in text.split(['\r', '\n']) {
        if line.trim().is_empty() {
            continue;
        }
        let syntax = match policy {
            DropPathSyntax::Posix => DropPathSyntax::Posix,
            DropPathSyntax::Windows => DropPathSyntax::Windows,
            DropPathSyntax::WslAuto if crate::platform::starts_with_windows_path(line) => {
                DropPathSyntax::Windows
            }
            DropPathSyntax::WslAuto => DropPathSyntax::Posix,
        };
        let line_tokens = match syntax {
            DropPathSyntax::Windows => split_windows_line(line)?,
            _ => split_posix_line(line)?,
        };
        for token in line_tokens {
            tokens.push(decode_file_uri(token)?);
            if tokens.len() > MAX_DROP_TOKENS {
                return None;
            }
        }
    }
    (!tokens.is_empty()).then_some(DropTokens {
        tokens,
        trailing_space,
    })
}

/// Every token as a local path with a name to upload as, or `None` when any
/// one is not (a relative path, `/`, or a share of another distro).
pub(crate) fn resolve_drop_paths(
    tokens: &[String],
    dropped_path: impl Fn(&str) -> Option<PathBuf>,
) -> Option<Vec<PathBuf>> {
    tokens
        .iter()
        .map(|token| dropped_path(token).filter(|path| path.file_name().is_some()))
        .collect()
}

fn split_posix_line(line: &str) -> Option<Vec<String>> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_token = false;
    let mut chars = line.chars();
    while let Some(ch) = chars.next() {
        match ch {
            ' ' | '\t' => {
                if in_token {
                    tokens.push(std::mem::take(&mut current));
                    in_token = false;
                }
            }
            '\'' => {
                in_token = true;
                loop {
                    match chars.next()? {
                        '\'' => break,
                        other => current.push(other),
                    }
                }
            }
            '"' => {
                in_token = true;
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => match chars.next()? {
                            escaped @ ('"' | '\\' | '$' | '`') => current.push(escaped),
                            other => {
                                current.push('\\');
                                current.push(other);
                            }
                        },
                        other => current.push(other),
                    }
                }
            }
            '\\' => {
                in_token = true;
                current.push(chars.next().unwrap_or('\\'));
            }
            other => {
                in_token = true;
                current.push(other);
            }
        }
    }
    if in_token {
        tokens.push(current);
    }
    Some(tokens)
}

fn split_windows_line(line: &str) -> Option<Vec<String>> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_token = false;
    let mut quoted = false;
    for ch in line.chars() {
        match ch {
            '"' => {
                in_token = true;
                quoted = !quoted;
            }
            ' ' | '\t' if !quoted => {
                if in_token {
                    tokens.push(std::mem::take(&mut current));
                    in_token = false;
                }
            }
            other => {
                in_token = true;
                current.push(other);
            }
        }
    }
    if quoted {
        return None;
    }
    if in_token {
        tokens.push(current);
    }
    Some(tokens)
}

/// `file://` URIs from local hosts become paths; everything else is unchanged.
fn decode_file_uri(token: String) -> Option<String> {
    let Some(rest) = token.strip_prefix("file://") else {
        return Some(token);
    };
    let (host, path) = match rest.find('/') {
        Some(index) => rest.split_at(index),
        None => return None,
    };
    if !host.is_empty() && !host.eq_ignore_ascii_case("localhost") {
        return None;
    }
    let mut bytes = Vec::with_capacity(path.len());
    let raw = path.as_bytes();
    let mut index = 0;
    while index < raw.len() {
        if raw[index] == b'%' {
            let hex = path.get(index + 1..index + 3)?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            bytes.push(raw[index]);
            index += 1;
        }
    }
    let decoded = String::from_utf8(bytes).ok()?;
    // `file:///C:/x` names `C:/x`, not a root-relative `/C:/x`.
    let bytes = decoded.as_bytes();
    if bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b':' {
        return Some(decoded[1..].to_owned());
    }
    Some(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn posix(text: &str) -> Option<Vec<String>> {
        split_drop_text(text, DropPathSyntax::Posix).map(|drop| drop.tokens)
    }

    fn windows(text: &str) -> Option<Vec<String>> {
        split_drop_text(text, DropPathSyntax::Windows).map(|drop| drop.tokens)
    }

    fn wsl(text: &str) -> Option<Vec<String>> {
        split_drop_text(text, DropPathSyntax::WslAuto).map(|drop| drop.tokens)
    }

    #[test]
    fn posix_text_splits_on_whitespace_and_newlines() {
        assert_eq!(
            posix("/a/b.txt /c/d\n/e/f\r\n").unwrap(),
            ["/a/b.txt", "/c/d", "/e/f"]
        );
    }

    #[test]
    fn a_lone_carriage_return_separates_lines_as_terminals_send_pasted_newlines() {
        assert_eq!(posix("/a/x\r/b/y").unwrap(), ["/a/x", "/b/y"]);
        assert_eq!(wsl("/a/x\r/b/y\r").unwrap(), ["/a/x", "/b/y"]);
        assert_eq!(wsl("C:\\x\r/home/u/y").unwrap(), [r"C:\x", "/home/u/y"]);
    }

    #[test]
    fn posix_quotes_and_escapes_are_unwrapped() {
        assert_eq!(
            posix(r#"'/a b/c' "/d \"e\" \$f\x" /g\ h"#).unwrap(),
            ["/a b/c", r#"/d "e" $f\x"#, "/g h"]
        );
        assert_eq!(posix("'/unterminated"), None);
    }

    #[test]
    fn windows_text_keeps_backslashes_and_only_double_quotes_group() {
        assert_eq!(
            windows(r#""C:\x y\z.txt" C:\a\b 'q'"#).unwrap(),
            [r"C:\x y\z.txt", r"C:\a\b", "'q'"]
        );
        assert_eq!(windows(r#""C:\open"#), None);
    }

    #[test]
    fn wsl_lines_pick_windows_syntax_only_when_they_start_with_a_windows_path() {
        assert_eq!(wsl(r"C:\a b\c.txt").unwrap(), [r"C:\a", r"b\c.txt"]);
        assert_eq!(wsl(r#""C:\x y""#).unwrap(), [r"C:\x y"]);
        assert_eq!(wsl(r"\\wsl$\Ubuntu\a").unwrap(), [r"\\wsl$\Ubuntu\a"]);
        assert_eq!(wsl(r"/home/u/a\ b").unwrap(), ["/home/u/a b"]);
        assert_eq!(
            wsl("C:\\x\n/home/u/a\\ b").unwrap(),
            [r"C:\x", "/home/u/a b"]
        );
    }

    #[test]
    fn file_uris_from_this_host_are_decoded() {
        assert_eq!(
            posix("file:///tmp/a%20b file://localhost/c").unwrap(),
            ["/tmp/a b", "/c"]
        );
        assert_eq!(windows("file:///C:/x%20y").unwrap(), ["C:/x y"]);
        assert_eq!(posix("file://other/tmp/a"), None);
        assert_eq!(posix("file:///bad%zz"), None);
    }

    #[test]
    fn the_trailing_space_a_terminal_adds_is_remembered() {
        assert!(
            split_drop_text("/a/b ", DropPathSyntax::Posix)
                .unwrap()
                .trailing_space
        );
        assert!(
            !split_drop_text("/a/b\n", DropPathSyntax::Posix)
                .unwrap()
                .trailing_space
        );
    }

    #[test]
    fn empty_text_and_more_than_the_name_limit_are_not_drops() {
        assert_eq!(posix(" \n"), None);
        let many = vec!["/x"; MAX_DROP_TOKENS + 1].join(" ");
        assert_eq!(posix(&many), None);
        let most = vec!["/x"; MAX_DROP_TOKENS].join(" ");
        assert_eq!(posix(&most).unwrap().len(), MAX_DROP_TOKENS);
    }

    #[test]
    fn one_relative_token_makes_the_whole_paste_plain_text() {
        let tokens = posix("/a/b notes.txt").unwrap();
        let resolve = |token: &str| token.starts_with('/').then(|| PathBuf::from(token));

        assert_eq!(resolve_drop_paths(&tokens, resolve), None);
        assert_eq!(
            resolve_drop_paths(&posix("/a/b /c").unwrap(), resolve),
            Some(vec![PathBuf::from("/a/b"), PathBuf::from("/c")])
        );
        assert_eq!(resolve_drop_paths(&posix("/").unwrap(), resolve), None);
    }
}
