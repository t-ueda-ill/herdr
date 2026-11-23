//! Prompt tasks, stored at `tasks.json` beside the session. Why not inside
//! session.json: its snapshot history would copy prompts and results into
//! every backup.

use std::io::Write;
use std::path::{Path, PathBuf};

use tracing::warn;

use crate::tasks::record::{TaskFile, TASK_FILE_VERSION};

pub(crate) fn tasks_path() -> PathBuf {
    crate::session::data_dir().join("tasks.json")
}

#[derive(Debug)]
pub(crate) enum LoadedTasks {
    Loaded(TaskFile),
    /// No file yet, or one this herdr cannot parse: a save may replace it.
    Empty,
    /// A newer herdr wrote it: keep it as it is.
    Newer,
    /// It exists but could not be read: keep it, so a later save does not
    /// replace the history with an empty table.
    Unreadable,
}

pub(crate) fn load_tasks() -> LoadedTasks {
    load_tasks_from(&tasks_path())
}

pub(crate) fn save_tasks(file: &TaskFile) -> std::io::Result<()> {
    save_tasks_to(&tasks_path(), file)
}

fn load_tasks_from(path: &Path) -> LoadedTasks {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return LoadedTasks::Empty,
        Err(err) => {
            warn!(
                path = %path.display(), err = %err,
                "failed to read the task file; tasks will not be saved"
            );
            return LoadedTasks::Unreadable;
        }
    };
    let version = serde_json::from_str::<serde_json::Value>(&content)
        .ok()
        .and_then(|value| value.get("version")?.as_u64());
    if version.is_some_and(|version| version > u64::from(TASK_FILE_VERSION)) {
        warn!(
            path = %path.display(), file_version = version, supported = TASK_FILE_VERSION,
            "the task file is from a newer herdr; tasks will not be saved"
        );
        return LoadedTasks::Newer;
    }
    match serde_json::from_str(&content) {
        Ok(file) => LoadedTasks::Loaded(file),
        Err(err) => {
            warn!(path = %path.display(), err = %err, "failed to parse the task file, ignoring");
            LoadedTasks::Empty
        }
    }
}

/// Only this user can read it: it holds prompts and results.
fn save_tasks_to(path: &Path, file: &TaskFile) -> std::io::Result<()> {
    let target = super::io::resolve_write_target(path)?;
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let json = serde_json::to_vec(file)?;
    let temp_path = target.with_extension("json.tmp");
    let _ = std::fs::remove_file(&temp_path);
    let mut temp = crate::platform::create_private_state_file(&temp_path)?;
    let written = temp.write_all(&json).and_then(|()| temp.sync_all());
    drop(temp);
    if let Err(err) = written.and_then(|()| crate::platform::replace_file(&temp_path, &target)) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(err);
    }
    crate::platform::sync_parent_directory(parent)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("herdr-task-file-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("tasks.json")
    }

    #[test]
    fn a_saved_file_loads_back() {
        let path = scratch("roundtrip");
        let file = crate::tasks::TaskTable::default().to_file();

        save_tasks_to(&path, &file).unwrap();
        let loaded = load_tasks_from(&path);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        let LoadedTasks::Loaded(loaded) = loaded else {
            panic!("expected a loaded file: {loaded:?}");
        };
        assert_eq!(loaded.version, TASK_FILE_VERSION);
    }

    #[test]
    fn a_missing_file_loads_as_empty() {
        assert!(matches!(
            load_tasks_from(&scratch("missing")),
            LoadedTasks::Empty
        ));
    }

    #[test]
    fn a_broken_file_loads_as_empty() {
        let path = scratch("broken");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{not json").unwrap();

        let loaded = load_tasks_from(&path);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        assert!(matches!(loaded, LoadedTasks::Empty));
    }

    #[test]
    fn a_file_that_cannot_be_read_is_kept() {
        let path = scratch("unreadable");
        // A folder where the file belongs fails the read with no NotFound.
        std::fs::create_dir_all(&path).unwrap();

        let loaded = load_tasks_from(&path);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        assert!(matches!(loaded, LoadedTasks::Unreadable));
    }

    #[test]
    fn a_file_from_a_newer_herdr_is_reported_as_newer() {
        let path = scratch("newer");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"version":99,"next_id":3,"tasks":[],"more":1}"#).unwrap();

        let loaded = load_tasks_from(&path);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        assert!(matches!(loaded, LoadedTasks::Newer));
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_readable_by_its_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let path = scratch("private");

        save_tasks_to(&path, &crate::tasks::TaskTable::default().to_file()).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        assert_eq!(mode, 0o600);
    }
}
