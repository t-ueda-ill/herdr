//! File viewer state: listings, expansion, toggles, and the rows they produce.
//!
//! Rows are rebuilt here whenever an input changes, so drawing a frame only
//! walks the cached rows.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::api::schema::{DirectoryEntry, DirectoryEntryKind};
use crate::protocol::ClientShellPane;

use super::super::TextEditor;
use super::fuzzy::{rank, FuzzyQuery};

/// `directory.list` stops after this many entries per directory.
pub(in crate::client::shell) const LIST_LIMIT_LABEL: &str = "2,000";
/// `directory.index` stops after this many files.
pub(in crate::client::shell) const INDEX_LIMIT_LABEL: &str = "50,000";
/// Search results beyond this are not turned into rows.
const MAX_SEARCH_ROWS: usize = 1_000;

pub(in crate::client::shell) const LOADING: &str = "loading…";
pub(in crate::client::shell) const NO_LIVE_PANE: &str = "no live pane to send to";
pub(in crate::client::shell) const PANE_NOT_FOCUSED: &str = "listed pane is not focused";
pub(in crate::client::shell) const EDIT_FILES_ONLY: &str = "edit opens files only";
pub(in crate::client::shell) const CONTROL_CHARACTERS_IN_PATH: &str =
    "path contains control characters; copy instead";

/// Where an off-host viewer's files live: the environment a pane's shell runs in.
/// Why not a pane id: panes come and go, the environment is what the tab browses.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(in crate::client::shell) enum FileLocation {
    Ssh {
        host: String,
    },
    Container {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ssh_host: Option<String>,
    },
}

impl FileLocation {
    /// `None` for a pane running a host shell.
    pub(in crate::client::shell) fn of_pane(pane: &ClientShellPane) -> Option<Self> {
        match (&pane.container, &pane.ssh_host) {
            (Some(name), ssh_host) => Some(Self::Container {
                name: name.clone(),
                ssh_host: ssh_host.clone(),
            }),
            (None, Some(host)) => Some(Self::Ssh { host: host.clone() }),
            (None, None) => None,
        }
    }

    /// The location a server reports for a pane there.
    pub(in crate::client::shell) fn to_pane_location(
        location: Option<&Self>,
    ) -> crate::api::schema::PaneLocation {
        use crate::api::schema::{PaneLocation, PaneLocationKind};
        match location {
            None => PaneLocation {
                kind: PaneLocationKind::Host,
                ssh_host: None,
                container: None,
            },
            Some(Self::Ssh { host }) => PaneLocation {
                kind: PaneLocationKind::Ssh,
                ssh_host: Some(host.clone()),
                container: None,
            },
            Some(Self::Container { name, ssh_host }) => PaneLocation {
                kind: PaneLocationKind::Container,
                ssh_host: ssh_host.clone(),
                container: Some(name.clone()),
            },
        }
    }

    /// `devbox`, `app`, or `app@devbox` (a container behind an ssh host).
    pub(in crate::client::shell) fn label(&self) -> String {
        match self {
            Self::Ssh { host } => host.clone(),
            Self::Container {
                name,
                ssh_host: Some(host),
            } => format!("{name}@{host}"),
            Self::Container { name, .. } => name.clone(),
        }
    }
}

/// Label of `location` as notices name it; `None` is the host itself.
pub(in crate::client::shell) fn location_label(location: Option<&FileLocation>) -> String {
    location.map_or_else(|| "host".to_owned(), FileLocation::label)
}

#[derive(Debug, Clone, PartialEq)]
pub(in crate::client::shell) struct DirectoryListing {
    pub(in crate::client::shell) canonical: String,
    pub(in crate::client::shell) entries: Vec<DirectoryEntry>,
    pub(in crate::client::shell) truncated: bool,
    /// Entries past the server's limit; absent from servers that do not count them.
    pub(in crate::client::shell) omitted: Option<u64>,
    /// Entries the server could not name because they are not valid UTF-8.
    pub(in crate::client::shell) unlistable: Option<u64>,
    pub(in crate::client::shell) git_error: Option<String>,
}

#[derive(Debug, Clone)]
pub(in crate::client::shell) enum DirectoryState {
    Loading,
    Loaded(DirectoryListing),
    /// The inline notice shown in place of the entries.
    Failed(String),
    /// Resolves to one of its own ancestors; never expanded.
    Loop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::client::shell) struct IndexedPath {
    pub(in crate::client::shell) relative: String,
    pub(in crate::client::shell) is_dir: bool,
}

