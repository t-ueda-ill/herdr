//! The bottom line of a container or ssh pane whose connection ended with an
//! error. Render and pointer input share [`disconnect_bar_items`], so a click
//! lands on the label it hit.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use super::text::display_width;
use crate::app::state::Palette;

const SEPARATOR: &str = " · ";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DisconnectBarButton {
    Reconnect,
    Close,
}

const BUTTONS: [(DisconnectBarButton, &str); 2] = [
    (DisconnectBarButton::Reconnect, "↵ reconnect"),
    (DisconnectBarButton::Close, "close"),
];

/// One label of the bar, at a column relative to the pane's left edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DisconnectBarItem {
    pub(crate) x: u16,
    pub(crate) text: String,
    pub(crate) button: Option<DisconnectBarButton>,
}

fn status_text(exit_code: Option<u32>) -> String {
    match exit_code {
        Some(code) => format!("disconnected (exit {code})"),
        None => "disconnected".to_owned(),
    }
}

/// The labels that fit in `width`. A narrow pane drops the status first, then
/// buttons from the right, so reconnecting stays reachable longest.
pub(crate) fn disconnect_bar_items(width: u16, exit_code: Option<u32>) -> Vec<DisconnectBarItem> {
    let buttons = BUTTONS.map(|(button, label)| (Some(button), label.to_owned()));
    let status = (None, status_text(exit_code));
    let with_status: Vec<_> = std::iter::once(status).chain(buttons.clone()).collect();
    let fits = |labels: &[(Option<DisconnectBarButton>, String)]| {
        let total: usize = labels
            .iter()
            .map(|(_, text)| display_width(text))
            .sum::<usize>()
            + display_width(SEPARATOR) * labels.len().saturating_sub(1);
        // One column of margin on the left, as other bottom button lines have.
        total < usize::from(width)
    };
    let labels = if fits(&with_status) {
        with_status
    } else {
        buttons.to_vec()
    };
    let mut items = Vec::new();
    let mut x = 1usize;
    for (position, (button, text)) in labels.into_iter().enumerate() {
        let separator = if position == 0 {
            0
        } else {
            display_width(SEPARATOR)
        };
        let label_x = x + separator;
        if label_x + display_width(&text) > usize::from(width) {
            break;
        }
        x = label_x + display_width(&text);
        items.push(DisconnectBarItem {
            x: label_x as u16,
            text,
            button,
        });
    }
    items
}

/// The button under `column` (relative to the pane's left edge).
pub(crate) fn disconnect_bar_button_at(
    width: u16,
    exit_code: Option<u32>,
    column: u16,
) -> Option<DisconnectBarButton> {
    disconnect_bar_items(width, exit_code)
        .into_iter()
        .find(|item| {
            column >= item.x
                && usize::from(column) < usize::from(item.x) + display_width(&item.text)
        })
        .and_then(|item| item.button)
}

/// Draws over the pane's last row, leaving the exited screen above it.
pub(crate) fn render_disconnect_bar(
    buffer: &mut Buffer,
    pane: Rect,
    exit_code: Option<u32>,
    palette: &Palette,
) {
    if pane.width == 0 || pane.height == 0 {
        return;
    }
    let y = pane.bottom() - 1;
    let background = Style::default().bg(palette.panel_bg);
    buffer.set_style(Rect::new(pane.x, y, pane.width, 1), background);
    let mut previous_end = None;
    for item in disconnect_bar_items(pane.width, exit_code) {
        let x = pane.x + item.x;
        if let Some(end) = previous_end {
            buffer.set_string(end, y, SEPARATOR, background.fg(palette.overlay0));
        }
        let style = match item.button {
            Some(_) => background.fg(palette.text),
            None => background.fg(palette.red),
        };
        buffer.set_string(x, y, &item.text, style);
        previous_end = Some(x + display_width(&item.text) as u16);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(width: u16) -> Vec<String> {
        disconnect_bar_items(width, Some(255))
            .into_iter()
            .map(|item| item.text)
            .collect()
    }

    #[test]
    fn a_wide_pane_shows_why_and_both_buttons() {
        assert_eq!(
            texts(80),
            ["disconnected (exit 255)", "↵ reconnect", "close"]
        );
        assert_eq!(
            disconnect_bar_items(80, None)[0].text,
            "disconnected",
            "a signal has no exit code to show"
        );
    }

    #[test]
    fn a_narrow_pane_drops_the_status_before_the_buttons() {
        assert_eq!(texts(30), ["↵ reconnect", "close"]);
        assert_eq!(texts(14), ["↵ reconnect"]);
        assert!(texts(5).is_empty());
    }

    #[test]
    fn a_click_finds_the_label_it_lands_on() {
        let items = disconnect_bar_items(80, Some(255));
        let reconnect = &items[1];
        let close = &items[2];

        assert_eq!(
            disconnect_bar_button_at(80, Some(255), reconnect.x),
            Some(DisconnectBarButton::Reconnect)
        );
        assert_eq!(
            disconnect_bar_button_at(80, Some(255), close.x + 4),
            Some(DisconnectBarButton::Close)
        );
        assert_eq!(disconnect_bar_button_at(80, Some(255), 1), None);
        assert_eq!(disconnect_bar_button_at(80, Some(255), close.x + 5), None);
    }
}
