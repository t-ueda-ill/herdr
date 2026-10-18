//! The shell half of staging a bridged clipboard image outside the server
//! host. Container and ssh panes run the same script, so the pasted path has
//! the same `${TMPDIR:-/tmp}/herdr-clipboard-image-XXXXXX/<file>` layout either
//! way.

/// Directory name prefix of every staged image. Cleanup only removes
/// directories carrying it.
pub(crate) const STAGE_DIR_PREFIX: &str = "herdr-clipboard-image-";

/// Why not `docker cp` or `scp`: neither can pick the mode, so a non-root
/// agent may be unable to read the file. Running as the pane's user with
/// `umask 077` matches the host staging directory's 0700/0600.
/// Why not a reusable per-uid directory: its path is predictable, so on a
/// shared host another user can plant a symlink there; `mkdir -p` accepts it
/// and `chmod`/`cat >` then follow it into an unrelated directory. Why not a
/// `test -L` guard: it would still race between the check and the use.
/// `mktemp -d` creates a fresh 0700 directory and never follows an existing
/// path.
/// The stale sweep rides on this run instead of costing another one. It lists
/// only real directories (`-type d` does not match symlinks) owned by the
/// current uid, and its failure stays non-fatal because it only delays cleanup.
/// The prefix is singular so the sweep never recurses into the old
/// `herdr-clipboard-images-<uid>` layout.
/// Why remove the directory on a failed write: the caller never learns the
/// path, so nothing else would delete it.
/// Why no `'` or `\`: the ssh backend wraps the script in single quotes for
/// the remote login shell, which must hand it to `sh` unchanged.
pub(crate) const STAGE_SCRIPT: &str = r#"t="${TMPDIR:-/tmp}"; umask 077; d=$(mktemp -d "$t/herdr-clipboard-image-XXXXXX") || exit 1; find "$t" -maxdepth 1 -type d -name "herdr-clipboard-image-*" -user "$(id -u)" -mtime +0 -exec rm -rf {} + 2>/dev/null; cat > "$d/$1" && printf %s "$d/$1" || { rm -rf "$d"; exit 1; }"#;

/// Why `rmdir` and not `rm -rf`: it can only ever remove an empty directory,
/// so a path that slipped past validation cannot delete anything else.
/// Shared by both cleanup commands.
pub(crate) const CLEANUP_SCRIPT: &str =
    r#"rm -f "$@"; for f; do rmdir "${f%/*}" 2>/dev/null || :; done"#;

