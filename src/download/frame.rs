//! What a download source produces ([`Frame`]) and the packer that turns frames
//! into the records of one read answer. Pure: no I/O, no clocks. The same
//! packer serves every source, so names, skipped trees, and limits are checked
//! in one place.

use std::collections::HashSet;

use base64::Engine as _;

use crate::api::schema::{
    PaneDownloadChunk, PaneDownloadRecord, PaneUploadEntryHeader, PaneUploadEntryKind,
};
use crate::upload::path::{validate_link_target, Failure, RelPath, MAX_PATH_BYTES};

use super::{
    notice_text, MAX_DOWNLOAD_BYTES, MAX_DOWNLOAD_ENTRIES, MAX_READ_META_BYTES, MAX_READ_RECORDS,
    MAX_WARNING_BYTES, MAX_WARNING_LINE_BYTES,
};

/// Why a source left an entry out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SkipReason {
    /// A FIFO, socket, or device.
    Special,
    PermissionDenied,
    /// A name the source could not hand over (not UTF-8, control characters).
    Name,
    Link,
}

impl SkipReason {
    fn describe(self) -> &'static str {
        match self {
            Self::Special => "not a regular file, folder, or link",
            Self::PermissionDenied => "permission denied",
            Self::Name => "unusable name",
            Self::Link => "link not copied",
        }
    }
}

/// Why a file's bytes cannot be trusted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Changed(pub(crate) String);

/// One unit a source emits, in the order `find` or a directory walk would
/// visit: a parent before its children.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Frame {
    /// Raw bytes for `rel` and `link_target`: the other side's filesystem
    /// decides them, and not every name is UTF-8. A file's `header.size` is
    /// required; `header.link_target` is ignored in favour of `link_target`.
    Entry {
        rel: Vec<u8>,
        header: PaneUploadEntryHeader,
        link_target: Option<Vec<u8>>,
    },
    /// The next bytes of the file the last `Entry` opened.
    Data(Vec<u8>),
    /// A file's trailer: unchanged, or why its bytes cannot be trusted.
    FileEnd(Result<(), Changed>),
    Skipped {
        rel: Vec<u8>,
        reason: SkipReason,
    },
    WalkWarning(String),
    End,
}

/// What one read answer may hold. Tests shrink it; production takes the
/// constants and the session's `chunk_bytes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Limits {
    pub(crate) chunk_bytes: usize,
    pub(crate) max_records: usize,
    pub(crate) max_meta_bytes: usize,
    pub(crate) max_entries: u64,
    pub(crate) max_bytes: u64,
}

impl Limits {
    pub(crate) fn new(chunk_bytes: usize) -> Self {
        Self {
            // Why at least 1: a zero budget could never make progress.
            chunk_bytes: chunk_bytes.max(1),
            max_records: MAX_READ_RECORDS,
            max_meta_bytes: MAX_READ_META_BYTES,
            max_entries: MAX_DOWNLOAD_ENTRIES,
            max_bytes: MAX_DOWNLOAD_BYTES,
        }
    }
}

fn failed(message: impl Into<String>) -> Failure {
    ("download_failed", message.into())
}

fn changed(message: impl Into<String>) -> Failure {
    ("download_source_changed", message.into())
}

fn too_large(message: impl Into<String>) -> Failure {
    ("download_too_large", message.into())
}

fn shown(bytes: &[u8]) -> String {
    notice_text(&String::from_utf8_lossy(bytes))
}

/// A record before base64, so a following `Data` frame can extend it.
struct Pending {
    path: String,
    entry: Option<PaneUploadEntryHeader>,
    offset: u64,
    data: Vec<u8>,
    last: bool,
}

impl Pending {
    fn into_record(self) -> PaneDownloadRecord {
        PaneDownloadRecord {
            path: self.path,
            entry: self.entry,
            offset: self.offset,
            data: base64::engine::general_purpose::STANDARD.encode(&self.data),
            last: self.last,
        }
    }
}