#[derive(Debug, Clone)]
pub(in crate::client::shell) enum FileIndex {
    /// The endpoint does not advertise `directory.index`.
    Unsupported,
    Loading,
    Ready {
        repo_root: String,
        paths: Vec<IndexedPath>,
        truncated: bool,
    },
    /// Outside Git, or Git failed: search falls back to the visible rows.
    Unavailable {
        notice: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::client::shell) struct FileViewerEntry {
    /// Absolute host path, `/`-separated whatever the client OS.
    pub(in crate::client::shell) path: String,
    pub(in crate::client::shell) is_dir: bool,
    pub(in crate::client::shell) expanded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::client::shell) struct FileViewerRow {
    pub(in crate::client::shell) depth: u16,
    pub(in crate::client::shell) label: String,
    /// `None` for an inline notice row, which cannot be selected.
    pub(in crate::client::shell) entry: Option<FileViewerEntry>,
    /// The ".." row. Why `entry` stays `None`: every file operation reads the
    /// selection through `selected_entry`, so none of them can reach a path
    /// from this row, while movement and clicks still land on it.
    pub(in crate::client::shell) parent_link: bool,
}

impl FileViewerRow {
    fn notice(depth: u16, text: impl Into<String>) -> Self {
        Self {
            depth,
            label: text.into(),
            entry: None,
            parent_link: false,
        }
    }

    fn parent_link() -> Self {
        Self {
            depth: 0,
            label: "..".to_owned(),
            entry: None,
            parent_link: true,
        }
    }

    fn selectable(&self) -> bool {
        self.entry.is_some() || self.parent_link
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::client::shell) enum FileViewerButton {
    Send,
    Keep,
    Edit,
    Copy,
    Close,
}

impl FileViewerButton {
    pub(in crate::client::shell) const ALL: [(Self, &'static str); 5] = [
        (Self::Send, "↵ send"),
        (Self::Keep, "space keep"),
        (Self::Edit, "^e edit"),
        (Self::Copy, "^y copy"),
        (Self::Close, "esc"),
    ];
}

/// Bookkeeping of the background refresh of listings already shown. Kept apart
/// from `DirectoryState` on purpose: a refresh must never turn a `Loaded`
/// directory into `Loading`, which would blank the rows until the answer.
#[derive(Debug, Default)]
pub(in crate::client::shell) struct ListingRefresh {
    /// Directories with a refresh request on the wire.
    pub(in crate::client::shell) in_flight: HashSet<String>,
    /// Directories a manual refresh still has to ask for, one per tick.
    pub(in crate::client::shell) queue: VecDeque<String>,
    /// The tree changed shape (or the user asked), so the index is stale.
    pub(in crate::client::shell) index_due: bool,
    pub(in crate::client::shell) index_in_flight: bool,
    /// A manual refresh asked for the index, so the rate limit does not apply.
    pub(in crate::client::shell) index_forced: bool,
    pub(in crate::client::shell) last_index_refresh: Option<std::time::Instant>,
    /// The notice a failed root refresh raised, so a later success clears
    /// that one and never an unrelated notice.
    pub(in crate::client::shell) failure_notice: Option<String>,
}

impl ListingRefresh {
    pub(in crate::client::shell) fn busy(&self) -> bool {
        !self.in_flight.is_empty() || self.index_in_flight
    }

