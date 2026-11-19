//! Where a preview's files come from: this machine's filesystem, or a fixed
//! POSIX `sh` script run in an ssh or container pane's environment.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Largest file sent whole. Off-host output passes through one process pipe,
/// so a bigger file is only served in ranges.
pub(crate) const WHOLE_LIMIT: u64 = 32 * 1024 * 1024;
/// Largest range sent in one response; a media player asks for the rest.
pub(crate) const SLICE_LIMIT: u64 = 8 * 1024 * 1024;
/// Paths change detection follows, at most. Each is one argument of the
/// off-host script, so the list stays far below a command line's limit.
pub(crate) const WATCH_PATH_LIMIT: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Window {
    /// The whole file, refused above [`WHOLE_LIMIT`].
    Whole,
    /// Only the size.
    Probe,
    Slice {
        start: u64,
        len: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileBytes {
    pub(crate) size: u64,
    pub(crate) bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReadError {
    NotFound,
    Directory,
    /// Unreadable, or a link that leads outside the preview root.
    Forbidden,
    TooLarge,
    Failed(String),
}

pub(crate) trait FileSource: Send + Sync + 'static {
    /// `relative` is already free of `..` and empty components.
    fn read(&self, relative: &str, window: Window) -> Result<FileBytes, ReadError>;
    /// Changes whenever one of `paths` (relative, as served) appears,
    /// disappears, or is written.
    ///
    /// Why only served paths instead of the whole tree: a preview root is often
    /// a repository whose build output churns and holds more files than a
    /// listing can afford every second, especially in a remote environment.
    fn fingerprint(&self, paths: &[String]) -> Result<String, ReadError>;
}

pub(crate) struct LocalSource {
    root: PathBuf,
}

impl LocalSource {
    pub(crate) fn new(root: &Path) -> std::io::Result<Self> {
        let root = std::fs::canonicalize(root)?;
        if !root.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("not a directory: {}", root.display()),
            ));
        }
        Ok(Self { root })
    }
}

fn io_error(error: std::io::Error) -> ReadError {
    match error.kind() {
        std::io::ErrorKind::NotFound => ReadError::NotFound,
        std::io::ErrorKind::PermissionDenied => ReadError::Forbidden,
        _ => ReadError::Failed(error.to_string()),
    }
}

impl FileSource for LocalSource {
    fn read(&self, relative: &str, window: Window) -> Result<FileBytes, ReadError> {
        let path = std::fs::canonicalize(self.root.join(relative)).map_err(io_error)?;
        if !path.starts_with(&self.root) {
            return Err(ReadError::Forbidden);
        }
        let metadata = std::fs::metadata(&path).map_err(io_error)?;
        if metadata.is_dir() {
            return Err(ReadError::Directory);
        }
        if !metadata.is_file() {
            return Err(ReadError::NotFound);
        }
        let size = metadata.len();
        let (start, len) = match window {
            Window::Probe => {
                return Ok(FileBytes {
                    size,
                    bytes: Vec::new(),
                })
            }
            Window::Whole if size > WHOLE_LIMIT => return Err(ReadError::TooLarge),
            Window::Whole => (0, size),
            Window::Slice { start, len } => (start, len.min(SLICE_LIMIT)),
        };
        let mut file = File::open(&path).map_err(io_error)?;
        file.seek(SeekFrom::Start(start)).map_err(io_error)?;
        let mut bytes = Vec::new();
        file.take(len).read_to_end(&mut bytes).map_err(io_error)?;
        Ok(FileBytes { size, bytes })
    }

    fn fingerprint(&self, paths: &[String]) -> Result<String, ReadError> {
        use std::hash::{Hash, Hasher};
        if !self.root.is_dir() {
            return Err(ReadError::NotFound);
        }
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for path in paths {
            path.hash(&mut hasher);
            match std::fs::metadata(self.root.join(path)) {
                Ok(metadata) => {
                    metadata.len().hash(&mut hasher);
                    metadata
                        .modified()
                        .ok()
                        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                        .map(|age| age.as_nanos())
                        .hash(&mut hasher);
                }
                Err(_) => "missing".hash(&mut hasher),
            }
        }
        Ok(hasher.finish().to_string())
    }
}

pub(crate) const MARKER: &[u8] = b"HERDR-PREVIEW-1\0";

