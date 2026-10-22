use super::super::render::{display_width, put_text};
use super::*;
use crossterm::event::KeyCode;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

fn text(b: &mut Buffer, r: Rect, value: &str, style: Style) {
    put_text(b, r.x, r.y, r.width, value, style);
}
fn line(area: Rect, offset: u16) -> Rect {
    Rect::new(
        area.x,
        area.y.saturating_add(offset),
        area.width,
        u16::from(offset < area.height),
    )
}

impl ClientShellState {
    pub(in crate::client::shell) fn render_investigation(
        &mut self,
        b: &mut Buffer,
        layout: ClientShellLayout,
    ) -> Option<crate::protocol::CursorState> {
        self.investigation_hits = Hits::default();
        let active = self.investigation_active();
        let snapshot = self.snapshot.as_deref()?;
        let tabs: Vec<_> = self
            .workbench
            .tabs
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                t.endpoint == self.active_endpoint_id
                    && t.browser.workspace_id == snapshot.focused_workspace_id
            })
            .map(|(i, t)| {
                (
                    i,
                    format!(
                        "{}{}",
                        t.browser
                            .location
                            .as_ref()
                            .map_or_else(String::new, |location| format!("{}:", location.label())),
                        t.browser
                            .root
                            .as_deref()
                            .unwrap_or("loading…")
                            .rsplit('/')
                            .find(|s| !s.is_empty())
                            .unwrap_or("/")
                    ),
                )
            })
            .collect();
        if tabs.is_empty() {
            return None;
        }
        let p = &self.config.palette;
        let base = Style::default().fg(p.text).bg(p.panel_bg);
        let muted = base.fg(p.overlay0);
        let selected = base.fg(p.panel_bg).bg(p.accent);
        let bar = layout.tab_bar;
        if bar.width > 0 && bar.height > 0 {
            let width = ((tabs.len() as u16).saturating_mul(18))
                .min(bar.width / 2)
                .max(1);
            let terminal = Rect::new(bar.x, bar.y, bar.width.saturating_sub(width), bar.height);
            self.hits.tabs.clear();
            super::super::render::render_tab_bar(
                b,
                terminal,
                snapshot,
                &self.config,
                &mut self.tab_scroll,
                &mut self.reveal_focused_tab,
                None,
                &mut self.hits,
            );
            self.investigation_hits.terminal_tabs = self.hits.tabs.clone();
            let mut x = terminal.right();
            let visible = (width / 12).max(1) as usize;
            let selected_position = tabs
                .iter()
                .position(|(i, _)| Some(*i) == self.workbench.active)
                .unwrap_or(0);
            let start = selected_position.saturating_sub(visible.saturating_sub(1));
            for (index, label) in tabs.iter().skip(start).take(visible) {
                let remaining = bar.right().saturating_sub(x);
                let rect = Rect::new(x, bar.y, remaining.min(width / (visible as u16)), 1);
                b.set_style(
                    rect,
                    if active && Some(*index) == self.workbench.active {
                        selected
                    } else {
                        base
                    },
                );
                text(
                    b,
                    rect,
                    &format!(" ◈ {label} "),
                    if active && Some(*index) == self.workbench.active {
                        selected
                    } else {
                        base
                    },
                );
                self.investigation_hits.tabs.push((rect, *index));
                x = rect.right();
            }
        }
        if !active {
            return None;
        }
        let area = layout.pane_surface;
        b.set_style(area, base);
        for y in area.y..area.bottom() {
            text(
                b,
                Rect::new(area.x, y, area.width, 1),
                &" ".repeat(area.width as usize),
                base,
            );
        }
        let index = self.workbench.active?;
        let tab = &mut self.workbench.tabs[index];
        let head = format!(
            " {}{} · {}",
            tab.browser.location_prefix(),
            tab.browser.root.as_deref().unwrap_or("loading…"),
            if tab.content_search && !tab.search_ready {
                "content search (updating / stale)"
            } else if tab.content_search {
                "content search"
            } else {
                "files"
            }
        );
        let title = format!("{head} · F10 actions");
        text(b, line(area, 0), &title, base.add_modifier(Modifier::BOLD));
        let actions = Rect::new(
            area.right().saturating_sub(13).max(area.x),
            area.y,
            13.min(area.width),
            1,
        );
        text(b, actions, "[F10 actions]", base.fg(p.accent));
        self.investigation_hits.buttons.push((actions, usize::MAX));
        // Dropped rather than overlapped when the title needs the room.
        let refresh_width = 12u16;
        let refresh_x = actions.x.saturating_sub(refresh_width + 1);
        if actions.x > area.x + refresh_width
            && refresh_x
                >= area
                    .x
                    .saturating_add(display_width(&head))
                    .saturating_add(1)
        {
            let refresh = Rect::new(refresh_x, area.y, refresh_width, 1);
            text(b, refresh, "[F5 refresh]", base.fg(p.accent));
            self.investigation_hits.buttons.push((refresh, 23));
        }
        let file_bar = line(area, 1);
        let mut x = file_bar.x;
        // Show a bounded neighborhood around the current file tab.
        let first = tab.active_file.unwrap_or(0).saturating_sub(2);
        for (i, document) in tab.documents.iter().enumerate().skip(first) {
            if x >= file_bar.right() {
                break;
            }
            let label = format!(
                " {} × ",
                document
                    .path
                    .rsplit(['/', '\\'])
                    .next()
                    .unwrap_or(&document.path)
            );
            let rect = Rect::new(
                x,
                file_bar.y,
                (display_width(&label) + 1).min(file_bar.right() - x),
                file_bar.height,
            );
            let style = if tab.active_file == Some(i) {
                selected
            } else {
                base.bg(p.surface0)
            };
            b.set_style(rect, style);
            text(b, rect, &label, style);
            if rect.width >= 4 {
                let close = Rect::new(rect.right() - 2, rect.y, 1, 1);
                text(b, close, "×", style);
                self.investigation_hits.file_close.push((close, i));
            }
            self.investigation_hits.files.push((rect, i));
            x = rect.right();
        }
        if let Some(document) = &tab.preview {
            let label = format!(
                " {} (preview)",
                document
                    .path
                    .rsplit(['/', '\\'])
                    .next()
                    .unwrap_or(&document.path)
            );
            if x < file_bar.right() {
                let rect = Rect::new(x, file_bar.y, file_bar.right() - x, file_bar.height);
                text(b, rect, &label, muted.add_modifier(Modifier::ITALIC));
                self.investigation_hits.files.push((rect, usize::MAX));
            }
        }
        let search_height = if tab.content_search { 5 } else { 1 };
        let mut cursor = None;
        let fields: Vec<_> = if tab.content_search {
            vec![
                (Focus::Query, "find", &tab.query),
                (Focus::Include, "include", &tab.include),
                (Focus::Exclude, "exclude", &tab.exclude),
                (Focus::Replacement, "replace", &tab.replacement),
            ]
        } else {
            vec![(Focus::Paths, "files", &tab.browser.query)]
        };
        for (offset, (focus, label, editor)) in fields.into_iter().enumerate() {
            let rect = line(area, 2 + offset as u16);
            text(b, rect, &format!(" {label}: "), muted);
            let field = Rect::new(
                rect.x + 9.min(rect.width),
                rect.y,
                rect.width.saturating_sub(9),
                rect.height,
            );
            if tab.focus == focus {
                cursor = super::super::text_editor::render(b, field, editor, base.bg(p.surface0));
            } else {
                text(b, field, editor.as_str(), base);
            }
            self.investigation_hits.fields.push((field, focus));
        }
        if tab.content_search {
            let mut x = area.x;
            for (action, label) in [
                (
                    16,
                    format!(" [{}] regex ", if tab.regex { "x" } else { " " }),
                ),
                (
                    17,
                    format!(" [{}] case ", if tab.case_sensitive { "x" } else { " " }),
                ),
                (
                    18,
                    format!(" [{}] word ", if tab.whole_word { "x" } else { " " }),
                ),
                (
                    19,
                    format!(" [{}] ignored ", if tab.ignored { "x" } else { " " }),
                ),
                (14, " [preview replace] ".into()),
            ] {
                let rect = Rect::new(
                    x,
                    area.y + 6,
                    display_width(&label).min(area.right().saturating_sub(x)),
                    u16::from(area.height > 6),
                );
                text(b, rect, &label, base.fg(p.accent));
                self.investigation_hits.buttons.push((rect, action));
                x = rect.right();
            }
        }
        let body = Rect::new(
            area.x,
            area.y.saturating_add(2 + search_height),
            area.width,
            area.height.saturating_sub(4 + search_height),
        );
        let (list, document_area) = if tab.horizontal {
            let width = (body.width / 3).max(1);
            (
                Rect::new(body.x, body.y, width, body.height),
                Rect::new(
                    body.x.saturating_add(width),
                    body.y,
                    body.width.saturating_sub(width),
                    body.height,
                ),
            )
        } else {
            let height = (body.height / 3).max(1).min(body.height);
            (
                Rect::new(body.x, body.y, body.width, height),
                Rect::new(
                    body.x,
                    body.y.saturating_add(height),
                    body.width,
                    body.height.saturating_sub(height),
                ),
            )
        };
        self.investigation_hits.list = list;
        let row_count = if tab.content_search {
            tab.matches.len()
        } else {
            tab.browser.rows.len()
        };
        let selected_row = if tab.content_search {
            tab.selected_match
        } else {
            tab.browser.selected
        };
        let scroll = selected_row
            .saturating_add(1)
            .saturating_sub(usize::from(list.height))
            .min(row_count);
        self.investigation_hits.scroll = scroll;
        for row in scroll..row_count.min(scroll + usize::from(list.height)) {
            let rect = line(list, (row - scroll) as u16);
            let label = if tab.content_search {
                let found = &tab.matches[row];
                let root = tab.browser.root.as_deref().unwrap_or_default();
                let path = found
                    .path
                    .strip_prefix(root)
                    .unwrap_or(&found.path)
                    .trim_start_matches('/');
                format!(
                    " [{}] {}:{}  {}",
                    if tab.omitted.contains(&row) { " " } else { "x" },
                    path,
                    found.line,
                    found.text
                )
            } else {
                let item = &tab.browser.rows[row];
                let marked = item
                    .entry
                    .as_ref()
                    .is_some_and(|e| tab.selected_files.contains(&e.path));
                format!(
                    "{}{:indent$}{}",
                    if marked { "●" } else { " " },
                    "",
                    item.label,
                    indent = usize::from(item.depth) * 2
                )
            };
            let style = if row == selected_row {
                if tab.focus == Focus::Results {
                    selected
                } else {
                    base.bg(p.surface0)
                }
            } else if tab.hovered == Some(row) {
                base.bg(p.surface1)
            } else {
                base
            };
            b.set_style(rect, style);
            text(b, rect, &label, style);
            self.investigation_hits.rows.push((rect, row));
        }
        if row_count == 0 {
            text(
                b,
                line(list, 0),
                if tab.searching {
                    " searching…"
                } else {
                    " no results"
                },
                muted,
            );
        }
        let header = line(document_area, 0);
        let document = tab.document();
        text(
            b,
            header,
            &document.map_or_else(
                || " select a file to preview".into(),
                |d| {
                    format!(
                        " {}{}",
                        d.path,
                        if d.pending.is_some() {
                            " · file changed (selection held)"
                        } else {
                            ""
                        }
                    )
                },
            ),
            base.fg(p.accent),
        );
        let content = Rect::new(
            document_area.x,
            document_area.y.saturating_add(1),
            document_area.width,
            document_area.height.saturating_sub(1),
        );
        self.investigation_hits.body = content;
        if let Some(document) = document {
            if !document.loaded {
                text(b, line(content, 0), " loading…", muted);
            } else {
                let range = document.range();
                for (offset, line_index) in (document.scroll..document.lines.len())
                    .take(content.height as usize)
                    .enumerate()
                {
                    let rect = line(content, offset as u16);
                    text(b, rect, &format!("{:>5} │", line_index + 1), muted);
                    let mut column = 0usize;
                    for (byte, value) in document.line_text(line_index).grapheme_indices(true) {
                        let size = if value == "\t" {
                            4 - column % 4
                        } else {
                            UnicodeWidthStr::width(value)
                        };
                        if column + size <= document.horizontal {
                            column += size;
                            continue;
                        }
                        let x = content
                            .x
                            .saturating_add(7)
                            .saturating_add(column.saturating_sub(document.horizontal) as u16);
                        if x >= content.right() {
                            break;
                        }
                        let selected_byte = range
                            .as_ref()
                            .is_some_and(|r| r.contains(&(document.lines[line_index] + byte)));
                        let style = if selected_byte { selected } else { base };
                        if value == "\t" {
                            put_text(b, x, rect.y, content.right() - x, &" ".repeat(size), style);
                        } else if !value.chars().any(char::is_control) {
                            put_text(b, x, rect.y, content.right() - x, value, style);
                        }
                        column += size;
                    }
                }
                if tab.focus == Focus::Body {
                    let line = document.line();
                    let column = document.text[document.lines[line]..document.cursor]
                        .graphemes(true)
                        .fold(0, |x, v| {
                            x + if v == "\t" {
                                4 - x % 4
                            } else {
                                UnicodeWidthStr::width(v)
                            }
                        });
                    let x = content
                        .x
                        .saturating_add(7)
                        .saturating_add(column.saturating_sub(document.horizontal) as u16);
                    let y = content
                        .y
                        .saturating_add(line.saturating_sub(document.scroll) as u16);
                    if x < content.right() && y < content.bottom() {
                        cursor = Some(crate::protocol::CursorState {
                            x,
                            y,
                            visible: true,
                            shape: Default::default(),
                        });
                    }
                }
            }
        }
        let footer = line(area, area.height.saturating_sub(2));
        let notice = if tab.notice.is_empty() {
            tab.browser.notice.as_deref().unwrap_or_default()
        } else {
            &tab.notice
        };
        text(b, footer, notice, muted);
        let clipboard = self
            .workbench
            .clipboard
            .as_ref()
            .map(|c| {
                format!(
                    " · {} {} items",
                    if c.cut { "cut" } else { "copy" },
                    c.paths.len()
                )
            })
            .unwrap_or_default();
        text(b,line(area,area.height.saturating_sub(1)),&format!(" Enter pin · Space read · Esc back · Tab focus · Ctrl+P files · Ctrl+Shift+F search{clipboard}"),muted);
        if tab.focus == Focus::Find {
            let rect = line(area, area.height.saturating_sub(2));
            text(b, rect, " find: ", muted);
            let rect = Rect::new(
                rect.x + 7.min(rect.width),
                rect.y,
                rect.width.saturating_sub(7),
                rect.height,
            );
            cursor = super::super::text_editor::render(b, rect, &tab.find, base);
        }
        if let Some(dialog) = &tab.dialog {
            cursor = render_dialog(b, area, dialog, tab, p, &mut self.investigation_hits);
        }
        self.hits.panes.clear();
        self.hits.pane_splits.clear();
        self.hits.popup = None;
        cursor
    }
}
fn render_dialog(
    b: &mut Buffer,
    area: Rect,
    dialog: &Dialog,
    tab: &Investigation,
    p: &Palette,
    hits: &mut Hits,
) -> Option<crate::protocol::CursorState> {
    let width = area.width.saturating_sub(4).min(110);
    let height = area.height.saturating_sub(2).min(28);
    let popup = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    let base = Style::default().fg(p.text).bg(p.surface0);
    for y in popup.y..popup.bottom() {
        text(
            b,
            Rect::new(popup.x, y, popup.width, 1),
            &" ".repeat(popup.width as usize),
            base,
        );
    }
    let mut values = Vec::new();
    match dialog {
        Dialog::Menu { selected, scroll } => {
            // Why clamped, not corrected: render only reads, and a resize
            // can leave the stored offset past the last page.
            let scroll = (*scroll).min(MENU.len().saturating_sub(popup.height as usize));
            for (i, label) in MENU
                .iter()
                .enumerate()
                .skip(scroll)
                .take(popup.height as usize)
            {
                let rect = line(popup, (i - scroll) as u16);
                text(
                    b,
                    rect,
                    &format!(" {label}"),
                    if i == *selected {
                        base.bg(p.accent).fg(p.panel_bg)
                    } else {
                        base
                    },
                );
                hits.menu.push((rect, i));
            }
        }
        Dialog::Input { title, input, .. } => {
            text(b, line(popup, 0), title, base);
            text(b, line(popup, 3), "Enter continue · Esc cancel", base);
            let apply = line(popup, popup.height.saturating_sub(1));
            text(b, apply, "[continue]  [cancel]", base.fg(p.accent));
            hits.dialog_keys.push((
                Rect::new(apply.x, apply.y, 10.min(apply.width), 1),
                KeyCode::Enter,
                crossterm::event::KeyModifiers::empty(),
            ));
            hits.dialog_keys.push((
                Rect::new(
                    (apply.x + 12).min(apply.right()),
                    apply.y,
                    8.min(apply.width.saturating_sub(12)),
                    1,
                ),
                KeyCode::Esc,
                crossterm::event::KeyModifiers::empty(),
            ));
            return super::super::text_editor::render(b, line(popup, 2), input, base);
        }
        Dialog::Confirm {
            params,
            selected,
            outcomes,
        } => {
            values.push(format!(
                "{:?}: {} items",
                params.operation,
                params.paths.len()
            ));
            if let Some(destination) = &params.destination {
                values.push(format!("destination: {destination}"));
            }
            values.push(format!(
                "collision: {}skip  {}rename  {}replace files",
                if *selected == 0 { "●" } else { " " },
                if *selected == 1 { "●" } else { " " },
                if *selected == 2 { "●" } else { " " }
            ));
            values.push("Left/Right choose · Enter execute · Esc cancel".into());
            for item in outcomes
                .iter()
                .take(popup.height.saturating_sub(5) as usize)
            {
                values.push(format!(
                    "{} → {}{}",
                    item.path,
                    item.destination.as_deref().unwrap_or("—"),
                    item.error
                        .as_ref()
                        .map_or(String::new(), |e| format!(" · {e}"))
                ));
            }
        }
        Dialog::Upload(dialog) => {
            values.push("Upload dropped files into".into());
            values.push(dialog.destination.clone());
            values.push(dialog.summary.clone());
            if !dialog.conflicts.is_empty() {
                values.push(format!("already there: {}", dialog.conflicts.join(", ")));
            }
            if let Some(note) = &dialog.note {
                values.push(note.clone());
            }
            values.push("Left/Right choose · Enter confirm · Esc cancel".into());
            if popup.height >= 2 {
                use crossterm::event::KeyModifiers as M;
                let y = popup.bottom().saturating_sub(1);
                let mut x = popup.x;
                for (index, choice) in dialog.choices().iter().enumerate() {
                    let label = choice.label();
                    let rect = Rect::new(
                        x,
                        y,
                        display_width(label).min(popup.right().saturating_sub(x)),
                        1,
                    );
                    let style = if index == dialog.selected {
                        base.bg(p.accent).fg(p.panel_bg)
                    } else {
                        base.fg(p.accent)
                    };
                    text(b, rect, label, style);
                    // `1`.. picks the n-th button, as the keys do.
                    let key = char::from_digit(index as u32 + 1, 10).unwrap_or('1');
                    hits.dialog_keys
                        .push((rect, KeyCode::Char(key), M::empty()));
                    x = rect.right().saturating_add(2).min(popup.right());
                }
            }
        }
        Dialog::Send { path, pane, .. } => {
            values.push(format!("Send path to terminal pane {pane}?"));
            values.push(path.clone());
            values.push("Enter sends text only · Esc cancel".into());
        }
        Dialog::Replace { scroll } => {
            values.push(format!(
                "Replacement preview: {} files · {} matches · {} reads pending",
                tab.plan.len(),
                tab.matches.len().saturating_sub(tab.omitted.len()),
                tab.prepare_pending + tab.prepare_queue.len()
            ));
            values.push("Ctrl+Enter apply · Esc cancel".into());
            // Diff generation is performed only when an explicit preview opens.
            // This display streams lines and never mutates the plan.
            let lines = tab.plan.iter().flat_map(|plan| plan.diff.iter().cloned());
            values.extend(
                lines
                    .skip(*scroll)
                    .take(popup.height.saturating_sub(2) as usize),
            );
        }
    }
    for (i, value) in values.iter().enumerate().take(popup.height as usize) {
        text(b, line(popup, i as u16), value, base);
    }
    if !matches!(dialog, Dialog::Menu { .. } | Dialog::Upload(_)) && popup.height >= 4 {
        use crossterm::event::KeyModifiers as M;
        let y = popup.bottom().saturating_sub(1);
        let label = match dialog {
            Dialog::Replace { .. } => "[apply replacement]",
            Dialog::Send { .. } => "[send path]",
            Dialog::Confirm { .. } => "[execute]",
            _ => "[continue]",
        };
        let apply = Rect::new(popup.x, y, display_width(label).min(popup.width), 1);
        text(b, apply, label, base.fg(p.accent));
        hits.dialog_keys.push((
            apply,
            KeyCode::Enter,
            if matches!(dialog, Dialog::Replace { .. }) {
                M::CONTROL
            } else {
                M::empty()
            },
        ));
        let cancel = Rect::new(
            apply.right().saturating_add(2).min(popup.right()),
            y,
            8.min(popup.right().saturating_sub(apply.right() + 2)),
            1,
        );
        text(b, cancel, "[cancel]", base);
        hits.dialog_keys.push((cancel, KeyCode::Esc, M::empty()));
        if matches!(dialog, Dialog::Confirm { .. }) {
            let mut x = popup.x;
            for (key, label) in [
                (KeyCode::Char('1'), "[skip] "),
                (KeyCode::Char('2'), "[rename] "),
                (KeyCode::Char('3'), "[replace files] "),
            ] {
                let rect = Rect::new(
                    x,
                    popup.y + 2,
                    display_width(label).min(popup.right().saturating_sub(x)),
                    1,
                );
                text(b, rect, label, base.fg(p.accent));
                hits.dialog_keys.push((rect, key, M::empty()));
                x = rect.right();
            }
        }
    }
    None
}
