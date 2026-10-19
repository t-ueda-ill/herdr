use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::clipboard_stage_script::STAGE_DIR_PREFIX;

const STAGED_CLIPBOARD_IMAGE_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// Where a bridged image must land so the target pane's processes can open it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StageTarget {
    /// The pane runs on the server host and shares its filesystem.
    Host,
    /// The pane shell runs where the server reaches it only through a CLI.
    OffHost(OffHostTarget),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OffHostTarget {
    /// The pane shell runs inside this container, as `user` when set. `ssh`
    /// names the host whose docker the container lives on; `None` is the
    /// server's own docker.
    Container {
        id: String,
        user: Option<String>,
        ssh: Option<RemoteDockerHost>,
    },
    /// The pane shell is an ssh login to `host` whose master listens on
    /// `control_path`.
    Ssh { host: String, control_path: PathBuf },
}

/// An ssh host that runs the docker CLI for a container pane, joined through
/// the pane's master. Why a required `control_path`: without a shared
/// connection every paste would need its own login, and batch mode cannot
/// provide one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteDockerHost {
    pub(crate) host: String,
    pub(crate) control_path: PathBuf,
}

impl OffHostTarget {
    /// The client-facing reason a stage to this target failed.
    pub(crate) fn failure_detail(&self, err: &str) -> String {
        match self {
            Self::Container { ssh: None, .. } => {
                format!("Image could not be copied into the container: {err}")
            }
            Self::Container { ssh: Some(_), .. } => {
                format!("Image could not be copied into the container on the ssh host: {err}")
            }
            Self::Ssh { .. } => format!("Image could not be copied to the ssh host: {err}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StagedClipboardImage {
    /// Host path for `StageTarget::Host`, the pane environment's path otherwise.
    pub(crate) path: PathBuf,
    pub(crate) paste_text: String,
}

/// A file staged outside the server host, kept so disconnect can remove it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OffHostClipboardFile {
    pub(crate) target: OffHostTarget,
    pub(crate) path: String,
}

pub(crate) fn stage(
    client_id: u64,
    extension: &str,
    data: &[u8],
) -> io::Result<StagedClipboardImage> {
    stage_in(&std::env::temp_dir(), client_id, extension, data)
}

/// Stages into a fresh private directory under `base`. Why not a shared
/// per-uid directory: `create_dir_all` and `set_permissions` follow a symlink
/// planted at that predictable path, so another local user could redirect the
/// write or chmod. `DirBuilder::create` fails on any existing entry, symlink
/// included, so a fresh directory is never reached through a planted path.
fn stage_in(
    base: &Path,
    client_id: u64,
    extension: &str,
    data: &[u8],
) -> io::Result<StagedClipboardImage> {
    cleanup_stale(base);

    let unique = unique_stamp();
    let pid = std::process::id();

    for attempt in 0..100 {
        let dir = base.join(format!("{STAGE_DIR_PREFIX}{unique}-{pid}-{attempt}"));
        match create_private_dir(&dir) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
        let path = dir.join(staged_file_name(client_id, extension, unique, attempt));
        let written = write_private_file(&path, data);
        if let Err(err) = written {
            let _ = fs::remove_file(&path);
            let _ = fs::remove_dir(&dir);
            return Err(err);
        }
        return Ok(StagedClipboardImage {
            paste_text: path.to_string_lossy().into_owned(),
            path,
        });
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "failed to allocate unique clipboard image staging path",
    ))
}

fn write_private_file(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    restrict_file_options(&mut options);
    options.open(path)?.write_all(data)
}

/// Removes staged files and their per-image directory. The parent is removed
/// only when it carries the staging prefix so an unrelated directory is never
/// touched.
pub(crate) fn remove_files(paths: Vec<PathBuf>) {
    for path in paths {
        let _ = fs::remove_file(&path);
        if let Some(parent) = path.parent().filter(|parent| has_stage_prefix(parent)) {
            let _ = fs::remove_dir(parent);
        }
    }
}

fn has_stage_prefix(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(STAGE_DIR_PREFIX))
}