    pub(in crate::client::shell) fn forget(&mut self) {
        *self = Self::default();
    }
}

#[derive(Debug)]
pub(in crate::client::shell) struct ClientFileViewerOverlay {
    /// Fixed for one opened viewer. Why not `generation`: a root change bumps
    /// it, yet an editor opened before the change still belongs to this viewer.
    pub(in crate::client::shell) instance: u64,
    /// Bumped on every root change so answers for an older root are dropped.
    pub(in crate::client::shell) generation: u64,
    pub(in crate::client::shell) workspace_id: Option<String>,
    /// Unknown until the first listing names it.
    pub(in crate::client::shell) root: Option<String>,
    /// `None` browses the host's filesystem. Decided from the snapshot when the
    /// viewer opens, so no answer has to name it.
    pub(in crate::client::shell) location: Option<FileLocation>,
    /// An old server without `pane.directory.*` listed the host's directory
    /// for an off-host pane; the title says so.
    pub(in crate::client::shell) host_fallback: bool,
    /// Live pane whose directory the first listing showed; paths go there.
    /// `None` until that listing arrives, or when the server found no live pane.
    pub(in crate::client::shell) pane_id: Option<String>,
    /// Pane of `location` the last request went through. Why remembered:
    /// Compose replicas share one location, and following the focus would
    /// move this tab into a replica it was not browsing.
    pub(in crate::client::shell) location_pane: Option<String>,
    pub(in crate::client::shell) directories: HashMap<String, DirectoryState>,
    pub(in crate::client::shell) expanded: HashSet<String>,
    pub(in crate::client::shell) show_hidden: bool,
    pub(in crate::client::shell) show_ignored: bool,
    pub(in crate::client::shell) query: TextEditor,
    pub(in crate::client::shell) search_focused: bool,
    pub(in crate::client::shell) index: FileIndex,
    /// The index failed because its pane was gone; ask again once a pane of
    /// the location runs. Why not retry every unavailable index: outside a
    /// repository it stays unavailable by design.
    pub(in crate::client::shell) index_retry: bool,
    /// Directories whose listing failed for lack of a pane, to ask again once
    /// one runs. Why not every failed directory: a permission error stays one.
    pub(in crate::client::shell) listing_retry: HashSet<String>,
    pub(in crate::client::shell) refresh: ListingRefresh,
    pub(in crate::client::shell) rows: Vec<FileViewerRow>,
    /// Selectable rows while searching, counted once per rebuild.
    pub(in crate::client::shell) match_count: usize,
    pub(in crate::client::shell) selected: usize,
    pub(in crate::client::shell) scroll: usize,
    /// One line for action feedback: refusals, copies, editor errors.
    pub(in crate::client::shell) notice: Option<String>,
    /// Selected as soon as a rebuild produces its row.
    pub(in crate::client::shell) pending_select: Option<String>,
    /// Directory a search result asked to show, while its ancestors are listed.
    pub(in crate::client::shell) revealing: Option<String>,
    pub(in crate::client::shell) last_click: Option<(usize, std::time::Instant)>,
    /// Shows a ".." row above the tree. Why not always: the modal fallback
    /// has no row action for it.
    pub(in crate::client::shell) parent_row: bool,
}

pub(in crate::client::shell) struct FileViewerOptions {
    pub(in crate::client::shell) generation: u64,
    pub(in crate::client::shell) workspace_id: Option<String>,
    pub(in crate::client::shell) show_hidden: bool,
    pub(in crate::client::shell) show_ignored: bool,
    pub(in crate::client::shell) index_supported: bool,
    pub(in crate::client::shell) location: Option<FileLocation>,
}

impl ClientFileViewerOverlay {
    pub(in crate::client::shell) fn new(options: FileViewerOptions) -> Self {
        let mut viewer = Self {
            instance: options.generation,
            generation: options.generation,
            workspace_id: options.workspace_id,
            root: None,
            location: options.location,
            host_fallback: false,
            pane_id: None,
            location_pane: None,
            directories: HashMap::new(),
            expanded: HashSet::new(),
            show_hidden: options.show_hidden,
            show_ignored: options.show_ignored,
            query: TextEditor::default(),
            search_focused: false,
            index: if options.index_supported {
                FileIndex::Loading
            } else {
                FileIndex::Unsupported
            },
            index_retry: false,
            listing_retry: HashSet::new(),
            refresh: ListingRefresh::default(),
            rows: Vec::new(),
            match_count: 0,
            selected: 0,
            scroll: 0,
            notice: None,
            pending_select: None,
            revealing: None,
            last_click: None,
            parent_row: false,
        };
        viewer.rebuild(true);
        viewer
    }

    pub(in crate::client::shell) fn indexing(&self) -> bool {
        matches!(self.index, FileIndex::Loading)
    }

    pub(in crate::client::shell) fn searching(&self) -> bool {
        !self.query.is_empty()
    }

    /// Search ranks the repository index; without one it narrows the tree.
    pub(in crate::client::shell) fn index_search_active(&self) -> bool {
        self.searching() && matches!(self.index, FileIndex::Ready { .. })
    }

    /// `devbox:` before a path of that environment, `host:` for the fallback.
    pub(in crate::client::shell) fn location_prefix(&self) -> String {
        match &self.location {
            Some(location) => format!("{}:", location.label()),
            None if self.host_fallback => "host:".to_owned(),
            None => String::new(),
        }
    }

    pub(in crate::client::shell) fn title(&self) -> String {
        let prefix = self.location_prefix();
        if self.index_search_active() {
            if let FileIndex::Ready { repo_root, .. } = &self.index {
                return format!("files · {prefix}{repo_root} (search)");
            }
        }
        format!("files · {prefix}{}", self.root.as_deref().unwrap_or("…"))
    }

    /// Parent of `path` in this viewer's path syntax.
    pub(in crate::client::shell) fn parent_path<'a>(&self, path: &'a str) -> Option<&'a str> {
        (self.parent_fn())(path)
    }

    fn parent_fn(&self) -> fn(&str) -> Option<&str> {
        if self.location.is_some() {
            parent_posix_path
        } else {
            parent_host_path
        }
    }

    /// Directories from `root` down to `path`, in this viewer's path syntax.
    pub(in crate::client::shell) fn path_chain(&self, root: &str, path: &str) -> Vec<String> {
        path_chain_with(root, path, self.parent_fn())
    }

