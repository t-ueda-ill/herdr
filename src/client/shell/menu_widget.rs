//! Anchored popup menu shared by every framed menu in the shell.
//!
//! Layout, scrolling and input are buffer-free so call sites only map their
//! own actions onto item indices; `render_menu` is the single draw path.

use super::*;
use crossterm::event::{MouseButton, MouseEventKind};

/// One row of a popup menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MenuItem<'a> {
    pub(crate) label: &'a str,
    pub(crate) enabled: bool,
    /// Draws an attention dot before the label and widens the menu for it.
    pub(crate) badge: bool,
    /// The item's key, drawn dimmed at the right edge.
    pub(crate) hint: Option<&'a str>,
}

impl<'a> MenuItem<'a> {
    pub(crate) fn new(label: &'a str) -> Self {
        Self {
            label,
            enabled: true,
            badge: false,
            hint: None,
        }
    }
}

/// Where the menu box is placed; every variant keeps the box on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MenuAnchor {
    /// Top-left corner at a cell, pushed back left/up when it would overflow.
    TopLeft { x: u16, y: u16 },
    /// Directly above `rect`, right edges aligned.
    AboveRightAligned(Rect),
    /// Directly above `rect`, left edges aligned.
    AboveLeftAligned(Rect),
    /// Below `rect` from its left edge, flipped above it when there is no room.
    NearRect(Rect),
}

/// Per-menu presentation knobs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct MenuLook {
    pub(crate) min_width: u16,
    /// Blank columns before each label, taken from the two padding columns
    /// rather than widening the box.
    pub(crate) indent: u16,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct MenuRender {
    pub(crate) area: Rect,
    /// Visible rows with the absolute item index each one activates.
    pub(crate) rows: Vec<(Rect, usize)>,
    /// First visible item; feed it back on the next render.
    pub(crate) scroll: usize,
}

