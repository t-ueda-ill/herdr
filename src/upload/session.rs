//! The order an upload's writes must follow. Pure: no processes, no I/O. The
//! session worker asks it what each write means before touching a sink.

use std::collections::{HashMap, HashSet};

use crate::api::schema::{PaneUploadEntryHeader, PaneUploadEntryKind, PaneUploadProgress};

use super::path::{validate_link_target, validate_names, Failure, RelPath};
use super::{MAX_UPLOAD_BYTES, MAX_UPLOAD_ENTRIES, UPLOAD_CHUNK_BYTES};

/// Permission bits a sink may apply. Why not setuid/setgid/sticky: a client
/// must not be able to plant a setuid file in someone's home directory.
pub(crate) const MODE_MASK: u32 = 0o777;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Declared {
    pub(crate) names: Vec<String>,
    pub(crate) entries: u64,
    pub(crate) bytes: u64,
}

impl Declared {
    pub(crate) fn validate(&self) -> Result<(), Failure> {
        validate_names(&self.names)?;
        if self.entries > MAX_UPLOAD_ENTRIES || self.bytes > MAX_UPLOAD_BYTES {
            return Err((
                "upload_too_large",
                format!(
                    "uploads are limited to {MAX_UPLOAD_ENTRIES} entries and {MAX_UPLOAD_BYTES} bytes"
                ),
            ));
        }
        if self.entries < self.names.len() as u64 {
            return Err((
                "invalid_params",
                "entries must count at least every name".into(),
            ));
        }
        Ok(())
    }
}

/// One `pane.upload.write`, with its data already decoded.
#[derive(Debug, Clone, Copy)]
pub(crate) struct WriteRequest<'a> {
    pub(crate) path: &'a str,
    pub(crate) entry: Option<&'a PaneUploadEntryHeader>,
    pub(crate) offset: u64,
    pub(crate) data_len: usize,
    pub(crate) last: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EntryStart {
    Directory {
        path: RelPath,
    },
    Symlink {
        path: RelPath,
        target: String,
    },
    File {
        path: RelPath,
        size: u64,
        mode: u32,
        mtime: Option<i64>,
    },
}

/// What a sink must do for one accepted write: start an entry (or not), then
/// append the write's data to the open file, then close it when `end_file`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WritePlan {
    pub(crate) start: Option<EntryStart>,
    pub(crate) end_file: bool,
}

/// Mode and time a directory receives once everything inside it is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DirectoryAttrs {
    pub(crate) path: RelPath,
    pub(crate) mode: u32,
    pub(crate) mtime: Option<i64>,
}

#[derive(Debug)]
struct OpenFile {
    path: RelPath,
    size: u64,
    received: u64,
}

#[derive(Debug)]
pub(crate) struct Session {
    declared: Declared,
    names: HashSet<String>,
    kinds: HashMap<RelPath, PaneUploadEntryKind>,
    directories: Vec<DirectoryAttrs>,
    open: Option<OpenFile>,
    entries: u64,
    /// Bytes promised by started files, so a file that would overrun the
    /// declared total is refused before any of it is written.
    reserved_bytes: u64,
    received_bytes: u64,
}

fn invalid(message: impl Into<String>) -> Failure {
    ("invalid_params", message.into())
}

fn too_large(message: impl Into<String>) -> Failure {
    ("upload_too_large", message.into())
}

impl Session {
    pub(crate) fn new(declared: Declared) -> Result<Self, Failure> {
        declared.validate()?;
        let names = declared.names.iter().cloned().collect();
        Ok(Self {
            declared,
            names,
            kinds: HashMap::new(),
            directories: Vec::new(),
            open: None,
            entries: 0,
            reserved_bytes: 0,
            received_bytes: 0,
        })
    }

    pub(crate) fn names(&self) -> &[String] {
        &self.declared.names
    }

    pub(crate) fn has_writes(&self) -> bool {
        self.entries > 0
    }

    pub(crate) fn progress(&self) -> PaneUploadProgress {
        PaneUploadProgress {
            received_entries: self.entries,
            received_bytes: self.received_bytes,
        }
    }

