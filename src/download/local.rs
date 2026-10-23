//! Writes a download's records into a folder on this machine: the client side of
//! `pane.download.*`, shared by the CLI and the TUI. The staging, publishing,
//! numbering and rollback are the upload host sink's; this adds what a stream
//! from the other side needs on top: order checks on every record, a set of
//! trees this machine cannot take, and symlinks that wait until the end.

use std::collections::HashSet;
use std::path::PathBuf;

use base64::Engine as _;

use crate::api::schema::{PaneDownloadRecord, PaneUploadCollision};
use crate::upload::path::{
    Failure, MAX_NAMES, MAX_NAMES_TOTAL_BYTES, MAX_NAME_BYTES, MAX_PATH_BYTES,
};
use crate::upload::session::{
    Declared, DirectoryAttrs, EntryStart, Session, WritePlan, WriteRequest,
};
use crate::upload::sink::{Committed, UploadSink};
use crate::upload::sink_host::{storable, HostSink};

use super::{notice_text, MAX_DOWNLOAD_BYTES, MAX_DOWNLOAD_ENTRIES};

/// Names, and everything below them, that this machine does not take.
#[derive(Debug, Default)]
pub(crate) struct SkipSet {
    roots: HashSet<String>,
}

impl SkipSet {
    pub(crate) fn insert(&mut self, rel: &str) {
        self.roots.insert(rel.to_owned());
    }

    /// Whether `rel` is a skipped root or below one. Prefixes end at a `/`, so
    /// skipping `a` keeps `ab/x`.
    pub(crate) fn covers(&self, rel: &str) -> bool {
        if self.roots.is_empty() {
            return false;
        }
        if self.roots.contains(rel) {
            return true;
        }
        rel.match_indices('/')
            .any(|(at, _)| self.roots.contains(&rel[..at]))
    }
}

/// Answers whether a name is already taken in the stage. Why a seam: whether
/// two names that differ only in case or normalisation are one depends on the
/// filesystem, and a test on Linux has to be able to make them one.
pub(crate) trait StagedNames: Send {
    /// `rel` was just created in the stage.
    fn started(&mut self, rel: &str);
    fn exists(&self, sink: &HostSink, rel: &str) -> bool;
}

struct Filesystem;

impl StagedNames for Filesystem {
    fn started(&mut self, _rel: &str) {}

    fn exists(&self, sink: &HostSink, rel: &str) -> bool {
        sink.staged_exists(rel)
    }
}

/// An upload failure said in download words, so the other side's vocabulary
/// does not reach the person reading it. `from_session`: the upload session
/// only objects to a stream that breaks its rules, which is the server's fault.
fn as_download_failure(failure: Failure, from_session: bool) -> Failure {
    match failure.0 {
        "upload_too_large" => (
            "download_too_large",
            format!(
                "downloads are limited to {MAX_DOWNLOAD_ENTRIES} entries and {MAX_DOWNLOAD_BYTES} bytes"
            ),
        ),
        "upload_failed" => (
            "download_failed",
            failure
                .1
                .replace("the upload has no", "the download has no"),
        ),
        "invalid_params" if from_session => ("download_failed", failure.1),
        _ => failure,
    }
}

fn session_failure(failure: Failure) -> Failure {
    as_download_failure(failure, true)
}

fn sink_failure(failure: Failure) -> Failure {
    as_download_failure(failure, false)
}

fn download_failed(message: impl Into<String>) -> Failure {
    ("download_failed", message.into())
}

pub(crate) struct LocalWriter {
    session: Session,
    sink: HostSink,
    skipped: SkipSet,
    /// Waiting for the end. Why not created as they arrive: on a filesystem
    /// that folds case, a link `A` made early would be followed by a later
    /// write to `a/x` and land outside the stage.
    links: Vec<EntryStart>,
    warnings: Vec<String>,
    names: Box<dyn StagedNames>,
    /// Whether the open file went to the sink; a skipped file's data does not.
    file_open: bool,
}