/// The answer being built.
#[derive(Default)]
struct Batch {
    records: Vec<Pending>,
    data_bytes: usize,
    meta_bytes: usize,
}

impl Batch {
    /// Why checked after adding rather than before: one record's names are at
    /// most about 8 KiB, so the budget is overshot by less than that, and no
    /// frame has to be put back. Data is split exactly, because `chunk_bytes`
    /// is what the client agreed to.
    fn full(&self, limits: &Limits) -> bool {
        self.data_bytes >= limits.chunk_bytes
            || self.meta_bytes >= limits.max_meta_bytes
            || self.records.len() >= limits.max_records
    }
}

/// Extra warning text kept in reserve for the closing "and N more" line.
const MORE_LINE_RESERVE: usize = 32;

/// Collected warnings, bounded so a tree of unreadable folders cannot grow
/// them without limit.
#[derive(Default)]
struct Warnings {
    lines: Vec<String>,
    bytes: usize,
    dropped: u64,
}

impl Warnings {
    fn add(&mut self, text: &str) {
        let mut line = notice_text(text);
        if line.len() > MAX_WARNING_LINE_BYTES {
            let mut end = MAX_WARNING_LINE_BYTES;
            while !line.is_char_boundary(end) {
                end -= 1;
            }
            line.truncate(end);
        }
        if self.bytes + line.len() + MORE_LINE_RESERVE > MAX_WARNING_BYTES {
            self.dropped += 1;
            return;
        }
        self.bytes += line.len();
        self.lines.push(line);
    }

    fn finish(&mut self) -> Vec<String> {
        let mut lines = std::mem::take(&mut self.lines);
        if self.dropped > 0 {
            lines.push(format!("and {} more", self.dropped));
        }
        lines
    }
}

enum File {
    None,
    /// A file being sent. Its header rides with the first data, so a file is
    /// one record where it can be.
    Open {
        rel: String,
        size: u64,
        /// Bytes accepted from the source.
        received: u64,
        /// Bytes already in records.
        emitted: u64,
        header: Option<PaneUploadEntryHeader>,
    },
    /// A skipped file whose `Data` and `FileEnd` are still to come.
    Discarding,
}

pub(crate) struct Packer {
    names: Vec<String>,
    name_set: HashSet<String>,
    limits: Limits,
    seen_tops: HashSet<String>,
    /// Directories already sent, as the source spelled them.
    dirs: HashSet<Vec<u8>>,
    /// Every entry left out, whatever the reason: its descendants follow it.
    skipped: HashSet<Vec<u8>>,
    file: File,
    /// Bytes of the open file not yet in a record, from `carry_at`.
    carry: Vec<u8>,
    carry_at: usize,
    /// Entries sent or skipped as a root; what is dropped below a root is not counted.
    entries_accepted: u64,
    bytes_declared: u64,
    sent_entries: u64,
    sent_bytes: u64,
    warnings: Warnings,
    ended: bool,
    finished: bool,
}

impl Packer {
    /// `names`: the top-level names of the download, in the order requested.
    pub(crate) fn new(names: Vec<String>, limits: Limits) -> Self {
        Self {
            name_set: names.iter().cloned().collect(),
            names,
            limits,
            seen_tops: HashSet::new(),
            dirs: HashSet::new(),
            skipped: HashSet::new(),
            file: File::None,
            carry: Vec::new(),
            carry_at: 0,
            entries_accepted: 0,
            bytes_declared: 0,
            sent_entries: 0,
            sent_bytes: 0,
            warnings: Warnings::default(),
            ended: false,
            finished: false,
        }
    }