    /// The root and every shown expanded directory whose listing is held,
    /// sorted so a sweep walks them in a stable order. Why not the collapsed
    /// cached listings: nothing shows them, so asking for them is wasted work.
    /// Collapsing a folder removes only its own path from `expanded`, so a
    /// folder counts as shown only when every folder between it and the root
    /// is expanded as well.
    pub(in crate::client::shell) fn refresh_targets(&self) -> Vec<String> {
        let Some(root) = self.root.as_deref() else {
            return Vec::new();
        };
        let loaded =
            |path: &str| matches!(self.directories.get(path), Some(DirectoryState::Loaded(_)));
        let shown = |path: &str| {
            let chain = self.path_chain(root, path);
            chain.first().map(String::as_str) == Some(root)
                && chain
                    .iter()
                    .skip(1)
                    .all(|directory| self.expanded.contains(directory))
        };
        let mut targets: Vec<String> = self
            .expanded
            .iter()
            .filter(|path| shown(path))
            .map(String::as_str)
            .chain([root])
            .filter(|path| loaded(path))
            .map(str::to_owned)
            .collect();
        targets.sort();
        targets.dedup();
        targets
    }

    /// Forgets `path` and everything below it, as a directory that is gone.
    pub(in crate::client::shell) fn drop_directory(&mut self, path: &str) {
        let inside = |other: &str| host_path_contains(path, other);
        self.expanded.retain(|other| !inside(other));
        self.directories.retain(|other, _| !inside(other));
        self.listing_retry.retain(|other| !inside(other));
        self.refresh.queue.retain(|other| !inside(other));
    }

    /// Replaces the listing of `path` with a newer one. Returns false, with
    /// nothing touched, when it says the same thing as the one held.
    pub(in crate::client::shell) fn apply_refreshed_listing(
        &mut self,
        path: &str,
        listing: DirectoryListing,
    ) -> bool {
        let is_root = self.root.as_deref() == Some(path);
        let state = if !is_root && self.loops_back(path, &listing.canonical) {
            DirectoryState::Loop
        } else {
            DirectoryState::Loaded(listing)
        };
        let shape_changed = match (self.directories.get(path), &state) {
            (Some(DirectoryState::Loaded(old)), DirectoryState::Loaded(new)) => {
                if old == new {
                    return false;
                }
                names_differ(old, new)
            }
            _ => true,
        };
        let selected = self.selected_entry().map(|entry| entry.path.clone());
        if let DirectoryState::Loaded(new) = &state {
            let children: HashSet<String> = new
                .entries
                .iter()
                .filter(|entry| entry_is_dir(entry))
                .map(|entry| join_host_path(path, &entry.name))
                .collect();
            let vanished: Vec<String> = self
                .expanded
                .iter()
                .filter(|other| self.parent_path(other) == Some(path) && !children.contains(*other))
                .cloned()
                .collect();
            for other in vanished {
                self.drop_directory(&other);
            }
        }
        self.directories.insert(path.to_owned(), state);
        self.rebuild(true);
        // A row that disappeared leaves the selection on whatever slid into
        // its place; the nearest surviving ancestor is the less surprising row.
        if let Some(mut gone) = selected.filter(|selected| {
            self.selected_entry()
                .is_none_or(|entry| entry.path != *selected)
        }) {
            while let Some(parent) = self.parent_path(&gone).map(str::to_owned) {
                if let Some(index) = self
                    .rows
                    .iter()
                    .position(|row| row.entry.as_ref().is_some_and(|entry| entry.path == parent))
                {
                    self.selected = index;
                    break;
                }
                gone = parent;
            }
        }
        self.refresh.index_due |= shape_changed;
        true
    }

    pub(in crate::client::shell) fn selected_entry(&self) -> Option<&FileViewerEntry> {
        self.rows
            .get(self.selected)
            .and_then(|row| row.entry.as_ref())
    }

    /// Moves over selectable rows only, stopping at either end.
    pub(in crate::client::shell) fn move_selection(&mut self, delta: isize) {
        let selectable: Vec<usize> = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| row.selectable().then_some(index))
            .collect();
        let Some(current) = selectable
            .iter()
            .position(|index| *index >= self.selected)
            .or_else(|| selectable.len().checked_sub(1))
        else {
            return;
        };
        let next = current
            .saturating_add_signed(delta)
            .min(selectable.len() - 1);
        self.selected = selectable[next];
    }

    pub(in crate::client::shell) fn entry_visible(&self, entry: &DirectoryEntry) -> bool {
        (self.show_hidden || !entry.hidden) && (self.show_ignored || entry.ignored != Some(true))
    }

