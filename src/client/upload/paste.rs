//! The text pasted into a terminal pane after an upload (spec Q2, Q20).

/// Published absolute paths as one shell-ready line: plain paths unchanged,
/// anything else single-quoted, separated by spaces.
pub(crate) fn pasted_paths(paths: &[String], trailing_space: bool) -> String {
    let mut text = paths
        .iter()
        .map(|path| quote(path))
        .collect::<Vec<_>>()
        .join(" ");
    if trailing_space {
        text.push(' ');
    }
    text
}

fn quote(path: &str) -> String {
    let plain = !path.is_empty()
        && path.chars().all(|ch| {
            ch.is_ascii_alphanumeric()
                || matches!(
                    ch,
                    '_' | '.' | '/' | '+' | ':' | '@' | '%' | '=' | ',' | '-'
                )
        });
    if plain {
        return path.to_owned();
    }
    format!("'{}'", path.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paste(paths: &[&str]) -> String {
        let paths: Vec<String> = paths.iter().map(|path| (*path).to_owned()).collect();
        pasted_paths(&paths, false)
    }

    #[test]
    fn plain_paths_are_pasted_unquoted_and_joined_by_spaces() {
        assert_eq!(
            paste(&["/home/u/a.txt", "/srv/x-1,2=3@h:4%5+6"]),
            "/home/u/a.txt /srv/x-1,2=3@h:4%5+6"
        );
    }

    #[test]
    fn paths_with_shell_characters_are_single_quoted() {
        assert_eq!(paste(&["/tmp/a b"]), "'/tmp/a b'");
        assert_eq!(paste(&["/tmp/it's"]), r"'/tmp/it'\''s'");
        assert_eq!(paste(&["/tmp/$HOME"]), "'/tmp/$HOME'");
        assert_eq!(paste(&["/tmp/日本語.txt"]), "'/tmp/日本語.txt'");
    }

    #[test]
    fn a_trailing_space_from_the_drop_is_kept() {
        assert_eq!(pasted_paths(&["/a".to_owned()], true), "/a ");
    }
}
