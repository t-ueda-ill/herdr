//! Uploads into the server's own filesystem (spec Q13): the same stage and
//! publish rules as the remote receiver, with `std::fs` instead of a process.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::api::schema::PaneUploadCollision;

use super::path::Failure;
use super::session::{DirectoryAttrs, EntryStart};
use super::sink::{
    is_kept_stage, kept_stage_failure, numbered_name, upload_failed, Began, Committed, UploadSink,
    MAX_RENAME_NUMBER,
};
use super::STAGE_PREFIX;

struct OpenFile {
    file: File,
    path: PathBuf,
    mode: u32,
    mtime: Option<i64>,
}

/// Removes a stage that still holds replaced originals, after the publish
/// has been reported.
type StageCleanup = Box<dyn Fn(PathBuf) + Send + Sync>;

/// Why detached: deleting a large replaced tree on a slow disk can outlast
/// the client's wait for the finish reply, which would report a failure for
/// an upload that is already published. Why not skip the wait for every
/// stage: a stage without originals is cheap, and removing it inline keeps
/// the warning on the reply.
fn remove_in_background(stage: PathBuf) {
    let worker = stage.clone();
    let spawned = std::thread::Builder::new()
        .name("upload-stage-cleanup".into())
        .spawn(move || {
            if let Err(error) = remove_stage(&worker) {
                tracing::warn!(stage = %worker.display(), %error, "could not remove an upload stage");
            }
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "could not start the stage cleanup; removing it inline");
        if let Err(error) = remove_stage(&stage) {
            tracing::warn!(stage = %stage.display(), %error, "could not remove an upload stage");
        }
    }
}

pub(crate) struct HostSink {
    cleanup: StageCleanup,
    requested: PathBuf,
    token: String,
    destination: Option<PathBuf>,
    stage: Option<PathBuf>,
    open: Option<OpenFile>,
    warnings: Vec<String>,
}

/// Refuses a name that would not stay one entry of its directory here.
pub(crate) fn storable(rel: &str, component: &str) -> Result<(), Failure> {
    let mut parts = Path::new(component).components();
    let single = matches!(
        (parts.next(), parts.next()),
        (Some(std::path::Component::Normal(name)), None) if name == std::ffi::OsStr::new(component)
    );
    if single && crate::platform::stores_file_name(component) {
        Ok(())
    } else {
        Err((
            "invalid_params",
            format!("{rel:?} has a name this host cannot store"),
        ))
    }
}

fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

fn to_time(mtime: i64) -> Option<SystemTime> {
    let seconds = u64::try_from(mtime).ok()?;
    UNIX_EPOCH.checked_add(Duration::from_secs(seconds))
}

fn destination_failure(path: &Path, error: &io::Error) -> Failure {
    let shown = path.display();
    match error.kind() {
        io::ErrorKind::NotFound => ("not_found", format!("not found: {shown}")),
        io::ErrorKind::PermissionDenied => {
            ("permission_denied", format!("permission denied: {shown}"))
        }
        _ => upload_failed(format!("cannot use {shown}: {error}")),
    }
}

/// `rename_no_replace`, falling back to check-then-rename where the
/// filesystem lacks an atomic no-replace rename (some network filesystems).
fn publish_rename(from: &Path, to: &Path) -> io::Result<()> {
    match crate::platform::rename_no_replace(from, to) {
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::InvalidInput | io::ErrorKind::Unsupported
            ) =>
        {
            if exists(to) {
                return Err(io::Error::from(io::ErrorKind::AlreadyExists));
            }
            fs::rename(from, to)
        }
        other => other,
    }
}

/// Gives every directory below `root` its owner's write bit so the tree can
/// be removed even when uploaded directories were read-only.
fn make_removable(root: &Path) {
    let Ok(metadata) = fs::symlink_metadata(root) else {
        return;
    };
    if !metadata.is_dir() {
        return;
    }
    let _ = crate::platform::set_permission_bits(root, 0o700);
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten() {
            make_removable(&entry.path());
        }
    }
}