    /// Checks one write against everything accepted so far and records it.
    /// A rejected write changes nothing.
    pub(crate) fn accept(&mut self, write: WriteRequest<'_>) -> Result<WritePlan, Failure> {
        if write.data_len > UPLOAD_CHUNK_BYTES {
            return Err(invalid(format!(
                "a chunk carries at most {UPLOAD_CHUNK_BYTES} bytes"
            )));
        }
        match write.entry {
            Some(header) => self.start_entry(write, header),
            None => self.continue_file(write),
        }
    }

    fn start_entry(
        &mut self,
        write: WriteRequest<'_>,
        header: &PaneUploadEntryHeader,
    ) -> Result<WritePlan, Failure> {
        if let Some(open) = &self.open {
            return Err(invalid(format!(
                "{} is not finished; send its remaining bytes first",
                open.path.as_str()
            )));
        }
        let path = RelPath::parse(write.path, &self.names)?;
        if write.offset != 0 {
            return Err(invalid("an entry header must come with offset 0"));
        }
        if self.kinds.contains_key(&path) {
            return Err(invalid(format!("{} was already sent", path.as_str())));
        }
        if let Some(parent) = path.parent_path() {
            if self.kinds.get(&parent) != Some(&PaneUploadEntryKind::Directory) {
                return Err(invalid(format!(
                    "{} must follow its parent directory {}",
                    path.as_str(),
                    parent.as_str()
                )));
            }
        }
        if header.mode > MODE_MASK {
            return Err(invalid(format!("mode {:o} is outside 0o777", header.mode)));
        }
        let entries = self.entries + 1;
        if entries > self.declared.entries || entries > MAX_UPLOAD_ENTRIES {
            return Err(too_large("more entries than declared"));
        }
        let mtime = header.mtime.filter(|mtime| super::stamp::in_range(*mtime));
        let (start, end_file, size) = match header.kind {
            PaneUploadEntryKind::Directory | PaneUploadEntryKind::Symlink
                if write.data_len > 0 || !write.last =>
            {
                return Err(invalid(
                    "directories and symlinks are one write with no data",
                ));
            }
            PaneUploadEntryKind::Directory => {
                (EntryStart::Directory { path: path.clone() }, false, 0)
            }
            PaneUploadEntryKind::Symlink => {
                let target = header
                    .link_target
                    .clone()
                    .ok_or_else(|| invalid("a symlink needs link_target"))?;
                validate_link_target(&target)?;
                (
                    EntryStart::Symlink {
                        path: path.clone(),
                        target,
                    },
                    false,
                    0,
                )
            }
            PaneUploadEntryKind::File => {
                let size = header.size.ok_or_else(|| invalid("a file needs size"))?;
                let reserved = self.reserved_bytes.saturating_add(size);
                if reserved > self.declared.bytes || reserved > MAX_UPLOAD_BYTES {
                    return Err(too_large("more bytes than declared"));
                }
                Self::check_chunk(size, 0, write)?;
                (
                    EntryStart::File {
                        path: path.clone(),
                        size,
                        mode: header.mode,
                        mtime,
                    },
                    write.last,
                    size,
                )
            }
            PaneUploadEntryKind::Unknown => {
                return Err(invalid("unknown entry kind"));
            }
        };
        self.entries = entries;
        self.kinds.insert(path.clone(), header.kind);
        match header.kind {
            PaneUploadEntryKind::Directory => self.directories.push(DirectoryAttrs {
                path,
                mode: header.mode,
                mtime,
            }),
            PaneUploadEntryKind::File => {
                self.reserved_bytes += size;
                self.received_bytes += write.data_len as u64;
                if !write.last {
                    self.open = Some(OpenFile {
                        path,
                        size,
                        received: write.data_len as u64,
                    });
                }
            }
            PaneUploadEntryKind::Symlink | PaneUploadEntryKind::Unknown => {}
        }
        Ok(WritePlan {
            start: Some(start),
            end_file,
        })
    }

    /// A chunk at `received` must stay inside `size` and end exactly there when last.
    fn check_chunk(size: u64, received: u64, write: WriteRequest<'_>) -> Result<(), Failure> {
        let end = received + write.data_len as u64;
        if end > size {
            return Err(invalid(format!(
                "{} has more bytes than its declared size {size}",
                write.path
            )));
        }
        if write.last && end != size {
            return Err(invalid(format!(
                "{} ended at {end} of its declared size {size}",
                write.path
            )));
        }
        Ok(())
    }