impl LocalWriter {
    /// Creates the stage inside `destination`. Returns the names it will keep:
    /// those this machine can store. The server's declared limits are the
    /// download limits, since the real count is only known once the stream ran.
    pub(crate) fn open(
        destination: PathBuf,
        names: &[String],
    ) -> Result<(Self, Vec<String>), Failure> {
        Self::open_with(destination, names, false)
    }

    /// As `open`, but replaced originals are gone when `publish` returns, for
    /// a process that exits right after.
    pub(crate) fn open_blocking(
        destination: PathBuf,
        names: &[String],
    ) -> Result<(Self, Vec<String>), Failure> {
        Self::open_with(destination, names, true)
    }

    fn open_with(
        destination: PathBuf,
        names: &[String],
        inline_cleanup: bool,
    ) -> Result<(Self, Vec<String>), Failure> {
        let session = Session::new(Declared {
            names: names.to_vec(),
            entries: MAX_DOWNLOAD_ENTRIES,
            bytes: MAX_DOWNLOAD_BYTES,
        })
        .map_err(session_failure)?;
        let mut skipped = SkipSet::default();
        let mut warnings = Vec::new();
        let mut kept = Vec::new();
        for name in names {
            if storable(name, name).is_ok() {
                kept.push(name.clone());
            } else {
                skipped.insert(name);
                warnings.push(format!(
                    "skipped {}: this machine cannot store that name",
                    notice_text(name)
                ));
            }
        }
        if kept.is_empty() {
            return Err(download_failed(
                "none of the selected names can be stored on this machine",
            ));
        }
        let mut sink = HostSink::new(destination, crate::upload::stage_token());
        if inline_cleanup {
            sink = sink.with_inline_cleanup();
        }
        sink.begin(&kept).map_err(sink_failure)?;
        Ok((
            Self {
                session,
                sink,
                skipped,
                links: Vec::new(),
                warnings,
                names: Box::new(Filesystem),
                file_open: false,
            },
            kept,
        ))
    }

    #[cfg(all(test, unix))]
    pub(crate) fn with_staged_names(mut self, names: Box<dyn StagedNames>) -> Self {
        self.names = names;
        self
    }

    /// Takes one record of the stream. A failure is final: the caller aborts.
    pub(crate) fn apply(&mut self, record: &PaneDownloadRecord) -> Result<(), Failure> {
        let data = base64::engine::general_purpose::STANDARD
            .decode(record.data.as_bytes())
            .map_err(|error| download_failed(format!("data is not base64: {error}")))?;
        // Why every record, skipped or not: the session wants a file's last
        // record before the next entry, a parent before its children, and no rel
        // twice. Hiding skipped records from it would let a server that repeats
        // itself pass, and break the next entry.
        let plan = self
            .session
            .accept(WriteRequest {
                path: &record.path,
                entry: record.entry.as_ref(),
                offset: record.offset,
                data_len: data.len(),
                last: record.last,
            })
            .map_err(session_failure)?;
        if self.skipped.covers(&record.path) {
            return Ok(());
        }
        self.write(record, plan, &data)
    }

    fn write(
        &mut self,
        record: &PaneDownloadRecord,
        plan: WritePlan,
        data: &[u8],
    ) -> Result<(), Failure> {
        if let Some(start) = &plan.start {
            if let Some(reason) = self.refusal(&record.path, start)? {
                self.skipped.insert(&record.path);
                self.warnings
                    .push(format!("skipped {}: {reason}", notice_text(&record.path)));
                return Ok(());
            }
            match start {
                EntryStart::Symlink { .. } => self.links.push(start.clone()),
                _ => {
                    self.sink.start(start).map_err(sink_failure)?;
                    self.names.started(&record.path);
                    self.file_open = matches!(start, EntryStart::File { .. });
                }
            }
        }
        if self.file_open {
            if !data.is_empty() {
                self.sink
                    .data(data)
                    .map_err(|failure| saving_failure(&record.path, failure))?;
            }
            if plan.end_file {
                self.file_open = false;
                self.sink
                    .end_file()
                    .map_err(|failure| saving_failure(&record.path, failure))?;
            }
        }
        Ok(())
    }

