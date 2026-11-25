//! One pane's recording on disk: a series of gzip-compressed asciicast files
//! that rolls over to a new part when a part grows past its size limit.

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use flate2::write::GzEncoder;
use flate2::Compression;

use super::cast::{self, EventCode, Utf8Stream};
use super::clock::{LocalClock, Stamp, TimeFormat};

pub(crate) const FILE_EXTENSION: &str = "cast.gz";
/// Why a cap at all: one long-lived pane would otherwise grow one file that
/// no viewer can open in reasonable time.
pub(crate) const DEFAULT_PART_LIMIT_BYTES: u64 = 64 * 1024 * 1024;
/// Why bounded: a name that keeps colliding means something else is writing
/// there, and looping forever would stall every pane's recording.
const MAX_NAME_ATTEMPTS: u32 = 1000;

/// What names and heads every part of one pane's recording.
#[derive(Debug, Clone)]
pub(crate) struct CastIdentity {
    pub dir: PathBuf,
    pub stem: String,
    pub title: String,
    /// Spells the local time in the time markers.
    pub time_format: TimeFormat,
}

pub(crate) struct CastFile {
    identity: CastIdentity,
    part_limit_bytes: u64,
    part: u32,
    path: PathBuf,
    encoder: GzEncoder<CountingFile>,
    /// The header `timestamp`: the whole second the part started in, so that
    /// `timestamp + event time` is the event's wall-clock time.
    origin_secs: u64,
    /// The last event time written; times never step back.
    last_elapsed: Duration,
    /// The minute (since the epoch) of the last time marker.
    marked_minute: Option<u64>,
    clock: LocalClock,
    size: (u16, u16),
    output: Utf8Stream,
    input: Utf8Stream,
    unflushed: bool,
}