fn remove_stage(stage: &Path) -> io::Result<()> {
    // Why check the name: this deletes recursively, so it must only ever
    // touch a directory this sink created.
    let ours = stage
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(STAGE_PREFIX));
    if !ours {
        return Err(io::Error::other("refusing to remove a non-stage directory"));
    }
    make_removable(stage);
    fs::remove_dir_all(stage)
}

impl HostSink {
    /// `requested` is an absolute directory on this host.
    pub(crate) fn new(requested: PathBuf, token: String) -> Self {
        Self {
            cleanup: Box::new(remove_in_background),
            requested,
            token,
            destination: None,
            stage: None,
            open: None,
            warnings: Vec::new(),
        }
    }

    /// Removes a stage holding originals before publishing returns. Why: a
    /// process that exits right after reporting, as the CLI does, would kill
    /// the detached thread and leave the replaced files behind.
    pub(crate) fn with_inline_cleanup(mut self) -> Self {
        self.cleanup = Box::new(|stage| {
            if let Err(error) = remove_stage(&stage) {
                tracing::warn!(stage = %stage.display(), %error, "could not remove a stage");
            }
        });
        self
    }

    /// Replaces how a stage holding originals is removed after publishing.
    #[cfg(all(test, unix))]
    pub(crate) fn with_cleanup(mut self, cleanup: StageCleanup) -> Self {
        self.cleanup = cleanup;
        self
    }

    fn stage(&self) -> Result<&Path, Failure> {
        self.stage
            .as_deref()
            .ok_or_else(|| upload_failed("the upload has no stage"))
    }

    fn staged(&self, rel: &str) -> Result<PathBuf, Failure> {
        let staged = self.stage()?.join("i");
        let mut path = staged.clone();
        for component in rel.split('/') {
            // Why: on Windows `\` separates and `C:` restarts a path, so the
            // name would escape its entry.
            storable(rel, component)?;
            path.push(component);
        }
        // Why check again: anything `push` could still read as a root or a
        // prefix would put the write outside the stage.
        if !path.starts_with(&staged) || path == staged {
            return Err((
                "invalid_params",
                format!("{rel:?} has a name this host cannot store"),
            ));
        }
        Ok(path)
    }

    /// Whether `rel` already names something in the stage. Why not a plain
    /// `exists` on the caller's side: the stage path is private, and a name this
    /// host cannot store has no staged path at all.
    pub(crate) fn staged_exists(&self, rel: &str) -> bool {
        self.staged(rel).is_ok_and(|path| exists(&path))
    }

    fn apply_attrs(&mut self, path: &Path, shown: &str, mode: u32, mtime: Option<i64>) {
        // Why time first: a mode without read access would stop the open.
        if let Some(time) = mtime.and_then(to_time) {
            if crate::platform::set_path_modified(path, time).is_err() {
                self.warnings
                    .push(format!("could not set the modification time of {shown}"));
            }
        }
        if crate::platform::set_permission_bits(path, mode).is_err() {
            self.warnings
                .push(format!("could not set the permissions of {shown}"));
        }
    }

    fn rollback(
        &self,
        destination: &Path,
        stage: &Path,
        done: &[Published],
    ) -> Result<(), Failure> {
        let mut intact = true;
        for entry in done.iter().rev() {
            if fs::rename(
                destination.join(&entry.published),
                stage.join("i").join(&entry.name),
            )
            .is_err()
            {
                intact = false;
            }
            if let Some(aside) = &entry.aside {
                if publish_rename(aside, &destination.join(&entry.name)).is_err() {
                    intact = false;
                }
            }
        }
        if intact {
            Ok(())
        } else {
            Err(kept_stage_failure(&crate::platform::file_path_display(
                stage,
            )))
        }
    }

    fn free_name(destination: &Path, stage: &Path, name: &str, split: bool) -> Option<String> {
        (1..=MAX_RENAME_NUMBER)
            .map(|number| numbered_name(name, number, split))
            .find(|candidate| {
                !exists(&destination.join(candidate)) && !exists(&stage.join("i").join(candidate))
            })
    }

