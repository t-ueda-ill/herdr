//! The sending side: turns local paths into the `pane.upload.*` requests that
//! recreate them. Shared by `herdr pane upload` and the TUI's drop handling.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use base64::Engine as _;

use crate::api::schema::{PaneUploadEntryHeader, PaneUploadEntryKind, PaneUploadWriteParams};

use super::path::{validate_link_target, validate_names, MAX_NAMES};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourceEntry {
    /// `/`-separated path inside the upload.
    pub(crate) rel: String,
    pub(crate) local: PathBuf,
    pub(crate) header: PaneUploadEntryHeader,
    /// Which file this was at scan time. Uploads never read it; downloads
    /// compare it with the file they open, to notice a path swapped since.
    pub(crate) identity: Option<crate::platform::FileIdentity>,
}

/// How a walk differs from an upload's. The default is the upload's.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ScanOptions {
    /// A folder that cannot be listed is kept as an empty entry with a
    /// warning, instead of failing the whole walk.
    pub(crate) skip_unreadable_dirs: bool,
    /// A symlink named directly in `paths` is replaced by what it points at
    /// (its entry keeps the link's name). Links met inside folders stay links.
    pub(crate) follow_top_level_symlinks: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Scan {
    pub(crate) names: Vec<String>,
    /// Depth first, every directory before what it holds.
    pub(crate) entries: Vec<SourceEntry>,
    pub(crate) bytes: u64,
    /// FIFOs, sockets, and devices, which cannot be uploaded (spec Q5).
    pub(crate) skipped_special: u64,
    /// Entries left out for reasons worth telling the user.
    pub(crate) warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DuplicateNames {
    /// Two paths with the same last component are an error.
    Reject,
    /// Later paths are sent as `name (1).ext`, like a collision in a terminal
    /// pane drop (spec Q4), so dropping `/a/x` and `/b/x` together works.
    Rename,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ScanError {
    /// The walk passed `max_entries` and stopped.
    TooManyEntries(u64),
    Other(String),
}

impl std::fmt::Display for ScanError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooManyEntries(max) => write!(formatter, "more than {max} entries"),
            Self::Other(message) => formatter.write_str(message),
        }
    }
}

impl From<String> for ScanError {
    fn from(message: String) -> Self {
        Self::Other(message)
    }
}

/// `name (n).ext` with the receiver's rule: only files split an extension,
/// and only at a dot that is not the first character.
fn alternate_name(name: &str, n: u32, split_extension: bool) -> String {
    let dot = split_extension
        .then(|| name.rfind('.'))
        .flatten()
        .filter(|&index| index > 0);
    match dot {
        Some(index) => format!("{} ({n}){}", &name[..index], &name[index..]),
        None => format!("{name} ({n})"),
    }
}

pub(crate) fn mtime(metadata: &fs::Metadata) -> Option<i64> {
    let seconds = metadata
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    i64::try_from(seconds).ok()
}

/// A name the receiver can carry in one header line.
fn usable_name(name: &std::ffi::OsStr, shown: &Path, warnings: &mut Vec<String>) -> Option<String> {
    let Some(name) = name.to_str() else {
        warnings.push(format!(
            "skipped {}: its name is not UTF-8",
            shown.display()
        ));
        return None;
    };
    if name.chars().any(char::is_control) {
        warnings.push(format!(
            "skipped {}: its name has a control character",
            shown.display()
        ));
        return None;
    }
    Some(name.to_owned())
}

struct Walker {
    scan: Scan,
    max_entries: u64,
    options: ScanOptions,
}

