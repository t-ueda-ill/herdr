use super::*;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
static SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// Create a sibling and rename it only after a second revision check. Preserve
/// permissions, and never follow symlinks for a write target.
fn write_revision(path: &Path, expected: &str, text: &str) -> Result<String> {
    let metadata = fs::symlink_metadata(path).map_err(failure)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(failure(
            "replacement requires a regular file, not a symlink",
        ));
    }
    if text.len() as u64 > MAX_TEXT_BYTES {
        return Err(failure("replacement exceeds text limit"));
    }
    let original = read(path)?;
    if revision(original.as_bytes()) != expected {
        return Err(failure("file changed; refresh before replacing"));
    }
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary =
        path.with_file_name(format!(".herdr-replace-{}-{sequence}", std::process::id()));
    // Cleanup is allowed only after create_new established ownership.
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(failure)?;
    let result = (|| {
        file.set_permissions(metadata.permissions())
            .map_err(failure)?;
        file.write_all(text.as_bytes()).map_err(failure)?;
        file.sync_all().map_err(failure)?;
        drop(file);
        if fs::symlink_metadata(path)
            .map_err(failure)?
            .file_type()
            .is_symlink()
            || revision(read(path)?.as_bytes()) != expected
        {
            return Err(failure("file changed during replacement; retry"));
        }
        crate::platform::replace_file(&temporary, path).map_err(failure)?;
        Ok(revision(text.as_bytes()))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub(crate) fn replace(params: FilesReplaceParams) -> Result<ResponseResult> {
    if params.changes.len() > 1_000 {
        return Err(failure("at most 1000 files per replacement"));
    }
    let mut seen = std::collections::HashSet::new();
    let mut outcomes = Vec::new();
    for change in params.changes {
        let result = absolute(&change.path).and_then(|path| {
            let canonical = path.canonicalize().map_err(failure)?;
            if !seen.insert(canonical) {
                return Err(failure("duplicate replacement target"));
            }
            write_revision(&path, &change.revision, &change.text)
        });
        let (revision, error) = match result {
            Ok(r) => (Some(r), None),
            Err(e) => (None, Some(e.1)),
        };
        outcomes.push(FileOutcome {
            path: change.path,
            destination: None,
            error,
            revision,
        });
    }
    Ok(ResponseResult::FilesChanged { outcomes })
}
fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}
fn remove(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(failure)?;
    if metadata.is_dir() {
        fs::remove_dir_all(path).map_err(failure)
    } else {
        fs::remove_file(path).map_err(failure)
    }
}
fn copy(source: &Path, target: &Path, budget: &mut usize) -> Result<()> {
    if *budget == 0 {
        return Err(failure("copy exceeds 50000 entries"));
    }
    *budget -= 1;
    let metadata = fs::symlink_metadata(source).map_err(failure)?;
    if metadata.file_type().is_symlink() {
        return crate::platform::copy_symbolic_link(source, target).map_err(failure);
    }
    if metadata.is_dir() {
        fs::create_dir(target).map_err(failure)?;
        for entry in fs::read_dir(source).map_err(failure)? {
            let entry = entry.map_err(failure)?;
            copy(&entry.path(), &target.join(entry.file_name()), budget)?;
        }
        fs::set_permissions(target, metadata.permissions()).map_err(failure)?;
    } else if metadata.is_file() {
        let mut destination = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(target)
            .map_err(failure)?;
        let mut input = fs::File::open(source).map_err(failure)?;
        std::io::copy(&mut input, &mut destination).map_err(failure)?;
        destination
            .set_permissions(metadata.permissions())
            .map_err(failure)?;
    } else {
        return Err(failure("unsupported file type"));
    }
    Ok(())
}
fn destination(params: &FilesOperateParams, source: &Path) -> Result<PathBuf> {
    let requested = absolute(
        params
            .destination
            .as_deref()
            .ok_or_else(|| failure("destination is required"))?,
    )?;
    let mut target = if requested.is_dir() {
        requested.join(
            source
                .file_name()
                .ok_or_else(|| failure("cannot copy a root directory"))?,
        )
    } else {
        requested
    };
    if target == source {
        return Err(failure("source and destination are the same"));
    }
    let parent = target
        .parent()
        .ok_or_else(|| failure("destination parent missing"))?
        .canonicalize()
        .map_err(failure)?;
    let canonical_source = source.canonicalize().map_err(failure)?;
    if parent.starts_with(&canonical_source) {
        return Err(failure("cannot copy or move a directory inside itself"));
    }
    if exists(&target) {
        if target.canonicalize().map_err(failure)? == canonical_source {
            return Err(failure("source and destination identify the same file"));
        }
        match params.collision {
            FileCollision::Skip => return Err(failure("skipped: destination exists")),
            FileCollision::Rename => {
                let original = target.clone();
                let name = source
                    .file_name()
                    .ok_or_else(|| failure("missing filename"))?
                    .to_string_lossy();
                let mut available = false;
                for index in 1..10_000 {
                    target = original.with_file_name(format!("{name} (copy {index})"));
                    if !exists(&target) {
                        available = true;
                        break;
                    }
                }
                if !available {
                    return Err(failure("no available copy name"));
                }
            }
            FileCollision::Replace => {
                if !params.confirmed {
                    return Err(failure("replacement requires confirmation"));
                }
                if !fs::symlink_metadata(&target).map_err(failure)?.is_file()
                    || !fs::symlink_metadata(source).map_err(failure)?.is_file()
                {
                    return Err(failure(
                        "directories and symlinks cannot be replaced or merged",
                    ));
                }
                // Never truncate an existing destination before copy succeeds.
            }
        }
    }
    Ok(target)
}
pub(crate) fn operate(params: FilesOperateParams) -> Result<ResponseResult> {
    if params.paths.len() > 1_000 {
        return Err(failure("at most 1000 source items per operation"));
    }
    if matches!(
        params.operation,
        FileOperation::Delete | FileOperation::Trash
    ) && !params.confirmed
    {
        return Err(failure("deletion requires confirmation"));
    }
    if params.paths.len() > 1
        && matches!(params.operation, FileOperation::Copy | FileOperation::Move)
        && !params
            .destination
            .as_deref()
            .is_some_and(|p| Path::new(p).is_dir())
    {
        return Err(failure(
            "multiple sources require an existing destination directory",
        ));
    }
    let mut outcomes = Vec::new();
    for source in &params.paths {
        let result = (|| {
            let path = absolute(source)?;
            match params.operation {
                FileOperation::CreateFile => {
                    fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&path)
                        .map_err(failure)?;
                    Ok(None)
                }
                FileOperation::CreateDirectory => {
                    fs::create_dir(&path).map_err(failure)?;
                    Ok(None)
                }
                FileOperation::Trash => {
                    crate::platform::trash_file(&path).map_err(|e| ("trash_unavailable", format!("trash unavailable: {e}; permanent deletion requires a separate confirmation")))?;
                    Ok(None)
                }
                FileOperation::Delete => {
                    remove(&path)?;
                    Ok(None)
                }
                FileOperation::Copy | FileOperation::Move => {
                    let target = destination(&params, &path)?;
                    if params.operation == FileOperation::Move {
                        if exists(&target) {
                            if params.collision != FileCollision::Replace || !params.confirmed {
                                return Err(failure("destination appeared; retry and confirm"));
                            }
                            crate::platform::replace_file(&path, &target).map_err(failure)?;
                        } else {
                            crate::platform::rename_no_replace(&path,&target).map_err(|e| failure(format!("move failed (cross-volume moves require copy then explicit delete): {e}")))?;
                        }
                    } else {
                        let stage = target.with_file_name(format!(
                            ".herdr-copy-{}-{}",
                            std::process::id(),
                            SEQUENCE.fetch_add(1, Ordering::Relaxed)
                        ));
                        fs::create_dir(&stage).map_err(failure)?;
                        let staged_file = stage.join("item");
                        if let Err(e) = copy(&path, &staged_file, &mut 50_000) {
                            let _ = remove(&stage);
                            return Err(e);
                        }
                        let publish = if params.collision == FileCollision::Replace
                            && params.confirmed
                            && exists(&target)
                        {
                            if !fs::symlink_metadata(&target).map_err(failure)?.is_file() {
                                let _ = remove(&stage);
                                return Err(failure("destination changed to a non-file"));
                            }
                            crate::platform::replace_file(&staged_file, &target)
                        } else {
                            crate::platform::rename_no_replace(&staged_file, &target)
                        };
                        if let Err(e) = publish {
                            let _ = remove(&stage);
                            return Err(failure(e));
                        }
                        let _ = fs::remove_dir(&stage);
                    }
                    Ok(Some(target.to_string_lossy().into_owned()))
                }
            }
        })();
        let (destination, error) = match result {
            Ok(p) => (p, None),
            Err(e) => (None, Some(e.1)),
        };
        outcomes.push(FileOutcome {
            path: source.clone(),
            destination,
            error,
            revision: None,
        });
    }
    Ok(ResponseResult::FilesChanged { outcomes })
}

pub(crate) fn plan(mut params: FilesOperateParams) -> Result<ResponseResult> {
    if params.paths.len() > 1000 {
        return Err(failure("too many source items"));
    }
    params.confirmed = true;
    let outcomes = params
        .paths
        .iter()
        .map(|source| {
            let result = absolute(source).and_then(|path| {
                if matches!(params.operation, FileOperation::Copy | FileOperation::Move) {
                    destination(&params, &path).map(|p| Some(p.to_string_lossy().into_owned()))
                } else if matches!(
                    params.operation,
                    FileOperation::CreateFile | FileOperation::CreateDirectory
                ) {
                    if exists(&path) {
                        Err(failure("destination exists"))
                    } else {
                        Ok(None)
                    }
                } else {
                    fs::symlink_metadata(&path).map_err(failure)?;
                    Ok(None)
                }
            });
            let (destination, error) = match result {
                Ok(p) => (p, None),
                Err(e) => (None, Some(e.1)),
            };
            FileOutcome {
                path: source.clone(),
                destination,
                error,
                revision: None,
            }
        })
        .collect();
    Ok(ResponseResult::FilesPlanned { outcomes })
}