/// Copies the image into the pane's environment through `docker exec` or
/// the pane's ssh master. Blocks for up to the CLI deadline, so it runs on a
/// worker thread, never the app loop.
pub(crate) fn stage_off_host(
    client_id: u64,
    extension: &str,
    data: Vec<u8>,
    target: &OffHostTarget,
) -> Result<StagedClipboardImage, String> {
    // Why not retry on a name clash like the host path: `cat >` cannot fail
    // on an existing file, so the per-server counter guarantees uniqueness.
    static NEXT_OFF_HOST_ATTEMPT: AtomicU64 = AtomicU64::new(0);
    let attempt = NEXT_OFF_HOST_ATTEMPT.fetch_add(1, Ordering::Relaxed);
    let file_name = staged_file_name(client_id, extension, unique_stamp(), attempt);
    let path = match target {
        OffHostTarget::Container { id, user, ssh } => {
            let host = docker_host(ssh.as_ref())?;
            crate::container::clipboard::stage_image(&host, id, user.as_deref(), &file_name, data)
                .map_err(|err| err.to_string())?
        }
        OffHostTarget::Ssh { host, control_path } => {
            stage_over_ssh(host, control_path, &file_name, data)?
        }
    };
    Ok(StagedClipboardImage {
        path: PathBuf::from(&path),
        paste_text: path,
    })
}

/// Why not fall back to the local docker for a remote id: the id names a
/// container on another machine, so a local `docker exec` would fail or,
/// worse, hit an unrelated container.
#[cfg(unix)]
fn docker_host(
    ssh: Option<&RemoteDockerHost>,
) -> Result<crate::container::docker::DockerHost, String> {
    use crate::container::docker::DockerHost;
    Ok(match ssh {
        Some(remote) => DockerHost::Ssh {
            host: remote.host.clone(),
            control_path: Some(remote.control_path.clone()),
        },
        None => DockerHost::Local,
    })
}

#[cfg(windows)]
fn docker_host(
    ssh: Option<&RemoteDockerHost>,
) -> Result<crate::container::docker::DockerHost, String> {
    match ssh {
        Some(_) => Err(SSH_SIDE_CHANNEL_UNSUPPORTED.to_owned()),
        None => Ok(crate::container::docker::DockerHost::Local),
    }
}

#[cfg(unix)]
fn stage_over_ssh(
    host: &str,
    control_path: &Path,
    file_name: &str,
    data: Vec<u8>,
) -> Result<String, String> {
    crate::ssh_host::clipboard::stage_image(host, control_path, file_name, data)
        .map_err(|err| err.to_string())
}

#[cfg(windows)]
fn stage_over_ssh(
    _host: &str,
    _control_path: &Path,
    _file_name: &str,
    _data: Vec<u8>,
) -> Result<String, String> {
    Err(SSH_SIDE_CHANNEL_UNSUPPORTED.to_owned())
}

#[cfg(unix)]
fn remove_over_ssh(host: &str, control_path: &Path, paths: &[String]) -> Result<(), String> {
    crate::ssh_host::clipboard::remove_images(host, control_path, paths)
        .map_err(|err| err.to_string())
}

#[cfg(windows)]
fn remove_over_ssh(_host: &str, _control_path: &Path, _paths: &[String]) -> Result<(), String> {
    Err(SSH_SIDE_CHANNEL_UNSUPPORTED.to_owned())
}

#[cfg(windows)]
const SSH_SIDE_CHANNEL_UNSUPPORTED: &str = "ssh panes are not supported on Windows hosts";

/// Removes off-host staged files on a worker thread, one CLI run per
/// container and user or per ssh master. Errors are ignored: the container or
/// master may already be gone, and the stage script's age sweep removes
/// leftovers on the next paste.
pub(crate) fn remove_off_host_files(files: Vec<OffHostClipboardFile>) {
    let batches = off_host_cleanup_batches(files);
    if !batches.is_empty() {
        run_off_host_cleanup(batches);
    }
}

