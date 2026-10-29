use super::super::render::menu_widget;
use super::*;
use crate::api::schema::*;
use crossterm::event::{KeyModifiers as M, MouseButton, MouseEvent, MouseEventKind};

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
    /// Breaks a pending `gg` for input the list router never sees.
    pub(in crate::client::shell) fn forget_pending_g(&mut self) {
        if let Some(tab) = self.investigation_mut() {
            tab.pending_g = false;
        }
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
        let docked = !self.investigation_full_screen();
        let Some(tab) = self.investigation_mut() else {
            return;
        };
        // Why taken on every key: any other key between the two breaks `gg`.
        let pending_g = std::mem::take(&mut tab.pending_g);
        if tab.dialog.is_some() {
            self.route_investigation_dialog(code, modifiers, key, outcome);
            return;
        }
        // Why before the selection menu: its F10 check ignores modifiers.
        if code == KeyCode::F(10) && shift {
            tab.dialog = Some(Dialog::menu(MenuTarget::Tree, MenuPlacement::Header));
            return;
        }
        if code == KeyCode::F(10) || code == KeyCode::Char('m') && modifiers == M::ALT {
            tab.dialog = Some(Dialog::menu(
                tab.selection_menu_target(),
                MenuPlacement::Selection,
            ));
            return;
        }
        if code == KeyCode::F(5) {
            self.investigation_menu_action(REFRESH_FILES, outcome);
            return;
        }
        if code == KeyCode::F(2) {
            self.investigation_menu_action(RENAME, outcome);
            return;
        }
        if code == KeyCode::Delete && tab.focus == Focus::Results {
            self.investigation_menu_action(TRASH, outcome);
            return;
        }
        if ctrl {
            match code {
                KeyCode::Char('e') => {
                    self.investigation_menu_action(EDIT_FILE, outcome);
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
                _ => {}
            }
        }
        if code == KeyCode::Tab || code == KeyCode::BackTab {
            let order = if tab.content_search {
                vec![
                    Focus::Query,
                    Focus::Include,
                    Focus::Exclude,
                    Focus::Replacement,
                    Focus::Results,
                ]
            } else {
                vec![Focus::Paths, Focus::Results]
            };
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
            Focus::Paths | Focus::Query | Focus::Include | Focus::Exclude | Focus::Replacement
        ) {
            if code == KeyCode::Esc {
                tab.focus = Focus::Results;
                return;
            }
            if code == KeyCode::Enter {
                if tab.focus == Focus::Replacement {
                    self.investigation_prepare_replace(outcome);
                } else {
                    if matches!(tab.focus, Focus::Query | Focus::Include | Focus::Exclude) {
                        tab.schedule_search();
                    }
                    tab.focus = Focus::Results;
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
        let first_g = code == KeyCode::Char('g') && modifiers.is_empty();
        if pending_g && first_g {
            tab.move_list(isize::MIN);
            return;
        }
        if first_g {
            tab.pending_g = true;
            return;
        }
        let page = isize::try_from(self.investigation_hits.list.height.max(1)).unwrap_or(1);
        let Some(tab) = self.investigation_mut() else {
            return;
        };
        if ctrl {
            match code {
                KeyCode::Char('d' | 'u') => {
                    let half = (page / 2).max(1);
                    tab.move_list(if code == KeyCode::Char('d') {
                        half
                    } else {
                        -half
                    });
                    return;
                }
                KeyCode::Char('c') => {
                    self.investigation_menu_action(COPY_FILES, outcome);
                    return;
                }
                KeyCode::Char('x') => {
                    self.investigation_menu_action(CUT_FILES, outcome);
                    return;
                }
                KeyCode::Char('v') => {
                    self.investigation_paste(false, outcome);
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
            // Why the terminal only once nothing is selected: Esc first
            // drops a selection, as it did before split. Why not in full
            // screen: there the terminals are not on screen to take keys.
            KeyCode::Esc if docked && tab.selected_files.is_empty() => {
                self.workbench.files_focused = false;
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
                    let mode = if code == KeyCode::Enter {
                        OpenFileMode::Editor
                    } else {
                        OpenFileMode::Pager
                    };
                    self.investigation_open_file(mode, outcome);
                }
            }
            KeyCode::Home | KeyCode::End => {
                tab.move_list(if code == KeyCode::Home {
                    isize::MIN
                } else {
                    isize::MAX
                });
            }
            KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown => {
                let delta = match code {
                    KeyCode::Up => -1,
                    KeyCode::Down => 1,
                    KeyCode::PageUp => -page,
                    _ => page,
                };
                if shift && !tab.content_search {
                    if let Some(e) = tab.browser.selected_entry() {
                        tab.selected_files.insert(e.path.clone());
                    }
                }
                tab.move_list(delta);
                if shift && !tab.content_search {
                    if let Some(e) = tab.browser.selected_entry() {
                        tab.selected_files.insert(e.path.clone());
                    }
                }
            }
            KeyCode::Char('n' | 'N') if tab.content_search => {
                tab.hovered = None;
                tab.selected_match = tab.adjacent_file_match(code == KeyCode::Char('n'));
            }
            KeyCode::Char('K') if shift => {
                self.investigation_menu_action(TOGGLE_DETAILS, outcome);
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
        let menu_rows = self.investigation_hits.menu.len();
        let FilesPlacement {
            presentation, dock, ..
        } = self.workbench.placement;
        let Some(tab) = self.investigation_mut() else {
            return;
        };
        if code == KeyCode::Esc {
            tab.dialog = None;
            return;
        }
        let labels = tab.menu_labels(presentation, dock);
        let Some(dialog) = tab.dialog.as_mut() else {
            return;
        };
        match dialog {
            Dialog::Menu {
                selected, scroll, ..
            } => {
                let items: Vec<menu_widget::MenuItem<'_>> = labels
                    .iter()
                    .map(|label| menu_widget::MenuItem::new(label))
                    .collect();
                match menu_widget::menu_key(code, Some(*selected), &items, true) {
                    menu_widget::MenuKey::Move(index) => {
                        *selected = index;
                        *scroll = menu_widget::scroll_window(
                            Some(index),
                            *scroll,
                            menu_rows,
                            labels.len(),
                        );
                    }
                    menu_widget::MenuKey::Activate(index) => {
                        if let Some(label) = labels.get(index) {
                            self.investigation_menu_action(label, outcome);
                        }
                    }
                    menu_widget::MenuKey::Close => tab.dialog = None,
                    menu_widget::MenuKey::Inert | menu_widget::MenuKey::Unhandled => {}
                }
            }
            Dialog::Input {
                input,
                purpose,
                sources,
                ..
            } => {
                if code == KeyCode::Enter {
                    let value = input.to_string();
                    let purpose = purpose.clone();
                    let sources = sources.clone();
                    // Why a placeholder home: the real one is looked up where
                    // the files are written, but every other rule is known
                    // here, and a typo should leave the dialog open.
                    if purpose == InputPurpose::Download {
                        if let Err(refusal) = crate::client::download::destination::resolve(
                            &value,
                            std::path::Path::new("/"),
                            crate::client::download::destination::PathSyntax::current(),
                        ) {
                            tab.notice = refusal;
                            return;
                        }
                    }
                    tab.dialog = None;
                    match purpose {
                        InputPurpose::Root => {
                            self.change_file_viewer_root(value, None, outcome);
                        }
                        InputPurpose::Download => self.queue_download(sources, value),
                        InputPurpose::Operation(operation) => {
                            let destination = (!sources.is_empty()).then_some(value.clone());
                            let paths = if sources.is_empty() {
                                vec![value]
                            } else {
                                sources
                            };
                            self.investigation_plan_operation(
                                operation_params(operation, paths, destination),
                                outcome,
                            );
                        }
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
        if matches!(mouse.kind, MouseEventKind::Down(_)) {
            self.forget_pending_g();
        }
        let right = mouse.kind == MouseEventKind::Down(MouseButton::Right);
        if right || mouse.kind == MouseEventKind::Down(MouseButton::Left) {
            if let Some((_, index)) = self
                .investigation_hits
                .tabs
                .iter()
                .find(|(r, _)| contains(*r, point))
            {
                let index = *index;
                self.show_files(index);
                if let Some(tab) = self.investigation_mut() {
                    tab.hovered = None;
                    if right {
                        open_files_menu(tab, mouse);
                    }
                }
                outcome.repaint = true;
                return true;
            }
        }
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            if let Some((_, id)) = self
                .investigation_hits
                .terminal_tabs
                .iter()
                .find(|(r, _)| contains(*r, point))
            {
                let _ = id;
                self.show_terminal_side();
                outcome.repaint = true;
                return false;
            }
        }
        if !self.investigation_active() {
            return false;
        }
        let region = self.investigation_hits.region;
        let docked = self.investigation_hits.docked;
        let dialog_open = self.investigation().is_some_and(|tab| tab.dialog.is_some());
        // Why a dialog takes every event: it is modal, and a menu that runs
        // past the files region onto the terminals must still be clickable.
        let files_drag = self.workbench.files_drag
            && matches!(
                mouse.kind,
                MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)
            );
        if mouse.kind == MouseEventKind::Up(MouseButton::Left) {
            self.workbench.files_drag = false;
        }
        if files_drag && !contains(region, point) {
            // Why swallowed rather than clamped into the region: the drag
            // means nothing in the files list, and the terminals beneath must
            // not see a drag or release they never saw pressed.
            return true;
        }
        if !dialog_open && !contains(region, point) {
            // Why still here: the pointer leaving the region leaves the list.
            if mouse.kind == MouseEventKind::Moved {
                if let Some(tab) = self.investigation_mut() {
                    if tab.hovered.take().is_some() {
                        outcome.repaint = true;
                    }
                }
            }
            return false;
        }
        // Why any button but not the wheel: a press says where the reader is
        // working, while scrolling the tree beside a terminal should leave
        // keys with that terminal.
        // Why only inside the region: a dialog takes presses on the
        // terminals too, but keeping it modal must not move the keys there.
        let press_in_region =
            matches!(mouse.kind, MouseEventKind::Down(_)) && contains(region, point);
        if docked && press_in_region && !self.workbench.files_focused {
            self.workbench.files_focused = true;
            outcome.repaint = true;
        }
        if docked && press_in_region && mouse.kind == MouseEventKind::Down(MouseButton::Left) {
            self.workbench.files_drag = true;
        }
        let FilesPlacement {
            presentation, dock, ..
        } = self.workbench.placement;
        // Sidebar and tab strip remain live; only the list and details capture input.
        let menu_rows = self.investigation_hits.menu.len();
        let area = self.investigation_hits.details;
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
        let title = contains(self.investigation_hits.title, point);
        let button = self
            .investigation_hits
            .buttons
            .iter()
            .find(|(r, _)| contains(*r, point))
            .map(|(_, i)| *i);
        // Why not looked up per item: every workbench menu item is enabled.
        let menu =
            menu_widget::menu_pointer(mouse.kind, &self.investigation_hits.menu, point, |_| true);
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
            let labels = tab.menu_labels(presentation, dock);
            if let Some(Dialog::Menu {
                selected, scroll, ..
            }) = tab.dialog.as_mut()
            {
                match menu {
                    menu_widget::MenuPointer::Highlight(index) => {
                        if *selected != index {
                            *selected = index;
                            outcome.repaint = true;
                        }
                    }
                    menu_widget::MenuPointer::Scroll { up } => {
                        let (next, highlighted) = menu_widget::wheel_window(
                            up,
                            *scroll,
                            Some(*selected),
                            menu_rows,
                            labels.len(),
                        );
                        if next != *scroll {
                            *scroll = next;
                            *selected = highlighted.unwrap_or(*selected);
                            outcome.repaint = true;
                        }
                    }
                    menu_widget::MenuPointer::Activate(index) => {
                        if let Some(label) = labels.get(index) {
                            self.investigation_menu_action(label, outcome);
                        }
                    }
                    menu_widget::MenuPointer::Close => {
                        tab.dialog = None;
                        outcome.repaint = true;
                    }
                    menu_widget::MenuPointer::Ignore => {}
                }
                return true;
            }
            if mouse.kind == MouseEventKind::Moved {
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
            return docked || contains(area, point) || contains(list, point);
        }
        if matches!(
            mouse.kind,
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
        ) {
            tab.hovered = None;
        }
        if let Some(index) =
            button.filter(|_| matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)))
        {
            match index {
                Some(label) => {
                    self.investigation_menu_action(label, outcome);
                }
                None => tab.dialog = Some(Dialog::menu(MenuTarget::Tree, MenuPlacement::Header)),
            }
            outcome.repaint = true;
            return true;
        }
        if title && mouse.kind == MouseEventKind::Down(MouseButton::Right) {
            open_files_menu(tab, mouse);
            outcome.repaint = true;
            return true;
        }
        if field.is_some() && mouse.kind == MouseEventKind::Down(MouseButton::Right) {
            let target = if tab.content_search {
                MenuTarget::Search
            } else {
                MenuTarget::Tree
            };
            tab.dialog = Some(Dialog::menu(
                target,
                MenuPlacement::Pointer {
                    x: mouse.column,
                    y: mouse.row,
                },
            ));
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
            if mouse.kind == MouseEventKind::Down(MouseButton::Right) {
                tab.dialog = Some(Dialog::menu(
                    MenuTarget::Details,
                    MenuPlacement::Pointer {
                        x: mouse.column,
                        y: mouse.row,
                    },
                ));
                outcome.repaint = true;
            }
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
                        } else if double {
                            self.investigation_open_file(OpenFileMode::Editor, outcome);
                        }
                    }
                }
                MouseEventKind::Down(MouseButton::Right) => {
                    let target = match row {
                        Some(index) => {
                            tab.focus = Focus::Results;
                            select_for_menu(tab, index);
                            tab.row_menu_target(index)
                        }
                        None if tab.content_search => MenuTarget::Search,
                        None => MenuTarget::Tree,
                    };
                    tab.dialog = Some(Dialog::menu(
                        target,
                        MenuPlacement::Pointer {
                            x: mouse.column,
                            y: mouse.row,
                        },
                    ));
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
                }
                _ => {}
            }
            outcome.repaint = true;
            return true;
        }
        if !docked {
            return false;
        }
        // Docked, the pane surface around the region holds terminals: an
        // event on the title or footer must not fall through to
        // their handlers, and a right-click there opens the workbench menu.
        if mouse.kind == MouseEventKind::Down(MouseButton::Right) {
            let target = if tab.content_search {
                MenuTarget::Search
            } else {
                MenuTarget::Tree
            };
            tab.dialog = Some(Dialog::menu(
                target,
                MenuPlacement::Pointer {
                    x: mouse.column,
                    y: mouse.row,
                },
            ));
            outcome.repaint = true;
        }
        true
    }
}

/// Opens the files-wide menu with its corner at the clicked cell.
/// Why not over another dialog: replacing a confirmation or a typed name
/// would lose it.
fn open_files_menu(tab: &mut Investigation, mouse: MouseEvent) {
    if tab
        .dialog
        .as_ref()
        .is_some_and(|dialog| !matches!(dialog, Dialog::Menu { .. }))
    {
        return;
    }
    tab.dialog = Some(Dialog::menu(
        MenuTarget::Tree,
        MenuPlacement::Pointer {
            x: mouse.column,
            y: mouse.row,
        },
    ));
}

/// Makes the right-clicked row the one the menu acts on. A row inside a
/// multi-selection keeps it, so the menu acts on every marked entry.
fn select_for_menu(tab: &mut Investigation, index: usize) {
    if tab.content_search {
        tab.selected_match = index;
        return;
    }
    tab.browser.selected = index;
    let marked = tab
        .browser
        .selected_entry()
        .is_some_and(|entry| tab.selected_files.contains(&entry.path));
    if !marked {
        tab.selected_files.clear();
    }
}

/// A files action users can rebind by name in `ui.file_workbench_keys`.
struct NamedKey {
    name: &'static str,
    /// The key a configured one replaces.
    alias: (KeyCode, M),
    /// The key the router handles for it.
    target: (KeyCode, M),
    /// Only on the list without a dialog: elsewhere the key types or moves
    /// in a menu.
    list_only: bool,
}

/// A key known by `name`, wherever the files have the keys.
const fn named(name: &'static str, key: (KeyCode, M)) -> NamedKey {
    NamedKey {
        name,
        alias: key,
        target: key,
        list_only: false,
    }
}

/// `alias` standing for `target` on the list.
const fn on_list(name: &'static str, alias: (KeyCode, M), target: (KeyCode, M)) -> NamedKey {
    NamedKey {
        name,
        alias,
        target,
        list_only: true,
    }
}

const fn bare(code: KeyCode) -> (KeyCode, M) {
    (code, M::empty())
}

const fn plain(c: char) -> (KeyCode, M) {
    bare(KeyCode::Char(c))
}

const fn shifted(c: char) -> (KeyCode, M) {
    (KeyCode::Char(c), M::SHIFT)
}

const fn ctrl(c: char) -> (KeyCode, M) {
    (KeyCode::Char(c), M::CONTROL)
}

// Why `pin` and `read`: the names users configured when Enter pinned a
// preview and Space read one; they now open the editor and pager.
// Why the vim letters stand for the arrows: a configured key replaces only
// the letter, and the arrows keep working.
const NAMED_KEYS: &[NamedKey] = &[
    on_list("pin", bare(KeyCode::Enter), bare(KeyCode::Enter)),
    on_list("read", plain(' '), plain(' ')),
    on_list("down", plain('j'), bare(KeyCode::Down)),
    on_list("up", plain('k'), bare(KeyCode::Up)),
    on_list("collapse", plain('h'), bare(KeyCode::Left)),
    on_list("expand", plain('l'), bare(KeyCode::Right)),
    on_list("top", plain('g'), bare(KeyCode::Home)),
    on_list("bottom", shifted('G'), bare(KeyCode::End)),
    on_list("half_page_down", ctrl('d'), ctrl('d')),
    on_list("half_page_up", ctrl('u'), ctrl('u')),
    on_list("mark", plain('m'), bare(KeyCode::Insert)),
    on_list("details", shifted('K'), shifted('K')),
    on_list("next_file", plain('n'), plain('n')),
    on_list("previous_file", shifted('N'), shifted('N')),
    named("find_files", ctrl('p')),
    named("search", (KeyCode::Char('f'), M::CONTROL.union(M::SHIFT))),
    named("replace", ctrl('h')),
    named("edit", ctrl('e')),
    named("copy", ctrl('c')),
    named("cut", ctrl('x')),
    named("paste", ctrl('v')),
    named("select_all", ctrl('a')),
    named("rename", bare(KeyCode::F(2))),
    named("delete", bare(KeyCode::Delete)),
    named("actions", bare(KeyCode::F(10))),
    named("files_menu", (KeyCode::F(10), M::SHIFT)),
    named("refresh", bare(KeyCode::F(5))),
];

impl ClientShellState {
    /// The key as the router should see it: a configured key becomes its
    /// action's, the alias it replaced does nothing, and an unconfigured
    /// alias becomes its target.
    fn investigation_key(&self, key: &crate::input::TerminalKey) -> crate::input::TerminalKey {
        let on_list = self
            .investigation()
            .is_some_and(|t| t.focus == Focus::Results && t.dialog.is_none());
        // Why: legacy terminals send a capital without Shift.
        let mut shifted_capital;
        let key = match key.code {
            KeyCode::Char(c) if c.is_ascii_uppercase() && !key.modifiers.contains(M::SHIFT) => {
                shifted_capital = key.clone();
                shifted_capital.modifiers |= M::SHIFT;
                &shifted_capital
            }
            _ => key,
        };
        let matches = |combo| crate::config::terminal_key_matches_combo(key, combo);
        let mut disabled = false;
        let mut aliased = None;
        for named in NAMED_KEYS {
            if named.list_only && !on_list {
                continue;
            }
            match self.config.file_workbench_keys.get(named.name) {
                Some(configured) => {
                    if crate::config::parse_key_combo(configured).is_some_and(matches) {
                        return with_combo(key, named.target);
                    }
                    if matches(named.alias) {
                        disabled = true;
                    }
                }
                // Why `top` passes `g` through: `gg` is two presses, which
                // the router counts.
                None if named.name != "top" && aliased.is_none() && matches(named.alias) => {
                    aliased = Some(named.target);
                }
                None => {}
            }
        }
        if disabled {
            let mut result = key.clone();
            result.code = KeyCode::Null;
            result.generated_text = None;
            return result;
        }
        aliased.map_or_else(|| key.clone(), |target| with_combo(key, target))
    }
}

fn with_combo(
    key: &crate::input::TerminalKey,
    (code, modifiers): (KeyCode, M),
) -> crate::input::TerminalKey {
    let mut remapped = key.clone();
    remapped.code = code;
    remapped.modifiers = modifiers;
    remapped
}
