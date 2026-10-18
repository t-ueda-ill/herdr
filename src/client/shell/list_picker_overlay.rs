use super::*;

/// One row of a filterable list picker.
pub(super) struct PickerRow<'a> {
    pub(super) title: &'a str,
    pub(super) status: &'a str,
    pub(super) detail: &'a str,
}

/// A filterable list modal: title, `/` filter, two-line rows, open/cancel
/// buttons. Why not one renderer per picker: open worktree and open ssh host
/// must look and click the same way.
pub(super) struct ListPicker<'a> {
    pub(super) title: &'a str,
    pub(super) filter_hint: &'a str,
    pub(super) count_noun: &'a str,
    pub(super) empty_message: &'a str,
    pub(super) total: usize,
    pub(super) filtered: Vec<usize>,
    pub(super) selected: usize,
    pub(super) query: &'a TextEditor,
    pub(super) search_focused: bool,
    pub(super) opening: bool,
    pub(super) error: Option<&'a str>,
}

pub(super) fn render_list_picker_overlay<'a>(
    b: &mut Buffer,
    picker: &ListPicker<'_>,
    picker_row: impl Fn(usize) -> PickerRow<'a>,
    p: &Palette,
) -> Option<OverlayRender> {
    let popup_height = (picker.total.saturating_mul(2) + 7).clamp(12, 26) as u16;
    let popup = popup(b.area, 96, popup_height)?;
    let inner = panel(b, popup, p.accent, p.panel_bg)?;
    put_text(
        b,
        inner.x,
        inner.y,
        inner.width,
        picker.title,
        Style::default()
            .fg(p.text)
            .bg(p.panel_bg)
            .add_modifier(Modifier::BOLD),
    );
    let search = Rect::new(inner.x, inner.y + 1, inner.width, 1);
    let filtered = &picker.filtered;
    put_text(
        b,
        search.x,
        search.y,
        search.width,
        &if picker.search_focused {
            " / ".to_owned()
        } else if !picker.query.is_empty() {
            format!(" / {}", picker.query)
        } else {
            format!(" / {}", picker.filter_hint)
        },
        Style::default()
            .fg(if picker.search_focused {
                p.text
            } else {
                p.overlay0
            })
            .bg(p.panel_bg),
    );
    let count = if filtered.len() == picker.total {
        format!("{} {}", picker.total, picker.count_noun)
    } else {
        format!("{}/{} {}", filtered.len(), picker.total, picker.count_noun)
    };
    let cursor = if picker.search_focused {
        text_editor::render(
            b,
            Rect::new(
                search.x + 3,
                search.y,
                search.width.saturating_sub(4 + display_width(&count)),
                1,
            ),
            picker.query,
            Style::default().fg(p.text).bg(p.panel_bg),
        )
    } else {
        None
    };
    put_right_text(
        b,
        search,
        search.y,
        &count,
        Style::default().fg(p.overlay0).bg(p.panel_bg),
    );
    put_text(
        b,
        inner.x,
        inner.y + 2,
        inner.width,
        &"─".repeat(inner.width as usize),
        Style::default().fg(p.surface1).bg(p.panel_bg),
    );
    let body = Rect::new(
        inner.x,
        inner.y + 3,
        inner.width,
        inner.height.saturating_sub(6),
    );
    let visible_count = (body.height / 2).max(1) as usize;
    let selected_position = filtered
        .iter()
        .position(|index| *index == picker.selected)
        .unwrap_or(0);
    let start = selected_position
        .saturating_sub(visible_count.saturating_sub(1))
        .min(filtered.len().saturating_sub(visible_count));
    let mut row_hits = Vec::new();
    for (visible, entry_index) in filtered
        .iter()
        .copied()
        .skip(start)
        .take(visible_count)
        .enumerate()
    {
        let entry = picker_row(entry_index);
        let rect = Rect::new(body.x, body.y + visible as u16 * 2, body.width, 2);
        row_hits.push((rect, entry_index));
        let selected = entry_index == picker.selected;
        let style = if selected {
            Style::default().fg(contrast(p)).bg(p.accent)
        } else {
            Style::default().fg(p.text).bg(p.panel_bg)
        };
        b.set_style(rect, style);
        put_text(
            b,
            rect.x,
            rect.y,
            rect.width,
            &format!(" {}", entry.title),
            style.add_modifier(Modifier::BOLD),
        );
        if !entry.status.is_empty() {
            put_right_text(b, rect, rect.y, entry.status, style);
        }
        put_text(
            b,
            rect.x,
            rect.y + 1,
            rect.width,
            &format!(" {}", entry.detail),
            if selected {
                style
            } else {
                Style::default().fg(p.overlay0).bg(p.panel_bg)
            },
        );
    }
    if filtered.is_empty() {
        put_text(
            b,
            body.x,
            body.y,
            body.width,
            picker.empty_message,
            Style::default().fg(p.overlay0).bg(p.panel_bg),
        );
    }
    if picker.opening {
        put_text(
            b,
            inner.x,
            inner.bottom() - 3,
            inner.width,
            " opening…",
            Style::default().fg(p.accent).bg(p.panel_bg),
        );
    } else if let Some(error) = picker.error {
        put_text(
            b,
            inner.x,
            inner.bottom() - 3,
            inner.width,
            &format!(" {error}"),
            Style::default().fg(p.red).bg(p.panel_bg),
        );
    }
    let buttons = row(inner, &[10, 12], 2, inner.height.saturating_sub(1));
    let [primary, cancel] = buttons.as_slice() else {
        return None;
    };
    button(
        b,
        *primary,
        " ↵ open ",
        Style::default()
            .fg(contrast(p))
            .bg(p.accent)
            .add_modifier(Modifier::BOLD),
    );
    button(
        b,
        *cancel,
        " esc cancel ",
        Style::default()
            .fg(p.text)
            .bg(p.surface0)
            .add_modifier(Modifier::BOLD),
    );
    Some(OverlayRender {
        area: popup,
        primary: *primary,
        clear: Rect::default(),
        cancel: *cancel,
        navigator_popup: Rect::default(),
        navigator_search: Rect::default(),
        navigator_rows: Vec::new(),
        worktree_search: search,
        worktree_rows: row_hits,
        cursor: cursor.filter(|_| !picker.opening),
        ..OverlayRender::default()
    })
}