/// Removes one batch on the calling thread; blocks for up to the CLI deadline.
pub(crate) fn remove_off_host_batch(batch: &OffHostCleanupBatch) -> Result<(), String> {
    match &batch.target {
        OffHostTarget::Container { id, user, ssh } => {
            let host = docker_host(ssh.as_ref())?;
            crate::container::clipboard::remove_images(&host, id, user.as_deref(), &batch.paths)
                .map_err(|err| err.to_string())
        }
        OffHostTarget::Ssh { host, control_path } => {
            remove_over_ssh(host, control_path, &batch.paths)
        }
    }
}

#[cfg(not(test))]
fn run_off_host_cleanup(batches: Vec<OffHostCleanupBatch>) {
    let spawned = crate::thread_spawn::spawn_named("herdr-clipboard-cleanup", move || {
        for batch in batches {
            if let Err(err) = remove_off_host_batch(&batch) {
                tracing::debug!(
                    target = ?batch.target,
                    err = %err,
                    "off-host clipboard image cleanup failed"
                );
            }
        }
    });
    if let Err(err) = spawned {
        tracing::warn!(err = %err, "failed to spawn off-host clipboard cleanup thread");
    }
}

/// Unit tests record cleanups instead of reaching docker or ssh.
#[cfg(test)]
fn run_off_host_cleanup(batches: Vec<OffHostCleanupBatch>) {
    test_cleanup::record(batches);
}

#[cfg(test)]
pub(crate) mod test_cleanup {
    use std::cell::RefCell;

    use super::OffHostCleanupBatch;

    thread_local! {
        static RECORDED: RefCell<Vec<OffHostCleanupBatch>> = const { RefCell::new(Vec::new()) };
    }

    pub(super) fn record(batches: Vec<OffHostCleanupBatch>) {
        RECORDED.with(|recorded| recorded.borrow_mut().extend(batches));
    }

    /// Cleanups requested on the current thread since the last call.
    pub(crate) fn take() -> Vec<OffHostCleanupBatch> {
        RECORDED.with(|recorded| std::mem::take(&mut *recorded.borrow_mut()))
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct OffHostCleanupBatch {
    pub(crate) target: OffHostTarget,
    pub(crate) paths: Vec<String>,
}

/// Groups files by target, keeping first-seen order.
pub(crate) fn off_host_cleanup_batches(
    files: Vec<OffHostClipboardFile>,
) -> Vec<OffHostCleanupBatch> {
    let mut batches: Vec<OffHostCleanupBatch> = Vec::new();
    for file in files {
        match batches.iter_mut().find(|batch| batch.target == file.target) {
            Some(batch) => batch.paths.push(file.path),
            None => batches.push(OffHostCleanupBatch {
                target: file.target,
                paths: vec![file.path],
            }),
        }
    }
    batches
}

pub(crate) fn staged_file_name(
    client_id: u64,
    extension: &str,
    unique: u128,
    attempt: u64,
) -> String {
    let extension = sanitize_extension(extension);
    format!("client-{client_id}-clipboard-{unique}-{attempt}.{extension}")
}

fn unique_stamp() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

fn sanitize_extension(extension: &str) -> &'static str {
    if extension.eq_ignore_ascii_case("png") {
        "png"
    } else if extension.eq_ignore_ascii_case("jpg") || extension.eq_ignore_ascii_case("jpeg") {
        "jpg"
    } else if extension.eq_ignore_ascii_case("gif") {
        "gif"
    } else if extension.eq_ignore_ascii_case("webp") {
        "webp"
    } else if extension.eq_ignore_ascii_case("bmp") {
        "bmp"
    } else {
        "png"
    }
}

/// Non-recursive on purpose: a recursive create would accept an existing path.
#[cfg(unix)]
fn create_private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    fs::DirBuilder::new().mode(0o700).create(dir)
}

#[cfg(windows)]
fn create_private_dir(dir: &Path) -> io::Result<()> {
    fs::DirBuilder::new().create(dir)
}

