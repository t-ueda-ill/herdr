use super::super::file_viewer::FileLocation;
use super::super::*;
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
}
#[derive(Debug)]
pub(in crate::client::shell) struct Replacement {
    pub before: FileChange,
    pub after: FileChange,
    pub diff: Vec<String>,
}
#[derive(Debug)]
pub(in crate::client::shell) enum Dialog {
    Menu {
        selected: usize,
    },
    Input {
        title: String,
        input: TextEditor,
        operation: FileOperation,
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
}
impl Investigation {
    pub fn new(browser: ClientFileViewerOverlay, endpoint: ClientEndpointId, boot: String) -> Self {
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
    pub buttons: Vec<(Rect, usize)>,
    pub dialog_keys: Vec<(Rect, KeyCode, crossterm::event::KeyModifiers)>,
    pub terminal_tabs: Vec<(Rect, String)>,
}
pub(in crate::client::shell) const MENU: &[&str] = &[
    "new investigation",
    "change root",
    "toggle split direction",
    "copy path",
    "send path to terminal…",
    "edit file",
    "copy files",
    "cut files",
    "paste files…",
    "rename…",
    "new file…",
    "new directory…",
    "trash…",
    "permanently delete…",
    "preview replacement…",
    "undo last replacement",
    "toggle regex",
    "toggle case sensitivity",
    "toggle whole word",
    "include ignored files",
    "close investigation",
    "toggle matches in current file",
    "refresh content search",
    "refresh files",
];
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
