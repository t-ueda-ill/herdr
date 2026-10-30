use super::super::render::{display_width, menu_widget, put_text};
use super::*;
use crossterm::event::KeyCode;

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
    /// Stores the window the last frame drew for the actions menu.
    ///
    /// Why not in render: it only reads. A shrinking terminal leaves the
    /// stored offset hiding the selection, and Enter would run it unseen.
    pub(in crate::client::shell) fn reconcile_menu_scroll(&mut self) {
        // Why not when nothing was drawn: a hidden workbench leaves default
        // hits, and writing their 0 would jump the menu when shown again.
        if self.investigation_hits.menu.is_empty() {
            return;
        }
        let drawn = self.investigation_hits.menu_scroll;
        if let Some(Dialog::Menu { scroll, .. }) =
            self.investigation_mut().and_then(|t| t.dialog.as_mut())
        {
            *scroll = drawn;
        }
    }
    pub(in crate::client::shell) fn render_investigation(
        &mut self,
        b: &mut Buffer,
        layout: ClientShellLayout,
    ) -> Option<crate::protocol::CursorState> {
        self.investigation_hits = Hits::default();
        let active = self.investigation_active();
        let in_front = self.files_has_focus();
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
                    if in_front && Some(*index) == self.workbench.active {
                        selected
                    } else {
                        base
                    },
                );
                text(
                    b,
                    rect,
                    &format!(" ◈ {label} "),
                    if in_front && Some(*index) == self.workbench.active {
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
        let (area, docked) = match layout.files_region {
            Some(region) => (region, true),
            None => (layout.pane_surface, false),
        };
        self.investigation_hits.region = area;
        self.investigation_hits.docked = docked;
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
        let presentation = self.workbench.placement.presentation;
        let dock = self.workbench.placement.dock;
        // Why taken before the tab: the tab is borrowed mutably below.
        let footer_items = self.files_footer_items(&self.workbench.tabs[index]);
        let menu_hints: Vec<Option<String>> = match &self.workbench.tabs[index].dialog {
            Some(Dialog::Menu { target, .. }) => self.workbench.tabs[index]
                .menu_labels(presentation, dock)
                .iter()
                .map(|label| self.files_menu_hint(*target, label))
                .collect(),
            _ => Vec::new(),
        };
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
        text(b, line(area, 0), &head, base.add_modifier(Modifier::BOLD));
        self.investigation_hits.title = line(area, 0);
        let menu_width = 3u16;
        let actions = Rect::new(
            area.right().saturating_sub(menu_width).max(area.x),
            area.y,
            menu_width.min(area.width),
            1,
        );
        text(b, actions, " ≡ ", base.fg(p.accent));
        self.investigation_hits.buttons.push((actions, None));
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
            self.investigation_hits
                .buttons
                .push((refresh, Some(REFRESH_FILES)));
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
            let rect = line(area, 1 + offset as u16);
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
                    TOGGLE_REGEX,
                    format!(" [{}] regex ", if tab.regex { "x" } else { " " }),
                ),
                (
                    TOGGLE_CASE,
                    format!(" [{}] case ", if tab.case_sensitive { "x" } else { " " }),
                ),
                (
                    TOGGLE_WHOLE_WORD,
                    format!(" [{}] word ", if tab.whole_word { "x" } else { " " }),
                ),
                (
                    INCLUDE_IGNORED,
                    format!(" [{}] ignored ", if tab.ignored { "x" } else { " " }),
                ),
                (PREVIEW_REPLACEMENT, " [preview replace] ".into()),
            ] {
                let rect = Rect::new(
                    x,
                    area.y + 5,
                    display_width(&label).min(area.right().saturating_sub(x)),
                    u16::from(area.height > 5),
                );
                text(b, rect, &label, base.fg(p.accent));
                self.investigation_hits.buttons.push((rect, Some(action)));
                x = rect.right();
            }
        }
        let body = Rect::new(
            area.x,
            area.y.saturating_add(1 + search_height),
            area.width,
            area.height.saturating_sub(3 + search_height),
        );
        let max_lines = self.config.files_details.max_lines;
        let details = if self.workbench.hide_details {
            Vec::new()
        } else {
            let rows = usize::from(max_lines.min(body.height / 3));
            super::details::details_lines(tab, &self.config.files_details, rows)
        };
        let band = details_band_height(details.len(), max_lines, body.height);
        let list = Rect::new(body.x, body.y, body.width, body.height - band);
        let band_area = Rect::new(body.x, list.bottom(), body.width, band);
        self.investigation_hits.details = band_area;
        b.set_style(band_area, base.bg(p.surface0));
        for (offset, value) in details.iter().take(usize::from(band)).enumerate() {
            text(
                b,
                line(band_area, offset as u16),
                &format!(" {value}"),
                base.bg(p.surface0),
            );
        }
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
        let footer = line(area, area.height.saturating_sub(2));
        let notice = if tab.notice.is_empty() {
            tab.browser.notice.as_deref().unwrap_or_default()
        } else {
            &tab.notice
        };
        text(b, footer, notice, muted);
        let keys = line(area, area.height.saturating_sub(1));
        text(b, keys, &fit_footer(&footer_items, keys.width), muted);
        if let Some(dialog) = &tab.dialog {
            let labels = tab.menu_labels(presentation, dock);
            cursor = render_dialog(
                b,
                area,
                dialog,
                labels,
                &menu_hints,
                tab,
                p,
                &mut self.investigation_hits,
            );
        }
        // Why only over the whole surface: docked, the terminals beside the
        // region are still drawn and must keep their pointer targets.
        if !docked {
            self.hits.panes.clear();
            self.hits.pane_splits.clear();
            self.hits.popup = None;
        }
        cursor
    }
}
#[allow(clippy::too_many_arguments)] // Why: each is a distinct read-only input of one draw.
fn render_menu(
    b: &mut Buffer,
    labels: &[&str],
    hints: &[Option<String>],
    target: MenuTarget,
    placement: MenuPlacement,
    selected: usize,
    scroll: usize,
    tab: &Investigation,
    p: &Palette,
    hits: &mut Hits,
) {
    let items: Vec<menu_widget::MenuItem<'_>> = labels
        .iter()
        .zip(
            hints
                .iter()
                .map(Option::as_deref)
                .chain(std::iter::repeat(None)),
        )
        .map(|(label, hint)| menu_widget::MenuItem {
            hint,
            ..menu_widget::MenuItem::new(label)
        })
        .collect();
    let anchor = match placement {
        MenuPlacement::Pointer { x, y } => menu_widget::MenuAnchor::TopLeft { x, y },
        MenuPlacement::Selection => selection_anchor(target, tab, hits),
        MenuPlacement::Header => header_anchor(hits),
    };
    if let Some(rendered) = menu_widget::render_menu(
        b,
        &items,
        anchor,
        super::super::context_menu::CONTEXT_MENU_LOOK,
        Some(selected),
        scroll,
        p,
    ) {
        hits.menu = rendered.rows;
        hits.menu_area = rendered.area;
        hits.menu_scroll = rendered.scroll;
    }
}
/// Under the title line's menu button, else the region's top-left when
/// the frame had no room for it.
fn header_anchor(hits: &Hits) -> menu_widget::MenuAnchor {
    hits.buttons
        .iter()
        .find(|(_, label)| label.is_none())
        .map_or(
            menu_widget::MenuAnchor::TopLeft {
                x: hits.region.x,
                y: hits.region.y,
            },
            |(rect, _)| menu_widget::MenuAnchor::NearRect(*rect),
        )
}
/// Under the focused search field or the selected row when it is drawn,
/// else the top-left of the pane the target lives in.
fn selection_anchor(
    target: MenuTarget,
    tab: &Investigation,
    hits: &Hits,
) -> menu_widget::MenuAnchor {
    if target == MenuTarget::Details {
        return menu_widget::MenuAnchor::TopLeft {
            x: hits.details.x,
            y: hits.details.y,
        };
    }
    if let Some((field, _)) = hits
        .fields
        .iter()
        .find(|(_, focus)| target == MenuTarget::Search && *focus == tab.focus)
    {
        return menu_widget::MenuAnchor::NearRect(*field);
    }
    let selected = if tab.content_search {
        tab.selected_match
    } else {
        tab.browser.selected
    };
    hits.rows.iter().find(|(_, row)| *row == selected).map_or(
        menu_widget::MenuAnchor::TopLeft {
            x: hits.list.x,
            y: hits.list.y,
        },
        |(rect, _)| menu_widget::MenuAnchor::NearRect(*rect),
    )
}
fn render_dialog(
    b: &mut Buffer,
    area: Rect,
    dialog: &Dialog,
    labels: &[&str],
    hints: &[Option<String>],
    tab: &Investigation,
    p: &Palette,
    hits: &mut Hits,
) -> Option<crate::protocol::CursorState> {
    if let Dialog::Menu {
        target,
        anchor,
        selected,
        scroll,
    } = dialog
    {
        render_menu(
            b, labels, hints, *target, *anchor, *selected, *scroll, tab, p, hits,
        );
        return None;
    }
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
        // Drawn by `render_menu` above.
        Dialog::Menu { .. } => {}
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

/// Rows the details band takes: its lines, at most `max_lines` and a third
/// of the body, and none when the list would keep fewer than five rows.
fn details_band_height(lines: usize, max_lines: u16, body: u16) -> u16 {
    const MIN_LIST_ROWS: u16 = 5;
    let wanted = u16::try_from(lines)
        .unwrap_or(u16::MAX)
        .min(max_lines)
        .min(body / 3);
    if wanted == 0 || body.saturating_sub(wanted) < MIN_LIST_ROWS {
        0
    } else {
        wanted
    }
}

/// The items that fit `width` whole, from the left, with the key list's
/// key kept at the right edge.
fn fit_footer(items: &[String], width: u16) -> String {
    const KEYS: &str = "? keys";
    let width = usize::from(width);
    let keys = usize::from(display_width(KEYS));
    let room = width.saturating_sub(keys + 2);
    let mut shown = String::new();
    for item in items {
        let next = if shown.is_empty() {
            format!(" {item}")
        } else {
            format!("{shown} · {item}")
        };
        if usize::from(display_width(&next)) > room {
            break;
        }
        shown = next;
    }
    let gap = width.saturating_sub(usize::from(display_width(&shown)) + keys + 1);
    format!("{shown}{} {KEYS}", " ".repeat(gap))
}

#[cfg(test)]
mod band_tests {
    use super::details_band_height;

    #[test]
    fn the_band_fits_its_lines_within_a_third_of_the_body_and_yields_to_a_short_list() {
        assert_eq!(details_band_height(2, 8, 30), 2);
        assert_eq!(details_band_height(20, 8, 60), 8);
        assert_eq!(details_band_height(20, 8, 12), 4);
        assert_eq!(details_band_height(2, 8, 6), 0);
        assert_eq!(details_band_height(0, 8, 30), 0);
    }
}