    fn continue_file(&mut self, write: WriteRequest<'_>) -> Result<WritePlan, Failure> {
        let Some(open) = &self.open else {
            return Err(invalid(format!(
                "{} needs an entry header on its first write",
                write.path
            )));
        };
        if open.path.as_str() != write.path {
            return Err(invalid(format!(
                "{} is not finished; send its remaining bytes first",
                open.path.as_str()
            )));
        }
        if write.offset != open.received {
            return Err(invalid(format!(
                "{} expected offset {}, not {}",
                write.path, open.received, write.offset
            )));
        }
        Self::check_chunk(open.size, open.received, write)?;
        let received = open.received + write.data_len as u64;
        self.received_bytes += write.data_len as u64;
        if write.last {
            self.open = None;
        } else if let Some(open) = &mut self.open {
            open.received = received;
        }
        Ok(WritePlan {
            start: None,
            end_file: write.last,
        })
    }

    /// Ready to publish: no file is half sent and every name arrived.
    pub(crate) fn check_complete(&self) -> Result<(), Failure> {
        if let Some(open) = &self.open {
            return Err(invalid(format!("{} is not finished", open.path.as_str())));
        }
        let missing: Vec<&str> = self
            .declared
            .names
            .iter()
            .filter(|name| !self.kinds.contains_key(&RelPath::top_level(name)))
            .map(String::as_str)
            .collect();
        if !missing.is_empty() {
            return Err(invalid(format!(
                "names were declared but never sent: {}",
                missing.join(", ")
            )));
        }
        Ok(())
    }

    /// Directories below the top level, deepest first, so a read-only parent
    /// is never applied before its children.
    pub(crate) fn nested_directories(&self) -> Vec<DirectoryAttrs> {
        let mut nested: Vec<DirectoryAttrs> = self
            .directories
            .iter()
            .filter(|dir| !dir.path.is_top())
            .cloned()
            .collect();
        nested.sort_by_key(|dir| std::cmp::Reverse(dir.path.depth()));
        nested
    }

    /// Top-level directories, whose attributes apply only after publishing.
    /// Why not before: renaming a directory into another parent needs write
    /// access to the directory itself, which a 0555 directory would lack.
    pub(crate) fn top_directories(&self) -> Vec<DirectoryAttrs> {
        self.directories
            .iter()
            .filter(|dir| dir.path.is_top())
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declared(names: &[&str], entries: u64, bytes: u64) -> Declared {
        Declared {
            names: names.iter().map(|name| (*name).to_owned()).collect(),
            entries,
            bytes,
        }
    }

    fn session(names: &[&str]) -> Session {
        Session::new(declared(names, 100, 1_000_000)).unwrap()
    }

    fn header(kind: PaneUploadEntryKind, size: Option<u64>) -> PaneUploadEntryHeader {
        PaneUploadEntryHeader {
            kind,
            mode: 0o644,
            mtime: None,
            size,
            link_target: (kind == PaneUploadEntryKind::Symlink).then(|| "target".into()),
        }
    }

    fn start(
        session: &mut Session,
        path: &str,
        header: &PaneUploadEntryHeader,
        len: usize,
        last: bool,
    ) -> Result<WritePlan, Failure> {
        session.accept(WriteRequest {
            path,
            entry: Some(header),
            offset: 0,
            data_len: len,
            last,
        })
    }

    fn more(
        session: &mut Session,
        path: &str,
        offset: u64,
        len: usize,
        last: bool,
    ) -> Result<WritePlan, Failure> {
        session.accept(WriteRequest {
            path,
            entry: None,
            offset,
            data_len: len,
            last,
        })
    }

    fn code<T>(result: Result<T, Failure>) -> &'static str {
        match result {
            Ok(_) => "ok",
            Err((code, _)) => code,
        }
    }