    /// Builds the answer to read `seq` from the frames `next` hands over.
    /// `Ok(None)` from `next` means nothing is ready: the answer closes with
    /// what it has, possibly no records (a heartbeat). Waiting for more is the
    /// caller's job, so this stays free of clocks.
    ///
    /// After `End` the records already gathered go out alone; the answer with
    /// `done` and the warnings follows, so warnings never share an answer with
    /// records.
    pub(crate) fn fill(
        &mut self,
        seq: u64,
        next: &mut dyn FnMut() -> Result<Option<Frame>, Failure>,
    ) -> Result<PaneDownloadChunk, Failure> {
        if self.finished {
            return Err(("download_not_found", "the download is finished".into()));
        }
        let mut batch = Batch::default();
        loop {
            self.drain_carry(&mut batch)?;
            if batch.full(&self.limits) || self.ended {
                break;
            }
            match next()? {
                None => break,
                Some(frame) => self.accept(frame, &mut batch)?,
            }
        }
        let done = self.ended && batch.records.is_empty() && self.carry_at >= self.carry.len();
        let warnings = if done {
            self.finished = true;
            self.warnings.finish()
        } else {
            Vec::new()
        };
        Ok(PaneDownloadChunk {
            seq,
            records: batch
                .records
                .into_iter()
                .map(Pending::into_record)
                .collect(),
            done,
            sent_entries: self.sent_entries,
            sent_bytes: self.sent_bytes,
            warnings,
        })
    }

    fn accept(&mut self, frame: Frame, batch: &mut Batch) -> Result<(), Failure> {
        match frame {
            Frame::Entry {
                rel,
                header,
                link_target,
            } => self.accept_entry(rel, header, link_target, batch),
            Frame::Data(bytes) => self.accept_data(bytes),
            Frame::FileEnd(result) => self.accept_file_end(result),
            Frame::Skipped { rel, reason } => self.accept_skipped(&rel, reason),
            Frame::WalkWarning(text) => {
                self.warnings.add(&text);
                Ok(())
            }
            Frame::End => self.accept_end(),
        }
    }

    fn is_skipped(&self, rel: &[u8]) -> bool {
        self.skipped.contains(rel)
            || rel
                .iter()
                .enumerate()
                .any(|(at, byte)| *byte == b'/' && self.skipped.contains(&rel[..at]))
    }

    /// A source that names something outside the request is wrong or hostile,
    /// not merely unlucky, so this is fatal rather than a skip.
    fn check_top(&self, rel: &[u8]) -> Result<(), Failure> {
        let first = rel.split(|byte| *byte == b'/').next().unwrap_or_default();
        let known = std::str::from_utf8(first).is_ok_and(|name| self.name_set.contains(name));
        if known {
            Ok(())
        } else {
            Err(failed(format!(
                "the source sent {}, which was not requested",
                shown(rel)
            )))
        }
    }

    /// Why a parent must already be known: the client rejects a child whose
    /// parent it has not been told about, which would fail the whole download.
    fn check_parent(&self, rel: &[u8]) -> Result<(), Failure> {
        let Some(slash) = rel.iter().rposition(|byte| *byte == b'/') else {
            return Ok(());
        };
        if self.dirs.contains(&rel[..slash]) {
            Ok(())
        } else {
            Err(failed(format!(
                "the source sent {} before its folder",
                shown(rel)
            )))
        }
    }

    /// Leaves `rel` out. Why every skipped entry is remembered whatever the
    /// reason: a helper may keep listing what is below a folder it could not
    /// open, and the client refuses a child of something it never received.
    fn skip(&mut self, rel: &[u8], why: &str, is_file: bool) -> Result<(), Failure> {
        if !rel.contains(&b'/') {
            return Err(changed(format!(
                "{} could not be downloaded: {why}",
                shown(rel)
            )));
        }
        // Why counted: every root is remembered below, so a source that skips
        // without end would otherwise grow this set without bound.
        self.count_entry()?;
        // Why not remembered when overlong: everything below it is longer still
        // and fails the name check on its own, and keeping it would let one
        // entry hold a whole helper field in memory.
        if rel.len() <= MAX_PATH_BYTES {
            self.skipped.insert(rel.to_vec());
        }
        self.warnings.add(&format!("skipped {}: {why}", shown(rel)));
        if is_file {
            self.file = File::Discarding;
        }
        Ok(())
    }

