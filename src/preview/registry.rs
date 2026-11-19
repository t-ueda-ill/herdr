//! The previews the server is running, keyed by folder and place.

use crate::api::schema::{PaneLocation, PreviewInfo};

use super::server::PreviewServer;

struct Entry {
    id: String,
    root: String,
    location: Option<PaneLocation>,
    server: PreviewServer,
}

#[derive(Default)]
pub(crate) struct PreviewRegistry {
    entries: Vec<Entry>,
    next_id: u64,
}

/// `root` without its trailing slashes, keeping `/` itself.
pub(crate) fn normalize_root(root: &str) -> String {
    let trimmed = root.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// `entry` as a path below `root` for the URL. `Err` when it is elsewhere.
pub(crate) fn entry_relative(root: &str, entry: Option<&str>) -> Result<String, String> {
    let Some(entry) = entry else {
        return Ok(String::new());
    };
    let root = normalize_root(root);
    let entry = normalize_root(entry);
    if entry == root {
        return Ok(String::new());
    }
    let prefix = if root == "/" {
        "/".to_owned()
    } else {
        format!("{root}/")
    };
    entry
        .strip_prefix(&prefix)
        .and_then(|relative| super::route::relative_path(&format!("/{relative}")).ok())
        .filter(|relative| !relative.is_empty())
        .ok_or_else(|| format!("{entry} is not under {root}"))
}

impl Entry {
    fn info(&self, entry: &str) -> PreviewInfo {
        PreviewInfo {
            preview_id: self.id.clone(),
            root: self.root.clone(),
            location: self.location.clone(),
            port: self.server.port(),
            url: self.server.url(entry),
        }
    }
}

impl PreviewRegistry {
    fn prune(&mut self) {
        self.entries.retain(|entry| !entry.server.is_stopped());
    }

    /// The running preview of `root` in `location`, opened at `entry`.
    pub(crate) fn find(
        &mut self,
        root: &str,
        location: Option<&PaneLocation>,
        entry: &str,
    ) -> Option<PreviewInfo> {
        self.prune();
        let root = normalize_root(root);
        self.entries
            .iter()
            .find(|candidate| candidate.root == root && candidate.location.as_ref() == location)
            .map(|candidate| candidate.info(entry))
    }

    pub(crate) fn insert(
        &mut self,
        root: &str,
        location: Option<PaneLocation>,
        server: PreviewServer,
        entry: &str,
    ) -> PreviewInfo {
        self.prune();
        self.next_id += 1;
        let added = Entry {
            id: format!("preview-{}", self.next_id),
            root: normalize_root(root),
            location,
            server,
        };
        let info = added.info(entry);
        self.entries.push(added);
        info
    }

    pub(crate) fn list(&mut self) -> Vec<PreviewInfo> {
        self.prune();
        self.entries.iter().map(|entry| entry.info("")).collect()
    }

    /// False when no running preview has that id.
    pub(crate) fn stop(&mut self, id: &str) -> bool {
        self.prune();
        let Some(index) = self.entries.iter().position(|entry| entry.id == id) else {
            return false;
        };
        self.entries.remove(index).server.stop();
        true
    }
}

#[cfg(test)]
mod tests;