#[cfg(unix)]
fn restrict_file_options(options: &mut fs::OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;

    options.mode(0o600);
}

#[cfg(windows)]
fn restrict_file_options(_options: &mut fs::OpenOptions) {}

#[cfg(unix)]
fn owned_by_current_user(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    metadata.uid() == unsafe { libc::geteuid() }
}

#[cfg(windows)]
fn owned_by_current_user(_metadata: &fs::Metadata) -> bool {
    true
}

/// Removes stale staging directories directly under `base`. Why not
/// `remove_dir_all` or following symlinks: `base` is a shared temp directory
/// where anyone can plant a prefix-named entry, so only real directories we
/// own are touched, and only their regular files plus the directory itself are
/// removed, never anything a link or nested entry could point at.
fn cleanup_stale(base: &Path) {
    let Ok(entries) = fs::read_dir(base) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !has_stage_prefix(&path) {
            continue;
        }
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if !metadata.is_dir() || !owned_by_current_user(&metadata) {
            continue;
        }
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        if modified.elapsed().unwrap_or_default() <= STAGED_CLIPBOARD_IMAGE_MAX_AGE {
            continue;
        }
        let Ok(children) = fs::read_dir(&path) else {
            continue;
        };
        for child in children.flatten() {
            let is_file = child.file_type().is_ok_and(|kind| kind.is_file());
            if is_file {
                let _ = fs::remove_file(child.path());
            }
        }
        let _ = fs::remove_dir(&path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_extension_accepts_known_image_extensions() {
        assert_eq!(sanitize_extension("PNG"), "png");
        assert_eq!(sanitize_extension("jpeg"), "jpg");
        assert_eq!(sanitize_extension("webp"), "webp");
        assert_eq!(sanitize_extension("sh"), "png");
    }

    #[test]
    fn staged_file_name_keeps_the_client_scheme_and_sanitizes_the_extension() {
        assert_eq!(
            staged_file_name(7, "JPEG", 123, 4),
            "client-7-clipboard-123-4.jpg"
        );
        assert_eq!(
            staged_file_name(7, "../../etc/passwd", 123, 0),
            "client-7-clipboard-123-0.png"
        );
    }

    struct TempBase(PathBuf);

    impl TempBase {
        fn new(label: &str) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "herdr-clipboard-test-base-{label}-{}-{}-{}",
                std::process::id(),
                unique_stamp(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).expect("create test base");
            Self(path)
        }
    }

    impl Drop for TempBase {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn make_old(path: &Path) {
        let old = SystemTime::now() - STAGED_CLIPBOARD_IMAGE_MAX_AGE - Duration::from_secs(3600);
        crate::platform::set_file_modified_for_test(path, old).expect("set mtime");
    }

    #[test]
    fn stage_in_writes_the_image_into_a_fresh_prefixed_directory_under_the_base() {
        let base = TempBase::new("fresh");

        let staged = stage_in(&base.0, 3, "png", b"img").expect("stage");

        let dir = staged.path.parent().expect("parent");
        assert_eq!(dir.parent(), Some(base.0.as_path()));
        assert!(has_stage_prefix(dir));
        assert_eq!(fs::read(&staged.path).expect("read"), b"img");
        assert_eq!(staged.paste_text, staged.path.to_string_lossy());
    }

    #[test]
    fn stage_in_gives_each_image_its_own_directory() {
        let base = TempBase::new("distinct");

        let first = stage_in(&base.0, 3, "png", b"a").expect("stage");
        let second = stage_in(&base.0, 3, "png", b"b").expect("stage");

        assert_ne!(first.path.parent(), second.path.parent());
    }

    #[cfg(unix)]
    #[test]
    fn stage_in_restricts_the_directory_to_0700_and_the_file_to_0600() {
        use std::os::unix::fs::PermissionsExt;

        let base = TempBase::new("modes");

        let staged = stage_in(&base.0, 3, "png", b"img").expect("stage");

        let mode = |path: &Path| fs::metadata(path).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode(&staged.path), 0o600);
        assert_eq!(mode(staged.path.parent().expect("parent")), 0o700);
    }

    #[cfg(unix)]
    #[test]
    fn stage_in_leaves_a_symlink_at_the_old_per_uid_path_untouched() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let base = TempBase::new("old-layout");
        let victim = base.0.join("victim");
        fs::create_dir(&victim).expect("victim");
        fs::set_permissions(&victim, fs::Permissions::from_mode(0o755)).expect("chmod");
        let uid = unsafe { libc::geteuid() };
        symlink(
            &victim,
            base.0.join(format!("herdr-clipboard-images-{uid}")),
        )
        .expect("symlink");

        stage_in(&base.0, 3, "png", b"img").expect("stage");

        let mode = fs::metadata(&victim).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o755);
        assert_eq!(fs::read_dir(&victim).expect("read_dir").count(), 0);
    }

    #[test]
    fn cleanup_stale_removes_old_staging_directories_and_keeps_fresh_ones() {
        let base = TempBase::new("cleanup");
        let old = stage_in(&base.0, 1, "png", b"old").expect("stage");
        let old_dir = old.path.parent().expect("parent").to_path_buf();
        let fresh = stage_in(&base.0, 2, "png", b"fresh").expect("stage");
        let unrelated = base.0.join("unrelated");
        fs::create_dir(&unrelated).expect("unrelated");

        // Stage all images before aging: stage_in() also runs cleanup_stale().
        make_old(&old.path);
        make_old(&old_dir);
        make_old(&unrelated);

        assert!(old_dir.is_dir());
        assert_eq!(fs::read(&old.path).expect("read old image"), b"old");
        assert!(fresh.path.is_file());
        assert!(unrelated.is_dir());

        cleanup_stale(&base.0);

        assert!(!old_dir.exists());
        assert!(fresh.path.exists());
        assert!(unrelated.exists());
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_stale_skips_a_prefixed_symlink_and_its_target() {
        let base = TempBase::new("cleanup-link");
        let target = base.0.join("target");
        fs::create_dir(&target).expect("target");
        let victim_file = target.join("old.png");
        fs::write(&victim_file, b"x").expect("write");
        make_old(&victim_file);
        make_old(&target);
        std::os::unix::fs::symlink(&target, base.0.join("herdr-clipboard-image-link"))
            .expect("symlink");

        cleanup_stale(&base.0);

        assert!(victim_file.exists());
    }

    #[test]
    fn remove_files_removes_the_file_and_its_prefixed_directory() {
        let base = TempBase::new("remove");
        let staged = stage_in(&base.0, 1, "png", b"img").expect("stage");
        let dir = staged.path.parent().expect("parent").to_path_buf();

        remove_files(vec![staged.path.clone()]);

        assert!(!staged.path.exists());
        assert!(!dir.exists());
    }

    #[test]
    fn remove_files_keeps_a_parent_directory_without_the_staging_prefix() {
        let base = TempBase::new("remove-other");
        let dir = base.0.join("other");
        fs::create_dir(&dir).expect("dir");
        let file = dir.join("a.png");
        fs::write(&file, b"x").expect("write");

        remove_files(vec![file.clone()]);

        assert!(!file.exists());
        assert!(dir.exists());
    }

    fn container(id: &str, user: Option<&str>) -> OffHostTarget {
        OffHostTarget::Container {
            id: id.into(),
            user: user.map(str::to_owned),
            ssh: None,
        }
    }

    fn remote_container(
        host: &str,
        control_path: &str,
        id: &str,
        user: Option<&str>,
    ) -> OffHostTarget {
        OffHostTarget::Container {
            id: id.into(),
            user: user.map(str::to_owned),
            ssh: Some(RemoteDockerHost {
                host: host.into(),
                control_path: control_path.into(),
            }),
        }
    }

    fn ssh(host: &str, control_path: &str) -> OffHostTarget {
        OffHostTarget::Ssh {
            host: host.into(),
            control_path: control_path.into(),
        }
    }

    fn file(target: &OffHostTarget, path: &str) -> OffHostClipboardFile {
        OffHostClipboardFile {
            target: target.clone(),
            path: path.into(),
        }
    }

    #[test]
    fn off_host_cleanup_batches_one_run_per_container_user_and_ssh_master() {
        let a = container("a", None);
        let b = container("b", Some("node"));
        let a_node = container("a", Some("node"));
        let prod = ssh("prod", "/tmp/hssh-1/x-%C");
        let deploy_prod = ssh("deploy@prod", "/tmp/hssh-1/x-%C");
        let prod_other_master = ssh("prod", "/tmp/hssh-1/y-%C");
        let batches = off_host_cleanup_batches(vec![
            file(&a, "/tmp/1.png"),
            file(&prod, "/tmp/2.png"),
            file(&b, "/tmp/3.png"),
            file(&a, "/tmp/4.png"),
            file(&a_node, "/tmp/5.png"),
            file(&deploy_prod, "/tmp/6.png"),
            file(&prod, "/tmp/7.png"),
            file(&prod_other_master, "/tmp/8.png"),
        ]);
        let batch = |target: &OffHostTarget, paths: &[&str]| OffHostCleanupBatch {
            target: target.clone(),
            paths: paths.iter().map(|path| (*path).to_owned()).collect(),
        };
        assert_eq!(
            batches,
            vec![
                batch(&a, &["/tmp/1.png", "/tmp/4.png"]),
                batch(&prod, &["/tmp/2.png", "/tmp/7.png"]),
                batch(&b, &["/tmp/3.png"]),
                batch(&a_node, &["/tmp/5.png"]),
                batch(&deploy_prod, &["/tmp/6.png"]),
                batch(&prod_other_master, &["/tmp/8.png"]),
            ]
        );
        assert!(off_host_cleanup_batches(Vec::new()).is_empty());
    }

    #[test]
    fn off_host_cleanup_batches_remote_containers_per_host_master_id_and_user() {
        let local = container("a", None);
        let remote = remote_container("prod", "/tmp/x-%C", "a", None);
        let other_host = remote_container("staging", "/tmp/z-%C", "a", None);
        let other_master = remote_container("prod", "/tmp/y-%C", "a", None);
        let other_user = remote_container("prod", "/tmp/x-%C", "a", Some("node"));
        let other_id = remote_container("prod", "/tmp/x-%C", "b", None);
        let batches = off_host_cleanup_batches(vec![
            file(&remote, "/tmp/1.png"),
            file(&local, "/tmp/2.png"),
            file(&other_host, "/tmp/3.png"),
            file(&other_master, "/tmp/4.png"),
            file(&other_user, "/tmp/5.png"),
            file(&other_id, "/tmp/6.png"),
            file(&remote, "/tmp/7.png"),
        ]);
        let targets: Vec<_> = batches
            .iter()
            .map(|batch| (batch.target.clone(), batch.paths.clone()))
            .collect();
        let owned = |paths: &[&str]| paths.iter().map(|p| (*p).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            targets,
            vec![
                (remote, owned(&["/tmp/1.png", "/tmp/7.png"])),
                (local, owned(&["/tmp/2.png"])),
                (other_host, owned(&["/tmp/3.png"])),
                (other_master, owned(&["/tmp/4.png"])),
                (other_user, owned(&["/tmp/5.png"])),
                (other_id, owned(&["/tmp/6.png"])),
            ]
        );
    }

    #[test]
    fn failure_detail_names_where_the_image_was_going() {
        assert_eq!(
            container("a", None).failure_detail("boom"),
            "Image could not be copied into the container: boom"
        );
        assert_eq!(
            remote_container("prod", "/tmp/c", "a", None).failure_detail("boom"),
            "Image could not be copied into the container on the ssh host: boom"
        );
        assert_eq!(
            ssh("prod", "/tmp/c").failure_detail("boom"),
            "Image could not be copied to the ssh host: boom"
        );
    }
}