impl CastFile {
    pub(crate) fn create(
        identity: CastIdentity,
        part_limit_bytes: u64,
        cols: u16,
        rows: u16,
        at: Stamp,
    ) -> io::Result<Self> {
        ensure_private_dir(&identity.dir)?;
        let (part, path, file) = create_part(&identity, 1)?;
        let mut cast = Self {
            identity,
            part_limit_bytes,
            part,
            path,
            encoder: GzEncoder::new(CountingFile::new(file), Compression::fast()),
            origin_secs: epoch_secs(at),
            last_elapsed: Duration::ZERO,
            marked_minute: None,
            clock: LocalClock::default(),
            size: (cols, rows),
            output: Utf8Stream::default(),
            input: Utf8Stream::default(),
            unflushed: false,
        };
        cast.write_header()?;
        Ok(cast)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn write_bytes(
        &mut self,
        at: Stamp,
        code: EventCode,
        bytes: &[u8],
    ) -> io::Result<()> {
        let stream = match code {
            EventCode::Input => &mut self.input,
            _ => &mut self.output,
        };
        let text = stream.decode(bytes);
        if text.is_empty() {
            return Ok(());
        }
        self.write_event(at, code, &text)
    }

    /// Marks bytes left out of the recording, cutting both text streams so
    /// that characters on either side of the gap are not joined.
    ///
    /// Why both streams: the dropped count does not say whether output or
    /// input lost bytes, and the marker rides on whichever event comes next,
    /// so a partial input character can also come back as U+FFFD when only
    /// output was dropped.
    pub(crate) fn write_gap(&mut self, at: Stamp, label: &str) -> io::Result<()> {
        if let Some(text) = self.output.break_at_gap() {
            self.write_event(at, EventCode::Output, &text)?;
        }
        if let Some(text) = self.input.break_at_gap() {
            self.write_event(at, EventCode::Input, &text)?;
        }
        self.write_event(at, EventCode::Marker, label)
    }

    pub(crate) fn resize(&mut self, at: Stamp, cols: u16, rows: u16) -> io::Result<()> {
        if self.size == (cols, rows) {
            return Ok(());
        }
        self.size = (cols, rows);
        self.write_event(at, EventCode::Resize, &cast::resize_data(cols, rows))
    }

    /// Makes everything written so far readable by `zcat` and `zless`
    /// while the file stays open.
    pub(crate) fn flush(&mut self) -> io::Result<()> {
        if !self.unflushed {
            return Ok(());
        }
        self.unflushed = false;
        self.encoder.flush()
    }

    /// Writes the gzip trailer so the file passes `gzip -t`.
    pub(crate) fn finish(mut self, at: Stamp) -> io::Result<PathBuf> {
        self.release_partial_characters(at)?;
        self.encoder.try_finish()?;
        Ok(self.path)
    }

    fn write_event(&mut self, at: Stamp, code: EventCode, data: &str) -> io::Result<()> {
        self.mark_time(at)?;
        self.write_line(at, code, data)?;
        if self.encoder.get_ref().written() >= self.part_limit_bytes {
            self.roll_over(at)?;
        }
        Ok(())
    }

    /// Writes the local date and time as a marker before the first event of
    /// each minute, so a player's marker list and `zless` show when things
    /// happened without converting the file.
    fn mark_time(&mut self, at: Stamp) -> io::Result<()> {
        let minute = epoch_secs(at) / 60;
        if self.marked_minute == Some(minute) {
            return Ok(());
        }
        self.marked_minute = Some(minute);
        let label = self.identity.time_format.spell(self.clock.local(at));
        self.write_line(at, EventCode::Marker, &label)
    }

    fn write_line(&mut self, at: Stamp, code: EventCode, data: &str) -> io::Result<()> {
        let line = cast::event(self.elapsed(at), code, data);
        self.encoder.write_all(line.as_bytes())?;
        self.unflushed = true;
        Ok(())
    }

    fn roll_over(&mut self, at: Stamp) -> io::Result<()> {
        self.encoder.try_finish()?;
        let (part, path, file) = create_part(&self.identity, self.part + 1)?;
        self.part = part;
        self.path = path;
        self.encoder = GzEncoder::new(CountingFile::new(file), Compression::fast());
        self.origin_secs = epoch_secs(at);
        self.last_elapsed = Duration::ZERO;
        self.marked_minute = None;
        self.write_header()
    }

    fn release_partial_characters(&mut self, at: Stamp) -> io::Result<()> {
        if let Some(text) = self.output.finish() {
            self.write_event(at, EventCode::Output, &text)?;
        }
        if let Some(text) = self.input.finish() {
            self.write_event(at, EventCode::Input, &text)?;
        }
        Ok(())
    }

    fn write_header(&mut self) -> io::Result<()> {
        let header = cast::header(
            self.size.0,
            self.size.1,
            self.origin_secs,
            &self.identity.title,
        );
        self.encoder.write_all(header.as_bytes())?;
        self.unflushed = true;
        Ok(())
    }

    /// Seconds since `origin_secs` by the wall clock.
    ///
    /// Why not the monotonic clock: it stops while the machine sleeps, and
    /// `herdr log show` would then print times that are behind. Why clamp:
    /// asciicast times must not decrease when the clock is set back.
    fn elapsed(&mut self, at: Stamp) -> Duration {
        let since = at
            .wall
            .duration_since(UNIX_EPOCH + Duration::from_secs(self.origin_secs))
            .unwrap_or(Duration::ZERO);
        self.last_elapsed = self.last_elapsed.max(since);
        self.last_elapsed
    }
}

fn epoch_secs(at: Stamp) -> u64 {
    at.wall
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
}

pub(crate) fn part_path(identity: &CastIdentity, part: u32) -> PathBuf {
    let name = if part <= 1 {
        format!("{}.{FILE_EXTENSION}", identity.stem)
    } else {
        format!("{}.{part}.{FILE_EXTENSION}", identity.stem)
    };
    identity.dir.join(name)
}

/// Opens the first free part at or after `first`. Never reuses a file: an
/// existing name may hold an earlier recording or a link planted there.
fn create_part(identity: &CastIdentity, first: u32) -> io::Result<(u32, PathBuf, File)> {
    for part in first..first.saturating_add(MAX_NAME_ATTEMPTS) {
        let path = part_path(identity, part);
        // Owner-only on every platform: a plain create inherits the folder's
        // access rules on Windows, and recordings can hold typed passwords.
        match crate::platform::create_config_temporary(&path, true) {
            Ok(file) => return Ok((part, path, file)),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!(
            "no free recording file name for {}",
            part_path(identity, first).display()
        ),
    ))
}

/// Creates the missing folders of `dir` for this user only, and leaves the
/// permissions of folders that already exist as the user set them.
pub(super) fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    let mut missing = Vec::new();
    let mut cursor = Some(dir);
    while let Some(path) = cursor {
        if path.as_os_str().is_empty() || path.is_dir() {
            break;
        }
        missing.push(path);
        cursor = path.parent();
    }
    for path in missing.into_iter().rev() {
        match crate::platform::create_private_dir(path) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => {}
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

struct CountingFile {
    file: File,
    written: u64,
}

impl CountingFile {
    fn new(file: File) -> Self {
        Self { file, written: 0 }
    }

    fn written(&self) -> u64 {
        self.written
    }
}

impl Write for CountingFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.file.write(buf)?;
        self.written += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-recording-sink-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn identity(dir: &Path) -> CastIdentity {
        CastIdentity {
            dir: dir.to_path_buf(),
            stem: "20261124T063012Z-pane-3".into(),
            title: "herdr default pane 3".into(),
            time_format: TimeFormat::default(),
        }
    }

    /// 2026-11-18 11:06:40 UTC, on a whole second.
    fn start() -> Stamp {
        Stamp {
            mono: std::time::Instant::now(),
            wall: UNIX_EPOCH + Duration::from_secs(1_795_000_000),
        }
    }

    fn create(dir: &Path, part_limit_bytes: u64, at: Stamp) -> CastFile {
        let mut cast = CastFile::create(identity(dir), part_limit_bytes, 80, 24, at).unwrap();
        cast.clock = LocalClock::with_source(|| None);
        cast
    }

    fn gunzip(path: &Path) -> String {
        let mut text = String::new();
        flate2::read::MultiGzDecoder::new(File::open(path).unwrap())
            .read_to_string(&mut text)
            .unwrap();
        text
    }

    fn lines(path: &Path) -> Vec<serde_json::Value> {
        gunzip(path)
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn a_finished_file_is_gzip_with_a_header_and_timed_events() {
        let dir = scratch("finished");
        let start = start();
        let mut cast = create(&dir, u64::MAX, start);
        cast.write_bytes(start + Duration::from_millis(250), EventCode::Output, b"$ ")
            .unwrap();
        cast.write_bytes(
            start + Duration::from_millis(500),
            EventCode::Input,
            b"ls\r",
        )
        .unwrap();
        cast.resize(start + Duration::from_secs(1), 100, 30)
            .unwrap();
        let path = cast.finish(start + Duration::from_secs(2)).unwrap();

        assert_eq!(path, dir.join("20261124T063012Z-pane-3.cast.gz"));
        let lines = lines(&path);
        assert_eq!(lines[0]["width"], 80);
        assert_eq!(lines[0]["timestamp"], 1_795_000_000u64);
        assert_eq!(
            lines[1],
            serde_json::json!([0.25, "m", "[2026/11/18 11:06:40.250]"])
        );
        assert_eq!(lines[2], serde_json::json!([0.25, "o", "$ "]));
        assert_eq!(lines[3], serde_json::json!([0.5, "i", "ls\r"]));
        assert_eq!(lines[4], serde_json::json!([1.0, "r", "100x30"]));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_flushed_open_file_is_already_readable_by_gzip_readers() {
        let dir = scratch("flushed");
        let start = start();
        let mut cast = create(&dir, u64::MAX, start);
        cast.write_bytes(start, EventCode::Output, b"hello")
            .unwrap();
        cast.flush().unwrap();

        let mut text = String::new();
        // Why read_to_string may fail: the open file has no gzip trailer yet,
        // and the decoder reports that only after yielding the flushed data.
        let _ = flate2::read::GzDecoder::new(File::open(cast.path()).unwrap())
            .read_to_string(&mut text);
        assert!(text.contains("\"hello\""), "{text}");
        drop(cast);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn output_and_input_keep_their_own_partial_characters() {
        let dir = scratch("partial");
        let start = start();
        let mut cast = create(&dir, u64::MAX, start);
        let a = "あ".as_bytes();
        let i = "い".as_bytes();
        cast.write_bytes(start, EventCode::Output, &a[..1]).unwrap();
        cast.write_bytes(start, EventCode::Input, &i[..2]).unwrap();
        cast.write_bytes(start, EventCode::Output, &a[1..]).unwrap();
        cast.write_bytes(start, EventCode::Input, &i[2..]).unwrap();
        let path = cast.finish(start).unwrap();

        let lines = lines(&path);
        assert_eq!(lines[2][2], "あ");
        assert_eq!(lines[3][2], "い");
        assert_eq!(lines.len(), 4);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_resize_to_the_same_size_writes_nothing() {
        let dir = scratch("same-size");
        let start = start();
        let mut cast = create(&dir, u64::MAX, start);
        cast.resize(start, 80, 24).unwrap();
        let path = cast.finish(start).unwrap();
        assert_eq!(lines(&path).len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_part_over_its_limit_rolls_over_to_a_numbered_file_with_its_own_header() {
        let dir = scratch("roll-over");
        let start = start();
        let mut cast = create(&dir, 1, start);
        cast.write_bytes(start + Duration::from_secs(5), EventCode::Output, b"one")
            .unwrap();
        cast.write_bytes(start + Duration::from_secs(6), EventCode::Output, b"two")
            .unwrap();
        let last = cast.finish(start + Duration::from_secs(7)).unwrap();

        let first = dir.join("20261124T063012Z-pane-3.cast.gz");
        let second = dir.join("20261124T063012Z-pane-3.2.cast.gz");
        assert_eq!(lines(&first)[2][2], "one");
        assert_eq!(lines(&second)[0]["version"], 2);
        assert_eq!(lines(&second)[0]["timestamp"], 1_795_000_005u64);
        assert_eq!(lines(&second)[1][1], "m");
        assert_eq!(lines(&second)[2], serde_json::json!([1.0, "o", "two"]));
        assert_eq!(last, dir.join("20261124T063012Z-pane-3.3.cast.gz"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_existing_file_is_never_overwritten() {
        let dir = scratch("existing");
        std::fs::create_dir_all(&dir).unwrap();
        let taken = dir.join("20261124T063012Z-pane-3.cast.gz");
        std::fs::write(&taken, b"earlier").unwrap();
        let start = start();
        let cast = create(&dir, u64::MAX, start);
        assert_eq!(cast.path(), dir.join("20261124T063012Z-pane-3.2.cast.gz"));
        assert_eq!(std::fs::read(&taken).unwrap(), b"earlier");
        drop(cast);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn new_folders_and_files_are_for_their_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let root = scratch("private");
        let dir = root.join("logs").join("default");
        let start = start();
        let cast = create(&dir, u64::MAX, start);
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&root), 0o700);
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(cast.path()), 0o600);
        drop(cast);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn an_existing_folder_keeps_the_permissions_the_user_gave_it() {
        use std::os::unix::fs::PermissionsExt;
        let root = scratch("existing-folder");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o750)).unwrap();
        ensure_private_dir(&root.join("default")).unwrap();
        let mode = std::fs::metadata(&root).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o750);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_time_marker_is_written_once_per_minute_in_local_time() {
        let dir = scratch("minutes");
        let start = start();
        let mut cast = create(&dir, u64::MAX, start);
        cast.clock = LocalClock::with_source(|| Some(9 * 3600));
        cast.write_bytes(start, EventCode::Output, b"a").unwrap();
        cast.write_bytes(start + Duration::from_secs(10), EventCode::Output, b"b")
            .unwrap();
        cast.write_bytes(start + Duration::from_secs(20), EventCode::Output, b"c")
            .unwrap();
        let path = cast.finish(start + Duration::from_secs(21)).unwrap();

        let events: Vec<_> = lines(&path).into_iter().skip(1).collect();
        assert_eq!(
            events,
            vec![
                serde_json::json!([0.0, "m", "[2026/11/18 20:06:40.000]"]),
                serde_json::json!([0.0, "o", "a"]),
                serde_json::json!([10.0, "o", "b"]),
                serde_json::json!([20.0, "m", "[2026/11/18 20:07:00.000]"]),
                serde_json::json!([20.0, "o", "c"]),
            ]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn event_times_count_from_the_whole_second_in_the_header() {
        let dir = scratch("origin");
        let start = start() + Duration::from_millis(700);
        let mut cast = create(&dir, u64::MAX, start);
        cast.write_bytes(start + Duration::from_millis(100), EventCode::Output, b"x")
            .unwrap();
        let path = cast.finish(start).unwrap();

        let lines = lines(&path);
        assert_eq!(lines[0]["timestamp"], 1_795_000_000u64);
        assert_eq!(lines[2], serde_json::json!([0.8, "o", "x"]));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn event_times_never_step_back_when_the_clock_is_set_back() {
        let dir = scratch("set-back");
        let start = start() + Duration::from_secs(5);
        let mut cast = create(&dir, u64::MAX, start);
        cast.write_bytes(start, EventCode::Output, b"before")
            .unwrap();
        let earlier = Stamp {
            wall: start.wall - Duration::from_secs(3),
            ..start
        };
        cast.write_bytes(earlier, EventCode::Output, b"after")
            .unwrap();
        let path = cast.finish(start).unwrap();

        let times: Vec<_> = lines(&path)
            .into_iter()
            .skip(1)
            .filter(|event| event[1] == "o")
            .map(|event| event[0].as_f64().unwrap())
            .collect();
        assert_eq!(times, vec![0.0, 0.0]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
