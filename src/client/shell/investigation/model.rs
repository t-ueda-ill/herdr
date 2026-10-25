use super::super::file_viewer::FileLocation;
use super::super::*;
use super::placement::{
    lenient, FilesDock, FilesPlacement, FilesPresentation, SavedFilesPlacement,
};
use crate::api::schema::{FileChange, FileCollision, FileMatch, FileOperation, FilesSearchParams};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::time::Instant;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(in crate::client::shell) struct SavedInvestigation {
    pub endpoint: String,
    pub workspace: Option<String>,
    pub root: String,
    pub files: Vec<SavedDocument>,
    pub active_file: Option<usize>,
    pub horizontal: bool,
    pub query: String,
    pub include: String,
    pub exclude: String,
    pub regex: bool,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub ignored: bool,
    pub content_search: bool,
    /// `None` for the host. Absent in entries saved before off-host browsing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<FileLocation>,
    /// The workspace's split or full screen choice, dock and ratio. `None`
    /// is the default (split, docked left), as for entries saved before the
    /// split existed (Q7a).
    #[serde(
        default,
        deserialize_with = "lenient",
        skip_serializing_if = "Option::is_none"
    )]
    pub placement: Option<SavedFilesPlacement>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(in crate::client::shell) struct SavedDocument {
    pub path: String,
    pub cursor: usize,
    pub scroll: usize,
    #[serde(default)]
    pub horizontal: usize,
}
#[derive(Debug)]
pub(in crate::client::shell) struct Document {
    pub path: String,
    pub text: String,
    pub revision: String,
    pub cursor: usize,
    pub anchor: Option<usize>,
    pub scroll: usize,
    pub horizontal: usize,
    pub lines: Vec<usize>,
    pub loaded: bool,
    pub request_generation: u64,
    pub restored_scroll: bool,
    pub pending: Option<(String, String)>,
}
impl Document {
    pub fn new(path: String) -> Self {
        Self {
            path,
            text: String::new(),
            revision: String::new(),
            cursor: 0,
            anchor: None,
            scroll: 0,
            horizontal: 0,
            lines: vec![0],
            loaded: false,
            request_generation: 0,
            restored_scroll: false,
            pending: None,
        }
    }
    pub fn update(&mut self, text: String, revision: String) {
        if self.range().is_some() && self.loaded {
            self.pending = Some((text, revision));
            return;
        }
        self.text = text;
        self.revision = revision;
        self.loaded = true;
        self.lines = vec![0];
        self.lines
            .extend(self.text.match_indices('\n').map(|(i, _)| i + 1));
        self.cursor = self.cursor.min(self.text.len());
        while !self.text.is_char_boundary(self.cursor) {
            self.cursor -= 1;
        }
        self.scroll = self.scroll.min(self.lines.len().saturating_sub(1));
    }
    pub fn clear_selection(&mut self) {
        self.anchor = None;
        if let Some((text, revision)) = self.pending.take() {
            self.update(text, revision);
        }
    }
    pub fn range(&self) -> Option<std::ops::Range<usize>> {
        let anchor = self.anchor?;
        (anchor != self.cursor).then_some(anchor.min(self.cursor)..anchor.max(self.cursor))
    }
    pub fn line(&self) -> usize {
        self.lines
            .partition_point(|offset| *offset <= self.cursor)
            .saturating_sub(1)
    }
    pub fn line_text(&self, line: usize) -> &str {
        let start = self.lines[line];
        let end = self.lines.get(line + 1).copied().unwrap_or(self.text.len());
        self.text[start..end].trim_end_matches(['\r', '\n'])
    }
    pub fn jump(&mut self, line: usize, column: usize) {
        let line = line.min(self.lines.len().saturating_sub(1));
        let text = self.line_text(line);
        let byte = text
            .grapheme_indices(true)
            .nth(column)
            .map_or(text.len(), |(i, _)| i);
        self.cursor = self.lines[line] + byte;
    }
    pub fn reveal_column(&mut self, width: usize) {
        use unicode_width::UnicodeWidthStr;
        let line = self.line();
        let column = self.text[self.lines[line]..self.cursor]
            .graphemes(true)
            .fold(0, |x, v| {
                x + if v == "\t" {
                    4 - x % 4
                } else {
                    UnicodeWidthStr::width(v)
                }
            });
        if column < self.horizontal {
            self.horizontal = column;
        } else if column >= self.horizontal + width.max(1) {
            self.horizontal = column + 1 - width.max(1);
        }
    }
    pub fn motion(&mut self, code: KeyCode, shift: bool, page: usize) {
        if shift {
            self.anchor.get_or_insert(self.cursor);
        } else {
            self.clear_selection();
        }
        let line = self.line();
        let column = self.text[self.lines[line]..self.cursor]
            .graphemes(true)
            .count();
        match code {
            KeyCode::Left => {
                self.cursor = self.text[..self.cursor]
                    .grapheme_indices(true)
                    .next_back()
                    .map_or(0, |(i, _)| i)
            }
            KeyCode::Right => {
                self.cursor += self.text[self.cursor..]
                    .graphemes(true)
                    .next()
                    .map_or(0, str::len)
            }
            KeyCode::Up => self.jump(line.saturating_sub(1), column),
            KeyCode::Down => self.jump(line.saturating_add(1), column),
            KeyCode::PageUp => self.jump(line.saturating_sub(page.max(1)), column),
            KeyCode::PageDown => self.jump(line.saturating_add(page.max(1)), column),
            KeyCode::Home => self.cursor = self.lines[line],
            KeyCode::End => self.cursor = self.lines[line] + self.line_text(line).len(),
            _ => {}
        }
        let line = self.line();
        if line < self.scroll {
            self.scroll = line;
        }
        if line >= self.scroll + page.max(1) {
            self.scroll = line + 1 - page.max(1);
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::client::shell) enum Focus {
    Paths,
    Query,
    Include,
    Exclude,
    Replacement,
    Results,
    Body,
    Find,
}
#[derive(Debug)]
pub(in crate::client::shell) struct Investigation {
    pub id: u64,
    pub endpoint: ClientEndpointId,
    pub boot: String,
    pub connection_generation: Option<u64>,
    pub browser: ClientFileViewerOverlay,
    pub documents: Vec<Document>,
    pub preview: Option<Document>,
    pub active_file: Option<usize>,
    pub horizontal: bool,
    pub focus: Focus,
    pub content_search: bool,
    pub query: TextEditor,
    pub include: TextEditor,
    pub exclude: TextEditor,
    pub replacement: TextEditor,
    pub find: TextEditor,
    pub regex: bool,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub ignored: bool,
    pub matches: Vec<FileMatch>,
    pub selected_match: usize,
    /// Row under the pointer. Why separate from `browser.selected`: selection
    /// drives previews, hovering must stay a pure highlight.
    pub hovered: Option<usize>,
    pub omitted: BTreeSet<usize>,
    pub selected_files: BTreeSet<String>,
    pub search_generation: u64,
    pub cancel_generation: u64,
    pub search_due: Option<Instant>,
    pub search_ready: bool,
    pub searching: bool,
    pub preview_generation: u64,
    pub last_poll: Instant,
    /// Separate from `last_poll` so listings interleave with the document
    /// poll instead of competing for the same off-host slot.
    pub last_list_poll: Instant,
    pub pending_reads: BTreeSet<String>,
    pub dialog: Option<Dialog>,
    pub plan: Vec<Replacement>,
    pub undo: Vec<FileChange>,
    pub prepare_generation: u64,
    pub prepare_queue: VecDeque<String>,
    pub prepare_pending: usize,
    pub operation_pending: bool,
    pub notice: String,
    /// A finished upload's folder to list again, and the row to select then.
    /// Why deferred to the tick: the upload ends outside input handling,
    /// where no listing request can be sent.
    pub upload_refresh: Option<(String, Option<String>)>,
    /// The placement of this tab's workspace, the same in all its tabs
    /// (Q6a); `None` is the default. Why kept per tab rather than only in
    /// `Workbench::placement`: that is the one being shown, and another
    /// workspace reopens with its own.
    pub placement: Option<SavedFilesPlacement>,
}
#[derive(Debug)]
pub(in crate::client::shell) struct Replacement {
    pub before: FileChange,
    pub after: FileChange,
    pub diff: Vec<String>,
}
/// What Enter does with an input dialog's text.
/// Why not a variant of `Dialog` per purpose: pasting, drawing and the footer
/// match on `Dialog::Input`, and a variant forgotten there drops typed text.
#[derive(Debug, Clone, PartialEq)]
pub(in crate::client::shell) enum InputPurpose {
    /// `files.operate` with this operation (host only).
    Operation(FileOperation),
    /// Change the workbench root.
    Root,
    /// Save the selected entries on this machine.
    Download,
}
#[derive(Debug)]
pub(in crate::client::shell) enum Dialog {
    Menu {
        target: MenuTarget,
        anchor: MenuPlacement,
        selected: usize,
        /// First visible row. Why stored: derived from `selected` it moved
        /// under the pointer while hovering, so a click hit another item.
        scroll: usize,
    },
    Input {
        title: String,
        input: TextEditor,
        purpose: InputPurpose,
        sources: Vec<String>,
    },
    Confirm {
        params: crate::api::schema::FilesOperateParams,
        selected: usize,
        outcomes: Vec<crate::api::schema::FileOutcome>,
    },
    Replace {
        scroll: usize,
    },
    Send {
        path: String,
        pane: String,
        boot: String,
    },
    /// Files dropped onto the tree: where they go and how to treat names
    /// that exist (spec Q4, Q18). Asked for every tree drop.
    Upload(UploadDialog),
}
impl Dialog {
    pub(in crate::client::shell) fn menu(target: MenuTarget, anchor: MenuPlacement) -> Self {
        Self::Menu {
            target,
            anchor,
            selected: 0,
            scroll: 0,
        }
    }
}
/// Where a workbench menu opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::client::shell) enum MenuPlacement {
    /// Top-left corner at the clicked cell.
    Pointer { x: u16, y: u16 },
    /// Under the selected row, or at the preview when that is focused.
    /// Why not a cell fixed at open time: the row moves when the frame is
    /// resized or scrolled before the next draw.
    Selection,
    /// Under the title line's menu button, else the region's top-left.
    /// Why resolved at draw time like `Selection`: the button moves with
    /// the region's width and dock.
    Header,
}
#[derive(Debug)]
pub(in crate::client::shell) struct UploadDialog {
    /// The client loop's upload job, whose session waits for the answer.
    pub job: u64,
    pub destination: String,
    pub summary: String,
    /// Top-level names that already exist in the destination.
    pub conflicts: Vec<String>,
    /// Why the question is asked again (a name that appeared after begin).
    pub note: Option<String>,
    /// Offer replace / keep both even without named conflicts: a late
    /// conflict names them only in `note`.
    pub ask_collision: bool,
    pub selected: usize,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::client::shell) enum UploadChoice {
    Upload,
    Replace,
    KeepBoth,
    Cancel,
}
impl UploadChoice {
    pub fn label(self) -> &'static str {
        match self {
            Self::Upload => "[upload]",
            Self::Replace => "[replace]",
            Self::KeepBoth => "[keep both]",
            Self::Cancel => "[cancel]",
        }
    }
}
impl UploadDialog {
    pub fn choices(&self) -> &'static [UploadChoice] {
        if self.ask_collision || !self.conflicts.is_empty() {
            &[
                UploadChoice::Replace,
                UploadChoice::KeepBoth,
                UploadChoice::Cancel,
            ]
        } else {
            &[UploadChoice::Upload, UploadChoice::Cancel]
        }
    }
}
impl Investigation {
    pub fn new(
        mut browser: ClientFileViewerOverlay,
        endpoint: ClientEndpointId,
        boot: String,
    ) -> Self {
        browser.parent_row = true;
        browser.rebuild(true);
        Self {
            id: browser.instance,
            endpoint,
            boot,
            connection_generation: None,
            browser,
            documents: Vec::new(),
            preview: None,
            active_file: None,
            horizontal: false,
            focus: Focus::Results,
            content_search: false,
            query: TextEditor::default(),
            include: TextEditor::default(),
            exclude: TextEditor::default(),
            replacement: TextEditor::default(),
            find: TextEditor::default(),
            regex: false,
            case_sensitive: false,
            whole_word: false,
            ignored: false,
            matches: Vec::new(),
            selected_match: 0,
            hovered: None,
            omitted: BTreeSet::new(),
            selected_files: BTreeSet::new(),
            search_generation: 0,
            cancel_generation: 0,
            search_due: None,
            search_ready: false,
            searching: false,
            preview_generation: 0,
            last_poll: Instant::now(),
            last_list_poll: Instant::now(),
            pending_reads: BTreeSet::new(),
            dialog: None,
            plan: Vec::new(),
            undo: Vec::new(),
            prepare_generation: 0,
            prepare_queue: VecDeque::new(),
            prepare_pending: 0,
            operation_pending: false,
            notice: String::new(),
            upload_refresh: None,
            placement: None,
        }
    }
    pub fn document(&self) -> Option<&Document> {
        self.active_file
            .and_then(|i| self.documents.get(i))
            .or(self.preview.as_ref())
    }
    pub fn document_mut(&mut self) -> Option<&mut Document> {
        if let Some(index) = self.active_file {
            self.documents.get_mut(index)
        } else {
            self.preview.as_mut()
        }
    }
    pub fn target(&self) -> Option<(String, usize)> {
        if self.focus == Focus::Body {
            return self.document().map(|d| (d.path.clone(), d.cursor));
        }
        if self.content_search {
            self.matches
                .get(self.selected_match)
                .map(|m| (m.path.clone(), m.start))
        } else {
            self.browser
                .selected_entry()
                .filter(|e| !e.is_dir)
                .map(|e| (e.path.clone(), 0))
        }
    }
    /// The directory "open as root" makes the root: the selected directory, or
    /// the folder of the selected file or match. `None` on ".." and notices.
    pub fn root_candidate(&self) -> Option<String> {
        if self.focus != Focus::Body && !self.content_search {
            let entry = self.browser.selected_entry()?;
            if entry.is_dir {
                return Some(entry.path.clone());
            }
        }
        let (path, _) = self.target()?;
        self.browser.parent_path(&path).map(str::to_owned)
    }
    /// The menu target the keyboard opens on: the preview or a search field
    /// while it has the focus, otherwise the selected row.
    pub fn selection_menu_target(&self) -> MenuTarget {
        if self.focus == Focus::Body && self.document().is_some() {
            return MenuTarget::Preview;
        }
        // Why not the selected match: the keys are in a search field, and a
        // right-click there opens the search menu too.
        if self.content_search
            && matches!(
                self.focus,
                Focus::Query | Focus::Include | Focus::Exclude | Focus::Replacement
            )
        {
            return MenuTarget::Search;
        }
        if self.content_search {
            return if self.matches.get(self.selected_match).is_some() {
                MenuTarget::File
            } else {
                MenuTarget::Search
            };
        }
        self.row_menu_target(self.browser.selected)
    }
    /// The menu target of a tree row; `..` and notices act like empty space.
    pub fn row_menu_target(&self, row: usize) -> MenuTarget {
        if self.content_search {
            return MenuTarget::File;
        }
        match self.browser.rows.get(row) {
            Some(row) if row.parent_link => MenuTarget::Tree,
            Some(row) => match &row.entry {
                Some(entry) if entry.is_dir => MenuTarget::Directory,
                Some(_) => MenuTarget::File,
                None => MenuTarget::Tree,
            },
            None => MenuTarget::Tree,
        }
    }
    /// Labels of the open menu, empty when no menu is open.
    pub fn menu_labels(
        &self,
        presentation: FilesPresentation,
        dock: FilesDock,
    ) -> &'static [&'static str] {
        match &self.dialog {
            Some(Dialog::Menu { target, .. }) => {
                target.labels(self.content_search, presentation, dock)
            }
            _ => &[],
        }
    }
    pub fn paths(&self) -> Vec<String> {
        if self.focus == Focus::Body || self.content_search {
            return self.target().map(|(p, _)| p).into_iter().collect();
        }
        if !self.selected_files.is_empty() {
            return self.selected_files.iter().cloned().collect();
        }
        self.target()
            .map(|(p, _)| p)
            .or_else(|| self.browser.selected_entry().map(|e| e.path.clone()))
            .into_iter()
            .collect()
    }
    pub fn search_params(&self) -> Option<FilesSearchParams> {
        Some(FilesSearchParams {
            root: self.browser.root.clone()?,
            query: self.query.to_string(),
            include: self.include.to_string(),
            exclude: self.exclude.to_string(),
            regex: self.regex,
            case_sensitive: self.case_sensitive,
            whole_word: self.whole_word,
            include_ignored: self.ignored,
            search_id: format!("{}-{}-{}", std::process::id(), self.boot, self.id),
            generation: self.search_generation,
        })
    }
    pub fn schedule_search(&mut self) {
        self.search_generation = self.search_generation.wrapping_add(1);
        self.search_ready = false;
        self.search_due = Some(Instant::now() + std::time::Duration::from_millis(250));
        self.plan.clear();
        self.omitted.clear();
    }
    pub fn editor(&mut self) -> Option<&mut TextEditor> {
        match self.focus {
            Focus::Paths => Some(&mut self.browser.query),
            Focus::Query => Some(&mut self.query),
            Focus::Include => Some(&mut self.include),
            Focus::Exclude => Some(&mut self.exclude),
            Focus::Replacement => Some(&mut self.replacement),
            Focus::Find => Some(&mut self.find),
            _ => None,
        }
    }
    pub fn save(&self) -> Option<SavedInvestigation> {
        Some(SavedInvestigation {
            endpoint: self.endpoint.storage_key(),
            workspace: self.browser.workspace_id.clone(),
            root: self.browser.root.clone()?,
            files: self
                .documents
                .iter()
                .map(|d| SavedDocument {
                    path: d.path.clone(),
                    cursor: d.cursor,
                    scroll: d.scroll,
                    horizontal: d.horizontal,
                })
                .collect(),
            active_file: self.active_file,
            horizontal: self.horizontal,
            query: self.query.to_string(),
            include: self.include.to_string(),
            exclude: self.exclude.to_string(),
            regex: self.regex,
            case_sensitive: self.case_sensitive,
            whole_word: self.whole_word,
            ignored: self.ignored,
            content_search: self.content_search,
            location: self.browser.location.clone(),
            placement: self.placement,
        })
    }
}
#[derive(Debug, Default)]
pub(in crate::client::shell) struct Workbench {
    pub dispatch: Option<usize>,
    pub tabs: Vec<Investigation>,
    pub active: Option<usize>,
    pub visible: bool,
    pub saved: Vec<SavedInvestigation>,
    /// Connection generation whose first snapshot already pruned `saved`, by endpoint storage key.
    pub saved_judged: HashMap<String, Option<u64>>,
    pub clipboard: Option<FileClipboard>,
    pub editor: Option<EditorReturn>,
    pub pause: Option<SurfacePause>,
    pub terminal_requests: Vec<ClientShellAction>,
    pub terminal_input: Vec<ClientMessage>,
    pub terminal_requested: bool,
    pub editor_opening: bool,
    /// Upload jobs whose question a tab's dialog shows. Why tracked: a dialog
    /// closed any other way than answering must still cancel its job.
    pub asking_uploads: Vec<u64>,
    /// Split or full screen, dock and ratio of the files region.
    pub placement: FilesPlacement,
    /// The files region, not the terminal, was last brought forward.
    /// Why not derived from `visible`: in split both are on screen.
    pub files_focused: bool,
    /// A left press landed in the docked files region and is still held.
    /// Why tracked: the drag that follows belongs to the files even once
    /// the pointer crosses onto the terminals.
    pub files_drag: bool,
}
/// Saved investigations kept per endpoint; older ones are dropped first.
pub(in crate::client::shell) const SAVED_INVESTIGATIONS_PER_ENDPOINT: usize = 50;

