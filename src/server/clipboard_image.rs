use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
    /// The pane shell runs inside this container, as `user` when set.
    Container { id: String, user: Option<String> },
    /// The pane shell is an ssh login to `host` whose master listens on
    /// `control_path`.
    Ssh { host: String, control_path: PathBuf },
}

impl OffHostTarget {
    /// The client-facing reason a stage to this target failed.
    pub(crate) fn failure_detail(&self, err: &str) -> String {
        match self {
            Self::Container { .. } => {
                format!("Image could not be copied into the container: {err}")
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
    let dir = ensure_staging_dir()?;
    cleanup_stale(&dir);

    let unique = unique_stamp();

    for attempt in 0..100 {
        let path = dir.join(staged_file_name(client_id, extension, unique, attempt));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        restrict_file_options(&mut options);
        let mut file = match options.open(&path) {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        };
        file.write_all(data)?;
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

pub(crate) fn remove_files(paths: Vec<PathBuf>) {
    for path in paths {
        let _ = fs::remove_file(path);
    }
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
        OffHostTarget::Container { id, user } => {
            crate::container::clipboard::stage_image(id, user.as_deref(), &file_name, data)
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
        OffHostTarget::Container { id, user } => {
            crate::container::clipboard::remove_images(id, user.as_deref(), &batch.paths)
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

fn staging_dir() -> PathBuf {
    #[cfg(unix)]
    let user_id = unsafe { libc::geteuid() };
    #[cfg(windows)]
    let user_id = std::process::id();
    std::env::temp_dir().join(format!("herdr-clipboard-images-{user_id}"))
}

fn ensure_staging_dir() -> io::Result<PathBuf> {
    let dir = staging_dir();
    fs::create_dir_all(&dir)?;
    let metadata = fs::metadata(&dir)?;
    if !metadata.is_dir() {
        return Err(io::Error::other(format!(
            "clipboard image staging path is not a directory: {}",
            dir.display()
        )));
    }
    restrict_dir_permissions(&dir)?;
    Ok(dir)
}

#[cfg(unix)]
fn restrict_file_options(options: &mut fs::OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;

    options.mode(0o600);
}

#[cfg(windows)]
fn restrict_file_options(_options: &mut fs::OpenOptions) {}

#[cfg(unix)]
fn restrict_dir_permissions(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
}

#[cfg(windows)]
fn restrict_dir_permissions(_dir: &Path) -> io::Result<()> {
    Ok(())
}

fn cleanup_stale(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        if modified.elapsed().unwrap_or_default() > STAGED_CLIPBOARD_IMAGE_MAX_AGE {
            let _ = fs::remove_file(path);
        }
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

    fn container(id: &str, user: Option<&str>) -> OffHostTarget {
        OffHostTarget::Container {
            id: id.into(),
            user: user.map(str::to_owned),
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
    fn failure_detail_names_where_the_image_was_going() {
        assert_eq!(
            container("a", None).failure_detail("boom"),
            "Image could not be copied into the container: boom"
        );
        assert_eq!(
            ssh("prod", "/tmp/c").failure_detail("boom"),
            "Image could not be copied to the ssh host: boom"
        );
    }
}