    /// Rebuilds the rows after any input change. `keep_selection` holds the
    /// selected path in place; otherwise the first selectable row is chosen.
    pub(in crate::client::shell) fn rebuild(&mut self, keep_selection: bool) {
        let previous = keep_selection
            .then(|| self.selected_entry().map(|entry| entry.path.clone()))
            .flatten();
        let on_parent_link = keep_selection
            && self.pending_select.is_none()
            && self
                .rows
                .get(self.selected)
                .is_some_and(|row| row.parent_link);
        self.rows = if self.searching() {
            self.search_rows()
        } else {
            self.tree_rows()
        };
        self.match_count = self.rows.iter().filter(|row| row.entry.is_some()).count();
        let wanted = self.pending_select.clone().or(previous);
        let found = wanted.as_deref().and_then(|wanted| {
            self.rows
                .iter()
                .position(|row| row.entry.as_ref().is_some_and(|entry| entry.path == wanted))
        });
        match found {
            Some(index) => {
                self.selected = index;
                if self.pending_select.as_deref() == wanted.as_deref() {
                    self.pending_select = None;
                }
            }
            None if keep_selection => {
                self.selected = self.selected.min(self.rows.len().saturating_sub(1));
            }
            None => self.selected = 0,
        }
        if on_parent_link && self.rows.first().is_some_and(|row| row.parent_link) {
            self.selected = 0;
            return;
        }
        if self
            .rows
            .get(self.selected)
            .is_none_or(|row| row.entry.is_none())
        {
            self.select_nearest_entry();
        }
    }