impl Workbench {
    /// Drops saved investigations of `endpoint` whose workspace is absent from
    /// the first snapshot of this connection. Only workspace existence counts:
    /// an off-host investigation may wait for a pane that has not appeared yet.
    pub fn judge_saved_on_snapshot(
        &mut self,
        endpoint: &str,
        generation: Option<u64>,
        workspaces: &[crate::protocol::ClientShellWorkspace],
    ) {
        if self.saved_judged.get(endpoint) == Some(&generation) {
            return;
        }
        self.saved_judged.insert(endpoint.to_owned(), generation);
        self.saved.retain(|saved| {
            saved.endpoint != endpoint
                || saved
                    .workspace
                    .as_ref()
                    .is_none_or(|id| workspaces.iter().any(|w| &w.workspace_id == id))
        });
    }
}

/// Keeps the newest `limit` entries per endpoint, in the order given (oldest first).
pub(in crate::client::shell) fn retain_newest_per_endpoint(
    entries: Vec<SavedInvestigation>,
    limit: usize,
) -> Vec<SavedInvestigation> {
    let mut remaining: HashMap<String, usize> = HashMap::new();
    for entry in &entries {
        *remaining.entry(entry.endpoint.clone()).or_default() += 1;
    }
    entries
        .into_iter()
        .filter(|entry| match remaining.get_mut(&entry.endpoint) {
            Some(count) if *count > limit => {
                *count -= 1;
                false
            }
            _ => true,
        })
        .collect()
}