pub(crate) fn menu_size(items: &[MenuItem<'_>], look: MenuLook, screen: Rect) -> (u16, u16) {
    let width = items
        .iter()
        .map(|item| {
            display_width(item.label)
                .saturating_add(u16::from(item.badge) * 2)
                .saturating_add(item.hint.map_or(0, |hint| display_width(hint) + 2))
        })
        .max()
        .unwrap_or(0)
        .saturating_add(4)
        .max(look.min_width)
        .min(screen.width.max(1));
    let height = u16::try_from(items.len())
        .unwrap_or(u16::MAX)
        .saturating_add(2)
        .min(screen.height.max(1));
    (width, height)
}

pub(crate) fn menu_rect(anchor: MenuAnchor, width: u16, height: u16, screen: Rect) -> Rect {
    let max_x = screen.right().saturating_sub(width);
    let max_y = screen.bottom().saturating_sub(height);
    let (x, y) = match anchor {
        MenuAnchor::TopLeft { x, y } => (x.min(max_x), y.min(max_y)),
        MenuAnchor::AboveRightAligned(rect) => (
            rect.right().saturating_sub(width).min(max_x),
            rect.y.saturating_sub(height).max(screen.y),
        ),
        MenuAnchor::AboveLeftAligned(rect) => (
            rect.x.min(max_x),
            rect.y.saturating_sub(height).max(screen.y),
        ),
        MenuAnchor::NearRect(rect) => {
            let below = rect.bottom();
            let y = if below <= max_y {
                below
            } else if rect.y >= screen.y.saturating_add(height) {
                rect.y - height
            } else {
                max_y
            };
            (rect.x.min(max_x), y)
        }
    };
    Rect::new(x.max(screen.x), y.max(screen.y), width, height)
}

/// First visible item so that `highlighted` stays in view, moving `scroll`
/// as little as possible. Without a highlight the window only stays on screen.
pub(crate) fn scroll_window(
    highlighted: Option<usize>,
    scroll: usize,
    visible: usize,
    len: usize,
) -> usize {
    if visible == 0 || len <= visible {
        return 0;
    }
    let scroll = scroll.min(len - visible);
    match highlighted {
        Some(highlighted) if highlighted < scroll => highlighted,
        Some(highlighted) if highlighted >= scroll + visible => {
            (highlighted + 1 - visible).min(len - visible)
        }
        _ => scroll,
    }
}

/// Window after one wheel notch: `scroll` moves by one item and a highlight
/// that left the window is pulled back to its nearest edge.
pub(crate) fn wheel_window(
    up: bool,
    scroll: usize,
    highlighted: Option<usize>,
    visible: usize,
    len: usize,
) -> (usize, Option<usize>) {
    if visible == 0 || len <= visible {
        return (0, highlighted);
    }
    let scroll = if up {
        scroll.saturating_sub(1)
    } else {
        scroll.saturating_add(1)
    }
    .min(len - visible);
    let highlighted = highlighted.map(|index| index.clamp(scroll, scroll + visible - 1));
    (scroll, highlighted)
}

pub(crate) fn render_menu(
    buffer: &mut Buffer,
    items: &[MenuItem<'_>],
    anchor: MenuAnchor,
    look: MenuLook,
    highlighted: Option<usize>,
    scroll: usize,
    palette: &Palette,
) -> Option<MenuRender> {
    let screen = buffer.area;
    let (width, height) = menu_size(items, look, screen);
    let rect = menu_rect(anchor, width, height, screen);
    let inner = super::panel(buffer, rect, palette.accent, palette.panel_bg)?;
    let visible = usize::from(inner.height);
    let scroll = scroll_window(highlighted, scroll, visible, items.len());
    let mut rows = Vec::new();
    for (offset, (index, item)) in items.iter().enumerate().skip(scroll).enumerate() {
        if offset >= visible {
            break;
        }
        let row = Rect::new(inner.x, inner.y + offset as u16, inner.width, 1);
        draw_row(buffer, row, item, highlighted == Some(index), look, palette);
        rows.push((row, index));
    }
    // Why not a scrollbar column: it would steal label width from every
    // menu; arrows on the border only appear when rows are actually hidden.
    let arrow_x = rect.right().saturating_sub(2);
    let arrow_style = Style::default().fg(palette.accent).bg(palette.panel_bg);
    if scroll > 0 {
        put_text(buffer, arrow_x, rect.y, 1, "▲", arrow_style);
    }
    if scroll + visible < items.len() {
        put_text(
            buffer,
            arrow_x,
            rect.bottom().saturating_sub(1),
            1,
            "▼",
            arrow_style,
        );
    }
    Some(MenuRender {
        area: rect,
        rows,
        scroll,
    })
}

fn draw_row(
    buffer: &mut Buffer,
    row: Rect,
    item: &MenuItem<'_>,
    highlighted: bool,
    look: MenuLook,
    palette: &Palette,
) {
    let fg = if !item.enabled {
        palette.overlay0
    } else if highlighted {
        panel_contrast_fg(palette)
    } else {
        palette.text
    };
    let style = match (highlighted, item.enabled) {
        (true, true) => Style::default()
            .fg(fg)
            .bg(palette.accent)
            .add_modifier(Modifier::BOLD),
        // Why not the accent bar: a disabled row must not read as actionable.
        (true, false) => Style::default().fg(fg).bg(palette.surface0),
        (false, _) => Style::default().fg(fg).bg(palette.panel_bg),
    };
    buffer.set_style(row, style);
    let mut x = row.x;
    if item.badge {
        let badge_style = if highlighted {
            style
        } else {
            Style::default()
                .fg(palette.accent)
                .bg(palette.panel_bg)
                .add_modifier(Modifier::BOLD)
        };
        put_text(buffer, x, row.y, row.width.min(2), " ●", badge_style);
        x = x.saturating_add(2);
    }
    let x = x.saturating_add(look.indent);
    put_text(
        buffer,
        x,
        row.y,
        row.right().saturating_sub(x),
        item.label,
        style,
    );
    let label_end = x.saturating_add(display_width(item.label));
    if let Some(hint) = item.hint {
        let width = display_width(hint);
        let hint_x = row.right().saturating_sub(width.saturating_add(1));
        // Why left out rather than clipped: a cut key reads as another key.
        if hint_x > label_end {
            let hint_style = if highlighted {
                style
            } else {
                style.fg(palette.overlay0)
            };
            put_text(buffer, hint_x, row.y, width, hint, hint_style);
        }
    }
}

/// Item under the pointer, if any.
pub(crate) fn menu_hover(rows: &[(Rect, usize)], point: (u16, u16)) -> Option<usize> {
    rows.iter()
        .find(|(rect, _)| contains(*rect, point))
        .map(|(_, index)| *index)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MenuClick {
    Activate(usize),
    /// On a disabled row: swallowed, the menu stays open.
    Inert,
    Outside,
}

/// `is_enabled` is asked only for the clicked row, so callers can derive it
/// lazily instead of building every item per pointer event.
pub(crate) fn menu_click(
    rows: &[(Rect, usize)],
    point: (u16, u16),
    is_enabled: impl FnOnce(usize) -> bool,
) -> MenuClick {
    match menu_hover(rows, point) {
        Some(index) if is_enabled(index) => MenuClick::Activate(index),
        Some(_) => MenuClick::Inert,
        None => MenuClick::Outside,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MenuKey {
    /// New highlighted index; disabled rows can be highlighted.
    Move(usize),
    Activate(usize),
    Close,
    /// Consumed without effect, e.g. Enter on a disabled row.
    Inert,
    Unhandled,
}

/// Without a highlight, Down starts at the first item, Up at the last, and
/// Enter does nothing.
pub(crate) fn menu_key(
    code: KeyCode,
    highlighted: Option<usize>,
    items: &[MenuItem<'_>],
    vim_keys: bool,
) -> MenuKey {
    let last = items.len().saturating_sub(1);
    let up = || MenuKey::Move(highlighted.map_or(last, |index| index.saturating_sub(1).min(last)));
    let down = || MenuKey::Move(highlighted.map_or(0, |index| index.saturating_add(1).min(last)));
    match code {
        KeyCode::Esc => MenuKey::Close,
        KeyCode::Up => up(),
        KeyCode::Down => down(),
        KeyCode::Char('k') if vim_keys => up(),
        KeyCode::Char('j') if vim_keys => down(),
        KeyCode::Enter => {
            match highlighted.and_then(|index| items.get(index).map(|item| (index, item))) {
                Some((index, item)) if item.enabled => MenuKey::Activate(index),
                _ => MenuKey::Inert,
            }
        }
        _ => MenuKey::Unhandled,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MenuPointer {
    Highlight(usize),
    Activate(usize),
    /// A wheel notch over the rows; `up` is toward the first item.
    Scroll {
        up: bool,
    },
    /// Left click outside the rows: closes the menu and is swallowed.
    Close,
    /// Swallowed without effect or repaint.
    Ignore,
}

/// `is_enabled` is consulted only on a left click over a row.
pub(crate) fn menu_pointer(
    kind: MouseEventKind,
    rows: &[(Rect, usize)],
    point: (u16, u16),
    is_enabled: impl FnOnce(usize) -> bool,
) -> MenuPointer {
    match kind {
        MouseEventKind::Moved => {
            menu_hover(rows, point).map_or(MenuPointer::Ignore, MenuPointer::Highlight)
        }
        MouseEventKind::Down(MouseButton::Left) => match menu_click(rows, point, is_enabled) {
            MenuClick::Activate(index) => MenuPointer::Activate(index),
            MenuClick::Inert => MenuPointer::Ignore,
            MenuClick::Outside => MenuPointer::Close,
        },
        // Why only over the rows: a wheel elsewhere belongs to no menu row,
        // and scrolling the menu from across the screen would surprise.
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
            if menu_hover(rows, point).is_some() =>
        {
            MenuPointer::Scroll {
                up: kind == MouseEventKind::ScrollUp,
            }
        }
        _ => MenuPointer::Ignore,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Rect = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 24,
    };

    fn enabled_in<'a>(items: &'a [MenuItem<'_>]) -> impl Fn(usize) -> bool + 'a {
        |index| items.get(index).is_some_and(|item| item.enabled)
    }

    fn items(count: usize) -> Vec<MenuItem<'static>> {
        (0..count).map(|_| MenuItem::new("item")).collect()
    }

    #[test]
    fn top_left_anchor_keeps_the_clicked_cell_as_corner_when_it_fits() {
        let rect = menu_rect(MenuAnchor::TopLeft { x: 10, y: 5 }, 12, 6, SCREEN);
        assert_eq!(rect, Rect::new(10, 5, 12, 6));
    }

    #[test]
    fn top_left_anchor_is_pushed_back_on_screen_at_the_bottom_right() {
        let rect = menu_rect(MenuAnchor::TopLeft { x: 79, y: 23 }, 12, 6, SCREEN);
        assert_eq!(rect, Rect::new(68, 18, 12, 6));
    }

    #[test]
    fn above_right_aligned_anchor_sits_on_top_of_the_rect_sharing_its_right_edge() {
        let launcher = Rect::new(70, 20, 4, 1);
        let rect = menu_rect(MenuAnchor::AboveRightAligned(launcher), 12, 6, SCREEN);
        assert_eq!(rect, Rect::new(62, 14, 12, 6));
    }

    #[test]
    fn above_right_aligned_anchor_stops_at_the_top_and_left_edges() {
        let launcher = Rect::new(2, 3, 4, 1);
        let rect = menu_rect(MenuAnchor::AboveRightAligned(launcher), 12, 6, SCREEN);
        assert_eq!(rect, Rect::new(0, 0, 12, 6));
    }

    #[test]
    fn above_left_aligned_anchor_sits_on_top_of_the_rect_sharing_its_left_edge() {
        let footer = Rect::new(3, 20, 5, 1);
        let rect = menu_rect(MenuAnchor::AboveLeftAligned(footer), 12, 5, SCREEN);
        assert_eq!(rect, Rect::new(3, 15, 12, 5));
    }

    #[test]
    fn above_left_aligned_anchor_stops_at_the_top_and_right_edges() {
        let footer = Rect::new(75, 3, 5, 1);
        let rect = menu_rect(MenuAnchor::AboveLeftAligned(footer), 12, 6, SCREEN);
        assert_eq!(rect, Rect::new(68, 0, 12, 6));
    }

    #[test]
    fn near_rect_anchor_opens_below_the_rect_from_its_left_edge() {
        let row = Rect::new(4, 3, 30, 1);
        let rect = menu_rect(MenuAnchor::NearRect(row), 12, 6, SCREEN);
        assert_eq!(rect, Rect::new(4, 4, 12, 6));
    }

    #[test]
    fn near_rect_anchor_flips_above_the_rect_when_there_is_no_room_below() {
        let row = Rect::new(75, 20, 5, 1);
        let rect = menu_rect(MenuAnchor::NearRect(row), 12, 6, SCREEN);
        assert_eq!(rect, Rect::new(68, 14, 12, 6));
    }

    #[test]
    fn near_rect_anchor_clamps_to_the_bottom_when_neither_side_fits() {
        let row = Rect::new(0, 3, 10, 1);
        let rect = menu_rect(MenuAnchor::NearRect(row), 12, 22, SCREEN);
        assert_eq!(rect, Rect::new(0, 2, 12, 22));
    }

    #[test]
    fn menu_size_counts_borders_padding_badges_and_the_minimum_width() {
        let rows = [
            MenuItem::new("abc"),
            MenuItem {
                badge: true,
                ..MenuItem::new("abcd")
            },
        ];
        let look = MenuLook {
            min_width: 0,
            indent: 1,
        };
        assert_eq!(menu_size(&rows, look, SCREEN), (10, 4));
        let look = MenuLook {
            min_width: 14,
            indent: 0,
        };
        assert_eq!(menu_size(&rows, look, SCREEN), (14, 4));
    }

    #[test]
    fn a_key_hint_widens_the_menu_by_its_width_and_a_gap() {
        let plain = [MenuItem::new("abc")];
        let hinted = [MenuItem {
            hint: Some("F10"),
            ..MenuItem::new("abc")
        }];
        let look = MenuLook::default();
        assert_eq!(
            menu_size(&hinted, look, SCREEN).0,
            menu_size(&plain, look, SCREEN).0 + 5
        );
    }

    #[test]
    fn a_key_hint_that_does_not_fit_beside_its_label_is_left_out() {
        let items = [MenuItem {
            hint: Some("prefix+shift+x"),
            ..MenuItem::new("close files tab")
        }];
        let mut buffer = Buffer::empty(Rect::new(0, 0, 20, 5));
        render_menu(
            &mut buffer,
            &items,
            MenuAnchor::TopLeft { x: 0, y: 0 },
            MenuLook::default(),
            None,
            0,
            &Palette::catppuccin(),
        );
        let row: String = (0..20).map(|x| buffer[(x, 1)].symbol()).collect();
        assert!(row.contains("close files"), "{row}");
        assert!(!row.contains("prefix"), "{row}");
        assert!(row.ends_with('│'), "{row}");
    }

    #[test]
    fn menu_height_clamps_to_the_screen() {
        assert_eq!(
            menu_size(&items(40), MenuLook::default(), SCREEN).1,
            SCREEN.height
        );
    }

    #[test]
    fn scroll_window_keeps_the_offset_while_the_highlight_is_visible() {
        assert_eq!(scroll_window(Some(6), 4, 5, 20), 4);
        assert_eq!(scroll_window(Some(8), 4, 5, 20), 4);
    }

    #[test]
    fn scroll_window_moves_just_enough_to_reveal_the_highlight() {
        assert_eq!(scroll_window(Some(9), 4, 5, 20), 5);
        assert_eq!(scroll_window(Some(2), 4, 5, 20), 2);
    }

    #[test]
    fn scroll_window_never_scrolls_past_the_last_page_or_when_everything_fits() {
        assert_eq!(scroll_window(Some(19), 30, 5, 20), 15);
        assert_eq!(scroll_window(Some(3), 2, 5, 5), 0);
    }

    #[test]
    fn scroll_window_without_a_highlight_only_keeps_the_window_on_screen() {
        assert_eq!(scroll_window(None, 4, 5, 20), 4);
        assert_eq!(scroll_window(None, 30, 5, 20), 15);
        assert_eq!(scroll_window(None, 3, 5, 5), 0);
    }

    #[test]
    fn a_wheel_notch_moves_the_window_by_one_item_within_its_bounds() {
        assert_eq!(wheel_window(false, 4, Some(6), 5, 20), (5, Some(6)));
        assert_eq!(wheel_window(true, 4, Some(6), 5, 20), (3, Some(6)));
        assert_eq!(wheel_window(true, 0, Some(2), 5, 20), (0, Some(2)));
        assert_eq!(wheel_window(false, 15, Some(16), 5, 20), (15, Some(16)));
    }

    #[test]
    fn a_wheel_notch_pulls_a_highlight_that_left_the_window_to_its_nearest_edge() {
        assert_eq!(wheel_window(false, 4, Some(4), 5, 20), (5, Some(5)));
        assert_eq!(wheel_window(true, 4, Some(8), 5, 20), (3, Some(7)));
        assert_eq!(wheel_window(false, 4, None, 5, 20), (5, None));
    }

    #[test]
    fn a_wheel_notch_over_a_menu_that_fits_keeps_it_unscrolled() {
        assert_eq!(wheel_window(false, 0, Some(1), 5, 5), (0, Some(1)));
    }

    #[test]
    fn rendering_without_a_highlight_draws_every_row_plain() {
        let palette = Palette::catppuccin();
        let mut buffer = Buffer::empty(Rect::new(0, 0, 30, 8));
        let rendered = render_menu(
            &mut buffer,
            &items(3),
            MenuAnchor::TopLeft { x: 0, y: 0 },
            MenuLook::default(),
            None,
            0,
            &palette,
        )
        .expect("menu drawn");

        for (row, _) in &rendered.rows {
            assert_eq!(buffer[(row.x, row.y)].bg, palette.panel_bg);
        }
    }

    #[test]
    fn without_a_highlight_down_starts_at_the_first_item_up_at_the_last_and_enter_is_inert() {
        let rows = items(3);
        assert_eq!(
            menu_key(KeyCode::Down, None, &rows, false),
            MenuKey::Move(0)
        );
        assert_eq!(menu_key(KeyCode::Up, None, &rows, false), MenuKey::Move(2));
        assert_eq!(
            menu_key(KeyCode::Char('j'), None, &rows, true),
            MenuKey::Move(0)
        );
        assert_eq!(menu_key(KeyCode::Enter, None, &rows, false), MenuKey::Inert);
        assert_eq!(menu_key(KeyCode::Esc, None, &rows, false), MenuKey::Close);
    }

    #[test]
    fn rendering_more_items_than_fit_shows_a_window_holding_the_highlight() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 30, 8));
        let rendered = render_menu(
            &mut buffer,
            &items(20),
            MenuAnchor::TopLeft { x: 0, y: 0 },
            MenuLook::default(),
            Some(10),
            0,
            &Palette::catppuccin(),
        )
        .expect("menu drawn");

        assert_eq!(rendered.area.height, 8);
        assert_eq!(rendered.scroll, 5);
        let indices: Vec<usize> = rendered.rows.iter().map(|(_, index)| *index).collect();
        assert_eq!(indices, vec![5, 6, 7, 8, 9, 10]);
        assert_eq!(rendered.rows[0].0, Rect::new(1, 1, 6, 1));
        assert_eq!(buffer[(6, 0)].symbol(), "▲");
        assert_eq!(buffer[(6, 7)].symbol(), "▼");
    }

    #[test]
    fn hovering_a_scrolled_row_maps_to_its_absolute_item() {
        let rows = vec![(Rect::new(1, 1, 8, 1), 5), (Rect::new(1, 2, 8, 1), 6)];
        assert_eq!(menu_hover(&rows, (3, 2)), Some(6));
        assert_eq!(menu_hover(&rows, (3, 3)), None);
    }

    #[test]
    fn disabled_rows_render_dimmed() {
        let palette = Palette::catppuccin();
        let mut buffer = Buffer::empty(Rect::new(0, 0, 30, 8));
        let rows = [
            MenuItem::new("on"),
            MenuItem {
                enabled: false,
                ..MenuItem::new("off")
            },
        ];
        let rendered = render_menu(
            &mut buffer,
            &rows,
            MenuAnchor::TopLeft { x: 0, y: 0 },
            MenuLook::default(),
            Some(1),
            0,
            &palette,
        )
        .expect("menu drawn");

        let disabled = rendered.rows[1].0;
        assert_eq!(buffer[(disabled.x, disabled.y)].fg, palette.overlay0);
        let enabled = rendered.rows[0].0;
        assert_eq!(buffer[(enabled.x, enabled.y)].fg, palette.text);
    }

    #[test]
    fn disabled_rows_can_be_highlighted_but_neither_enter_nor_click_activates_them() {
        let rows = [
            MenuItem::new("on"),
            MenuItem {
                enabled: false,
                ..MenuItem::new("off")
            },
        ];
        assert_eq!(
            menu_key(KeyCode::Down, Some(0), &rows, false),
            MenuKey::Move(1)
        );
        assert_eq!(
            menu_key(KeyCode::Enter, Some(1), &rows, false),
            MenuKey::Inert
        );
        assert_eq!(
            menu_key(KeyCode::Enter, Some(0), &rows, false),
            MenuKey::Activate(0)
        );

        let hits = vec![(Rect::new(1, 1, 8, 1), 0), (Rect::new(1, 2, 8, 1), 1)];
        assert_eq!(
            menu_click(&hits, (2, 2), enabled_in(&rows)),
            MenuClick::Inert
        );
        assert_eq!(
            menu_click(&hits, (2, 1), enabled_in(&rows)),
            MenuClick::Activate(0)
        );
        assert_eq!(
            menu_click(&hits, (20, 20), enabled_in(&rows)),
            MenuClick::Outside
        );
    }

    #[test]
    fn pointer_moves_highlight_rows_and_left_clicks_activate_or_close() {
        let rows = items(2);
        let hits = vec![(Rect::new(1, 1, 8, 1), 0), (Rect::new(1, 2, 8, 1), 1)];
        let left = MouseEventKind::Down(MouseButton::Left);
        assert_eq!(
            menu_pointer(MouseEventKind::Moved, &hits, (2, 2), enabled_in(&rows)),
            MenuPointer::Highlight(1)
        );
        assert_eq!(
            menu_pointer(MouseEventKind::Moved, &hits, (20, 20), enabled_in(&rows)),
            MenuPointer::Ignore
        );
        assert_eq!(
            menu_pointer(left, &hits, (2, 1), enabled_in(&rows)),
            MenuPointer::Activate(0)
        );
        assert_eq!(
            menu_pointer(left, &hits, (20, 20), enabled_in(&rows)),
            MenuPointer::Close
        );
        assert_eq!(
            menu_pointer(
                MouseEventKind::ScrollDown,
                &hits,
                (20, 20),
                enabled_in(&rows)
            ),
            MenuPointer::Ignore
        );
    }

    #[test]
    fn the_wheel_over_the_rows_scrolls_the_menu() {
        let rows = items(2);
        let hits = vec![(Rect::new(1, 1, 8, 1), 0), (Rect::new(1, 2, 8, 1), 1)];
        assert_eq!(
            menu_pointer(MouseEventKind::ScrollDown, &hits, (2, 2), enabled_in(&rows)),
            MenuPointer::Scroll { up: false }
        );
        assert_eq!(
            menu_pointer(MouseEventKind::ScrollUp, &hits, (2, 1), enabled_in(&rows)),
            MenuPointer::Scroll { up: true }
        );
    }

    #[test]
    fn arrow_keys_stay_within_bounds_and_j_k_only_move_when_enabled() {
        let rows = items(3);
        assert_eq!(
            menu_key(KeyCode::Up, Some(0), &rows, false),
            MenuKey::Move(0)
        );
        assert_eq!(
            menu_key(KeyCode::Down, Some(2), &rows, false),
            MenuKey::Move(2)
        );
        assert_eq!(
            menu_key(KeyCode::Char('j'), Some(0), &rows, true),
            MenuKey::Move(1)
        );
        assert_eq!(
            menu_key(KeyCode::Char('k'), Some(1), &rows, true),
            MenuKey::Move(0)
        );
        assert_eq!(
            menu_key(KeyCode::Char('j'), Some(0), &rows, false),
            MenuKey::Unhandled
        );
        assert_eq!(
            menu_key(KeyCode::Esc, Some(1), &rows, false),
            MenuKey::Close
        );
    }
}