    /// Only the user moves onto ".."; a rebuild never lands there on its own,
    /// or the row inserted above the tree would take the selection from the
    /// first entry.
    fn select_nearest_entry(&mut self) {
        let entries: Vec<usize> = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| row.entry.is_some().then_some(index))
            .collect();
        let nearest = entries
            .iter()
            .copied()
            .find(|index| *index >= self.selected)
            .or_else(|| entries.last().copied());
        self.selected = match nearest {
            Some(index) => index,
            None => self
                .rows
                .iter()
                .position(|row| !row.parent_link)
                .unwrap_or(0),
        };
    }

    fn tree_rows(&self) -> Vec<FileViewerRow> {
        let mut rows = Vec::new();
        match &self.root {
            Some(root) => {
                if self.parent_row && self.parent_path(root).is_some() {
                    rows.push(FileViewerRow::parent_link());
                }
                self.push_directory_rows(root, 0, false, &mut rows);
            }
            None => rows.push(FileViewerRow::notice(0, LOADING)),
        }
        rows
    }

    fn push_directory_rows(
        &self,
        directory: &str,
        depth: u16,
        parent_git_failed: bool,
        rows: &mut Vec<FileViewerRow>,
    ) {
        let listing = match self.directories.get(directory) {
            None | Some(DirectoryState::Loading) => {
                rows.push(FileViewerRow::notice(depth, LOADING));
                return;
            }
            Some(DirectoryState::Failed(notice)) => {
                rows.push(FileViewerRow::notice(depth, notice.clone()));
                return;
            }
            Some(DirectoryState::Loop) => {
                rows.push(FileViewerRow::notice(depth, "loop"));
                return;
            }
            Some(DirectoryState::Loaded(listing)) => listing,
        };
        // Said once where Git starts failing, not again in every subdirectory.
        if let Some(error) = listing.git_error.as_deref().filter(|_| !parent_git_failed) {
            rows.push(FileViewerRow::notice(depth, git_error_notice(error)));
        }
        let git_failed = listing.git_error.is_some();
        let mut shown = 0;
        let mut hidden = 0;
        for entry in &listing.entries {
            if !self.entry_visible(entry) {
                hidden += 1;
                continue;
            }
            shown += 1;
            let path = join_host_path(directory, &entry.name);
            let is_dir = entry_is_dir(entry);
            let expanded = is_dir && self.expanded.contains(&path);
            rows.push(FileViewerRow {
                depth,
                label: entry_label(entry),
                entry: Some(FileViewerEntry {
                    path: path.clone(),
                    is_dir,
                    expanded,
                }),
                parent_link: false,
            });
            if expanded {
                self.push_directory_rows(&path, depth.saturating_add(1), git_failed, rows);
            }
        }
        if listing.truncated {
            rows.push(FileViewerRow::notice(
                depth,
                match listing.omitted {
                    Some(omitted) => format!("+{omitted} more"),
                    None => format!("list truncated ({LIST_LIMIT_LABEL})"),
                },
            ));
        }
        if let Some(unlistable) = listing.unlistable.filter(|count| *count > 0) {
            rows.push(FileViewerRow::notice(
                depth,
                format!("{unlistable} unlistable"),
            ));
        } else if shown == 0 && !listing.truncated {
            rows.push(FileViewerRow::notice(
                depth,
                if hidden == 0 {
                    "empty".to_owned()
                } else {
                    format!("{hidden} hidden")
                },
            ));
        }
    }

    fn search_rows(&self) -> Vec<FileViewerRow> {
        let mut rows = match &self.index {
            FileIndex::Ready {
                repo_root,
                paths,
                truncated,
            } => {
                let query = FuzzyQuery::new(&self.query);
                let ranked = rank(&query, paths.iter().map(|path| path.relative.as_str()));
                let mut rows: Vec<FileViewerRow> = ranked
                    .iter()
                    .take(MAX_SEARCH_ROWS)
                    .map(|index| {
                        let path = &paths[*index];
                        FileViewerRow {
                            depth: 0,
                            label: if path.is_dir {
                                format!("{}/", path.relative)
                            } else {
                                path.relative.clone()
                            },
                            entry: Some(FileViewerEntry {
                                path: join_host_path(repo_root, &path.relative),
                                is_dir: path.is_dir,
                                expanded: false,
                            }),
                            parent_link: false,
                        }
                    })
                    .collect();
                if ranked.len() > MAX_SEARCH_ROWS {
                    rows.push(FileViewerRow::notice(
                        0,
                        format!("first {MAX_SEARCH_ROWS} of {} matches", ranked.len()),
                    ));
                }
                if *truncated {
                    rows.push(FileViewerRow::notice(
                        0,
                        format!("index truncated ({INDEX_LIMIT_LABEL})"),
                    ));
                }
                rows
            }
            FileIndex::Unsupported | FileIndex::Loading | FileIndex::Unavailable { .. } => {
                self.visible_row_matches()
            }
        };
        if rows.iter().all(|row| row.entry.is_none()) {
            rows.insert(0, FileViewerRow::notice(0, "no matches"));
        }
        if let FileIndex::Unavailable {
            notice: Some(notice),
        } = &self.index
        {
            rows.push(FileViewerRow::notice(0, notice.clone()));
        }
        rows
    }

    /// Without an index, the filter narrows what the tree currently shows.
    fn visible_row_matches(&self) -> Vec<FileViewerRow> {
        let query = self.query.to_lowercase();
        let root = self.root.as_deref().unwrap_or("/");
        self.tree_rows()
            .into_iter()
            .filter_map(|row| {
                let entry = row.entry?;
                let relative = relative_host_path(root, &entry.path)?;
                if !relative.to_lowercase().contains(&query) {
                    return None;
                }
                let label = if entry.is_dir {
                    format!("{relative}/")
                } else {
                    relative.to_owned()
                };
                Some(FileViewerRow {
                    depth: 0,
                    label,
                    entry: Some(FileViewerEntry {
                        expanded: false,
                        ..entry
                    }),
                    parent_link: false,
                })
            })
            .collect()
    }

    /// True when `canonical` is the canonical path of `directory` or of one
    /// of its ancestors in the tree, or contains one of them.
    pub(in crate::client::shell) fn loops_back(&self, directory: &str, canonical: &str) -> bool {
        let Some(root) = self.root.as_deref() else {
            return false;
        };
        let mut ancestor = self.parent_path(directory);
        while let Some(current) = ancestor {
            if let Some(DirectoryState::Loaded(listing)) = self.directories.get(current) {
                if host_path_contains(canonical, &listing.canonical) {
                    return true;
                }
            }
            if current == root {
                break;
            }
            ancestor = self.parent_path(current);
        }
        false
    }
}

/// Whether entries were added, removed or changed kind, as opposed to only
/// their size or mtime changing. Why not any difference: a growing log would
/// otherwise ask for the repository index again on every poll.
fn names_differ(old: &DirectoryListing, new: &DirectoryListing) -> bool {
    let key = |entry: &DirectoryEntry| (entry.name.clone(), entry_is_dir(entry), entry.hidden);
    // Servers list in a stable order, so an order change counts as a change.
    old.entries.len() != new.entries.len()
        || !old.entries.iter().map(key).eq(new.entries.iter().map(key))
}

pub(in crate::client::shell) fn entry_is_dir(entry: &DirectoryEntry) -> bool {
    entry.kind == DirectoryEntryKind::Dir
        || (entry.kind == DirectoryEntryKind::Symlink
            && entry.target_kind == Some(DirectoryEntryKind::Dir))
}

fn entry_label(entry: &DirectoryEntry) -> String {
    match entry.kind {
        DirectoryEntryKind::Dir => format!("{}/", entry.name),
        DirectoryEntryKind::Symlink => format!("{}@", entry.name),
        DirectoryEntryKind::File | DirectoryEntryKind::Other => entry.name.clone(),
    }
}