/// `$1` root, `$2` relative path, `$3` offset, `$4` byte count, `$5` one of
/// `whole`, `probe`, `slice`.
///
/// Prints the marker, then one refusal token (`enoent`, `dir`, `notfile`,
/// `eacces`, `escape`, `toolarge`), or `size` and the size followed by the
/// requested bytes to the end of the output.
///
/// Why `cd -P` for the folder and `readlink -f` only for a linked file: `cd`
/// is in every POSIX shell, and an environment without `readlink -f` only
/// loses linked files, never lets one out of the root.
pub(crate) const READ_SCRIPT: &str = r#"
printf 'HERDR-PREVIEW-1\0'
f="$1/$2"
if [ ! -e "$f" ]; then printf 'enoent\0'; exit 0; fi
if [ -d "$f" ]; then printf 'dir\0'; exit 0; fi
if [ ! -f "$f" ]; then printf 'notfile\0'; exit 0; fi
if [ ! -r "$f" ]; then printf 'eacces\0'; exit 0; fi
root=$(cd -P -- "$1" 2>/dev/null && pwd -P) || { printf 'enoent\0'; exit 0; }
dir=$(cd -P -- "$(dirname -- "$f")" 2>/dev/null && pwd -P) || { printf 'enoent\0'; exit 0; }
if [ "$root" != / ]; then
  case "$dir/" in "$root"/*) ;; *) printf 'escape\0'; exit 0 ;; esac
  if [ -L "$f" ]; then
    target=$(readlink -f -- "$f" 2>/dev/null) || { printf 'escape\0'; exit 0; }
    case "$target" in "$root"/*) ;; *) printf 'escape\0'; exit 0 ;; esac
  fi
fi
size=$(wc -c < "$f" | tr -d ' ')
case "$5" in
  whole) if [ "$size" -gt "$4" ]; then printf 'toolarge\0'; exit 0; fi ;;
esac
printf 'size\0%s\0' "$size"
case "$5" in
  probe) exit 0 ;;
  whole) cat -- "$f" ;;
  *) if [ "$3" -gt 0 ]; then tail -c +"$(($3 + 1))" -- "$f" | head -c "$4"; else head -c "$4" -- "$f"; fi ;;
esac
"#;

/// `$1` root, then the relative paths to follow. Prints the marker and
/// `enoent`, or `sum` and the checksum of each existing path's change time,
/// size and name; a missing path simply leaves its line out.
///
/// Why `stat -c` with a fallback: GNU and BusyBox give sub-second times that
/// way; elsewhere `ls -ln` still notices size changes and minute changes.
pub(crate) const FINGERPRINT_SCRIPT: &str = r#"
printf 'HERDR-PREVIEW-1\0'
cd -- "$1" 2>/dev/null || { printf 'enoent\0'; exit 0; }
shift
printf 'sum\0'
if stat -c '%y' . >/dev/null 2>&1; then
  stat -c '%y %s %n' -- "$@" 2>/dev/null
else
  ls -ln -- "$@" 2>/dev/null
fi | cksum
"#;

/// Runs a script in the pane's environment with positional arguments.
pub(crate) type ScriptRun = dyn Fn(&str, &[&str]) -> Result<Vec<u8>, String> + Send + Sync;

pub(crate) struct OffHostSource {
    root: String,
    run: Box<ScriptRun>,
}

impl OffHostSource {
    pub(crate) fn new(root: String, run: Box<ScriptRun>) -> Self {
        Self { root, run }
    }
}

fn after_marker(output: &[u8]) -> Result<&[u8], ReadError> {
    let start = output
        .windows(MARKER.len())
        .position(|window| window == MARKER)
        .ok_or_else(|| ReadError::Failed("unexpected script output".into()))?;
    Ok(&output[start + MARKER.len()..])
}

fn token(output: &[u8]) -> Result<(&[u8], &[u8]), ReadError> {
    let end = output
        .iter()
        .position(|byte| *byte == 0)
        .ok_or_else(|| ReadError::Failed("truncated script output".into()))?;
    Ok((&output[..end], &output[end + 1..]))
}

pub(crate) fn parse_read(output: &[u8]) -> Result<FileBytes, ReadError> {
    let (first, rest) = token(after_marker(output)?)?;
    match first {
        b"enoent" | b"notfile" => Err(ReadError::NotFound),
        b"dir" => Err(ReadError::Directory),
        b"eacces" | b"escape" => Err(ReadError::Forbidden),
        b"toolarge" => Err(ReadError::TooLarge),
        b"size" => {
            let (size, bytes) = token(rest)?;
            let size = std::str::from_utf8(size)
                .ok()
                .and_then(|size| size.parse().ok())
                .ok_or_else(|| ReadError::Failed("bad size in script output".into()))?;
            Ok(FileBytes {
                size,
                bytes: bytes.to_vec(),
            })
        }
        _ => Err(ReadError::Failed("unknown script record".into())),
    }
}

pub(crate) fn parse_fingerprint(output: &[u8]) -> Result<String, ReadError> {
    let (first, rest) = token(after_marker(output)?)?;
    match first {
        b"enoent" => Err(ReadError::NotFound),
        b"sum" => Ok(String::from_utf8_lossy(rest).trim().to_owned()),
        _ => Err(ReadError::Failed("unknown script record".into())),
    }
}

impl FileSource for OffHostSource {
    fn read(&self, relative: &str, window: Window) -> Result<FileBytes, ReadError> {
        let (offset, count, mode) = match window {
            Window::Whole => (0, WHOLE_LIMIT, "whole"),
            Window::Probe => (0, 0, "probe"),
            Window::Slice { start, len } => (start, len.min(SLICE_LIMIT), "slice"),
        };
        let (offset, count) = (offset.to_string(), count.to_string());
        let output = (self.run)(READ_SCRIPT, &[&self.root, relative, &offset, &count, mode])
            .map_err(ReadError::Failed)?;
        parse_read(&output)
    }

    fn fingerprint(&self, paths: &[String]) -> Result<String, ReadError> {
        let args: Vec<&str> = std::iter::once(self.root.as_str())
            .chain(paths.iter().map(String::as_str))
            .collect();
        let output = (self.run)(FINGERPRINT_SCRIPT, &args).map_err(ReadError::Failed)?;
        parse_fingerprint(&output)
    }
}

#[cfg(test)]
mod tests;
