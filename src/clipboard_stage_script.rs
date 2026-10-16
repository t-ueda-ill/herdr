//! The shell half of staging a bridged clipboard image outside the server
//! host. Container and ssh panes run the same script, so the pasted path has
//! the same `${TMPDIR:-/tmp}/herdr-clipboard-images-<uid>` layout either way.

/// Why not `docker cp` or `scp`: neither can pick the mode, so a non-root
/// agent may be unable to read the file. Running as the pane's user with
/// `umask 077` matches the host staging directory's 0700/0600.
/// The stale-file sweep rides on this run instead of costing another one.
/// Why not write after a failed `mkdir`/`chmod`: the directory may belong to
/// another uid in a shared temp dir, so the script exits before `cat`. A
/// failed sweep stays non-fatal because it only delays cleanup.
/// Why no `'` or `\`: the ssh backend wraps the script in single quotes for
/// the remote login shell, which must hand it to `sh` unchanged.
pub(crate) const STAGE_SCRIPT: &str = r#"d="${TMPDIR:-/tmp}/herdr-clipboard-images-$(id -u)"; umask 077; mkdir -p "$d" && chmod 700 "$d" || exit 1; find "$d" -type f -mtime +0 -delete; cat > "$d/$1" && printf %s "$d/$1""#;

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
    Ok(stdout.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAME: &str = "client-7-clipboard-123-0.png";

    #[test]
    fn stage_script_writes_under_the_per_uid_temp_dir_and_prints_the_path() {
        assert!(STAGE_SCRIPT
            .starts_with(r#"d="${TMPDIR:-/tmp}/herdr-clipboard-images-$(id -u)"; umask 077;"#));
        assert!(STAGE_SCRIPT.contains(r#"mkdir -p "$d" && chmod 700 "$d" || exit 1;"#));
        assert!(STAGE_SCRIPT.contains(r#"; find "$d" -type f -mtime +0 -delete; cat"#));
        assert!(STAGE_SCRIPT.ends_with(r#"cat > "$d/$1" && printf %s "$d/$1""#));
    }

    #[test]
    fn stage_script_survives_single_quoting_for_a_remote_login_shell() {
        assert!(!STAGE_SCRIPT.contains('\''));
        assert!(!STAGE_SCRIPT.contains('\\'));
        assert!(!STAGE_SCRIPT.chars().any(char::is_control));
    }

    #[test]
    fn staged_path_accepts_an_absolute_path_naming_the_file() {
        let path = format!("/tmp/herdr-clipboard-images-1000/{NAME}");
        assert_eq!(staged_path_from_output(&path, NAME), Ok(path.clone()));
        let custom = format!("/var/run/user tmp/herdr-clipboard-images-0/{NAME}");
        assert_eq!(staged_path_from_output(&custom, NAME), Ok(custom.clone()));
    }

    #[test]
    fn staged_path_rejects_relative_foreign_or_multiline_output() {
        let rejected = [
            String::new(),
            format!("tmp/herdr-clipboard-images-1000/{NAME}"),
            "/tmp/herdr-clipboard-images-1000/other.png".to_owned(),
            format!("/tmp/x{NAME}"),
            format!("/tmp/herdr-clipboard-images-1000/{NAME}\n"),
            format!("/tmp/evil\n/tmp/herdr-clipboard-images-1000/{NAME}"),
            format!("/tmp/\x1b[31m/{NAME}"),
        ];
        for output in rejected {
            assert!(
                staged_path_from_output(&output, NAME).is_err(),
                "{output:?} must be rejected"
            );
        }
    }
}