/// Short inline text for a Git failure the server reported.
pub(in crate::client::shell) fn git_error_notice(error: &str) -> String {
    if error == "untrusted repository"
        || error.contains("dubious ownership")
        || error.contains("safe.directory")
    {
        "untrusted repository".to_owned()
    } else if error == "git timed out" {
        "git timed out".to_owned()
    } else if error.contains("not installed") {
        "git not installed".to_owned()
    } else {
        let line = error.lines().next().unwrap_or(error);
        // Off-host servers word some failures themselves ("git check-ignore
        // exited with status 2"); a second prefix would only repeat "git".
        if line.starts_with("git ") {
            line.to_owned()
        } else {
            format!("git: {line}")
        }
    }
}

/// Repository files plus every directory above them, so directories are
/// searchable although the index only lists files.
pub(in crate::client::shell) fn indexed_paths(files: Vec<String>) -> Vec<IndexedPath> {
    let mut directories = BTreeSet::new();
    for file in &files {
        let mut rest = file.as_str();
        while let Some((parent, _)) = rest.rsplit_once('/') {
            if !directories.insert(parent.to_owned()) {
                break;
            }
            rest = parent;
        }
    }
    directories
        .into_iter()
        .map(|relative| IndexedPath {
            relative,
            is_dir: true,
        })
        .chain(files.into_iter().map(|relative| IndexedPath {
            relative,
            is_dir: false,
        }))
        .collect()
}

// Host paths are handled as `/`-separated strings. Why not `std::path`: a
// Windows client talking to a Unix server would join with `\`.
pub(in crate::client::shell) fn join_host_path(directory: &str, name: &str) -> String {
    if directory.ends_with('/') {
        format!("{directory}{name}")
    } else {
        format!("{directory}/{name}")
    }
}

/// Parent in a POSIX environment's paths. Why not `parent_host_path`: it
/// understands drive letters and UNC prefixes, which an off-host path never has.
pub(in crate::client::shell) fn parent_posix_path(path: &str) -> Option<&str> {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    match trimmed.rsplit_once('/') {
        Some(("", _)) => Some("/"),
        Some((parent, _)) => Some(parent),
        None => None,
    }
}

pub(in crate::client::shell) fn parent_host_path(path: &str) -> Option<&str> {
    let trimmed = path.trim_end_matches('/');
    if trimmed.len() == 2 && trimmed.as_bytes()[1] == b':' {
        return None;
    }
    if trimmed.starts_with("//") && trimmed[2..].split('/').count() <= 2 {
        return None;
    }
    if trimmed.is_empty() {
        return None;
    }
    match trimmed.rsplit_once('/') {
        Some(("", _)) => Some("/"),
        Some((parent, _)) if parent.len() == 2 && parent.as_bytes()[1] == b':' => Some(&path[..3]),
        Some((parent, _)) => Some(parent),
        None => None,
    }
}

/// `path` equals `ancestor` or lies below it, compared by whole components.
pub(in crate::client::shell) fn host_path_contains(ancestor: &str, path: &str) -> bool {
    let ancestor = ancestor.trim_end_matches('/');
    path == ancestor
        || ancestor.is_empty()
        || path
            .strip_prefix(ancestor)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn relative_host_path<'a>(root: &str, path: &'a str) -> Option<&'a str> {
    let root = root.trim_end_matches('/');
    path.strip_prefix(root)?.strip_prefix('/')
}

/// Directories from `root` down to `path`, both included.
#[cfg(test)]
pub(in crate::client::shell) fn host_path_chain(root: &str, path: &str) -> Vec<String> {
    path_chain_with(root, path, parent_host_path)
}

fn path_chain_with(root: &str, path: &str, parent: fn(&str) -> Option<&str>) -> Vec<String> {
    let mut chain = Vec::new();
    let mut current = Some(path);
    while let Some(directory) = current {
        if !host_path_contains(root, directory) {
            break;
        }
        chain.push(directory.to_owned());
        if directory == root.trim_end_matches('/') || directory == root {
            break;
        }
        current = parent(directory);
    }
    chain.reverse();
    chain
}

