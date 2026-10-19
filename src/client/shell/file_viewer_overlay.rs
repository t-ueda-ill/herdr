use super::*;

/// Draws the cached rows of the file viewer. Why nothing is computed here:
/// filtering and ranking run when their inputs change, not once per frame.
pub(super) fn render_file_viewer_overlay(
    b: &mut Buffer,
    viewer: &ClientFileViewerOverlay,
    p: &Palette,
) -> Option<OverlayRender> {
    let popup = popup(b.area, 100, b.area.height.saturating_sub(2).min(32))?;
    let inner = panel(b, popup, p.accent, p.panel_bg)?;
    let title = left_ellipsized(&viewer.title(), inner.width.saturating_sub(2));
    put_text(
        b,
        inner.x + 1,
        inner.y,
        inner.width.saturating_sub(1),
        &title,
        Style::default()
            .fg(p.text)
            .bg(p.panel_bg)
            .add_modifier(Modifier::BOLD),
    );

    let filter = Rect::new(inner.x, inner.y.saturating_add(1), inner.width, 1);
    let status = if viewer.indexing() {
        "indexing…".to_owned()
    } else if viewer.searching() {
        match viewer.match_count {
            1 => "1 match".to_owned(),
            count => format!("{count} matches"),
        }
    } else {
        String::new()
    };
    put_text(
        b,
        filter.x,
        filter.y,
        filter.width,
        &if viewer.search_focused {
            " / ".to_owned()
        } else if viewer.searching() {
            format!(" / {}", viewer.query)
        } else {
            " / filter".to_owned()
        },
        Style::default()
            .fg(if viewer.search_focused {
                p.text
            } else {
                p.overlay0
            })
            .bg(p.panel_bg),
    );
    let cursor = if viewer.search_focused {
        text_editor::render(
            b,
            Rect::new(
                filter.x + 3,
                filter.y,
                filter.width.saturating_sub(4 + display_width(&status)),
                1,
            ),
            &viewer.query,
            Style::default().fg(p.text).bg(p.panel_bg),
        )
    } else {
        None
    };
    put_right_text(
        b,
        filter,
        filter.y,
        &status,
        Style::default().fg(p.overlay0).bg(p.panel_bg),
    );
    put_text(
        b,
        inner.x,
        inner.y.saturating_add(2),
        inner.width,
        &"─".repeat(usize::from(inner.width)),
        Style::default().fg(p.surface1).bg(p.panel_bg),
    );

    let body = Rect::new(
        inner.x,
        inner.y.saturating_add(3),
        inner.width,
        inner.height.saturating_sub(5),
    );
    let visible = usize::from(body.height);
    let max_scroll = viewer.rows.len().saturating_sub(visible);
    let scroll = viewer
        .scroll
        .max((viewer.selected + 1).saturating_sub(visible))
        .min(viewer.selected)
        .min(max_scroll);
    let mut row_hits = Vec::new();
    for (index, row) in viewer.rows.iter().enumerate().skip(scroll).take(visible) {
        let rect = Rect::new(body.x, body.y + (index - scroll) as u16, body.width, 1);
        let selected = index == viewer.selected && row.entry.is_some();
        let style = if selected {
            Style::default()
                .fg(contrast(p))
                .bg(p.accent)
                .add_modifier(Modifier::BOLD)
        } else if row.entry.is_none() {
            Style::default()
                .fg(p.overlay0)
                .bg(p.panel_bg)
                .add_modifier(Modifier::ITALIC)
        } else if row.entry.as_ref().is_some_and(|entry| entry.is_dir) {
            Style::default()
                .fg(p.text)
                .bg(p.panel_bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.text).bg(p.panel_bg)
        };
        b.set_style(rect, style);
        let indent = usize::from(row.depth) * 2 + 1;
        put_text(
            b,
            rect.x,
            rect.y,
            rect.width,
            &format!("{:indent$}{}", "", row.label),
            style,
        );
        if row.entry.is_some() {
            row_hits.push((rect, index));
        }
    }

    if let Some(notice) = viewer.notice.as_deref() {
        put_text(
            b,
            inner.x,
            inner.bottom().saturating_sub(2),
            inner.width,
            &format!(" {notice}"),
            Style::default().fg(p.subtext0).bg(p.panel_bg),
        );
    }
    let buttons = render_buttons(b, inner, viewer, p);
    Some(OverlayRender {
        area: popup,
        file_viewer_popup: popup,
        file_viewer_filter: filter,
        file_viewer_rows: row_hits,
        file_viewer_buttons: buttons,
        file_viewer_scroll: scroll,
        cursor,
        ..OverlayRender::default()
    })
}

/// One line of `label · label`, dropping buttons from the right when the
/// panel is too narrow.
fn render_buttons(
    b: &mut Buffer,
    inner: Rect,
    viewer: &ClientFileViewerOverlay,
    p: &Palette,
) -> Vec<(Rect, FileViewerButton)> {
    const SEPARATOR: &str = " · ";
    let y = inner.bottom().saturating_sub(1);
    let selected = viewer.selected_entry();
    let can_send = viewer.pane_id.is_some() && selected.is_some();
    let mut x = inner.x + 1;
    let mut hits = Vec::new();
    for (position, (button, label)) in FileViewerButton::ALL.iter().enumerate() {
        let separator = if position == 0 { "" } else { SEPARATOR };
        let width = display_width(separator) + display_width(label);
        if x.saturating_add(width) > inner.right() {
            break;
        }
        let enabled = match button {
            FileViewerButton::Send | FileViewerButton::Keep => can_send,
            FileViewerButton::Edit => can_send && selected.is_some_and(|entry| !entry.is_dir),
            FileViewerButton::Copy => selected.is_some(),
            FileViewerButton::Close => true,
        };
        put_text(
            b,
            x,
            y,
            display_width(separator),
            separator,
            Style::default().fg(p.overlay0).bg(p.panel_bg),
        );
        let label_x = x + display_width(separator);
        let rect = Rect::new(label_x, y, display_width(label), 1);
        put_text(
            b,
            rect.x,
            rect.y,
            rect.width,
            label,
            Style::default()
                .fg(if enabled { p.text } else { p.overlay0 })
                .bg(p.panel_bg),
        );
        hits.push((rect, *button));
        x = rect.right();
    }
    hits
}

/// Keeps the end of a long path, where the directory names that matter are.
fn left_ellipsized(text: &str, width: u16) -> String {
    if display_width(text) <= width {
        return text.to_owned();
    }
    let mut kept: Vec<char> = Vec::new();
    let mut used = 1;
    for ch in text.chars().rev() {
        let ch_width = display_width(ch.encode_utf8(&mut [0; 4]));
        if used + ch_width > width {
            break;
        }
        used += ch_width;
        kept.push(ch);
    }
    std::iter::once('…').chain(kept.into_iter().rev()).collect()
}