    /// Counts an entry the walk found, sent or skipped.
    fn count_entry(&mut self) -> Result<(), Failure> {
        self.entries_accepted += 1;
        if self.entries_accepted > self.limits.max_entries {
            return Err(too_large(format!(
                "the download has more than {} entries",
                self.limits.max_entries
            )));
        }
        Ok(())
    }

    fn accept_entry(
        &mut self,
        rel: Vec<u8>,
        header: PaneUploadEntryHeader,
        link_target: Option<Vec<u8>>,
        batch: &mut Batch,
    ) -> Result<(), Failure> {
        if !matches!(self.file, File::None) {
            return Err(failed(format!(
                "the source began {} inside an unfinished file",
                shown(&rel)
            )));
        }
        self.check_top(&rel)?;
        let is_file = header.kind == PaneUploadEntryKind::File;
        if self.is_skipped(&rel) {
            if is_file {
                self.file = File::Discarding;
            }
            return Ok(());
        }
        let path = match std::str::from_utf8(&rel) {
            Ok(text) => match RelPath::parse(text, &self.name_set) {
                Ok(path) => path,
                Err(_) => return self.skip(&rel, SkipReason::Name.describe(), is_file),
            },
            Err(_) => return self.skip(&rel, SkipReason::Name.describe(), is_file),
        };
        // Why after the name: an unusable name is skipped whatever its parent.
        self.check_parent(&rel)?;
        let path = path.as_str().to_owned();
        let mut entry = PaneUploadEntryHeader {
            kind: header.kind,
            // Why masked: a sticky or setuid directory is common, and the
            // client refuses modes above 0o777 for the whole download.
            mode: header.mode & 0o777,
            mtime: header.mtime,
            size: None,
            link_target: None,
        };
        match header.kind {
            PaneUploadEntryKind::Directory => {}
            PaneUploadEntryKind::Symlink => {
                let target = link_target
                    .as_deref()
                    .and_then(|bytes| std::str::from_utf8(bytes).ok())
                    .filter(|text| validate_link_target(text).is_ok());
                let Some(target) = target else {
                    return self.skip(&rel, "unusable link target", false);
                };
                entry.link_target = Some(target.to_owned());
            }
            PaneUploadEntryKind::File => {
                let Some(size) = header.size else {
                    return Err(failed(format!(
                        "the source sent the file {} without a size",
                        shown(&rel)
                    )));
                };
                entry.size = Some(size);
            }
            PaneUploadEntryKind::Unknown => {
                return Err(failed(format!(
                    "the source sent {} with an unknown kind",
                    shown(&rel)
                )));
            }
        }
        self.count_entry()?;
        if let Some(size) = entry.size {
            self.bytes_declared = self.bytes_declared.saturating_add(size);
            if self.bytes_declared > self.limits.max_bytes {
                return Err(too_large(format!(
                    "the download is larger than {} bytes",
                    self.limits.max_bytes
                )));
            }
        }
        if !path.contains('/') {
            self.seen_tops.insert(path.clone());
        }
        match header.kind {
            PaneUploadEntryKind::File => {
                let size = entry.size.unwrap_or(0);
                if size == 0 {
                    self.file = File::Open {
                        rel: path.clone(),
                        size: 0,
                        received: 0,
                        emitted: 0,
                        header: None,
                    };
                    self.push_record(batch, path, Some(entry), 0, Vec::new(), true);
                } else {
                    self.file = File::Open {
                        rel: path,
                        size,
                        received: 0,
                        emitted: 0,
                        header: Some(entry),
                    };
                }
            }
            kind => {
                if kind == PaneUploadEntryKind::Directory {
                    self.dirs.insert(rel);
                }
                self.push_record(batch, path, Some(entry), 0, Vec::new(), true);
            }
        }
        Ok(())
    }

