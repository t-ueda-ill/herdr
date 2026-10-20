//! Where an upload's bytes go. The session worker drives one sink in order:
//! `begin`, then entries (`start`, `data`, `end_file`), then `commit` or
//! `abort`. Every call blocks; only the worker thread makes them.

use crate::api::schema::PaneUploadCollision;

use super::path::Failure;
use super::session::{DirectoryAttrs, EntryStart};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Began {
    /// The physical destination directory.
    pub(crate) destination: String,
    /// Begin names that already exist there.
    pub(crate) conflicts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Committed {
    /// Every name was published; `paths` pairs each name with its final path.
    Published {
        paths: Vec<(String, String)>,
        warnings: Vec<String>,
    },
    /// `collision = fail` met these existing names. Nothing was published and
    /// the stage stays, so another commit may follow.
    Conflict(Vec<String>),
}

pub(crate) trait UploadSink: Send {
    /// Resolves the destination, creates the stage, and reports which names exist.
    fn begin(&mut self, names: &[String]) -> Result<Began, Failure>;
    fn start(&mut self, entry: &EntryStart) -> Result<(), Failure>;
    /// Appends to the file the last `start` opened.
    fn data(&mut self, bytes: &[u8]) -> Result<(), Failure>;
    fn end_file(&mut self) -> Result<(), Failure>;
    /// Applies directory attributes and publishes everything at once. A failure
    /// has already rolled back and removed the stage, unless its message says
    /// where the stage was kept.
    fn commit(
        &mut self,
        collision: PaneUploadCollision,
        nested: &[DirectoryAttrs],
        top: &[DirectoryAttrs],
    ) -> Result<Committed, Failure>;
    /// Removes the stage. Best effort: called after failures and on cancel.
    fn abort(&mut self);
}

pub(crate) fn upload_failed(message: impl Into<String>) -> Failure {
    ("upload_failed", message.into())
}

/// Refuses a destination whose physical path holds a control character.
/// Why not escape it later: the published paths are pasted into a pane, where
/// `ESC [201~` ends a bracketed paste and a newline submits a prompt, and
/// quoting means nothing to an agent's input box.
pub(crate) fn check_resolved_destination(path: &str) -> Result<(), Failure> {
    if path.chars().any(char::is_control) {
        return Err((
            "invalid_params",
            format!("the destination contains control characters: {path:?}"),
        ));
    }
    Ok(())
}

/// Q4: `name (1).ext` for files, `name (1)` for directories, symlinks, and
/// names whose only dot is the leading one.
pub(crate) fn numbered_name(name: &str, number: u32, split_extension: bool) -> String {
    if split_extension {
        if let Some(dot) = name.rfind('.').filter(|dot| *dot > 0) {
            let (stem, ext) = name.split_at(dot);
            return format!("{stem} ({number}){ext}");
        }
    }
    format!("{name} ({number})")
}

/// How many numbered names a rename tries before giving up, as in the receiver.
pub(crate) const MAX_RENAME_NUMBER: u32 = 9_999;

const KEPT_STAGE: &str = "could not be fully undone";

/// Rollback could not restore everything, so the stage keeps what it holds.
pub(crate) fn kept_stage_failure(stage: &str) -> Failure {
    upload_failed(format!(
        "publishing failed and {KEPT_STAGE}; replaced originals and \
         unpublished files were kept in {stage}"
    ))
}

/// Whether `failure` left its stage in place on purpose.
pub(crate) fn is_kept_stage(failure: &Failure) -> bool {
    failure.1.contains(KEPT_STAGE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbered_names_keep_the_last_extension_of_files_only() {
        assert_eq!(numbered_name("a.txt", 1, true), "a (1).txt");
        assert_eq!(numbered_name("a.tar.gz", 2, true), "a.tar (2).gz");
        assert_eq!(numbered_name(".bashrc", 1, true), ".bashrc (1)");
        assert_eq!(numbered_name("noext", 1, true), "noext (1)");
        assert_eq!(numbered_name("dir.d", 1, false), "dir.d (1)");
    }
}