#[derive(Debug)]
pub(in crate::client::shell) struct FileClipboard {
    pub endpoint: ClientEndpointId,
    pub boot: String,
    pub paths: Vec<String>,
    pub cut: bool,
}
#[derive(Debug, Clone)]
pub(in crate::client::shell) enum FileRequest {
    Read {
        id: u64,
        path: String,
        generation: u64,
        prepare: bool,
    },
    Search {
        id: u64,
        generation: u64,
    },
    Replace {
        id: u64,
        originals: Vec<FileChange>,
        undo: bool,
    },
    Plan {
        id: u64,
        params: crate::api::schema::FilesOperateParams,
    },
    Operate {
        id: u64,
    },
    Editor {
        id: u64,
    },
    EditorCheck {
        id: u64,
        pane: String,
    },
}
#[derive(Debug, Default)]
pub(in crate::client::shell) struct Hits {
    pub tabs: Vec<(Rect, usize)>,
    pub files: Vec<(Rect, usize)>,
    pub file_close: Vec<(Rect, usize)>,
    pub fields: Vec<(Rect, Focus)>,
    pub rows: Vec<(Rect, usize)>,
    pub body: Rect,
    pub list: Rect,
    pub scroll: usize,
    pub menu: Vec<(Rect, usize)>,
    /// The menu box, which may run past a docked region onto the terminals.
    pub menu_area: Rect,
    pub menu_scroll: usize,
    /// `None` opens the files-wide menu; `Some` runs that menu action.
    pub buttons: Vec<(Rect, Option<&'static str>)>,
    /// The title line, whose right-click opens the files-wide menu.
    pub title: Rect,
    pub dialog_keys: Vec<(Rect, KeyCode, crossterm::event::KeyModifiers)>,
    pub terminal_tabs: Vec<(Rect, String)>,
    /// Where the body was drawn: the files region in split, else the pane
    /// surface. Pointer events outside it are not the workbench's.
    pub region: Rect,
    /// The body was drawn beside the terminals rather than over them.
    pub docked: bool,
    /// The cursor the last docked frame showed while the files had focus.
    /// Why kept: terminal output patched straight onto the screen must keep
    /// it rather than move the cursor to the terminal.
    pub focused_cursor: Option<crate::protocol::CursorState>,
}
pub(in crate::client::shell) const NEW_INVESTIGATION: &str = "new files tab";
pub(in crate::client::shell) const CHANGE_ROOT: &str = "change root";
pub(in crate::client::shell) const TOGGLE_SPLIT_DIRECTION: &str = "toggle split direction";
pub(in crate::client::shell) const COPY_PATH: &str = "copy path";
pub(in crate::client::shell) const SEND_PATH: &str = "send path to terminal…";
pub(in crate::client::shell) const EDIT_FILE: &str = "edit file";
pub(in crate::client::shell) const COPY_FILES: &str = "copy files";
pub(in crate::client::shell) const CUT_FILES: &str = "cut files";
pub(in crate::client::shell) const PASTE_FILES: &str = "paste files…";
pub(in crate::client::shell) const RENAME: &str = "rename…";
pub(in crate::client::shell) const NEW_FILE: &str = "new file…";
pub(in crate::client::shell) const NEW_DIRECTORY: &str = "new directory…";
pub(in crate::client::shell) const TRASH: &str = "trash…";
pub(in crate::client::shell) const DELETE: &str = "permanently delete…";
pub(in crate::client::shell) const PREVIEW_REPLACEMENT: &str = "preview replacement…";
pub(in crate::client::shell) const UNDO_REPLACEMENT: &str = "undo last replacement";
pub(in crate::client::shell) const TOGGLE_REGEX: &str = "toggle regex";
pub(in crate::client::shell) const TOGGLE_CASE: &str = "toggle case sensitivity";
pub(in crate::client::shell) const TOGGLE_WHOLE_WORD: &str = "toggle whole word";
/// Content search over files that ignore rules leave out.
pub(in crate::client::shell) const INCLUDE_IGNORED: &str = "include ignored files";
/// Tree rows for files that ignore rules leave out, like the `i` key.
pub(in crate::client::shell) const SHOW_IGNORED: &str = "show ignored files";
pub(in crate::client::shell) const CLOSE_INVESTIGATION: &str = "close files tab";
pub(in crate::client::shell) const TOGGLE_FILE_MATCHES: &str = "toggle matches in current file";
pub(in crate::client::shell) const REFRESH_SEARCH: &str = "refresh content search";
pub(in crate::client::shell) const REFRESH_FILES: &str = "refresh files";
pub(in crate::client::shell) const OPEN_AS_ROOT: &str = "open as root";
pub(in crate::client::shell) const DOWNLOAD_ITEM: &str = "download…";
pub(in crate::client::shell) const SHOW_FULL_SCREEN: &str = "show full screen";
pub(in crate::client::shell) const SHOW_SPLIT: &str = "show split";
pub(in crate::client::shell) const DOCK_LEFT: &str = "dock left";
pub(in crate::client::shell) const DOCK_RIGHT: &str = "dock right";
pub(in crate::client::shell) const DOCK_BOTTOM: &str = "dock bottom";

/// Actions that only know the host's filesystem.
pub(in crate::client::shell) const HOST_ONLY: &[&str] = &[
    COPY_FILES,
    CUT_FILES,
    PASTE_FILES,
    RENAME,
    NEW_FILE,
    NEW_DIRECTORY,
    TRASH,
    DELETE,
    PREVIEW_REPLACEMENT,
    UNDO_REPLACEMENT,
    REFRESH_SEARCH,
];

/// What a workbench menu was opened on; it decides the items.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::client::shell) enum MenuTarget {
    File,
    Directory,
    /// The tree outside any entry row, or the `..` row.
    Tree,
    /// The search fields or the results list outside any match.
    Search,
    Preview,
}