    /// Moves every staged name into the destination, or none of them.
    fn publish(
        &self,
        collision: PaneUploadCollision,
        names: &[String],
    ) -> Result<Result<Vec<Published>, Vec<String>>, Failure> {
        let destination = self
            .destination
            .clone()
            .ok_or_else(|| upload_failed("the upload has no destination"))?;
        let stage = self.stage()?.to_path_buf();
        if collision == PaneUploadCollision::Fail {
            let conflicts: Vec<String> = names
                .iter()
                .filter(|name| exists(&destination.join(name)))
                .cloned()
                .collect();
            if !conflicts.is_empty() {
                return Ok(Err(conflicts));
            }
        }
        let mut done: Vec<Published> = Vec::new();
        for (index, name) in names.iter().enumerate() {
            let source = stage.join("i").join(name);
            let split = fs::symlink_metadata(&source).is_ok_and(|metadata| metadata.is_file());
            let mut aside = None;
            let mut published = name.clone();
            if exists(&destination.join(name)) {
                match collision {
                    PaneUploadCollision::Rename => {
                        let Some(free) = Self::free_name(&destination, &stage, name, split) else {
                            self.rollback(&destination, &stage, &done)?;
                            return Err(upload_failed(format!("no free numbered name for {name}")));
                        };
                        published = free;
                    }
                    PaneUploadCollision::Replace => {
                        let moved = stage.join("o").join(index.to_string());
                        if let Err(error) = publish_rename(&destination.join(name), &moved) {
                            self.rollback(&destination, &stage, &done)?;
                            return Err(upload_failed(format!("cannot replace {name}: {error}")));
                        }
                        aside = Some(moved);
                    }
                    PaneUploadCollision::Fail | PaneUploadCollision::Unknown => {
                        // Created after the check above.
                        self.rollback(&destination, &stage, &done)?;
                        return Ok(Err(vec![name.clone()]));
                    }
                }
            }
            loop {
                let error = match publish_rename(&source, &destination.join(&published)) {
                    Ok(()) => break,
                    Err(error) => error,
                };
                // Someone created the name after the check: try the next number.
                if error.kind() == io::ErrorKind::AlreadyExists
                    && collision == PaneUploadCollision::Rename
                {
                    if let Some(free) = Self::free_name(&destination, &stage, name, split) {
                        published = free;
                        continue;
                    }
                }
                let restored = aside
                    .as_ref()
                    .is_none_or(|moved| publish_rename(moved, &destination.join(name)).is_ok());
                self.rollback(&destination, &stage, &done)?;
                if !restored {
                    return Err(kept_stage_failure(&crate::platform::file_path_display(
                        &stage,
                    )));
                }
                return Err(upload_failed(format!("cannot publish {name}: {error}")));
            }
            done.push(Published {
                name: name.clone(),
                published,
                aside,
            });
        }
        Ok(Ok(done))
    }
}

/// One name moved into the destination, and where its replaced original went.
struct Published {
    name: String,
    published: String,
    aside: Option<PathBuf>,
}

impl UploadSink for HostSink {
    fn begin(&mut self, names: &[String]) -> Result<Began, Failure> {
        // Before any lookup: a drive-relative name would probe another path.
        for name in names {
            storable(name, name)?;
        }
        let requested = self.requested.clone();
        let metadata =
            fs::metadata(&requested).map_err(|error| destination_failure(&requested, &error))?;
        if !metadata.is_dir() {
            return Err((
                "not_a_directory",
                format!("not a directory: {}", requested.display()),
            ));
        }
        let destination = fs::canonicalize(&requested)
            .map_err(|error| destination_failure(&requested, &error))?;
        super::sink::check_resolved_destination(&destination.to_string_lossy())?;
        let stage = destination.join(format!("{STAGE_PREFIX}{}", self.token));
        fs::create_dir(&stage).map_err(|error| destination_failure(&destination, &error))?;
        self.stage = Some(stage.clone());
        let created = crate::platform::set_permission_bits(&stage, 0o700)
            .and_then(|()| fs::create_dir(stage.join("i")))
            .and_then(|()| fs::create_dir(stage.join("o")));
        if let Err(error) = created {
            self.abort();
            return Err(upload_failed(format!("cannot create the stage: {error}")));
        }
        let conflicts = names
            .iter()
            .filter(|name| exists(&destination.join(name)))
            .cloned()
            .collect();
        let shown = crate::platform::file_path_display(&destination);
        self.destination = Some(destination);
        Ok(Began {
            destination: shown,
            conflicts,
        })
    }

