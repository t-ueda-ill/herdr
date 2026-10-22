use super::*;
use crate::api::schema::*;
use crossterm::event::{KeyModifiers as M, MouseButton, MouseEvent, MouseEventKind};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

impl ClientShellState {
    pub(in crate::client::shell) fn insert_investigation_text(&mut self, text: &str) -> bool {
        let Some(tab) = self.investigation_mut() else {
            return false;
        };
        if let Some(Dialog::Input { input, .. }) = tab.dialog.as_mut() {
            input.insert(text);
            return true;
        }
        let focus = tab.focus;
        if let Some(editor) = tab.editor() {
            if editor.insert(text) {
                if focus == Focus::Paths {
                    tab.browser.rebuild(false);
                } else if matches!(focus, Focus::Query | Focus::Include | Focus::Exclude) {
                    tab.schedule_search();
                }
            }
        }
        true
    }
    pub(in crate::client::shell) fn route_investigation_key(
        &mut self,
        key: &crate::input::TerminalKey,
        outcome: &mut ClientShellInput,
    ) {
        outcome.repaint = true;
        let remapped = self.investigation_key(key);
        let key = &remapped;
        let (code, modifiers) = crate::config::normalize_key_combo((key.code, key.modifiers));
        let ctrl = modifiers.contains(M::CONTROL);
        let shift = modifiers.contains(M::SHIFT);
        let Some(tab) = self.investigation_mut() else {
            return;
        };
        if tab.dialog.is_some() {
            self.route_investigation_dialog(code, modifiers, key, outcome);
            return;
        }
        if code == KeyCode::F(10) || code == KeyCode::Char('m') && modifiers == M::ALT {
            tab.dialog = Some(Dialog::Menu { selected: 0 });
            return;
        }
        if code == KeyCode::F(5) {
            self.investigation_menu_action(23, outcome);
            return;
        }
        if code == KeyCode::F(2) {
            self.investigation_menu_action(9, outcome);
            return;
        }
        if code == KeyCode::Delete && tab.focus == Focus::Results {
            self.investigation_menu_action(12, outcome);
            return;
        }
        if ctrl {
            match code {
                KeyCode::Char('e') => {
                    self.investigation_menu_action(5, outcome);
                    return;
                }
                KeyCode::Char('p') => {
                    tab.content_search = false;
                    tab.hovered = None;
                    tab.focus = Focus::Paths;
                    return;
                }
                KeyCode::Char('f' | 'h')
                    if (shift || code == KeyCode::Char('h')) && tab.browser.location.is_some() =>
                {
                    if let Some(location) = tab.browser.location.as_ref() {
                        tab.notice = super::operations::unavailable_notice(location);
                    }
                    return;
                }
                KeyCode::Char('f') if shift => {
                    tab.content_search = true;
                    tab.hovered = None;
                    tab.focus = Focus::Query;
                    tab.schedule_search();
                    return;
                }
                KeyCode::Char('h') => {
                    tab.content_search = true;
                    tab.hovered = None;
                    tab.focus = Focus::Replacement;
                    return;
                }
                KeyCode::Char('w') => {
                    if let Some(index) = tab.active_file.take() {
                        tab.documents.remove(index);
                        tab.active_file = index
                            .checked_sub(1)
                            .or_else(|| (!tab.documents.is_empty()).then_some(0));
                    } else {
                        tab.preview = None;
                    }
                    self.persist_chrome_preferences(outcome);
                    return;
                }
                KeyCode::PageUp | KeyCode::PageDown => {
                    if !tab.documents.is_empty() {
                        let current = tab.active_file.unwrap_or(0);
                        let delta = if code == KeyCode::PageUp { -1 } else { 1 };
                        tab.active_file = Some(
                            (current as isize + delta).rem_euclid(tab.documents.len() as isize)
                                as usize,
                        );
                        tab.focus = Focus::Body;
                        self.persist_chrome_preferences(outcome);
                    }
                    return;
                }
                _ => {}
            }
        }
        if code == KeyCode::Tab || code == KeyCode::BackTab {
            let mut order = if tab.content_search {
                vec![
                    Focus::Query,
                    Focus::Include,
                    Focus::Exclude,
                    Focus::Replacement,
                    Focus::Results,
                    Focus::Body,
                ]
            } else {
                vec![Focus::Paths, Focus::Results, Focus::Body]
            };
            if tab.focus == Focus::Find {
                order.push(Focus::Find);
            }
            let current = order.iter().position(|f| *f == tab.focus).unwrap_or(0);
            let delta = if code == KeyCode::BackTab || shift {
                -1
            } else {
                1
            };
            tab.focus = order[(current as isize + delta).rem_euclid(order.len() as isize) as usize];
            return;
        }
        if ctrl && code == KeyCode::Char('v') && tab.editor().is_some() {
            if let Some(text) = crate::platform::read_clipboard_text() {
                self.insert_investigation_text(&text);
            }
            return;
        }
        if matches!(
            tab.focus,
            Focus::Paths
                | Focus::Query
                | Focus::Include
                | Focus::Exclude
                | Focus::Replacement
                | Focus::Find
        ) {
            if code == KeyCode::Esc {
                tab.focus = if tab.focus == Focus::Find {
                    Focus::Body
                } else {
                    Focus::Results
                };
                return;
            }
            if code == KeyCode::Enter {
                if tab.focus == Focus::Find {
                    let query = tab.find.to_string();
                    if let Some(document) = tab.document_mut() {
                        if !query.is_empty() {
                            let start = document.cursor.saturating_add(1).min(document.text.len());
                            let start = (start..=document.text.len())
                                .find(|i| document.text.is_char_boundary(*i))
                                .unwrap_or(document.text.len());
                            let found = document.text[start..]
                                .find(&query)
                                .map(|i| start + i)
                                .or_else(|| document.text[..start].find(&query));
                            if let Some(offset) = found {
                                document.cursor = offset + query.len();
                                document.anchor = Some(offset);
                                document.scroll = document.line().saturating_sub(3);
                            }
                        }
                    }
                } else if tab.focus == Focus::Replacement {
                    self.investigation_prepare_replace(outcome);
                } else {
                    if matches!(tab.focus, Focus::Query | Focus::Include | Focus::Exclude) {
                        tab.schedule_search();
                    }
                    tab.focus = Focus::Results;
                    self.request_investigation_preview(false, false, outcome);
                }
                return;
            }
            let focus = tab.focus;
            if let Some(editor) = tab.editor() {
                let changed = if let Some(text) = key
                    .generated_text
                    .as_deref()
                    .filter(|_| !ctrl && !modifiers.contains(M::ALT))
                {
                    editor.insert(text)
                } else {
                    editor.handle_key(key) == Some(true)
                };
                if changed {
                    if focus == Focus::Paths {
                        tab.browser.rebuild(false);
                    } else if matches!(focus, Focus::Query | Focus::Include | Focus::Exclude) {
                        tab.schedule_search();
                    }
                }
            }
            return;
        }
        if tab.focus == Focus::Body {
            if ctrl && code == KeyCode::Char('f') {
                tab.focus = Focus::Find;
                return;
            }
            let page = usize::from(self.investigation_hits.body.height);
            let width = usize::from(self.investigation_hits.body.width.saturating_sub(7));
            let Some(tab) = self.investigation_mut() else {
                return;
            };
            if let Some(document) = tab.document_mut() {
                match code {
                    KeyCode::Char('c') if ctrl => {
                        if let Some(range) = document.range() {
                            outcome.actions.push(ClientShellAction::ClipboardWrite(
                                document.text[range].as_bytes().to_vec(),
                            ));
                        }
                    }
                    KeyCode::Char('a') if ctrl => {
                        document.anchor = Some(0);
                        document.cursor = document.text.len();
                    }
                    KeyCode::Esc => {
                        if document.anchor.is_some() {
                            document.clear_selection();
                        } else {
                            tab.focus = Focus::Results;
                        }
                    }
                    KeyCode::Up
                    | KeyCode::Down
                    | KeyCode::Left
                    | KeyCode::Right
                    | KeyCode::PageUp
                    | KeyCode::PageDown
                    | KeyCode::Home
                    | KeyCode::End => {
                        document.motion(code, shift, page);
                        document.reveal_column(width);
                    }
                    _ => {}
                }
            } else if code == KeyCode::Esc {
                tab.focus = Focus::Results;
            }
            return;
        }
        if ctrl {
            match code {
                KeyCode::Char('c') => {
                    self.investigation_menu_action(6, outcome);
                    return;
                }
                KeyCode::Char('x') => {
                    self.investigation_menu_action(7, outcome);
                    return;
                }
                KeyCode::Char('v') => {
                    self.investigation_paste(outcome);
                    return;
                }
                KeyCode::Char('a') => {
                    if tab.content_search {
                        tab.omitted.clear();
                    } else {
                        tab.selected_files = tab
                            .browser
                            .rows
                            .iter()
                            .filter_map(|r| r.entry.as_ref().map(|e| e.path.clone()))
                            .collect();
                    }
                    return;
                }
                _ => {}
            }
        }
        match code {
            KeyCode::Char('/') => {
                tab.focus = Focus::Paths;
                tab.content_search = false;
                tab.hovered = None;
            }
            KeyCode::Esc => {
                tab.selected_files.clear();
            }
            KeyCode::Enter
                if !tab.content_search
                    && tab
                        .browser
                        .rows
                        .get(tab.browser.selected)
                        .is_some_and(|r| r.parent_link) =>
            {
                self.raise_file_viewer_root(outcome);
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                if !tab.content_search && tab.browser.selected_entry().is_some_and(|e| e.is_dir) {
                    let path = tab.browser.selected_entry().map(|e| e.path.clone());
                    if let Some(path) = path {
                        self.set_file_viewer_expanded(path, None, outcome);
                    }
                } else {
                    self.request_investigation_preview(code == KeyCode::Enter, true, outcome);
                }
            }
            KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown => {
                let delta = match code {
                    KeyCode::Up => -1,
                    KeyCode::Down => 1,
                    KeyCode::PageUp => -10,
                    _ => 10,
                };
                // Why not keep it: scrolling shifts rows under a resting pointer.
                tab.hovered = None;
                if tab.content_search {
                    tab.selected_match = (tab.selected_match as isize + delta)
                        .max(0)
                        .min(tab.matches.len().saturating_sub(1) as isize)
                        as usize;
                } else {
                    if shift {
                        if let Some(e) = tab.browser.selected_entry() {
                            tab.selected_files.insert(e.path.clone());
                        }
                    }
                    tab.browser.move_selection(delta);
                    if shift {
                        if let Some(e) = tab.browser.selected_entry() {
                            tab.selected_files.insert(e.path.clone());
                        }
                    }
                }
                self.request_investigation_preview(false, false, outcome);
            }
            KeyCode::Insert => {
                if tab.content_search {
                    if !tab.omitted.insert(tab.selected_match) {
                        tab.omitted.remove(&tab.selected_match);
                    }
                } else if let Some(entry) = tab.browser.selected_entry() {
                    let path = entry.path.clone();
                    if !tab.selected_files.insert(path.clone()) {
                        tab.selected_files.remove(&path);
                    }
                }
            }
            KeyCode::Left => self.collapse_or_climb_file_viewer(outcome),
            KeyCode::Right => {
                if let Some(entry) = tab.browser.selected_entry().filter(|e| e.is_dir) {
                    let path = entry.path.clone();
                    self.set_file_viewer_expanded(path, Some(true), outcome);
                }
            }
            KeyCode::Backspace => self.raise_file_viewer_root(outcome),
            KeyCode::Char('.') => {
                tab.browser.show_hidden = !tab.browser.show_hidden;
                tab.browser.rebuild(true);
            }
            KeyCode::Char('i') => {
                tab.browser.show_ignored = !tab.browser.show_ignored;
                tab.browser.rebuild(true);
            }
            _ => {}
        }
    }
    fn route_investigation_dialog(
        &mut self,
        code: KeyCode,
        modifiers: M,
        key: &crate::input::TerminalKey,
        outcome: &mut ClientShellInput,
    ) {
        let Some(tab) = self.investigation_mut() else {
            return;
        };
        if code == KeyCode::Esc {
            tab.dialog = None;
            return;
        }
        let Some(dialog) = tab.dialog.as_mut() else {
            return;
        };
        match dialog {
            Dialog::Menu { selected } => match code {
                KeyCode::Up => *selected = selected.saturating_sub(1),
                KeyCode::Down => *selected = (*selected + 1).min(MENU.len() - 1),
                KeyCode::Enter => {
                    let index = *selected;
                    self.investigation_menu_action(index, outcome);
                }
                _ => {}
            },
            Dialog::Input {
                title,
                input,
                operation,
                sources,
            } => {
                if code == KeyCode::Enter {
                    let value = input.to_string();
                    let root_change = title == "root";
                    let paths = if sources.is_empty() {
                        vec![value.clone()]
                    } else {
                        sources.clone()
                    };
                    let destination = (!sources.is_empty()).then_some(value.clone());
                    let operation = operation.clone();
                    tab.dialog = None;
                    if root_change {
                        self.change_file_viewer_root(value, None, outcome);
                    } else {
                        self.investigation_plan_operation(
                            operation_params(operation, paths, destination),
                            outcome,
                        );
                    }
                } else {
                    input.handle_key(key);
                }
            }
            Dialog::Confirm {
                params, selected, ..
            } => match code {
                KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::Char('1' | '2' | '3') => {
                    *selected = match code {
                        KeyCode::Char('1') => 0,
                        KeyCode::Char('2') => 1,
                        KeyCode::Char('3') => 2,
                        _ => (*selected + 1) % 3,
                    };
                    params.collision = match *selected {
                        1 => FileCollision::Rename,
                        2 => FileCollision::Replace,
                        _ => FileCollision::Skip,
                    };
                    let params = params.clone();
                    tab.dialog = None;
                    self.investigation_plan_operation(params, outcome);
                }
                KeyCode::Enter => {
                    let params = params.clone();
                    self.investigation_execute_operation(params, outcome);
                }
                _ => {}
            },
            Dialog::Replace { scroll } => match code {
                KeyCode::Up => *scroll = scroll.saturating_sub(1),
                KeyCode::Down => *scroll = scroll.saturating_add(1),
                KeyCode::PageUp => *scroll = scroll.saturating_sub(10),
                KeyCode::PageDown => *scroll = scroll.saturating_add(10),
                KeyCode::Enter if modifiers.contains(M::CONTROL) => {
                    self.investigation_apply_replace(false, outcome)
                }
                _ => {}
            },
            Dialog::Upload(dialog) => {
                let choices = dialog.choices();
                let count = choices.len();
                let chosen = match code {
                    KeyCode::Left => {
                        dialog.selected = (dialog.selected + count - 1) % count;
                        None
                    }
                    KeyCode::Right | KeyCode::Tab => {
                        dialog.selected = (dialog.selected + 1) % count;
                        None
                    }
                    KeyCode::Enter => choices.get(dialog.selected).copied(),
                    // The buttons, left to right.
                    KeyCode::Char(c @ '1'..='9') => choices.get(c as usize - '1' as usize).copied(),
                    _ => None,
                };
                if let Some(choice) = chosen {
                    self.answer_upload_dialog(choice, outcome);
                }
            }
            Dialog::Send { path, pane, boot } => {
                if code == KeyCode::Enter {
                    let path = path.clone();
                    let pane = pane.clone();
                    let boot = boot.clone();
                    tab.dialog = None;
                    if self.endpoint_boot_id(&self.active_endpoint_id) != Some(&boot)
                        || self.focused_pane_id().as_deref() != Some(&pane)
                    {
                        if let Some(tab) = self.investigation_mut() {
                            tab.notice = "terminal destination changed; return to the intended terminal and try again".into();
                        }
                    } else {
                        self.send_file_viewer_path(&path, false, outcome);
                    }
                }
            }
        }
    }
    pub(in crate::client::shell) fn handle_investigation_mouse(
        &mut self,
        mouse: MouseEvent,
        outcome: &mut ClientShellInput,
    ) -> bool {
        let point = (mouse.column, mouse.row);
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            if let Some((_, index)) = self
                .investigation_hits
                .tabs
                .iter()
                .find(|(r, _)| contains(*r, point))
            {
                let index = *index;
                self.workbench.active = Some(index);
                self.workbench.visible = true;
                if let Some(tab) = self.investigation_mut() {
                    tab.hovered = None;
                }
                outcome.repaint = true;
                return true;
            }
            if let Some((_, id)) = self
                .investigation_hits
                .terminal_tabs
                .iter()
                .find(|(r, _)| contains(*r, point))
            {
                let _ = id;
                self.workbench.visible = false;
                outcome.repaint = true;
                return false;
            }
        }
        if !self.investigation_active() {
            return false;
        }
        // Sidebar and tab strip remain live; only the document/list surface captures input.
        let area = self.investigation_hits.body;
        let list = self.investigation_hits.list;
        let field = self
            .investigation_hits
            .fields
            .iter()
            .find(|(r, _)| contains(*r, point))
            .map(|(_, f)| *f);
        let row = self
            .investigation_hits
            .rows
            .iter()
            .find(|(r, _)| contains(*r, point))
            .map(|(_, i)| *i);
        let close = self
            .investigation_hits
            .file_close
            .iter()
            .find(|(r, _)| contains(*r, point))
            .map(|(_, i)| *i);
        let file = self
            .investigation_hits
            .files
            .iter()
            .find(|(r, _)| contains(*r, point))
            .map(|(_, i)| *i);
        let button = self
            .investigation_hits
            .buttons
            .iter()
            .find(|(r, _)| contains(*r, point))
            .map(|(_, i)| *i);
        let menu = self
            .investigation_hits
            .menu
            .iter()
            .find(|(r, _)| contains(*r, point))
            .map(|(_, i)| *i);
        let dialog_key = self
            .investigation_hits
            .dialog_keys
            .iter()
            .find(|(r, _, _)| contains(*r, point))
            .map(|(_, k, m)| (*k, *m));
        let Some(tab) = self.investigation_mut() else {
            return false;
        };
        if tab.dialog.is_some() {
            tab.hovered = None;
            if mouse.kind == MouseEventKind::Moved {
                if let (Some(index), Some(Dialog::Menu { selected })) = (menu, tab.dialog.as_mut())
                {
                    if *selected != index {
                        *selected = index;
                        outcome.repaint = true;
                    }
                }
                return true;
            }
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                if let Some((code, modifiers)) = dialog_key {
                    self.route_investigation_dialog(
                        code,
                        modifiers,
                        &crate::input::TerminalKey::new(code, modifiers),
                        outcome,
                    );
                    outcome.repaint = true;
                    return true;
                }
            }
            if let Some(index) =
                menu.filter(|_| matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)))
            {
                self.investigation_menu_action(index, outcome);
            }
            return true;
        }
        if mouse.kind == MouseEventKind::Moved {
            // Why not fall through: the shared tail repaints on every event in
            // the list, which would redraw on each cell of pointer travel.
            let target = row.filter(|_| contains(list, point));
            if tab.hovered != target {
                tab.hovered = target;
                outcome.repaint = true;
            }
            return contains(area, point) || contains(list, point);
        }
        if matches!(
            mouse.kind,
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
        ) {
            tab.hovered = None;
        }
        if let Some(index) =
            close.filter(|_| matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)))
        {
            if index < tab.documents.len() {
                tab.documents.remove(index);
                tab.active_file = if tab.documents.is_empty() {
                    None
                } else {
                    Some(index.min(tab.documents.len() - 1))
                };
            }
            self.persist_chrome_preferences(outcome);
            outcome.repaint = true;
            return true;
        }
        if let Some(index) =
            button.filter(|_| matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)))
        {
            if index == usize::MAX {
                tab.dialog = Some(Dialog::Menu { selected: 0 });
            } else {
                self.investigation_menu_action(index, outcome);
            }
            outcome.repaint = true;
            return true;
        }
        if let Some(index) =
            file.filter(|_| matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)))
        {
            if index == usize::MAX {
                tab.active_file = None;
            } else {
                tab.active_file = Some(index);
            }
            tab.focus = Focus::Body;
            outcome.repaint = true;
            return true;
        }
        if let Some(focus) =
            field.filter(|_| matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)))
        {
            tab.focus = focus;
            outcome.repaint = true;
            return true;
        }
        if contains(area, point) {
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left)
                | MouseEventKind::Drag(MouseButton::Left) => {
                    tab.focus = Focus::Body;
                    if let Some(document) = tab.document_mut() {
                        let line = document.scroll + usize::from(mouse.row - area.y);
                        let column = usize::from(mouse.column.saturating_sub(area.x + 7))
                            + document.horizontal;
                        let text =
                            document.line_text(line.min(document.lines.len().saturating_sub(1)));
                        let mut width = 0;
                        let mut cluster = 0;
                        for value in text.graphemes(true) {
                            let size = if value == "\t" {
                                4 - width % 4
                            } else {
                                UnicodeWidthStr::width(value)
                            };
                            if width + size > column {
                                break;
                            }
                            width += size;
                            cluster += 1;
                        }
                        document.jump(line, cluster);
                        if matches!(mouse.kind, MouseEventKind::Down(_)) {
                            document.anchor = Some(document.cursor);
                        }
                    }
                }
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                    if let Some(document) = tab.document_mut() {
                        document.scroll = if mouse.kind == MouseEventKind::ScrollUp {
                            document.scroll.saturating_sub(3)
                        } else {
                            (document.scroll + 3).min(document.lines.len().saturating_sub(1))
                        };
                    }
                }
                MouseEventKind::Down(MouseButton::Right) => {
                    tab.dialog = Some(Dialog::Menu { selected: 0 })
                }
                _ => {}
            }
            outcome.repaint = true;
            return true;
        }
        if contains(list, point) {
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(index) = row {
                        tab.focus = Focus::Results;
                        if tab.content_search {
                            tab.selected_match = index;
                            if mouse.modifiers.contains(M::CONTROL) && !tab.omitted.insert(index) {
                                tab.omitted.remove(&index);
                            }
                        } else {
                            let previous = tab.browser.selected;
                            tab.browser.selected = index;
                            if mouse.modifiers.contains(M::CONTROL) {
                                if let Some(entry) = tab.browser.selected_entry() {
                                    let path = entry.path.clone();
                                    if !tab.selected_files.insert(path.clone()) {
                                        tab.selected_files.remove(&path);
                                    }
                                }
                            }
                            if mouse.modifiers.contains(M::SHIFT) {
                                for row in
                                    &tab.browser.rows[previous.min(index)..=previous.max(index)]
                                {
                                    if let Some(entry) = &row.entry {
                                        tab.selected_files.insert(entry.path.clone());
                                    }
                                }
                            }
                        }
                        let now = std::time::Instant::now();
                        let double = tab.browser.last_click.is_some_and(|(previous, at)| {
                            previous == index && now.duration_since(at).as_millis() < 350
                        });
                        tab.browser.last_click = Some((index, now));
                        if !mouse.modifiers.intersects(M::CONTROL | M::SHIFT) {
                            tab.selected_files.clear();
                        }
                        if !tab.content_search
                            && tab.browser.rows.get(index).is_some_and(|r| r.parent_link)
                        {
                            if double {
                                self.raise_file_viewer_root(outcome);
                            }
                        } else if !tab.content_search
                            && tab.browser.selected_entry().is_some_and(|e| e.is_dir)
                        {
                            if let Some(entry) = tab.browser.selected_entry() {
                                let path = entry.path.clone();
                                self.set_file_viewer_expanded(path, None, outcome);
                            }
                        } else {
                            self.request_investigation_preview(double, double, outcome);
                        }
                    }
                }
                MouseEventKind::Down(MouseButton::Right) => {
                    tab.dialog = Some(Dialog::Menu { selected: 0 })
                }
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                    let delta = if mouse.kind == MouseEventKind::ScrollUp {
                        -3
                    } else {
                        3
                    };
                    if tab.content_search {
                        tab.selected_match = (tab.selected_match as isize + delta)
                            .max(0)
                            .min(tab.matches.len().saturating_sub(1) as isize)
                            as usize;
                    } else {
                        tab.browser.move_selection(delta);
                    }
                    self.request_investigation_preview(false, false, outcome);
                }
                _ => {}
            }
            outcome.repaint = true;
            return true;
        }
        false
    }
}