const FILE_ITEMS: &[&str] = &[
    COPY_PATH,
    SEND_PATH,
    EDIT_FILE,
    COPY_FILES,
    CUT_FILES,
    RENAME,
    TRASH,
    DELETE,
    DOWNLOAD_ITEM,
];
const MATCH_ITEMS: &[&str] = &[
    COPY_PATH,
    SEND_PATH,
    EDIT_FILE,
    COPY_FILES,
    CUT_FILES,
    RENAME,
    TRASH,
    DELETE,
    DOWNLOAD_ITEM,
    TOGGLE_FILE_MATCHES,
];
const DIRECTORY_ITEMS: &[&str] = &[
    OPEN_AS_ROOT,
    COPY_PATH,
    COPY_FILES,
    CUT_FILES,
    PASTE_FILES,
    RENAME,
    NEW_FILE,
    NEW_DIRECTORY,
    TRASH,
    DELETE,
    DOWNLOAD_ITEM,
];
/// Tree menus by presentation and dock. Why one slice per combination:
/// labels are borrowed for the menu's lifetime, and the current mode and
/// dock are left out because choosing them would do nothing.
const TREE_SPLIT_LEFT: &[&str] = &[
    NEW_FILE,
    NEW_DIRECTORY,
    PASTE_FILES,
    REFRESH_FILES,
    SHOW_IGNORED,
    CHANGE_ROOT,
    NEW_INVESTIGATION,
    SHOW_FULL_SCREEN,
    DOCK_RIGHT,
    DOCK_BOTTOM,
    CLOSE_INVESTIGATION,
];
const TREE_SPLIT_RIGHT: &[&str] = &[
    NEW_FILE,
    NEW_DIRECTORY,
    PASTE_FILES,
    REFRESH_FILES,
    SHOW_IGNORED,
    CHANGE_ROOT,
    NEW_INVESTIGATION,
    SHOW_FULL_SCREEN,
    DOCK_LEFT,
    DOCK_BOTTOM,
    CLOSE_INVESTIGATION,
];
const TREE_SPLIT_BOTTOM: &[&str] = &[
    NEW_FILE,
    NEW_DIRECTORY,
    PASTE_FILES,
    REFRESH_FILES,
    SHOW_IGNORED,
    CHANGE_ROOT,
    NEW_INVESTIGATION,
    SHOW_FULL_SCREEN,
    DOCK_LEFT,
    DOCK_RIGHT,
    CLOSE_INVESTIGATION,
];
const TREE_FULL_LEFT: &[&str] = &[
    NEW_FILE,
    NEW_DIRECTORY,
    PASTE_FILES,
    REFRESH_FILES,
    SHOW_IGNORED,
    CHANGE_ROOT,
    NEW_INVESTIGATION,
    SHOW_SPLIT,
    DOCK_RIGHT,
    DOCK_BOTTOM,
    CLOSE_INVESTIGATION,
];
const TREE_FULL_RIGHT: &[&str] = &[
    NEW_FILE,
    NEW_DIRECTORY,
    PASTE_FILES,
    REFRESH_FILES,
    SHOW_IGNORED,
    CHANGE_ROOT,
    NEW_INVESTIGATION,
    SHOW_SPLIT,
    DOCK_LEFT,
    DOCK_BOTTOM,
    CLOSE_INVESTIGATION,
];
const TREE_FULL_BOTTOM: &[&str] = &[
    NEW_FILE,
    NEW_DIRECTORY,
    PASTE_FILES,
    REFRESH_FILES,
    SHOW_IGNORED,
    CHANGE_ROOT,
    NEW_INVESTIGATION,
    SHOW_SPLIT,
    DOCK_LEFT,
    DOCK_RIGHT,
    CLOSE_INVESTIGATION,
];
const SEARCH_ITEMS: &[&str] = &[
    TOGGLE_REGEX,
    TOGGLE_CASE,
    TOGGLE_WHOLE_WORD,
    INCLUDE_IGNORED,
    PREVIEW_REPLACEMENT,
    UNDO_REPLACEMENT,
    REFRESH_SEARCH,
];
const PREVIEW_ITEMS: &[&str] = &[
    COPY_PATH,
    EDIT_FILE,
    SEND_PATH,
    TOGGLE_FILE_MATCHES,
    TOGGLE_SPLIT_DIRECTION,
];