    /// Why this entry cannot be stored here, or `None`. A top-level name that
    /// collides cannot be skipped: the session already counts it as declared.
    fn refusal(&self, rel: &str, start: &EntryStart) -> Result<Option<&'static str>, Failure> {
        let name = rel.rsplit('/').next().unwrap_or(rel);
        if storable(rel, name).is_err() {
            return Ok(Some("this machine cannot store that name"));
        }
        if matches!(start, EntryStart::Symlink { .. }) || !self.names.exists(&self.sink, rel) {
            return Ok(None);
        }
        if !rel.contains('/') {
            return Err(download_failed(format!(
                "{} would get the same name on this machine as another selected item",
                notice_text(rel)
            )));
        }
        Ok(Some("it collides with another name on this filesystem"))
    }

    /// Creates the links, then publishes everything at once, renaming
    /// collisions as `name (1).ext` unless told otherwise. A failure leaves no
    /// stage behind unless its message says where one was kept.
    pub(crate) fn publish(mut self, collision: PaneUploadCollision) -> Result<Committed, Failure> {
        self.session.check_complete().map_err(session_failure)?;
        for link in std::mem::take(&mut self.links) {
            let EntryStart::Symlink { path, .. } = &link else {
                continue;
            };
            let rel = path.as_str().to_owned();
            // Why check again: a file or folder written after the link may have
            // taken its name on a filesystem that folds case.
            if self.names.exists(&self.sink, &rel) {
                self.warnings.push(format!(
                    "skipped the symlink {}: it collides with another name on this filesystem",
                    notice_text(&rel)
                ));
                continue;
            }
            self.sink.start(&link).map_err(sink_failure)?;
            self.names.started(&rel);
        }
        let outside = |dirs: Vec<DirectoryAttrs>| -> Vec<DirectoryAttrs> {
            dirs.into_iter()
                .filter(|dir| !self.skipped.covers(dir.path.as_str()))
                .collect()
        };
        let nested = outside(self.session.nested_directories());
        let top = outside(self.session.top_directories());
        let committed = self
            .sink
            .commit(collision, &nested, &top)
            .map_err(sink_failure)?;
        match committed {
            Committed::Published { paths, warnings } => {
                let mut all = std::mem::take(&mut self.warnings);
                all.extend(warnings);
                Ok(Committed::Published {
                    paths,
                    warnings: all,
                })
            }
            conflict => {
                // Why abort here: the sink keeps the stage for another commit,
                // and nothing will make one.
                self.sink.abort();
                Ok(conflict)
            }
        }
    }

    /// Removes the stage.
    pub(crate) fn abort(mut self) {
        self.sink.abort();
    }
}

/// A failed write or close, naming what was being saved.
fn saving_failure(rel: &str, failure: Failure) -> Failure {
    let reason = failure
        .1
        .strip_prefix("write failed: ")
        .unwrap_or(&failure.1);
    (
        "download_failed",
        format!("cannot save {}: {reason}", notice_text(rel)),
    )
}

/// A path on a Windows drive or share, which `\` also separates.
fn windows_style(path: &str) -> bool {
    let bytes = path.as_bytes();
    path.starts_with("\\\\")
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && bytes[2] == b'\\')
}

/// The last component of a path on the other side.
fn last_component(path: &str) -> &str {
    let cut = if windows_style(path) {
        path.rfind(['/', '\\'])
    } else {
        path.rfind('/')
    };
    cut.map_or(path, |at| &path[at + 1..])
}