    fn start(&mut self, entry: &EntryStart) -> Result<(), Failure> {
        match entry {
            EntryStart::Directory { path } => {
                let target = self.staged(path.as_str())?;
                fs::create_dir(&target).map_err(|error| {
                    upload_failed(format!("cannot create {}: {error}", path.as_str()))
                })?;
                let _ = crate::platform::set_permission_bits(&target, 0o700);
            }
            EntryStart::Symlink { path, target } => {
                let link = self.staged(path.as_str())?;
                if let Err(error) = crate::platform::create_symlink(target, &link) {
                    if cfg!(windows) {
                        self.warnings
                            .push(format!("skipped the symlink {}: {error}", path.as_str()));
                    } else {
                        return Err(upload_failed(format!(
                            "cannot create the symlink {}: {error}",
                            path.as_str()
                        )));
                    }
                }
            }
            EntryStart::File {
                path, mode, mtime, ..
            } => {
                let target = self.staged(path.as_str())?;
                let file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&target)
                    .map_err(|error| {
                        upload_failed(format!("cannot create {}: {error}", path.as_str()))
                    })?;
                self.open = Some(OpenFile {
                    file,
                    path: target,
                    mode: *mode,
                    mtime: *mtime,
                });
            }
        }
        Ok(())
    }

    fn data(&mut self, bytes: &[u8]) -> Result<(), Failure> {
        let open = self
            .open
            .as_mut()
            .ok_or_else(|| upload_failed("no file is open"))?;
        open.file
            .write_all(bytes)
            .map_err(|error| upload_failed(format!("write failed: {error}")))
    }

    fn end_file(&mut self) -> Result<(), Failure> {
        let open = self
            .open
            .take()
            .ok_or_else(|| upload_failed("no file is open"))?;
        if let Some(time) = open.mtime.and_then(to_time) {
            if open.file.set_modified(time).is_err() {
                self.warnings.push(format!(
                    "could not set the modification time of {}",
                    open.path.display()
                ));
            }
        }
        open.file
            .sync_all()
            .map_err(|error| upload_failed(format!("write failed: {error}")))?;
        drop(open.file);
        crate::platform::set_permission_bits(&open.path, open.mode)
            .map_err(|error| upload_failed(format!("cannot set permissions: {error}")))
    }

    fn commit(
        &mut self,
        collision: PaneUploadCollision,
        nested: &[DirectoryAttrs],
        top: &[DirectoryAttrs],
    ) -> Result<Committed, Failure> {
        if collision == PaneUploadCollision::Unknown {
            return Err(("invalid_params", "unknown collision policy".into()));
        }
        for dir in nested {
            let path = self.staged(dir.path.as_str())?;
            self.apply_attrs(&path, dir.path.as_str(), dir.mode, dir.mtime);
        }
        let names: Vec<String> = match self.stage().map(|stage| stage.join("i")) {
            Ok(staged) => {
                let mut names: Vec<String> = fs::read_dir(&staged)
                    .map_err(|error| upload_failed(format!("cannot read the stage: {error}")))?
                    .flatten()
                    .filter_map(|entry| entry.file_name().into_string().ok())
                    .collect();
                names.sort();
                names
            }
            Err(failure) => return Err(failure),
        };
        let done = match self.publish(collision, &names) {
            Ok(Ok(done)) => done,
            Ok(Err(conflicts)) => return Ok(Committed::Conflict(conflicts)),
            Err(failure) => {
                // A kept stage holds originals; only a clean rollback removes it.
                if is_kept_stage(&failure) {
                    self.stage = None;
                } else {
                    self.abort();
                }
                return Err(failure);
            }
        };
        let destination = self.destination.clone().unwrap_or_default();
        for dir in top {
            if let Some(entry) = done.iter().find(|entry| entry.name == dir.path.as_str()) {
                let path = destination.join(&entry.published);
                self.apply_attrs(&path, &entry.published, dir.mode, dir.mtime);
            }
        }
        if let Some(stage) = self.stage.take() {
            let holds_originals =
                fs::read_dir(stage.join("o")).is_ok_and(|mut originals| originals.next().is_some());
            if holds_originals {
                (self.cleanup)(stage);
            } else if let Err(error) = remove_stage(&stage) {
                self.warnings
                    .push(format!("could not remove {}: {error}", stage.display()));
            }
        }
        Ok(Committed::Published {
            paths: done
                .iter()
                .map(|entry| {
                    (
                        entry.name.clone(),
                        crate::platform::file_path_display(&destination.join(&entry.published)),
                    )
                })
                .collect(),
            warnings: std::mem::take(&mut self.warnings),
        })
    }

    fn abort(&mut self) {
        self.open = None;
        if let Some(stage) = self.stage.take() {
            if let Err(error) = remove_stage(&stage) {
                tracing::warn!(stage = %stage.display(), %error, "could not remove an upload stage");
            }
        }
    }
}