/// Validates the path the stage script printed. It becomes pane input, so a
/// control character could inject keystrokes and must never be pasted.
pub(crate) fn staged_path_from_output(stdout: &str, file_name: &str) -> Result<String, String> {
    let suffix = format!("/{file_name}");
    if !stdout.starts_with('/') {
        return Err(format!("staged path is not absolute: {stdout:?}"));
    }
    if stdout.chars().any(char::is_control) {
        return Err(format!(
            "staged path contains control characters: {stdout:?}"
        ));
    }
    if !stdout.ends_with(&suffix) {
        return Err(format!(
            "staged path does not name {file_name:?}: {stdout:?}"
        ));
    }
    let parent_is_ours = std::path::Path::new(stdout)
        .parent()
        .and_then(std::path::Path::file_name)
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|name| name.starts_with(STAGE_DIR_PREFIX));
    if !parent_is_ours {
        return Err(format!(
            "staged path is outside a {STAGE_DIR_PREFIX}* directory: {stdout:?}"
        ));
    }
    Ok(stdout.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAME: &str = "client-7-clipboard-123-0.png";

    #[test]
    fn stage_script_creates_a_fresh_private_dir_then_writes_and_prints_the_path() {
        assert!(STAGE_SCRIPT.starts_with(r#"t="${TMPDIR:-/tmp}"; umask 077; d=$(mktemp -d "#));
        assert!(STAGE_SCRIPT.contains(r#"herdr-clipboard-image-XXXXXX") || exit 1;"#));
        assert!(
            STAGE_SCRIPT.contains(r#"-type d -name "herdr-clipboard-image-*" -user "$(id -u)""#)
        );
        assert!(STAGE_SCRIPT.contains(r#"cat > "$d/$1" && printf %s "$d/$1""#));
    }

    #[test]
    fn stage_script_never_names_a_reusable_predictable_directory() {
        assert!(!STAGE_SCRIPT.contains("herdr-clipboard-images-"));
        assert!(STAGE_SCRIPT.contains("mktemp -d"));
        assert!(!STAGE_SCRIPT.contains("mkdir"));
    }

    #[test]
    fn stage_script_removes_the_fresh_directory_when_the_write_fails() {
        assert!(STAGE_SCRIPT.ends_with(r#"|| { rm -rf "$d"; exit 1; }"#));
    }

    #[test]
    fn stage_and_cleanup_scripts_survive_single_quoting_for_a_remote_login_shell() {
        for script in [STAGE_SCRIPT, CLEANUP_SCRIPT] {
            assert!(!script.contains('\''));
            assert!(!script.contains('\\'));
            assert!(!script.chars().any(char::is_control));
        }
    }

    #[test]
    fn staged_path_accepts_an_absolute_path_naming_the_file() {
        let path = format!("/tmp/herdr-clipboard-image-aB3dE9/{NAME}");
        assert_eq!(staged_path_from_output(&path, NAME), Ok(path.clone()));
        let custom = format!("/var/run/user tmp/herdr-clipboard-image-Zz09Qq/{NAME}");
        assert_eq!(staged_path_from_output(&custom, NAME), Ok(custom.clone()));
    }

    #[test]
    fn staged_path_rejects_relative_foreign_or_multiline_output() {
        let rejected = [
            String::new(),
            format!("tmp/herdr-clipboard-image-aB3dE9/{NAME}"),
            "/tmp/herdr-clipboard-image-aB3dE9/other.png".to_owned(),
            format!("/tmp/x{NAME}"),
            format!("/tmp/herdr-clipboard-image-aB3dE9/{NAME}\n"),
            format!("/tmp/evil\n/tmp/herdr-clipboard-image-aB3dE9/{NAME}"),
            format!("/tmp/\x1b[31m/{NAME}"),
            format!("/tmp/{NAME}"),
            format!("/home/user/{NAME}"),
            format!("/tmp/herdr-clipboard-images-1000/{NAME}"),
            format!("/tmp/other-dir/herdr-clipboard-image-aB3dE9/../{NAME}"),
        ];
        for output in rejected {
            assert!(
                staged_path_from_output(&output, NAME).is_err(),
                "{output:?} must be rejected"
            );
        }
    }

    #[cfg(unix)]
    mod execution {
        use super::*;
        use std::io::Write;
        use std::os::unix::fs::{symlink, PermissionsExt};
        use std::path::{Path, PathBuf};
        use std::process::{Command, Output, Stdio};

        struct TempRoot(PathBuf);

        impl TempRoot {
            fn new(name: &str) -> Self {
                let dir = std::env::temp_dir()
                    .join(format!("herdr-stage-script-{name}-{}", std::process::id()));
                let _ = std::fs::remove_dir_all(&dir);
                std::fs::create_dir_all(&dir).unwrap();
                Self(dir)
            }
        }

        impl Drop for TempRoot {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        fn mode(path: &Path) -> u32 {
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777
        }

        fn run_script(tmpdir: &Path, file_name: &str, stdin: &[u8]) -> Output {
            let mut child = Command::new("/bin/sh")
                .args(["-c", STAGE_SCRIPT, "sh", file_name])
                .env("TMPDIR", tmpdir)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            child.stdin.take().unwrap().write_all(stdin).unwrap();
            child.wait_with_output().unwrap()
        }

        fn staged_dirs(tmpdir: &Path) -> Vec<PathBuf> {
            std::fs::read_dir(tmpdir)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .filter(|path| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.starts_with(STAGE_DIR_PREFIX))
                })
                .collect()
        }

        #[test]
        fn stage_script_writes_a_0600_file_in_a_fresh_0700_directory_under_tmpdir() {
            let root = TempRoot::new("fresh");
            let output = run_script(&root.0, NAME, b"image-bytes");

            assert!(output.status.success(), "{output:?}");
            let printed = PathBuf::from(String::from_utf8(output.stdout).unwrap());
            let dir = printed.parent().unwrap();
            assert_eq!(dir.parent().unwrap(), root.0);
            assert!(dir
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with(STAGE_DIR_PREFIX));
            assert_eq!(std::fs::read(&printed).unwrap(), b"image-bytes");
            assert_eq!(mode(&printed), 0o600);
            assert_eq!(mode(dir), 0o700);
        }

        #[test]
        fn stage_script_leaves_a_planted_symlink_at_the_old_predictable_path_untouched() {
            let root = TempRoot::new("planted");
            let victim = root.0.join("victim");
            std::fs::create_dir(&victim).unwrap();
            std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o755)).unwrap();
            let uid = unsafe { libc::geteuid() };
            symlink(
                &victim,
                root.0.join(format!("herdr-clipboard-images-{uid}")),
            )
            .unwrap();

            let output = run_script(&root.0, NAME, b"image-bytes");

            assert!(output.status.success(), "{output:?}");
            assert_eq!(mode(&victim), 0o755);
            assert_eq!(std::fs::read_dir(&victim).unwrap().count(), 0);
        }

        #[test]
        fn stage_script_removes_its_directory_and_fails_when_the_file_cannot_be_written() {
            let root = TempRoot::new("write-fails");
            let output = run_script(&root.0, "missing/x.png", b"image-bytes");

            assert!(!output.status.success());
            assert!(output.stdout.is_empty());
            assert!(staged_dirs(&root.0).is_empty());
        }
    }
}
