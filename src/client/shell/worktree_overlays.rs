use super::*;

pub(super) fn render_worktree_create_overlay(
    b: &mut Buffer,
    create: &ClientWorktreeCreateOverlay,
    p: &Palette,
) -> Option<OverlayRender> {
    let popup = popup(b.area, 68, 12)?;
    let inner = panel(b, popup, p.accent, p.panel_bg)?;
    put_text(
        b,
        inner.x,
        inner.y,
        inner.width,
        "new worktree",
        Style::default()
            .fg(p.text)
            .bg(p.panel_bg)
            .add_modifier(Modifier::BOLD),
    );
    put_text(
        b,
        inner.x,
        inner.y + 2,
        inner.width,
        " branch",
        Style::default().fg(p.overlay0).bg(p.panel_bg),
    );
    let input = Rect::new(inner.x, inner.y + 3, inner.width, 1);
    b.set_style(input, Style::default().fg(p.text).bg(p.surface0));
    let cursor = text_editor::render(
        b,
        Rect::new(input.x + 1, input.y, input.width.saturating_sub(1), 1),
        &create.branch,
        Style::default().fg(p.text).bg(p.surface0),
    );
    put_text(
        b,
        inner.x,
        inner.y + 5,
        inner.width,
        " checkout",
        Style::default().fg(p.overlay0).bg(p.panel_bg),
    );
    put_text(
        b,
        inner.x,
        inner.y + 6,
        inner.width,
        &format!(" {}", create.checkout_path),
        Style::default().fg(p.subtext0).bg(p.panel_bg),
    );
    if create.creating {
        put_text(
            b,
            inner.x,
            inner.y + 8,
            inner.width,
            " creating…",
            Style::default().fg(p.accent).bg(p.panel_bg),
        );
    } else if let Some(error) = create.error.as_deref() {
        put_text(
            b,
            inner.x,
            inner.y + 8,
            inner.width,
            &format!(" {error}"),
            Style::default().fg(p.red).bg(p.panel_bg),
        );
    }
    let buttons = row(inner, &[20, 12], 2, 9);
    let [primary, cancel] = buttons.as_slice() else {
        return None;
    };
    button(
        b,
        *primary,
        " ↵ create and open ",
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
        worktree_search: Rect::default(),
        worktree_rows: Vec::new(),
        cursor: cursor.filter(|_| !create.creating),
        ..OverlayRender::default()
    })
}

pub(super) fn render_worktree_open_overlay(
    b: &mut Buffer,
    open: &ClientWorktreeOpenOverlay,
    p: &Palette,
) -> Option<OverlayRender> {
    super::list_picker_overlay::render_list_picker_overlay(
        b,
        &super::list_picker_overlay::ListPicker {
            title: "open worktree",
            filter_hint: "filter worktrees",
            count_noun: "checkouts",
            empty_message: " no matching worktrees",
            total: open.entries.len(),
            counted: None,
            filtered: open.filtered_indices(),
            selected: open.selected,
            query: &open.query,
            search_focused: open.search_focused,
            opening: open.opening,
            error: open.error.as_deref(),
            favorites: false,
        },
        |index| {
            let entry = &open.entries[index];
            super::list_picker_overlay::PickerRow {
                title: &entry.label,
                status: entry.status_label(),
                detail: &entry.path,
                favorite: false,
                unstarred: false,
            }
        },
        p,
    )
}

pub(super) fn render_worktree_remove_overlay(
    b: &mut Buffer,
    remove: &ClientWorktreeRemoveOverlay,
    p: &Palette,
) -> Option<OverlayRender> {
    let popup = popup(b.area, 72, 10)?;
    let inner = panel(b, popup, p.red, p.panel_bg)?;
    put_text(
        b,
        inner.x,
        inner.y,
        inner.width,
        " delete worktree checkout?",
        Style::default()
            .fg(p.red)
            .bg(p.panel_bg)
            .add_modifier(Modifier::BOLD),
    );
    put_text(
        b,
        inner.x,
        inner.y + 1,
        inner.width,
        " This removes the checkout folder:",
        Style::default().fg(p.text).bg(p.panel_bg),
    );
    put_text(
        b,
        inner.x,
        inner.y + 2,
        inner.width,
        &format!(" {}", remove.path),
        Style::default().fg(p.subtext0).bg(p.panel_bg),
    );
    put_text(
        b,
        inner.x,
        inner.y + 3,
        inner.width,
        " The branch is not deleted. The Herdr workspace will close.",
        Style::default().fg(p.text).bg(p.panel_bg),
    );
    if remove.force_confirmation {
        put_text(
            b,
            inner.x,
            inner.y + 4,
            inner.width,
            " Dirty or untracked files will be permanently deleted.",
            Style::default().fg(p.red).bg(p.panel_bg),
        );
    }
    if remove.removing {
        put_text(
            b,
            inner.x,
            inner.y + 5,
            inner.width,
            " removing…",
            Style::default().fg(p.accent).bg(p.panel_bg),
        );
    } else if let Some(error) = remove.error.as_deref() {
        put_text(
            b,
            inner.x,
            inner.y + 5,
            inner.width,
            &format!(" {error}"),
            Style::default().fg(p.red).bg(p.panel_bg),
        );
    }
    let buttons = row(inner, &[18, 12], 2, 7);
    let [primary, cancel] = buttons.as_slice() else {
        return None;
    };
    button(
        b,
        *primary,
        if remove.force_confirmation {
            " ↵ delete anyway "
        } else {
            " ↵ remove "
        },
        Style::default()
            .fg(contrast(p))
            .bg(p.red)
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
        worktree_search: Rect::default(),
        worktree_rows: Vec::new(),
        cursor: None,
        ..OverlayRender::default()
    })
}