/// POSIX shell word for `path`; plain paths stay readable. `None` when the
/// path holds a control character.
pub(in crate::client::shell) fn shell_quote_path(path: &str) -> Option<String> {
    // Why not quote control characters as well: the word reaches the pane as
    // typed keys or a paste, not as shell source, so a newline submits the line
    // and ESC can end a bracketed paste before any quote is read.
    if path.chars().any(char::is_control) {
        return None;
    }
    let plain = !path.is_empty()
        && path.chars().all(|ch| {
            ch.is_ascii_alphanumeric()
                || matches!(
                    ch,
                    '/' | '.' | '_' | '-' | '+' | ',' | ':' | '@' | '%' | '='
                )
        });
    Some(if plain {
        path.to_owned()
    } else {
        format!("'{}'", path.replace('\'', r"'\''"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_paths_stay_bare_and_everything_else_is_single_quoted() {
        let quote = |path| shell_quote_path(path).expect("a path without control characters");
        assert_eq!(quote("/repo/src/main.rs"), "/repo/src/main.rs");
        assert_eq!(quote("/repo/my file.txt"), "'/repo/my file.txt'");
        assert_eq!(quote("/repo/it's"), r"'/repo/it'\''s'");
        assert_eq!(quote("/repo/$HOME"), "'/repo/$HOME'");
        assert_eq!(quote("/repo/*.rs"), "'/repo/*.rs'");
    }

    #[test]
    fn paths_with_control_characters_have_no_shell_word() {
        for path in [
            "/repo/a\tb",
            "/repo/evil\nrm -rf ~",
            "/repo/a\rb",
            "/repo/end\u{1b}[201~paste",
            "/repo/del\u{7f}",
            "/repo/csi\u{9b}201~",
        ] {
            assert_eq!(shell_quote_path(path), None, "{path:?}");
        }
    }

    #[test]
    fn host_paths_join_and_split_on_slashes() {
        assert_eq!(join_host_path("/", "etc"), "/etc");
        assert_eq!(join_host_path("/repo", "src"), "/repo/src");
        assert_eq!(parent_host_path("/repo/src"), Some("/repo"));
        assert_eq!(parent_host_path("/repo"), Some("/"));
        assert_eq!(parent_host_path("/"), None);
        assert!(host_path_contains("/repo", "/repo/src"));
        assert!(host_path_contains("/repo", "/repo"));
        assert!(host_path_contains("/", "/repo"));
        assert!(!host_path_contains("/repo", "/repository"));
        assert_eq!(
            host_path_chain("/repo", "/repo/src/client"),
            ["/repo", "/repo/src", "/repo/src/client"]
        );
        assert_eq!(host_path_chain("/", "/etc"), ["/", "/etc"]);
    }

    #[test]
    fn posix_paths_treat_drive_and_unc_looking_names_as_plain_directories() {
        assert_eq!(parent_posix_path("/c:"), Some("/"));
        assert_eq!(parent_posix_path("/srv/c:/x"), Some("/srv/c:"));
        assert_eq!(parent_posix_path("//host/share"), Some("//host"));
        assert_eq!(parent_posix_path("/work/src"), Some("/work"));
        assert_eq!(parent_posix_path("/"), None);
        // The host helper stops at these; that is why off-host paths do not use it.
        assert_eq!(parent_host_path("//host/share"), None);
    }

    #[test]
    fn locations_label_themselves_for_titles_and_notices() {
        let ssh = FileLocation::Ssh {
            host: "devbox".into(),
        };
        let local = FileLocation::Container {
            name: "app".into(),
            ssh_host: None,
        };
        let remote = FileLocation::Container {
            name: "app".into(),
            ssh_host: Some("devbox".into()),
        };
        assert_eq!(ssh.label(), "devbox");
        assert_eq!(local.label(), "app");
        assert_eq!(remote.label(), "app@devbox");
        assert_eq!(location_label(None), "host");
    }

    #[test]
    fn indexed_paths_add_each_parent_directory_once() {
        let paths = indexed_paths(vec![
            "src/client/shell.rs".into(),
            "src/client/mod.rs".into(),
            "README.md".into(),
        ]);
        let directories: Vec<&str> = paths
            .iter()
            .filter(|path| path.is_dir)
            .map(|path| path.relative.as_str())
            .collect();
        assert_eq!(directories, ["src", "src/client"]);
        assert_eq!(paths.iter().filter(|path| !path.is_dir).count(), 3);
    }

    #[test]
    fn off_host_git_errors_read_like_the_host_notices_without_a_double_prefix() {
        assert_eq!(
            git_error_notice("untrusted repository"),
            "untrusted repository"
        );
        assert_eq!(git_error_notice("git not installed"), "git not installed");
        assert_eq!(
            git_error_notice("git check-ignore exited with status 2"),
            "git check-ignore exited with status 2"
        );
        assert_eq!(git_error_notice("fatal: bad"), "git: fatal: bad");
    }

    #[test]
    fn git_errors_become_short_inline_notices() {
        assert_eq!(
            git_error_notice("fatal: detected dubious ownership in repository at '/r'"),
            "untrusted repository"
        );
        assert_eq!(
            git_error_notice("git is not installed"),
            "git not installed"
        );
        assert_eq!(git_error_notice("fatal: bad\nmore"), "git: fatal: bad");
    }
}