impl Walker {
    /// `top`: `local` is one of the paths the caller named.
    fn visit(&mut self, local: &Path, rel: String, top: bool) -> Result<bool, ScanError> {
        let mut metadata = fs::symlink_metadata(local)
            .map_err(|error| format!("cannot read {}: {error}", local.display()))?;
        let mut resolved = None;
        if top && self.options.follow_top_level_symlinks && metadata.file_type().is_symlink() {
            // Why canonicalize rather than only `metadata`: everything below
            // (children, the later `open_regular_nofollow`) needs a path whose
            // last component is no longer a link.
            let target = fs::canonicalize(local)
                .map_err(|error| format!("cannot read {}: {error}", local.display()))?;
            metadata = fs::metadata(&target)
                .map_err(|error| format!("cannot read {}: {error}", local.display()))?;
            resolved = Some(target);
        }
        let local = resolved.as_deref().unwrap_or(local);
        let identity = crate::platform::metadata_identity(&metadata);
        let file_type = metadata.file_type();
        let mode = crate::platform::permission_bits(&metadata);
        let mtime = mtime(&metadata);
        let header = if file_type.is_symlink() {
            let target = fs::read_link(local)
                .map_err(|error| format!("cannot read {}: {error}", local.display()))?;
            let Some(target) = target.to_str().map(str::to_owned) else {
                self.scan.warnings.push(format!(
                    "skipped {}: its link target is not UTF-8",
                    local.display()
                ));
                return Ok(false);
            };
            if validate_link_target(&target).is_err() {
                self.scan.warnings.push(format!(
                    "skipped {}: its link target cannot be sent",
                    local.display()
                ));
                return Ok(false);
            }
            PaneUploadEntryHeader {
                kind: PaneUploadEntryKind::Symlink,
                mode,
                mtime,
                size: None,
                link_target: Some(target),
            }
        } else if file_type.is_dir() {
            PaneUploadEntryHeader {
                kind: PaneUploadEntryKind::Directory,
                mode,
                mtime,
                size: None,
                link_target: None,
            }
        } else if file_type.is_file() {
            self.scan.bytes = self.scan.bytes.saturating_add(metadata.len());
            PaneUploadEntryHeader {
                kind: PaneUploadEntryKind::File,
                mode,
                mtime,
                size: Some(metadata.len()),
                link_target: None,
            }
        } else {
            self.scan.skipped_special += 1;
            return Ok(false);
        };
        if self.scan.entries.len() as u64 >= self.max_entries {
            return Err(ScanError::TooManyEntries(self.max_entries));
        }
        let is_dir = header.kind == PaneUploadEntryKind::Directory;
        self.scan.entries.push(SourceEntry {
            rel: rel.clone(),
            local: local.to_path_buf(),
            header,
            identity,
        });
        if is_dir {
            let listing = match fs::read_dir(local) {
                Ok(listing) => listing,
                Err(error) if self.options.skip_unreadable_dirs => {
                    self.scan
                        .warnings
                        .push(format!("could not list {}: {error}", local.display()));
                    return Ok(true);
                }
                Err(error) => {
                    return Err(format!("cannot list {}: {error}", local.display()).into());
                }
            };
            let mut children: Vec<_> = listing.flatten().collect();
            children.sort_by_key(|child| child.file_name());
            for child in children {
                let path = child.path();
                let Some(name) = usable_name(&child.file_name(), &path, &mut self.scan.warnings)
                else {
                    continue;
                };
                self.visit(&path, format!("{rel}/{name}"), false)?;
            }
        }
        Ok(true)
    }
}

/// Walks `paths` without following symlinks. `max_entries` stops a walk that
/// would be refused anyway before it reads a whole disk.
pub(crate) fn scan(
    paths: &[PathBuf],
    duplicates: DuplicateNames,
    max_entries: u64,
) -> Result<Scan, String> {
    scan_limited(paths, duplicates, max_entries).map_err(|error| error.to_string())
}

/// [`scan`] that tells a walk stopped by `max_entries` apart from other errors.
pub(crate) fn scan_limited(
    paths: &[PathBuf],
    duplicates: DuplicateNames,
    max_entries: u64,
) -> Result<Scan, ScanError> {
    scan_limited_with(paths, duplicates, max_entries, ScanOptions::default())
}

/// [`scan_limited`] with the walk's behaviour chosen by `options`.
pub(crate) fn scan_limited_with(
    paths: &[PathBuf],
    duplicates: DuplicateNames,
    max_entries: u64,
    options: ScanOptions,
) -> Result<Scan, ScanError> {
    if paths.len() > MAX_NAMES {
        return Err(format!("at most {MAX_NAMES} paths can be uploaded at once").into());
    }
    let mut walker = Walker {
        scan: Scan::default(),
        max_entries,
        options,
    };
    let mut taken = HashSet::new();
    for path in paths {
        let Some(file_name) = path.file_name() else {
            return Err(format!("{} has no name to upload as", path.display()).into());
        };
        let Some(mut name) = usable_name(file_name, path, &mut walker.scan.warnings) else {
            continue;
        };
        if taken.contains(&name) {
            match duplicates {
                DuplicateNames::Reject => {
                    return Err(format!("two paths are both named {name}").into());
                }
                DuplicateNames::Rename => {
                    let is_file = fs::symlink_metadata(path).is_ok_and(|meta| meta.is_file());
                    let base = name.clone();
                    let mut n = 1;
                    while taken.contains(&name) {
                        name = alternate_name(&base, n, is_file);
                        n += 1;
                    }
                }
            }
        }
        if walker.visit(path, name.clone(), true)? {
            taken.insert(name.clone());
            walker.scan.names.push(name);
        }
    }
    if walker.scan.names.is_empty() {
        return Err(ScanError::Other("nothing to upload".into()));
    }
    validate_names(&walker.scan.names).map_err(|(_, message)| ScanError::Other(message))?;
    Ok(walker.scan)
}