impl MenuTarget {
    /// Labels shown for this target; a file row lists the match toggle only
    /// while it is a content-search result.
    /// The tree menu also depends on how the files region is shown.
    pub(in crate::client::shell) fn labels(
        self,
        content_search: bool,
        presentation: FilesPresentation,
        dock: FilesDock,
    ) -> &'static [&'static str] {
        use FilesDock::{Bottom, Left, Right};
        use FilesPresentation::{Full, Split};
        match self {
            Self::File if content_search => MATCH_ITEMS,
            Self::File => FILE_ITEMS,
            Self::Directory => DIRECTORY_ITEMS,
            Self::Tree => match (presentation, dock) {
                (Split, Left) => TREE_SPLIT_LEFT,
                (Split, Right) => TREE_SPLIT_RIGHT,
                (Split, Bottom) => TREE_SPLIT_BOTTOM,
                (Full, Left) => TREE_FULL_LEFT,
                (Full, Right) => TREE_FULL_RIGHT,
                (Full, Bottom) => TREE_FULL_BOTTOM,
            },
            Self::Search => SEARCH_ITEMS,
            Self::Preview => PREVIEW_ITEMS,
        }
    }
}
pub(in crate::client::shell) fn operation_params(
    operation: FileOperation,
    paths: Vec<String>,
    destination: Option<String>,
) -> crate::api::schema::FilesOperateParams {
    crate::api::schema::FilesOperateParams {
        operation,
        paths,
        destination,
        collision: FileCollision::Skip,
        confirmed: false,
    }
}

#[derive(Debug)]
pub(in crate::client::shell) struct EditorReturn {
    pub id: u64,
    pub pane: String,
    pub due: Instant,
    pub awaiting: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct SurfacePause {
    pub endpoint: ClientEndpointId,
    pub generation: u64,
    pub boot: String,
    pub confirmed: bool,
    pub resume_requested: bool,
}
