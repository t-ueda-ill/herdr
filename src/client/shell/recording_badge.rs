//! The `● rec` mark at the right of the sidebar heading while the session
//! records panes (`herdr --log`).
//!
//! Why the heading and not each row: recording is chosen per session, so a
//! mark on every workspace row would repeat one fact down the whole list.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use super::render::{display_width, put_text};
use super::Palette;
use crate::protocol::ClientShellSnapshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Badge {
    Recording,
    /// At least one pane's recording stopped or never started.
    Failed,
}

impl Badge {
    fn label(self) -> &'static str {
        match self {
            Self::Recording => "rec",
            Self::Failed => "rec failed",
        }
    }
}

fn badge(snapshot: &ClientShellSnapshot) -> Option<Badge> {
    let mut recording = false;
    for pane in &snapshot.panes {
        match pane.recording {
            Some(state) if state.failed => return Some(Badge::Failed),
            Some(_) => recording = true,
            None => {}
        }
    }
    recording.then_some(Badge::Recording)
}

/// Draws the mark right-aligned on `row`, the heading's line, when it fits
/// beside `heading`.
pub(super) fn render(
    buffer: &mut Buffer,
    row: Rect,
    heading: &str,
    snapshot: Option<&ClientShellSnapshot>,
    palette: &Palette,
) {
    let Some(badge) = snapshot.and_then(badge) else {
        return;
    };
    let label = badge.label();
    let label_width = display_width(label);
    let width = label_width.saturating_add(3);
    if row.height == 0 || row.width < display_width(heading).saturating_add(width) {
        return;
    }
    let x = row.right().saturating_sub(width);
    let dot = match badge {
        Badge::Recording => palette.red,
        Badge::Failed => palette.yellow,
    };
    put_text(
        buffer,
        x,
        row.y,
        2,
        "● ",
        Style::default().fg(dot).add_modifier(Modifier::BOLD),
    );
    put_text(
        buffer,
        x.saturating_add(2),
        row.y,
        label_width,
        label,
        Style::default().fg(palette.overlay0),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ClientShellPaneRecording;

    fn snapshot_with(recordings: &[Option<bool>]) -> ClientShellSnapshot {
        let mut snapshot = super::super::tests::snapshot();
        let template = snapshot.panes[0].clone();
        snapshot.panes = recordings
            .iter()
            .enumerate()
            .map(|(index, failed)| {
                let mut pane = template.clone();
                pane.pane_id = format!("p{index}");
                pane.recording = failed.map(|failed| ClientShellPaneRecording { failed });
                pane
            })
            .collect();
        snapshot
    }

    fn drawn(snapshot: &ClientShellSnapshot, width: u16) -> String {
        let area = Rect::new(0, 0, width, 1);
        let mut buffer = Buffer::empty(area);
        put_text(&mut buffer, 0, 0, width, " spaces", Style::default());
        render(
            &mut buffer,
            area,
            " spaces",
            Some(snapshot),
            &Palette::catppuccin(),
        );
        buffer.content().iter().map(|cell| cell.symbol()).collect()
    }

    #[test]
    fn a_recording_session_shows_rec_at_the_right_of_the_heading() {
        let text = drawn(&snapshot_with(&[Some(false), Some(false)]), 26);
        assert_eq!(text, " spaces             ● rec ");
    }

    #[test]
    fn one_failed_pane_turns_the_mark_into_rec_failed() {
        let text = drawn(&snapshot_with(&[Some(false), Some(true)]), 26);
        assert!(text.ends_with("● rec failed "), "{text:?}");
    }

    #[test]
    fn a_session_that_does_not_record_shows_no_mark() {
        let text = drawn(&snapshot_with(&[None, None]), 26);
        assert_eq!(text.trim_end(), " spaces");
    }

    #[test]
    fn the_mark_is_left_out_when_it_would_cover_the_heading() {
        let text = drawn(&snapshot_with(&[Some(false)]), 12);
        assert_eq!(text.trim_end(), " spaces");
    }
}