/// Why this selection cannot be sent, so a caller refuses before `begin`.
/// Why fold case on every OS: whether the destination tells `Makefile` from
/// `makefile` depends on its filesystem and there is no way to ask before the
/// download starts. Selecting both is rare, so refusing costs little.
pub(crate) fn check_selection(paths: &[String]) -> Result<(), String> {
    if paths.is_empty() || paths.len() > MAX_NAMES {
        return Err(format!("select 1 to {MAX_NAMES} items"));
    }
    let mut total = 0;
    let mut seen: Vec<(String, &str)> = Vec::new();
    for path in paths {
        if path.len() > MAX_PATH_BYTES {
            return Err(format!("a path is longer than {MAX_PATH_BYTES} bytes"));
        }
        total += path.len();
        if !path.starts_with('/') && !windows_style(path) {
            return Err(format!("not an absolute path: {}", notice_text(path)));
        }
        let name = last_component(path);
        if name.is_empty() || name.len() > MAX_NAME_BYTES {
            return Err(format!("cannot download {}", notice_text(path)));
        }
        let folded = name.to_lowercase();
        if let Some((_, first)) = seen.iter().find(|(known, _)| *known == folded) {
            return Err(format!(
                "two selected items would get the same name on this machine: {}, {}",
                notice_text(first),
                notice_text(name)
            ));
        }
        seen.push((folded, name));
    }
    if total > MAX_NAMES_TOTAL_BYTES {
        return Err(format!(
            "the paths may be at most {MAX_NAMES_TOTAL_BYTES} bytes together"
        ));
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests;

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(label: &str) -> Scratch {
        let path =
            std::env::temp_dir().join(format!("herdr-download-win-{}-{label}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Scratch(path)
    }

    #[test]
    fn a_name_this_machine_cannot_store_is_left_out_and_the_rest_is_kept() {
        let dest = scratch("unstorable");

        let (_, kept) =
            LocalWriter::open(dest.0.clone(), &["a:b".to_owned(), "ok".to_owned()]).unwrap();

        assert_eq!(kept, ["ok"]);
    }

    #[test]
    fn a_selection_where_no_name_can_be_stored_fails() {
        let dest = scratch("none-storable");

        let failure = LocalWriter::open(dest.0.clone(), &["a:b".to_owned()])
            .err()
            .unwrap();

        assert_eq!(failure.0, "download_failed");
        assert!(failure.1.contains("none of the selected names"));
    }

    /// Why this exists: the sink resolves the destination with
    /// `fs::canonicalize`, which gives `\\?\` paths on Windows, and the design
    /// expects that to lift the 260-character limit. Nobody had run it.
    #[test]
    fn a_path_longer_than_260_characters_is_published() {
        use crate::api::schema::{PaneUploadEntryHeader, PaneUploadEntryKind};

        let dest = scratch("long-path");
        let (mut writer, _) = LocalWriter::open(dest.0.clone(), &["t".to_owned()]).unwrap();
        let header = |kind, size| PaneUploadEntryHeader {
            kind,
            mode: 0o644,
            mtime: None,
            size,
            link_target: None,
        };
        let mut rel = "t".to_owned();
        let mut records = vec![PaneDownloadRecord {
            path: rel.clone(),
            entry: Some(header(PaneUploadEntryKind::Directory, None)),
            offset: 0,
            data: String::new(),
            last: true,
        }];
        for level in 0..3 {
            rel = format!("{rel}/{}{level}", "d".repeat(99));
            records.push(PaneDownloadRecord {
                path: rel.clone(),
                entry: Some(header(PaneUploadEntryKind::Directory, None)),
                offset: 0,
                data: String::new(),
                last: true,
            });
        }
        rel = format!("{rel}/{}.txt", "f".repeat(60));
        records.push(PaneDownloadRecord {
            path: rel.clone(),
            entry: Some(header(PaneUploadEntryKind::File, Some(2))),
            offset: 0,
            data: base64::engine::general_purpose::STANDARD.encode(b"hi"),
            last: true,
        });
        for record in &records {
            writer.apply(record).unwrap();
        }

        let committed = writer.publish(PaneUploadCollision::Rename).unwrap();

        let Committed::Published { paths, .. } = committed else {
            panic!("{committed:?}");
        };
        let root = std::path::PathBuf::from(&paths[0].1);
        let file = rel
            .split('/')
            .fold(root.parent().unwrap().to_path_buf(), |dir, part| {
                dir.join(part)
            });
        assert!(file.as_os_str().len() > 300);
        assert_eq!(std::fs::read(&file).unwrap(), b"hi");
    }
}
