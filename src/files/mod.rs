//! Host filesystem service. All calls run on bounded workers, never in render/layout.
mod mutations;
mod search;
#[cfg(test)]
mod tests;

use crate::api::schema::*;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

pub(crate) const MAX_TEXT_BYTES: u64 = 8 * 1024 * 1024;
pub(crate) const MAX_MATCHES: usize = 10_000;
pub(crate) type Failure = (&'static str, String);
pub(crate) type Result<T> = std::result::Result<T, Failure>;

pub(crate) fn failure(error: impl std::fmt::Display) -> Failure {
    ("file_error", error.to_string())
}
pub(crate) fn absolute(path: &str) -> Result<PathBuf> {
    let path = PathBuf::from(path);
    if !path.is_absolute() {
        return Err(failure("an absolute host path is required"));
    }
    Ok(path)
}
pub(crate) fn revision(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub(crate) fn read(path: &Path) -> Result<String> {
    if !fs::metadata(path).map_err(failure)?.is_file() {
        return Err(failure("not a regular file"));
    }
    let file = crate::platform::open_text_file(path).map_err(failure)?;
    if !file.metadata().map_err(failure)?.is_file() {
        return Err(failure("not a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_TEXT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(failure)?;
    validate_text(bytes)
}
pub(crate) const TOO_LARGE_MESSAGE: &str =
    "file exceeds the 8 MiB text limit; open in external editor";
/// The text checks shared by the host and off-host readers, so a file is
/// accepted or refused with the same words wherever it lives.
pub(crate) fn validate_text(bytes: Vec<u8>) -> Result<String> {
    if bytes.len() as u64 > MAX_TEXT_BYTES {
        return Err(failure(TOO_LARGE_MESSAGE));
    }
    if bytes.contains(&0) {
        return Err(failure("binary file; open in external editor"));
    }
    String::from_utf8(bytes).map_err(|_| failure("file is not UTF-8; open in external editor"))
}
pub(crate) fn read_document(params: FileReadParams) -> Result<ResponseResult> {
    let text = read(&absolute(&params.path)?)?;
    let revision = revision(text.as_bytes());
    let unchanged = params.revision.as_deref() == Some(&revision);
    Ok(ResponseResult::FileDocument {
        document: FileDocument {
            path: params.path,
            revision,
            text: if unchanged { String::new() } else { text },
            unchanged,
        },
    })
}
pub(crate) use mutations::{operate, plan, replace};
pub(crate) use search::search;