/// The write requests for a scan, read from disk lazily one chunk at a time.
pub(crate) struct Writes<'a> {
    scan: &'a Scan,
    upload_id: String,
    chunk_bytes: usize,
    index: usize,
    open: Option<(File, u64)>,
}

impl<'a> Writes<'a> {
    pub(crate) fn new(scan: &'a Scan, upload_id: String, chunk_bytes: usize) -> Self {
        Self {
            scan,
            upload_id,
            chunk_bytes: chunk_bytes.max(1),
            index: 0,
            open: None,
        }
    }

    fn file_chunk(
        &mut self,
        entry: &SourceEntry,
        size: u64,
    ) -> Result<PaneUploadWriteParams, String> {
        let changed = || {
            format!(
                "{} changed while it was being uploaded",
                entry.local.display()
            )
        };
        let first = self.open.is_none();
        if first {
            let file = File::open(&entry.local)
                .map_err(|error| format!("cannot read {}: {error}", entry.local.display()))?;
            self.open = Some((file, 0));
        }
        let Some((file, offset)) = self.open.as_mut() else {
            return Err(changed());
        };
        let start = *offset;
        let want = usize::try_from((size - start).min(self.chunk_bytes as u64)).unwrap_or(0);
        let mut data = vec![0u8; want];
        file.read_exact(&mut data).map_err(|_| changed())?;
        *offset += want as u64;
        let last = *offset == size;
        if last {
            let mut probe = [0u8; 1];
            if file.read(&mut probe).map_err(|_| changed())? != 0 {
                return Err(changed());
            }
            self.open = None;
            self.index += 1;
        }
        Ok(PaneUploadWriteParams {
            upload_id: self.upload_id.clone(),
            path: entry.rel.clone(),
            entry: first.then(|| entry.header.clone()),
            offset: start,
            data: base64::engine::general_purpose::STANDARD.encode(&data),
            last,
        })
    }
}