impl ClientShellState {
    fn investigation_key(&self, key: &crate::input::TerminalKey) -> crate::input::TerminalKey {
        let bindings = [
            ("pin", KeyCode::Enter, M::empty()),
            ("read", KeyCode::Char(' '), M::empty()),
            ("next_file", KeyCode::PageDown, M::CONTROL),
            ("previous_file", KeyCode::PageUp, M::CONTROL),
            ("close_file", KeyCode::Char('w'), M::CONTROL),
            ("find_files", KeyCode::Char('p'), M::CONTROL),
            ("search", KeyCode::Char('f'), M::CONTROL | M::SHIFT),
            ("replace", KeyCode::Char('h'), M::CONTROL),
            ("edit", KeyCode::Char('e'), M::CONTROL),
            ("copy", KeyCode::Char('c'), M::CONTROL),
            ("cut", KeyCode::Char('x'), M::CONTROL),
            ("paste", KeyCode::Char('v'), M::CONTROL),
            ("select_all", KeyCode::Char('a'), M::CONTROL),
            ("rename", KeyCode::F(2), M::empty()),
            ("delete", KeyCode::Delete, M::empty()),
            ("actions", KeyCode::F(10), M::empty()),
            ("refresh", KeyCode::F(5), M::empty()),
        ];
        let actual = crate::config::normalize_key_combo((key.code, key.modifiers));
        let mut disabled = false;
        for (name, code, modifiers) in bindings {
            if matches!(name, "pin" | "read")
                && self
                    .investigation()
                    .is_some_and(|t| t.focus != Focus::Results || t.dialog.is_some())
            {
                continue;
            }
            if let Some(configured) = self.config.file_workbench_keys.get(name) {
                if crate::config::parse_key_combo(configured) == Some(actual) {
                    let mut remapped = key.clone();
                    remapped.code = code;
                    remapped.modifiers = modifiers;
                    return remapped;
                }
                if actual == (code, modifiers) {
                    disabled = true;
                }
            }
        }
        let mut result = key.clone();
        if disabled {
            result.code = KeyCode::Null;
            result.generated_text = None;
        }
        result
    }
}
