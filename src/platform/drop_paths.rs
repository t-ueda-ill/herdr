//! Pure parts of reading a path a terminal pasted for a file drop. The
//! OS-specific entry points (`dropped_path`, `drop_path_syntax`) live in each
//! platform file and call into these, so every rule is testable on any host.

#[cfg(any(test, unix))]
use std::path::PathBuf;

/// How a client splits pasted drop text into path tokens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DropPathSyntax {
    /// Shell-like quoting: `'…'`, `"…"`, and backslash escapes.
    Posix,
    /// Only `"…"` quotes; backslashes are path separators.
    Windows,
    /// WSL: a line that starts with a Windows path uses Windows syntax,
    /// every other line Posix. Why not always Posix: Windows Terminal sends
    /// `C:\Users\a\b.txt` to WSL tabs too, and Posix unescaping would turn it
    /// into `C:Usersab.txt`.
    // Why allow: only Linux (WSL) picks it, but the splitter handles it on
    // every host so its tests run everywhere.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    WslAuto,
}

/// Where WSL mounts Windows drives when `/etc/wsl.conf` does not say.
#[cfg(target_os = "linux")]
pub(crate) const DEFAULT_WSL_AUTOMOUNT_ROOT: &str = "/mnt/";

/// `[automount] root` from `/etc/wsl.conf`, normalized to end with `/`.
#[cfg(any(test, target_os = "linux"))]
pub(crate) fn parse_wsl_automount_root(conf: &str) -> Option<String> {
    let mut in_automount = false;
    for line in conf.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.starts_with(';') || line.is_empty() {
            continue;
        }
        if let Some(section) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            in_automount = section.trim().eq_ignore_ascii_case("automount");
            continue;
        }
        if !in_automount {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if !key.trim().eq_ignore_ascii_case("root") {
            continue;
        }
        let value = value.trim().trim_matches(|c| c == '"' || c == '\'');
        if !value.starts_with('/') {
            return None;
        }
        let mut root = value.to_owned();
        if !root.ends_with('/') {
            root.push('/');
        }
        return Some(root);
    }
    None
}

/// The drive letter and the rest of `X:\rest` or `X:/rest`.
fn windows_drive_path(token: &str) -> Option<(char, &str)> {
    let mut chars = token.chars();
    let drive = chars.next().filter(char::is_ascii_alphabetic)?;
    if chars.next() != Some(':') {
        return None;
    }
    let rest = &token[2..];
    rest.starts_with(['\\', '/']).then_some((drive, rest))
}

/// Whether a line of drop text starts with a Windows path (possibly quoted).
pub(crate) fn starts_with_windows_path(line: &str) -> bool {
    let line = line.trim_start();
    let line = line.strip_prefix('"').unwrap_or(line);
    windows_drive_path(line).is_some() || line.starts_with("\\\\")
}

/// Maps a WSL drop token onto this distro's filesystem.
///
/// Why not `wslpath`: the client loop must not spawn processes, and a pure
/// mapping keeps tests deterministic.
#[cfg(any(test, target_os = "linux"))]
pub(crate) fn translate_wsl_drop_path(
    token: &str,
    automount_root: &str,
    distro: Option<&str>,
) -> Option<PathBuf> {
    if token.starts_with('/') {
        return Some(PathBuf::from(token));
    }
    if let Some((drive, rest)) = windows_drive_path(token) {
        let mut path = automount_root.to_owned();
        if !path.ends_with('/') {
            path.push('/');
        }
        path.push(drive.to_ascii_lowercase());
        let rest = rest.replace('\\', "/");
        path.push_str(&rest);
        return Some(PathBuf::from(path));
    }
    let unc = token.strip_prefix("\\\\")?;
    let (host, rest) = unc.split_once('\\')?;
    if !host.eq_ignore_ascii_case("wsl$") && !host.eq_ignore_ascii_case("wsl.localhost") {
        return None;
    }
    let (name, inner) = rest.split_once('\\').unwrap_or((rest, ""));
    if !distro.is_some_and(|distro| distro.eq_ignore_ascii_case(name)) {
        return None;
    }
    Some(PathBuf::from(format!("/{}", inner.replace('\\', "/"))))
}

/// A POSIX drop token is usable only as an absolute path.
#[cfg(any(test, unix))]
pub(crate) fn posix_drop_path(token: &str) -> Option<PathBuf> {
    token.starts_with('/').then(|| PathBuf::from(token))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_drive_paths_map_under_the_automount_root() {
        for (token, expected) in [
            ("C:\\Users\\a b\\c.txt", "/mnt/c/Users/a b/c.txt"),
            ("c:/x/y", "/mnt/c/x/y"),
            ("D:\\", "/mnt/d/"),
        ] {
            assert_eq!(
                translate_wsl_drop_path(token, "/mnt/", Some("Ubuntu")),
                Some(PathBuf::from(expected)),
                "{token}"
            );
        }
        assert_eq!(
            translate_wsl_drop_path("C:\\x", "/win", None),
            Some(PathBuf::from("/win/c/x"))
        );
    }

    #[test]
    fn wsl_share_paths_map_only_for_this_distro() {
        assert_eq!(
            translate_wsl_drop_path("\\\\wsl$\\Ubuntu\\home\\u\\f", "/mnt/", Some("Ubuntu")),
            Some(PathBuf::from("/home/u/f"))
        );
        assert_eq!(
            translate_wsl_drop_path("\\\\wsl.localhost\\ubuntu\\etc", "/mnt/", Some("Ubuntu")),
            Some(PathBuf::from("/etc"))
        );
        assert_eq!(
            translate_wsl_drop_path("\\\\wsl$\\Debian\\etc", "/mnt/", Some("Ubuntu")),
            None
        );
        assert_eq!(
            translate_wsl_drop_path("\\\\server\\share\\x", "/mnt/", Some("Ubuntu")),
            None
        );
    }

    #[test]
    fn posix_absolute_paths_pass_and_relative_ones_do_not() {
        assert_eq!(
            translate_wsl_drop_path("/home/u/a b", "/mnt/", None),
            Some(PathBuf::from("/home/u/a b"))
        );
        assert_eq!(translate_wsl_drop_path("notes.txt", "/mnt/", None), None);
        assert_eq!(translate_wsl_drop_path("C:relative", "/mnt/", None), None);
        assert_eq!(posix_drop_path("relative"), None);
    }

    #[test]
    fn the_automount_root_is_read_from_its_section() {
        let conf = "[boot]\nroot = /nope\n[automount]\nenabled = true\nroot = /win\n";
        assert_eq!(parse_wsl_automount_root(conf).as_deref(), Some("/win/"));
        assert_eq!(parse_wsl_automount_root("[automount]\n# root=/x\n"), None);
        assert_eq!(
            parse_wsl_automount_root("[Automount]\nroot = \"/m/\"\n").as_deref(),
            Some("/m/")
        );
    }

    #[test]
    fn lines_starting_with_windows_paths_are_recognized() {
        assert!(starts_with_windows_path("C:\\a b\\c.txt"));
        assert!(starts_with_windows_path("\"C:\\x y\""));
        assert!(starts_with_windows_path("\\\\wsl$\\Ubuntu\\x"));
        assert!(!starts_with_windows_path("/home/u/a\\ b"));
        assert!(!starts_with_windows_path("'C:\\x'"));
    }
}