impl Iterator for Writes<'_> {
    type Item = Result<PaneUploadWriteParams, String>;

    fn next(&mut self) -> Option<Self::Item> {
        let entry = self.scan.entries.get(self.index)?;
        if entry.header.kind == PaneUploadEntryKind::File {
            let size = entry.header.size.unwrap_or(0);
            return Some(self.file_chunk(entry, size));
        }
        self.index += 1;
        Some(Ok(PaneUploadWriteParams {
            upload_id: self.upload_id.clone(),
            path: entry.rel.clone(),
            entry: Some(entry.header.clone()),
            offset: 0,
            data: String::new(),
            last: true,
        }))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::upload::test_support::TempDir;

    fn rels(scan: &Scan) -> Vec<&str> {
        scan.entries
            .iter()
            .map(|entry| entry.rel.as_str())
            .collect()
    }

    #[test]
    fn a_tree_is_walked_depth_first_with_parents_first_and_symlinks_unfollowed() {
        let dir = TempDir::new("scan-tree");
        let root = dir.join("t");
        std::fs::create_dir_all(root.join("b/c")).unwrap();
        std::fs::write(root.join("a.txt"), b"hello").unwrap();
        std::fs::write(root.join("b/c/d"), b"xy").unwrap();
        std::os::unix::fs::symlink("/etc", root.join("link")).unwrap();

        let scan = scan(&[root], DuplicateNames::Reject, 100).unwrap();

        assert_eq!(scan.names, ["t"]);
        assert_eq!(
            rels(&scan),
            ["t", "t/a.txt", "t/b", "t/b/c", "t/b/c/d", "t/link"]
        );
        assert_eq!(scan.bytes, 7);
        let link = &scan.entries[5].header;
        assert_eq!(link.kind, PaneUploadEntryKind::Symlink);
        assert_eq!(link.link_target.as_deref(), Some("/etc"));
    }

    #[test]
    fn special_files_are_skipped_and_counted() {
        let dir = TempDir::new("scan-fifo");
        let root = dir.join("t");
        std::fs::create_dir(&root).unwrap();
        let status = std::process::Command::new("mkfifo")
            .arg(root.join("pipe"))
            .status()
            .unwrap();
        assert!(status.success());

        let scan = scan(&[root], DuplicateNames::Reject, 100).unwrap();

        assert_eq!(rels(&scan), ["t"]);
        assert_eq!(scan.skipped_special, 1);
    }

    #[test]
    fn executable_bits_and_modification_times_are_read() {
        let dir = TempDir::new("scan-mode");
        let file = dir.join("run.sh");
        std::fs::write(&file, b"").unwrap();
        std::fs::set_permissions(&file, std::os::unix::fs::PermissionsExt::from_mode(0o751))
            .unwrap();
        crate::platform::set_file_modified_for_test(
            &file,
            std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_234_567),
        )
        .unwrap();

        let scan = scan(&[file], DuplicateNames::Reject, 100).unwrap();

        assert_eq!(scan.entries[0].header.mode, 0o751);
        assert_eq!(scan.entries[0].header.mtime, Some(1_234_567));
    }

    #[test]
    fn duplicate_last_components_are_rejected() {
        let dir = TempDir::new("scan-dup");
        for parent in ["a", "b"] {
            std::fs::create_dir(dir.join(parent)).unwrap();
            std::fs::write(dir.join(parent).join("x.txt"), b"").unwrap();
        }
        let paths = [dir.join("a/x.txt"), dir.join("b/x.txt")];

        let error = scan(&paths, DuplicateNames::Reject, 100).unwrap_err();

        assert!(error.contains("x.txt"));
    }

    #[test]
    fn duplicate_last_components_can_be_renamed_like_a_pane_collision() {
        let dir = TempDir::new("scan-dup-rename");
        for parent in ["a", "b", "c"] {
            std::fs::create_dir_all(dir.join(parent).join("d")).unwrap();
            std::fs::write(dir.join(parent).join("x.tar.gz"), b"").unwrap();
        }
        let paths = [
            dir.join("a/x.tar.gz"),
            dir.join("b/x.tar.gz"),
            dir.join("c/x.tar.gz"),
            dir.join("a/d"),
            dir.join("b/d"),
        ];

        let scan = scan(&paths, DuplicateNames::Rename, 100).unwrap();

        assert_eq!(
            scan.names,
            ["x.tar.gz", "x.tar (1).gz", "x.tar (2).gz", "d", "d (1)"]
        );
        assert!(rels(&scan).contains(&"x.tar (1).gz"));
    }

    #[test]
    fn alternate_names_split_only_a_file_extension_after_the_first_character() {
        assert_eq!(alternate_name("a.txt", 1, true), "a (1).txt");
        assert_eq!(alternate_name(".bashrc", 1, true), ".bashrc (1)");
        assert_eq!(alternate_name("dir.d", 2, false), "dir.d (2)");
    }

    #[test]
    fn a_walk_past_the_entry_limit_is_told_apart_from_other_errors() {
        let dir = TempDir::new("scan-limit-kind");
        let root = dir.join("t");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("f"), b"").unwrap();

        assert_eq!(
            scan_limited(std::slice::from_ref(&root), DuplicateNames::Reject, 1),
            Err(ScanError::TooManyEntries(1))
        );
        assert!(matches!(
            scan_limited(&[dir.join("missing")], DuplicateNames::Reject, 1),
            Err(ScanError::Other(_))
        ));
    }

    #[test]
    fn names_with_control_characters_are_skipped_with_a_warning() {
        let dir = TempDir::new("scan-ctl");
        let root = dir.join("t");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("bad\nname"), b"").unwrap();

        let scan = scan(&[root], DuplicateNames::Reject, 100).unwrap();

        assert_eq!(rels(&scan), ["t"]);
        assert_eq!(scan.warnings.len(), 1);
    }

    #[test]
    fn a_walk_stops_once_it_passes_the_entry_limit() {
        let dir = TempDir::new("scan-limit");
        let root = dir.join("t");
        std::fs::create_dir(&root).unwrap();
        for index in 0..5 {
            std::fs::write(root.join(format!("f{index}")), b"").unwrap();
        }

        assert!(scan(std::slice::from_ref(&root), DuplicateNames::Reject, 3).is_err());
        assert!(scan(&[root], DuplicateNames::Reject, 6).is_ok());
    }

    #[test]
    fn writes_chunk_files_and_send_headers_only_on_the_first_chunk() {
        let dir = TempDir::new("writes");
        let root = dir.join("t");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("big"), vec![9u8; 10]).unwrap();
        std::fs::write(root.join("empty"), b"").unwrap();
        let scan = scan(&[root], DuplicateNames::Reject, 100).unwrap();

        let writes: Vec<PaneUploadWriteParams> = Writes::new(&scan, "up".into(), 4)
            .collect::<Result<_, _>>()
            .unwrap();

        let shape: Vec<(&str, bool, u64, usize, bool)> = writes
            .iter()
            .map(|write| {
                let len = base64::engine::general_purpose::STANDARD
                    .decode(&write.data)
                    .unwrap()
                    .len();
                (
                    write.path.as_str(),
                    write.entry.is_some(),
                    write.offset,
                    len,
                    write.last,
                )
            })
            .collect();
        assert_eq!(
            shape,
            [
                ("t", true, 0, 0, true),
                ("t/big", true, 0, 4, false),
                ("t/big", false, 4, 4, false),
                ("t/big", false, 8, 2, true),
                ("t/empty", true, 0, 0, true),
            ]
        );
        assert!(writes.iter().all(|write| write.upload_id == "up"));
    }

    #[test]
    fn a_file_that_grows_while_sending_is_an_error() {
        let dir = TempDir::new("writes-grow");
        let file = dir.join("f");
        std::fs::write(&file, b"abc").unwrap();
        let scan = scan(std::slice::from_ref(&file), DuplicateNames::Reject, 100).unwrap();
        std::fs::write(&file, b"abcdef").unwrap();

        let result: Result<Vec<_>, _> = Writes::new(&scan, "up".into(), 1024).collect();

        assert!(result.unwrap_err().contains("changed"));
    }

    #[test]
    fn an_upload_walk_still_keeps_a_selected_symlink_as_a_link() {
        let dir = TempDir::new("scan-default-options");
        std::fs::write(dir.join("real"), b"x").unwrap();
        std::os::unix::fs::symlink(dir.join("real"), dir.join("alias")).unwrap();

        let scan = scan(&[dir.join("alias")], DuplicateNames::Reject, 100).unwrap();

        assert_eq!(scan.entries[0].header.kind, PaneUploadEntryKind::Symlink);
    }

    #[test]
    fn a_selected_symlink_is_replaced_by_its_target_only_when_asked() {
        let dir = TempDir::new("scan-follow-top");
        std::fs::write(dir.join("real"), b"xyz").unwrap();
        std::os::unix::fs::symlink(dir.join("real"), dir.join("alias")).unwrap();
        let options = ScanOptions {
            follow_top_level_symlinks: true,
            ..ScanOptions::default()
        };

        let scan =
            scan_limited_with(&[dir.join("alias")], DuplicateNames::Reject, 100, options).unwrap();

        assert_eq!(scan.names, ["alias"]);
        assert_eq!(scan.entries[0].header.kind, PaneUploadEntryKind::File);
        assert_eq!(scan.entries[0].header.size, Some(3));
        assert!(scan.entries[0].identity.is_some());
    }

    #[test]
    fn an_unreadable_folder_becomes_an_empty_entry_with_a_warning_only_when_asked() {
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let dir = TempDir::new("scan-skip-unreadable");
        let root = dir.join("t");
        std::fs::create_dir_all(root.join("locked")).unwrap();
        std::fs::set_permissions(
            root.join("locked"),
            std::os::unix::fs::PermissionsExt::from_mode(0o0),
        )
        .unwrap();
        let options = ScanOptions {
            skip_unreadable_dirs: true,
            ..ScanOptions::default()
        };

        let strict = scan(std::slice::from_ref(&root), DuplicateNames::Reject, 100);
        let lenient = scan_limited_with(&[root], DuplicateNames::Reject, 100, options).unwrap();

        assert!(strict.is_err());
        assert_eq!(rels(&lenient), ["t", "t/locked"]);
        assert!(lenient.warnings[0].contains("could not list"));
    }
}
