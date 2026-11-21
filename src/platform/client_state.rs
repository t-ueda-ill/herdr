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

/// Makes `dir` for this user only, or takes it again when it already is one.
/// Why check an existing folder: another user who learned the name could have
/// made it, or a link, before us.
#[cfg(unix)]
pub(crate) fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
        other => return other,
    }
    let metadata = std::fs::symlink_metadata(dir)?;
    // SAFETY: geteuid takes no pointers and has no preconditions.
    let uid = unsafe { libc::geteuid() };
    if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("{} is not a private folder of this user", dir.display()),
        ));
    }
    Ok(())
}

/// The Windows temp folder is already per user; only a link is refused.
#[cfg(windows)]
pub(crate) fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    match std::fs::DirBuilder::new().create(dir) {
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
        other => return other,
    }
    let metadata = std::fs::symlink_metadata(dir)?;
    if !metadata.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("{} is not a folder", dir.display()),
        ));
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::ensure_private_dir;

    fn mode(path: &std::path::Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("herdr-private-dir-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&dir);
        dir
    }

    #[test]
    fn a_missing_folder_is_made_for_its_owner_only() {
        let dir = scratch("new");
        ensure_private_dir(&dir).unwrap();
        assert_eq!(mode(&dir), 0o700);
        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn an_own_private_folder_is_taken_again() {
        let dir = scratch("again");
        ensure_private_dir(&dir).unwrap();
        ensure_private_dir(&dir).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn a_folder_that_others_can_open_is_refused() {
        let dir = scratch("open");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let refused = ensure_private_dir(&dir);
        std::fs::remove_dir(&dir).unwrap();
        assert_eq!(
            refused.unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn a_link_to_a_private_folder_is_refused() {
        let target = scratch("target");
        let link = scratch("link");
        ensure_private_dir(&target).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let refused = ensure_private_dir(&link);
        std::fs::remove_file(&link).unwrap();
        std::fs::remove_dir(&target).unwrap();
        assert_eq!(
            refused.unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
    }
}