    #[test]
    fn a_file_sent_in_chunks_is_accepted_in_offset_order_and_ends_at_its_size() {
        let mut session = session(&["a"]);
        let file = header(PaneUploadEntryKind::File, Some(10));

        let first = start(&mut session, "a", &file, 4, false).unwrap();
        let second = more(&mut session, "a", 4, 6, true).unwrap();

        assert!(matches!(
            first.start,
            Some(EntryStart::File { size: 10, .. })
        ));
        assert!(!first.end_file);
        assert_eq!(
            second,
            WritePlan {
                start: None,
                end_file: true
            }
        );
        assert_eq!(session.progress().received_bytes, 10);
        assert_eq!(session.progress().received_entries, 1);
        assert_eq!(session.check_complete(), Ok(()));
    }

    #[test]
    fn an_empty_file_is_one_last_write_without_data() {
        let mut session = session(&["e"]);

        let plan = start(
            &mut session,
            "e",
            &header(PaneUploadEntryKind::File, Some(0)),
            0,
            true,
        );

        assert!(plan.unwrap().end_file);
    }

    #[test]
    fn a_child_must_follow_its_parent_directory() {
        let mut session = session(&["d"]);
        let file = header(PaneUploadEntryKind::File, Some(0));

        assert_eq!(
            code(start(&mut session, "d/f", &file, 0, true)),
            "invalid_params"
        );
        start(
            &mut session,
            "d",
            &header(PaneUploadEntryKind::Directory, None),
            0,
            true,
        )
        .unwrap();
        assert_eq!(code(start(&mut session, "d/f", &file, 0, true)), "ok");
    }

    #[test]
    fn nothing_can_be_placed_under_a_symlink_or_a_file() {
        let mut session = session(&["l", "f"]);
        start(
            &mut session,
            "l",
            &header(PaneUploadEntryKind::Symlink, None),
            0,
            true,
        )
        .unwrap();
        start(
            &mut session,
            "f",
            &header(PaneUploadEntryKind::File, Some(0)),
            0,
            true,
        )
        .unwrap();
        let file = header(PaneUploadEntryKind::File, Some(0));

        assert_eq!(
            code(start(&mut session, "l/x", &file, 0, true)),
            "invalid_params"
        );
        assert_eq!(
            code(start(&mut session, "f/x", &file, 0, true)),
            "invalid_params"
        );
    }

    #[test]
    fn the_same_path_cannot_be_sent_twice() {
        let mut session = session(&["a"]);
        let dir = header(PaneUploadEntryKind::Directory, None);
        start(&mut session, "a", &dir, 0, true).unwrap();

        assert_eq!(
            code(start(&mut session, "a", &dir, 0, true)),
            "invalid_params"
        );
    }

    #[test]
    fn a_chunk_at_the_wrong_offset_is_refused_and_changes_nothing() {
        let mut session = session(&["a"]);
        start(
            &mut session,
            "a",
            &header(PaneUploadEntryKind::File, Some(10)),
            4,
            false,
        )
        .unwrap();

        assert_eq!(code(more(&mut session, "a", 3, 6, true)), "invalid_params");
        assert_eq!(code(more(&mut session, "a", 4, 6, true)), "ok");
    }

    #[test]
    fn another_entry_cannot_start_before_the_open_file_ends() {
        let mut session = session(&["a", "b"]);
        let file = header(PaneUploadEntryKind::File, Some(10));
        start(&mut session, "a", &file, 4, false).unwrap();

        assert_eq!(
            code(start(&mut session, "b", &file, 0, false)),
            "invalid_params"
        );
        assert_eq!(code(more(&mut session, "b", 0, 1, false)), "invalid_params");
        assert!(session.check_complete().is_err());
    }

    #[test]
    fn a_file_cannot_grow_past_or_stop_short_of_its_declared_size() {
        let mut session = session(&["a"]);
        let file = header(PaneUploadEntryKind::File, Some(10));

        assert_eq!(
            code(start(&mut session, "a", &file, 11, false)),
            "invalid_params"
        );
        assert_eq!(
            code(start(&mut session, "a", &file, 9, true)),
            "invalid_params"
        );
    }

