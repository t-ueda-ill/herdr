//! `files.stat`: what the details band shows for one selected path.
use super::*;

/// Counting stops here so a huge directory cannot hold a worker.
pub(crate) const CHILDREN_LIMIT: u64 = 10_000;

pub(crate) fn stat(params: FileStatParams) -> Result<ResponseResult> {
    let path = absolute(&params.path)?;
    let metadata = fs::symlink_metadata(&path).map_err(failure)?;
    let file_type = metadata.file_type();
    let kind = if file_type.is_symlink() {
        DirectoryEntryKind::Symlink
    } else if file_type.is_dir() {
        DirectoryEntryKind::Dir
    } else if file_type.is_file() {
        DirectoryEntryKind::File
    } else {
        DirectoryEntryKind::Other
    };
    let mtime = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok());
    // A link to a directory counts the directory it reaches, like `ls dir/`.
    let children = fs::metadata(&path)
        .is_ok_and(|target| target.is_dir())
        .then(|| count_children(&path))
        .flatten();
    Ok(ResponseResult::FileStat {
        stat: FileStat {
            path: params.path,
            kind: Some(kind),
            size: file_type.is_file().then_some(metadata.len()),
            mtime,
            mode: crate::platform::display_mode(&metadata),
            owner: crate::platform::owner_name(&metadata),
            children: children.map(|(count, _)| count),
            children_limited: children.is_some_and(|(_, limited)| limited),
            link_target: file_type
                .is_symlink()
                .then(|| fs::read_link(&path).ok())
                .flatten()
                .map(|target| crate::platform::file_path_display(&target)),
        },
    })
}

fn count_children(path: &Path) -> Option<(u64, bool)> {
    let mut count = 0;
    for entry in fs::read_dir(path).ok()? {
        let Ok(entry) = entry else { continue };
        // Listings never show `.git`, so the count leaves it out too.
        if entry.file_name() == ".git" {
            continue;
        }
        if count == CHILDREN_LIMIT {
            return Some((count, true));
        }
        count += 1;
    }
    Some((count, false))
}
