//! Pure parts of finding where a client saves downloads. The OS-specific
//! entry point (`default_download_dir`) lives in each platform file and calls
//! into these, so every rule is testable on any host.

use std::ffi::OsString;
use std::path::PathBuf;

pub(crate) fn home_dir_from_env(
    is_windows: bool,
    env: impl Fn(&str) -> Option<OsString>,
) -> Result<PathBuf, ()> {
    if !is_windows {
        return env("HOME").map(PathBuf::from).ok_or(());
    }

    if let Some(path) = usable_home_path(env("USERPROFILE")) {
        return Ok(path);
    }
    if let (Some(drive), Some(path)) = (
        usable_home_component(env("HOMEDRIVE")),
        usable_home_component(env("HOMEPATH")),
    ) {
        let path = path.to_string_lossy();
        if !path.starts_with(['\\', '/']) {
            return usable_home_path(env("HOME")).ok_or(());
        }
        let combined = format!("{}{}", drive.to_string_lossy(), path);
        if let Some(path) = usable_home_path(Some(OsString::from(combined))) {
            return Ok(path);
        }
    }

    usable_home_path(env("HOME")).ok_or(())
}

fn usable_home_path(value: Option<OsString>) -> Option<PathBuf> {
    let value = value?;
    if value.is_empty() || value == "~" {
        return None;
    }
    Some(PathBuf::from(value))
}

fn usable_home_component(value: Option<OsString>) -> Option<OsString> {
    let value = value?;
    if value.is_empty() || value == "~" {
        return None;
    }
    Some(value)
}

/// `XDG_DOWNLOAD_DIR` from the contents of `user-dirs.dirs`. The last
/// assignment wins, as when the shell sources the file. `$HOME` is expanded
/// only as a leading `$HOME/`: that is all the file's own tool writes, and
/// running the line through a shell would execute whatever else it holds.
/// Why `None` for the home folder itself: `xdg-user-dirs` writes `"$HOME/"`
/// to say the user has no downloads folder, and saving there would scatter
/// files over the home folder.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn parse_user_dirs(contents: &str, home: &std::path::Path) -> Option<PathBuf> {
    let mut found = None;
    for line in contents.lines() {
        let line = line.trim();
        let Some(value) = line.strip_prefix("XDG_DOWNLOAD_DIR") else {
            continue;
        };
        let Some(value) = value.trim_start().strip_prefix('=') else {
            continue;
        };
        found = Some(value.trim());
    }
    let value = found?;
    let value = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(value);
    let path = if value == "$HOME" {
        home.to_path_buf()
    } else if let Some(rest) = value.strip_prefix("$HOME/") {
        // Why trim: `Path::join` with a rooted `rest` (from `$HOME//x`) would
        // replace the home folder instead of extending it.
        home.join(rest.trim_start_matches('/'))
    } else if value.starts_with('/') {
        PathBuf::from(value)
    } else {
        return None;
    };
    let is_home = path.components().eq(home.components());
    (!is_home).then_some(path)
}

/// `~/Downloads` on a Unix-like client. Why not trust any `$HOME`: an empty
/// or relative one would make the default folder depend on the working
/// directory the client was started from.
#[cfg_attr(windows, allow(dead_code))] // Why: Windows asks the Known Folder API.
pub(crate) fn home_downloads_dir(env: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    absolute_unix_home(env).map(|home| home.join("Downloads"))
}

fn absolute_unix_home(env: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    home_dir_from_env(false, env)
        .ok()
        .filter(|home| home.is_absolute())
}