    #[test]
    fn totals_beyond_the_declaration_are_upload_too_large() {
        let mut session = Session::new(declared(&["a", "b"], 2, 10)).unwrap();
        let file = header(PaneUploadEntryKind::File, Some(6));
        start(&mut session, "a", &file, 6, true).unwrap();

        assert_eq!(
            code(start(&mut session, "b", &file, 6, true)),
            "upload_too_large"
        );

        let mut session = Session::new(declared(&["a"], 1, 100)).unwrap();
        start(
            &mut session,
            "a",
            &header(PaneUploadEntryKind::Directory, None),
            0,
            true,
        )
        .unwrap();
        let child = header(PaneUploadEntryKind::File, Some(0));
        assert_eq!(
            code(start(&mut session, "a/x", &child, 0, true)),
            "upload_too_large"
        );
    }

    #[test]
    fn declarations_beyond_the_server_caps_are_upload_too_large() {
        let over_bytes = declared(&["a"], 1, MAX_UPLOAD_BYTES + 1);
        let over_entries = declared(&["a"], MAX_UPLOAD_ENTRIES + 1, 0);

        assert_eq!(code(Session::new(over_bytes)), "upload_too_large");
        assert_eq!(code(Session::new(over_entries)), "upload_too_large");
    }

    #[test]
    fn chunks_larger_than_the_chunk_limit_are_refused() {
        let mut session = Session::new(declared(&["a"], 1, 10_000_000)).unwrap();
        let file = header(PaneUploadEntryKind::File, Some(10_000_000));

        let result = start(&mut session, "a", &file, UPLOAD_CHUNK_BYTES + 1, false);

        assert_eq!(code(result), "invalid_params");
    }

    #[test]
    fn unknown_kinds_modes_above_0o777_and_symlinks_without_targets_are_refused() {
        let mut session = session(&["a"]);
        let unknown = header(PaneUploadEntryKind::Unknown, None);
        let mut setuid = header(PaneUploadEntryKind::File, Some(0));
        setuid.mode = 0o4755;
        let mut bare_link = header(PaneUploadEntryKind::Symlink, None);
        bare_link.link_target = None;

        assert_eq!(
            code(start(&mut session, "a", &unknown, 0, true)),
            "invalid_params"
        );
        assert_eq!(
            code(start(&mut session, "a", &setuid, 0, true)),
            "invalid_params"
        );
        assert_eq!(
            code(start(&mut session, "a", &bare_link, 0, true)),
            "invalid_params"
        );
    }

    #[test]
    fn directories_and_symlinks_carry_no_data_and_end_in_one_write() {
        let mut session = session(&["d"]);
        let dir = header(PaneUploadEntryKind::Directory, None);

        assert_eq!(
            code(start(&mut session, "d", &dir, 1, true)),
            "invalid_params"
        );
        assert_eq!(
            code(start(&mut session, "d", &dir, 0, false)),
            "invalid_params"
        );
    }

    #[test]
    fn finishing_requires_every_declared_name() {
        let mut session = session(&["a", "b"]);
        start(
            &mut session,
            "a",
            &header(PaneUploadEntryKind::File, Some(0)),
            0,
            true,
        )
        .unwrap();

        let result = session.check_complete();

        assert!(result.unwrap_err().1.contains('b'));
    }

    #[test]
    fn nested_directory_attributes_come_deepest_first_and_top_ones_separately() {
        let mut session = session(&["t"]);
        let dir = header(PaneUploadEntryKind::Directory, None);
        for path in ["t", "t/a", "t/a/b", "t/c"] {
            start(&mut session, path, &dir, 0, true).unwrap();
        }

        let nested: Vec<String> = session
            .nested_directories()
            .into_iter()
            .map(|dir| dir.path.as_str().to_owned())
            .collect();
        let top: Vec<String> = session
            .top_directories()
            .into_iter()
            .map(|dir| dir.path.as_str().to_owned())
            .collect();

        assert_eq!(nested[0], "t/a/b");
        assert_eq!(nested.len(), 3);
        assert_eq!(top, ["t"]);
    }

    #[test]
    fn modification_times_outside_touch_range_are_dropped_instead_of_refused() {
        let mut session = session(&["a"]);
        let mut file = header(PaneUploadEntryKind::File, Some(0));
        file.mtime = Some(-5);

        let plan = start(&mut session, "a", &file, 0, true).unwrap();

        assert!(matches!(
            plan.start,
            Some(EntryStart::File { mtime: None, .. })
        ));
    }
}