    fn accept_data(&mut self, bytes: Vec<u8>) -> Result<(), Failure> {
        match &mut self.file {
            File::None => Err(failed("the source sent file data outside a file")),
            File::Discarding => Ok(()),
            File::Open {
                rel,
                size,
                received,
                ..
            } => {
                let total = received.saturating_add(bytes.len() as u64);
                if total > *size {
                    return Err(changed(format!(
                        "{} grew while it was read",
                        notice_text(rel)
                    )));
                }
                *received = total;
                self.carry.drain(..self.carry_at);
                self.carry_at = 0;
                self.carry.extend_from_slice(&bytes);
                Ok(())
            }
        }
    }

    fn accept_file_end(&mut self, result: Result<(), Changed>) -> Result<(), Failure> {
        match std::mem::replace(&mut self.file, File::None) {
            File::None => Err(failed("the source ended a file that was not open")),
            // Why not looked at: the file is not being downloaded.
            File::Discarding => Ok(()),
            File::Open {
                rel,
                size,
                received,
                ..
            } => {
                // Why a failure rather than a warning: the client has already
                // closed the file at its last record but publishes only after
                // `done`, so failing here publishes nothing. A warning would let
                // a file padded after it shrank be saved as if it were whole.
                if let Err(Changed(why)) = result {
                    return Err(changed(format!("{}: {why}", notice_text(&rel))));
                }
                if received < size {
                    return Err(changed(format!(
                        "{} shrank while it was read",
                        notice_text(&rel)
                    )));
                }
                Ok(())
            }
        }
    }

    fn accept_skipped(&mut self, rel: &[u8], reason: SkipReason) -> Result<(), Failure> {
        if !matches!(self.file, File::None) {
            return Err(failed("the source skipped an entry inside a file"));
        }
        self.check_top(rel)?;
        if self.is_skipped(rel) {
            return Ok(());
        }
        self.check_parent(rel)?;
        self.skip(rel, reason.describe(), false)
    }

    fn accept_end(&mut self) -> Result<(), Failure> {
        if !matches!(self.file, File::None) {
            return Err(failed("the source ended inside a file"));
        }
        if let Some(missing) = self
            .names
            .iter()
            .find(|name| !self.seen_tops.contains(*name))
        {
            return Err(changed(format!(
                "{} disappeared before it was sent",
                notice_text(missing)
            )));
        }
        self.ended = true;
        Ok(())
    }

    /// Moves carried bytes into records until they are gone or the answer is full.
    fn drain_carry(&mut self, batch: &mut Batch) -> Result<(), Failure> {
        while self.carry_at < self.carry.len() && !batch.full(&self.limits) {
            let File::Open {
                rel,
                size,
                emitted,
                header,
                ..
            } = &mut self.file
            else {
                return Err(failed("file data without an open file"));
            };
            let room = self
                .limits
                .chunk_bytes
                .saturating_sub(batch.data_bytes)
                .max(1);
            let take = room.min(self.carry.len() - self.carry_at);
            let bytes = self.carry[self.carry_at..self.carry_at + take].to_vec();
            self.carry_at += take;
            let offset = *emitted;
            *emitted += take as u64;
            let last = *emitted == *size;
            let path = rel.clone();
            let entry = header.take();
            self.push_record(batch, path, entry, offset, bytes, last);
        }
        Ok(())
    }

    fn push_record(
        &mut self,
        batch: &mut Batch,
        path: String,
        entry: Option<PaneUploadEntryHeader>,
        offset: u64,
        data: Vec<u8>,
        last: bool,
    ) {
        if entry.is_some() {
            self.sent_entries += 1;
        }
        self.sent_bytes += data.len() as u64;
        batch.data_bytes += data.len();
        // Why joined: one base64 pass and one record instead of one per frame.
        if entry.is_none() {
            if let Some(previous) = batch
                .records
                .last_mut()
                .filter(|previous| !previous.last && previous.path == path)
            {
                previous.data.extend_from_slice(&data);
                previous.last = last;
                return;
            }
        }
        batch.meta_bytes += path.len()
            + entry
                .as_ref()
                .and_then(|entry| entry.link_target.as_ref())
                .map_or(0, String::len);
        batch.records.push(Pending {
            path,
            entry,
            offset,
            data,
            last,
        });
    }
}

#[cfg(test)]
mod tests;