/// Where a Linux client saves downloads: `$XDG_DOWNLOAD_DIR`, then the user's
/// `user-dirs.dirs`, then `~/Downloads`. `read` returns a file's text.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn xdg_download_dir(
    env: impl Fn(&str) -> Option<OsString>,
    read: impl Fn(&std::path::Path) -> Option<String>,
) -> Option<PathBuf> {
    let home = absolute_unix_home(&env)?;
    let absolute = |key: &str| {
        env(key)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    if let Some(dir) = absolute("XDG_DOWNLOAD_DIR") {
        return Some(dir);
    }
    let config = absolute("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config"));
    if let Some(dir) =
        read(&config.join("user-dirs.dirs")).and_then(|contents| parse_user_dirs(&contents, &home))
    {
        return Some(dir);
    }
    home_downloads_dir(&env)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_dir_uses_windows_profile_before_literal_home() {
        assert_eq!(
            home_dir_from_env(true, |key| match key {
                "HOME" => Some("~".into()),
                "USERPROFILE" => Some(r"C:\Users\herdr".into()),
                _ => None,
            }),
            Ok(PathBuf::from(r"C:\Users\herdr"))
        );
    }

    #[test]
    fn home_dir_uses_windows_drive_and_path_when_profile_is_missing() {
        assert_eq!(
            home_dir_from_env(true, |key| match key {
                "HOMEDRIVE" => Some("C:".into()),
                "HOMEPATH" => Some(r"\Users\herdr".into()),
                _ => None,
            }),
            Ok(PathBuf::from(r"C:\Users\herdr"))
        );
    }

    #[test]
    fn home_dir_rejects_incomplete_windows_drive_and_path() {
        assert_eq!(
            home_dir_from_env(true, |key| match key {
                "HOMEDRIVE" => Some("C:".into()),
                "HOMEPATH" => Some("".into()),
                _ => None,
            }),
            Err(())
        );
        assert_eq!(
            home_dir_from_env(true, |key| match key {
                "HOMEDRIVE" => Some("C:".into()),
                "HOMEPATH" => Some("Users\\herdr".into()),
                _ => None,
            }),
            Err(())
        );
    }

    // Why Unix only: these cover the Linux and macOS lookup, which reads
    // slash-rooted paths that Windows does not call absolute and joins with `\`.
    #[cfg(unix)]
    mod unix_clients {
        use super::*;
        use std::path::Path;

        fn parse(contents: &str) -> Option<PathBuf> {
            parse_user_dirs(contents, Path::new("/home/me"))
        }

        #[test]
        fn user_dirs_expand_home_in_the_download_entry() {
            assert_eq!(
                parse("XDG_DOWNLOAD_DIR=\"$HOME/Downloads\"\n"),
                Some(PathBuf::from("/home/me/Downloads"))
            );
            assert_eq!(
                parse("XDG_DOWNLOAD_DIR=\"$HOME/ダウンロード\""),
                Some(PathBuf::from("/home/me/ダウンロード"))
            );
        }

        #[test]
        fn user_dirs_accept_an_absolute_path_and_an_unquoted_value() {
            assert_eq!(
                parse("XDG_DOWNLOAD_DIR=\"/data/dl\""),
                Some(PathBuf::from("/data/dl"))
            );
            assert_eq!(
                parse("XDG_DOWNLOAD_DIR=$HOME/dl"),
                Some(PathBuf::from("/home/me/dl"))
            );
        }

        #[test]
        fn user_dirs_skip_comments_other_keys_and_take_the_last_download_entry() {
            let contents = "# XDG_DOWNLOAD_DIR=\"$HOME/old\"\n\
                XDG_DESKTOP_DIR=\"$HOME/Desktop\"\n\
                XDG_DOWNLOAD_DIR=\"$HOME/first\"\n\
                XDG_DOWNLOAD_DIR=\"$HOME/second\"\n";
            assert_eq!(parse(contents), Some(PathBuf::from("/home/me/second")));
        }

        #[test]
        fn user_dirs_without_a_usable_download_entry_give_nothing() {
            assert_eq!(parse(""), None);
            assert_eq!(parse("XDG_DESKTOP_DIR=\"$HOME/Desktop\""), None);
            assert_eq!(parse("XDG_DOWNLOAD_DIR=\"relative/dir\""), None);
            assert_eq!(parse("XDG_DOWNLOAD_DIRS=\"$HOME/x\""), None);
            assert_eq!(parse("XDG_DOWNLOAD_DIR=\"$(rm -rf x)\""), None);
        }

        #[test]
        fn user_dirs_that_name_the_home_folder_mean_no_downloads_folder() {
            assert_eq!(parse("XDG_DOWNLOAD_DIR=\"$HOME/\""), None);
            assert_eq!(parse("XDG_DOWNLOAD_DIR=\"$HOME\""), None);
            assert_eq!(parse("XDG_DOWNLOAD_DIR=\"/home/me\""), None);
            assert_eq!(parse("XDG_DOWNLOAD_DIR=\"$HOME//\""), None);
        }

        #[test]
        fn a_doubled_slash_after_home_stays_under_home() {
            assert_eq!(
                parse("XDG_DOWNLOAD_DIR=\"$HOME//Downloads\""),
                Some(PathBuf::from("/home/me/Downloads"))
            );
        }

        #[test]
        fn the_downloads_folder_needs_an_absolute_home() {
            let downloads = |home: &'static str| {
                home_downloads_dir(move |key| (key == "HOME").then(|| home.into()))
            };
            assert_eq!(
                downloads("/home/me"),
                Some(PathBuf::from("/home/me/Downloads"))
            );
            assert_eq!(downloads(""), None);
            assert_eq!(downloads("rel"), None);
            assert_eq!(xdg(&[("HOME", "rel")], &[]), None);
        }

        fn xdg(env: &[(&str, &str)], files: &[(&str, &str)]) -> Option<PathBuf> {
            xdg_download_dir(
                |key| {
                    env.iter()
                        .find(|(name, _)| *name == key)
                        .map(|(_, value)| OsString::from(value))
                },
                |path| {
                    files
                        .iter()
                        .find(|(name, _)| Path::new(name) == path)
                        .map(|(_, contents)| (*contents).to_owned())
                },
            )
        }

        #[test]
        fn the_environment_variable_beats_the_user_dirs_file() {
            assert_eq!(
                xdg(
                    &[("HOME", "/home/me"), ("XDG_DOWNLOAD_DIR", "/srv/dl")],
                    &[(
                        "/home/me/.config/user-dirs.dirs",
                        "XDG_DOWNLOAD_DIR=\"$HOME/from-file\""
                    )],
                ),
                Some(PathBuf::from("/srv/dl"))
            );
        }

        #[test]
        fn a_relative_environment_variable_is_ignored() {
            assert_eq!(
                xdg(&[("HOME", "/home/me"), ("XDG_DOWNLOAD_DIR", "dl")], &[]),
                Some(PathBuf::from("/home/me/Downloads"))
            );
        }

        #[test]
        fn the_user_dirs_file_is_read_from_xdg_config_home_when_set() {
            assert_eq!(
                xdg(
                    &[("HOME", "/home/me"), ("XDG_CONFIG_HOME", "/cfg")],
                    &[("/cfg/user-dirs.dirs", "XDG_DOWNLOAD_DIR=\"$HOME/from-cfg\"")],
                ),
                Some(PathBuf::from("/home/me/from-cfg"))
            );
        }

        #[test]
        fn the_downloads_folder_in_home_is_the_last_resort() {
            assert_eq!(
                xdg(&[("HOME", "/home/me")], &[]),
                Some(PathBuf::from("/home/me/Downloads"))
            );
            assert_eq!(xdg(&[], &[]), None);
        }
    }
}
