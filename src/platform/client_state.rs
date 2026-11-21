use std::path::Path;

#[cfg(not(windows))]
pub(crate) fn create_private_state_file(path: &Path) -> std::io::Result<std::fs::File> {
    super::create_remote_ssh_config_file(path)
}

#[cfg(windows)]
pub(crate) fn create_private_state_file(path: &Path) -> std::io::Result<std::fs::File> {
    super::windows::create_remote_ssh_config_file(path)
}

#[cfg(not(windows))]
pub(crate) fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::rename(source, destination)
}

#[cfg(windows)]
pub(crate) fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    super::windows::replace_file(source, destination)
}

#[cfg(not(windows))]
pub(crate) fn sync_parent_directory(path: &Path) -> std::io::Result<()> {
    std::fs::File::open(path)?.sync_all()
}

#[cfg(windows)]
pub(crate) fn sync_parent_directory(_path: &Path) -> std::io::Result<()> {
    // replace_file uses MOVEFILE_WRITE_THROUGH on Windows.
    Ok(())
}

/// Makes a new folder that only this user can open. Non-recursive and never
/// reuses an existing path: an existing folder or link could be another
/// user's, and its permissions cannot be trusted.
#[cfg(unix)]
pub(crate) fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new().mode(0o700).create(dir)
}

/// A protected DACL, because the temp folder can be shared and a plain create
/// would inherit its access rules.
#[cfg(windows)]
pub(crate) fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    super::windows::create_remote_private_dir(dir)
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::create_private_dir;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("herdr-private-dir-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&dir);
        dir
    }

    #[test]
    fn a_new_folder_is_made_for_its_owner_only() {
        let dir = scratch("new");
        create_private_dir(&dir).unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        std::fs::remove_dir(&dir).unwrap();
        assert_eq!(mode, 0o700);
    }

    #[test]
    fn an_existing_folder_is_never_taken_over() {
        let dir = scratch("existing");
        create_private_dir(&dir).unwrap();
        let again = create_private_dir(&dir);
        std::fs::remove_dir(&dir).unwrap();
        assert_eq!(again.unwrap_err().kind(), std::io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn an_existing_link_is_never_followed() {
        let target = scratch("target");
        let link = scratch("link");
        create_private_dir(&target).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let refused = create_private_dir(&link);
        std::fs::remove_file(&link).unwrap();
        std::fs::remove_dir(&target).unwrap();
        assert_eq!(
            refused.unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists
        );
    }
}