impl Drop for HostSink {
    fn drop(&mut self) {
        self.abort();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::upload::sink_tests;
    use crate::upload::test_support::TempDir;

    fn factory(dest: &str) -> Box<dyn UploadSink> {
        // Inline cleanup: the contract tests look for the stage right after commit.
        Box::new(
            HostSink::new(PathBuf::from(dest), crate::upload::stage_token()).with_cleanup(
                Box::new(|stage| {
                    let _ = remove_stage(&stage);
                }),
            ),
        )
    }

    #[test]
    fn a_replace_reports_published_before_its_originals_are_removed() {
        let dest = TempDir::new("host-slow-cleanup");
        std::fs::create_dir(dest.join("d")).unwrap();
        std::fs::write(dest.join("d/old"), b"old").unwrap();
        let (held, release) = std::sync::mpsc::channel::<PathBuf>();
        let held = std::sync::Mutex::new(held);
        let mut sink = HostSink::new(dest.path().to_path_buf(), crate::upload::stage_token())
            .with_cleanup(Box::new(move |stage| {
                // A slow disk: the stage waits until the test lets it go.
                let _ = held.lock().unwrap().send(stage);
            }));
        sink.begin(&sink_tests::names(&["d"])).unwrap();
        sink_tests::dir(&mut sink, "d");
        sink_tests::file(&mut sink, "d/new", b"new", 0o644, None);

        let committed = sink.commit(PaneUploadCollision::Replace, &[], &[]).unwrap();

        assert_eq!(sink_tests::published(committed).len(), 1);
        assert!(dest.join("d/new").exists() && !dest.join("d/old").exists());
        let stage = release.try_recv().expect("cleanup was handed the stage");
        assert!(stage.join("o").exists(), "originals wait for the cleanup");
        remove_stage(&stage).unwrap();
        assert!(sink_tests::stages(dest.path()).is_empty());
    }

    sink_tests::sink_contract!(factory);

    #[test]
    fn a_relative_or_missing_destination_is_reported_before_any_stage_exists() {
        let dir = TempDir::new("host-missing");
        let mut sink = HostSink::new(dir.join("missing"), crate::upload::stage_token());

        let error = sink.begin(&["a".into()]).unwrap_err();

        assert_eq!(error.0, "not_found");
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(label: &str) -> Scratch {
        let path =
            std::env::temp_dir().join(format!("herdr-upload-win-{}-{label}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Scratch(path)
    }

    #[test]
    fn a_drive_prefixed_name_is_refused_before_it_can_leave_the_stage() {
        let dest = scratch("drive-name");
        let mut top = HostSink::new(dest.0.clone(), crate::upload::stage_token());
        let mut nested = HostSink::new(dest.0.clone(), crate::upload::stage_token());

        let refused = top.begin(&["C:x".into()]).unwrap_err();
        nested.begin(&["top".into()]).unwrap();
        nested
            .start(&EntryStart::Directory {
                path: crate::upload::path::RelPath::test_new("top"),
            })
            .unwrap();
        let nested_refused = nested
            .start(&EntryStart::File {
                path: crate::upload::path::RelPath::test_new("top/C:x"),
                size: 1,
                mode: 0o644,
                mtime: None,
            })
            .unwrap_err();

        assert_eq!(refused.0, "invalid_params");
        assert_eq!(nested_refused.0, "invalid_params");
        assert!(nested_refused.1.contains("cannot store"));
    }
}
