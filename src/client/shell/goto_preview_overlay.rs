//! The right column of the Go to picker: the selected row's state, request,
//! and result or screen.

use super::*;
use crate::api::schema::{AgentStatus, TaskInfo, TaskState};
use crate::client::shell::goto_preview::{PreviewBody, PreviewSource};

/// The picker's inner width from which the preview shows.
pub(super) const PREVIEW_MIN_INNER_WIDTH: u16 = 90;
/// Columns the list keeps when the preview shows.
pub(super) const LIST_MIN_WIDTH: u16 = 48;
/// The list width at which rows show the agent kind and state.
const LIST_FULL_WIDTH: u16 = 65;
/// Columns the preview keeps, with its separator.
const PREVIEW_MIN_WIDTH: u16 = 38;
/// Task text lines shown under "asked".
const ASKED_LINES: usize = 2;

/// The list width, or `None` when the preview does not fit.
pub(super) fn list_width(inner_width: u16) -> Option<u16> {
    // Why the list first: rows narrower than LIST_FULL_WIDTH drop their
    // state column, which the list needs more than the preview needs width.
    (inner_width >= PREVIEW_MIN_INNER_WIDTH).then(|| {
        LIST_FULL_WIDTH
            .min(inner_width - PREVIEW_MIN_WIDTH)
            .max(LIST_MIN_WIDTH)
            .max(inner_width * 11 / 20)
    })
}

/// Draws the preview into `area` and returns the open button, if any.
pub(super) fn render_goto_preview(
    b: &mut Buffer,
    area: Rect,
    n: &ClientNavigatorOverlay,
    row: Option<&ClientNavigatorRow>,
    endpoints: &[ClientShellEndpoint],
    p: &Palette,
) -> Rect {
    let Some(row) = row else {
        return Rect::default();
    };
    let base = Style::default().fg(p.text).bg(p.panel_bg);
    let dim = base.fg(p.overlay0);
    let mut lines = Lines::new(b, area);
    match &row.target {
        ClientNavigatorTarget::Machine { endpoint_id } => {
            lines.put(&row.label, base.add_modifier(Modifier::BOLD));
            lines.blank();
            if row.stale {
                lines.put("not available", dim);
            } else {
                let agents = endpoint_agents(endpoints, endpoint_id, None);
                lines.put(&status_counts(&agents), dim);
            }
            Rect::default()
        }
        ClientNavigatorTarget::Workspace {
            endpoint_id,
            workspace_id,
        } => {
            lines.put(&row.label, base.add_modifier(Modifier::BOLD));
            lines.blank();
            // Why not the cached snapshot: its counts are stale offline.
            if row.stale {
                lines.put("not available", dim);
            } else {
                let agents = endpoint_agents(endpoints, endpoint_id, Some(workspace_id));
                lines.put(&status_counts(&agents), dim);
            }
            Rect::default()
        }
        ClientNavigatorTarget::Pane {
            endpoint_id,
            pane_id,
        } => {
            let task = n.tasks.task(endpoint_id, pane_id);
            let agent_status = row.agent.as_ref().and(row.status);
            let (glyph, word, color) = pane_state(agent_status, task, p);
            lines.put_pair(
                &format!("{glyph} {word}"),
                base.fg(color).add_modifier(Modifier::BOLD),
                &format!(
                    "{} · {}",
                    row.label,
                    row.agent.as_deref().unwrap_or("terminal")
                ),
                dim,
            );
            lines.blank();
            match task {
                Some(task) => {
                    lines.put(&asked_line(task), dim);
                    for text in task
                        .text
                        .lines()
                        .filter(|line| !line.trim().is_empty())
                        .take(ASKED_LINES)
                    {
                        lines.put(text, base);
                    }
                }
                None => lines.put("no task", dim),
            }
            lines.blank();
            if row.stale {
                lines.put("not available", dim);
                return Rect::default();
            }
            let button = lines.reserve_bottom();
            // Why the task check: after the list changes, the text read for
            // the previous task must not sit under the new task's request.
            let shown_task = task
                .filter(|task| task.state.is_finished())
                .map(|task| task.task_id.as_str());
            match n
                .tasks
                .preview(endpoint_id, pane_id)
                .map(|preview| match &preview.body {
                    PreviewBody::Text { .. } if preview.task_id() != shown_task => {
                        &PreviewBody::Loading
                    }
                    body => body,
                }) {
                None | Some(PreviewBody::Loading) => lines.put("loading…", dim),
                Some(PreviewBody::Unavailable(message)) => lines.put(message, dim),
                Some(PreviewBody::Text {
                    source,
                    text,
                    note,
                    truncated,
                }) => {
                    lines.put_pair(
                        &source_label(*source, *truncated),
                        dim,
                        note.as_deref().unwrap_or_default(),
                        dim.add_modifier(Modifier::DIM),
                    );
                    lines.text(text, *source == PreviewSource::ScreenTail, base);
                }
            }
            match button {
                Some(y) => {
                    let label = " open result  space ";
                    let width = display_width(label).min(area.width);
                    let rect = Rect::new(area.x, y, width, 1);
                    super::button(
                        b,
                        rect,
                        label,
                        Style::default()
                            .fg(p.text)
                            .bg(p.surface0)
                            .add_modifier(Modifier::BOLD),
                    );
                    rect
                }
                None => Rect::default(),
            }
        }
    }
}

/// Lines written top-down in an area, cut at its bottom.
struct Lines<'a> {
    b: &'a mut Buffer,
    area: Rect,
    y: u16,
    bottom: u16,
}

impl<'a> Lines<'a> {
    fn new(b: &'a mut Buffer, area: Rect) -> Self {
        Self {
            b,
            area,
            y: area.y,
            bottom: area.bottom(),
        }
    }

    fn put(&mut self, text: &str, style: Style) {
        if self.y < self.bottom {
            put_text(self.b, self.area.x, self.y, self.area.width, text, style);
            self.y += 1;
        }
    }

    fn put_pair(&mut self, left: &str, left_style: Style, right: &str, right_style: Style) {
        if self.y >= self.bottom {
            return;
        }
        let left_width = display_width(left).min(self.area.width);
        put_text(self.b, self.area.x, self.y, left_width, left, left_style);
        let rest = Rect::new(
            self.area.x + left_width + 1,
            self.y,
            self.area.width.saturating_sub(left_width + 1),
            1,
        );
        if rest.width > 0 {
            put_right_text(self.b, rest, self.y, right, right_style);
        }
        self.y += 1;
    }

    fn blank(&mut self) {
        self.y = (self.y + 1).min(self.bottom);
    }

    /// Keeps the last line for a button, after a blank line.
    fn reserve_bottom(&mut self) -> Option<u16> {
        if self.bottom.saturating_sub(self.y) < 3 {
            return None;
        }
        self.bottom -= 2;
        Some(self.bottom + 1)
    }

    /// The text's last lines when `tail`, else its first lines.
    fn text(&mut self, text: &str, tail: bool, style: Style) {
        let all = text.trim_end().lines().collect::<Vec<_>>();
        let room = usize::from(self.bottom.saturating_sub(self.y));
        let shown = if tail {
            &all[all.len().saturating_sub(room)..]
        } else {
            &all[..all.len().min(room)]
        };
        for line in shown {
            self.put(line, style);
        }
    }
}

/// `result file`, or `result file · first 64 KiB` for a cut copy.
fn source_label(source: PreviewSource, truncated: bool) -> String {
    let label = match source {
        PreviewSource::ResultFile => "result file",
        PreviewSource::ScreenTail => "screen tail",
    };
    if truncated {
        format!("{label} · first 64 KiB")
    } else {
        label.to_owned()
    }
}

fn pane_state(
    agent: Option<AgentStatus>,
    task: Option<&TaskInfo>,
    p: &Palette,
) -> (&'static str, String, ratatui::style::Color) {
    let live = task.filter(|task| !task.state.is_finished());
    match (agent, live, task) {
        (Some(AgentStatus::Blocked), _, _) => ("▲", "needs input".into(), p.yellow),
        (_, Some(task), _) => match task.state {
            TaskState::NeedsInput => ("▲", "needs input".into(), p.yellow),
            TaskState::Queued => ("○", "queued".into(), p.overlay1),
            _ => ("●", "running".into(), status_color(AgentStatus::Working, p)),
        },
        (Some(AgentStatus::Working), _, _) => {
            ("●", "working".into(), status_color(AgentStatus::Working, p))
        }
        (_, None, Some(task)) if task.state == TaskState::Completed => {
            ("✓", "completed".into(), p.teal)
        }
        (_, None, Some(task)) => ("✗", state_word(task.state).into(), p.red),
        (Some(status), _, _) => (
            status_dot(status),
            status_text(status).into(),
            status_color(status, p),
        ),
        (None, _, _) => ("○", "shell".into(), p.overlay0),
    }
}

fn state_word(state: TaskState) -> &'static str {
    match state {
        TaskState::Queued => "queued",
        TaskState::Submitted => "submitted",
        TaskState::Running => "running",
        TaskState::NeedsInput => "needs input",
        TaskState::Completed => "completed",
        TaskState::Stalled => "stalled",
        TaskState::Lost => "lost",
        TaskState::Cancelled => "cancelled",
        _ => "unknown",
    }
}

/// `asked · t_41 · 14:02 → 14:09`
pub(super) fn asked_line(task: &TaskInfo) -> String {
    let mut line = format!("asked · {}", task.task_id);
    if let Some(at) = local_clock(task.created_at_ms) {
        line.push_str(&format!(" · {at}"));
    }
    if let Some(at) = task.finished_at_ms.and_then(local_clock) {
        line.push_str(&format!(" → {at}"));
    }
    line
}

fn local_clock(millis: u64) -> Option<String> {
    let seconds = i64::try_from(millis / 1000).ok()?;
    let local = crate::platform::local_datetime_at(seconds)?;
    Some(format!("{:02}:{:02}", local.hour(), local.minute()))
}

fn endpoint_agents<'a>(
    endpoints: &'a [ClientShellEndpoint],
    endpoint_id: &ClientEndpointId,
    workspace_id: Option<&String>,
) -> Vec<&'a crate::protocol::ClientShellAgent> {
    endpoints
        .iter()
        .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
        .and_then(|endpoint| endpoint.snapshot.as_deref())
        .map(|snapshot| {
            snapshot
                .agents
                .iter()
                .filter(|agent| workspace_id.is_none_or(|id| &agent.workspace_id == id))
                .collect()
        })
        .unwrap_or_default()
}

/// `1 needs input · 2 working · 3 done`, most urgent first.
pub(super) fn status_counts(agents: &[&crate::protocol::ClientShellAgent]) -> String {
    let order = [
        (AgentStatus::Blocked, "needs input"),
        (AgentStatus::Working, "working"),
        (AgentStatus::Done, "done"),
        (AgentStatus::Idle, "idle"),
        (AgentStatus::Unknown, "unknown"),
    ];
    let parts = order
        .iter()
        .filter_map(|(status, word)| {
            let count = agents
                .iter()
                .filter(|agent| agent.agent_status == *status)
                .count();
            (count > 0).then(|| format!("{count} {word}"))
        })
        .collect::<Vec<_>>();
    if parts.is_empty() {
        "no agents".into()
    } else {
        parts.join(" · ")
    }
}

/// The full view: the request and the whole result, in one scrolled list.
pub(super) fn render_goto_result_overlay(
    b: &mut Buffer,
    v: &crate::client::shell::goto_result::ClientGotoResultOverlay,
    p: &Palette,
) -> Option<OverlayRender> {
    let a = b.area;
    let width = a.width.saturating_sub(4).min(116);
    let height = a.height.saturating_sub(2).min(42);
    if width < 8 || height < 6 {
        return None;
    }
    let q = Rect::new(
        a.x + (a.width - width) / 2,
        a.y + (a.height - height) / 2,
        width,
        height,
    )
    .intersection(a);
    let i = super::panel(b, q, p.accent, p.panel_bg)?;
    let base = Style::default().fg(p.text).bg(p.panel_bg);
    let dim = base.fg(p.overlay0);
    put_text(
        b,
        q.x + 2,
        q.y,
        q.width.saturating_sub(4),
        " Result ",
        base.fg(p.accent),
    );

    let task = v.task.as_ref();
    let agent_status = v.row.agent.as_ref().and(v.row.status);
    let (glyph, word, color) = pane_state(agent_status, task, p);
    let mut header = Lines::new(b, Rect::new(i.x + 1, i.y, i.width.saturating_sub(2), 1));
    header.put_pair(
        &format!(
            "{} · {}",
            v.row.label,
            v.row.agent.as_deref().unwrap_or("terminal")
        ),
        base.add_modifier(Modifier::BOLD),
        &format!("{glyph} {word}"),
        base.fg(color).add_modifier(Modifier::BOLD),
    );
    put_text(
        b,
        i.x,
        i.y + 1,
        i.width,
        &"─".repeat(usize::from(i.width)),
        Style::default().fg(p.surface1).bg(p.panel_bg),
    );

    let body = Rect::new(
        i.x + 1,
        i.y + 2,
        i.width.saturating_sub(3),
        i.height.saturating_sub(3),
    );
    let wrap_width = usize::from(body.width.max(1));
    let mut lines: Vec<(String, Style)> = Vec::new();
    match task {
        Some(task) => {
            lines.push((asked_line(task), dim));
            for line in task.text.lines() {
                lines.extend(wrap(line, wrap_width).into_iter().map(|line| (line, base)));
            }
        }
        None => lines.push(("no task".into(), dim)),
    }
    lines.push((String::new(), base));
    match &v.body {
        PreviewBody::Loading => lines.push(("loading…".into(), dim)),
        PreviewBody::Unavailable(message) => lines.push((message.clone(), dim)),
        PreviewBody::Text {
            source,
            text,
            note,
            truncated,
        } => {
            let label = source_label(*source, *truncated);
            let label = match note {
                Some(note) => format!("{label} · {note}"),
                None => label,
            };
            lines.push((label, dim));
            lines.push((
                "─".repeat(wrap_width),
                Style::default().fg(p.surface1).bg(p.panel_bg),
            ));
            for line in text.trim_end().lines() {
                lines.extend(wrap(line, wrap_width).into_iter().map(|line| (line, base)));
            }
        }
    }

    let viewport = usize::from(body.height);
    let max_scroll = lines.len().saturating_sub(viewport);
    let scroll = v.scroll.min(max_scroll);
    for (row, (text, style)) in lines.iter().skip(scroll).take(viewport).enumerate() {
        put_text(b, body.x, body.y + row as u16, body.width, text, *style);
    }
    if max_scroll > 0 {
        let metrics = crate::pane::ScrollMetrics {
            offset_from_bottom: max_scroll - scroll,
            max_offset_from_bottom: max_scroll,
            viewport_rows: viewport,
        };
        let track = Rect::new(i.right() - 1, body.y, 1, body.height);
        crate::ui::render_scrollbar_buffer(b, metrics, track, p.overlay0, p.overlay1, "▐");
    }
    let footer = " j/k ↑↓ PgUp/PgDn Home/End scroll · y copy · enter open · ";
    let back_label = "esc back";
    put_text(b, i.x, i.bottom() - 1, i.width, footer, dim);
    let back_x = i.x.saturating_add(display_width(footer));
    let back = Rect::new(
        back_x,
        i.bottom() - 1,
        display_width(back_label).min(i.right().saturating_sub(back_x)),
        1,
    );
    put_text(b, back.x, back.y, back.width, back_label, dim);
    Some(OverlayRender {
        area: q,
        navigator_popup: q,
        goto_result_back: back,
        goto_result_max_scroll: max_scroll,
        goto_result_viewport: viewport,
        ..OverlayRender::default()
    })
}

/// Splits a line into pieces at most `width` cells wide, between grapheme
/// clusters so a combining mark stays with its base. A tab becomes four
/// spaces, since it has no width of its own. A cluster wider than `width`
/// takes a piece alone.
fn wrap(line: &str, width: usize) -> Vec<String> {
    use unicode_segmentation::UnicodeSegmentation as _;

    let line = line.replace('\t', "    ");
    let mut pieces = Vec::new();
    let mut piece = String::new();
    let mut used = 0;
    for cluster in line.graphemes(true) {
        let cells = UnicodeWidthStr::width(cluster);
        if used + cells > width && !piece.is_empty() {
            pieces.push(std::mem::take(&mut piece));
            used = 0;
        }
        piece.push_str(cluster);
        used += cells;
    }
    pieces.push(piece);
    pieces
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(status: AgentStatus) -> crate::protocol::ClientShellAgent {
        crate::protocol::ClientShellAgent {
            pane_id: "pane_1".into(),
            workspace_id: "ws_1".into(),
            tab_id: "tab_1".into(),
            name: None,
            display_agent: None,
            agent: Some("claude".into()),
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_status: status,
            state_change_seq: 1,
            state_labels: Vec::new(),
            tokens: Vec::new(),
            focused: false,
        }
    }

    #[test]
    fn wrapping_keeps_combining_marks_and_tabs_and_splits_wide_text() {
        assert_eq!(wrap("e\u{301}e\u{301}", 1), ["e\u{301}", "e\u{301}"]);
        assert_eq!(wrap("a\tb", 3), ["a  ", "  b"]);
        assert_eq!(wrap("日本語", 4), ["日本", "語"]);
        assert_eq!(wrap("日", 1), ["日"]);
    }

    #[test]
    fn agents_in_an_unknown_state_are_counted_too() {
        let unknown = agent(AgentStatus::Unknown);
        let working = agent(AgentStatus::Working);

        assert_eq!(status_counts(&[&unknown]), "1 unknown");
        assert_eq!(
            status_counts(&[&working, &unknown]),
            "1 working · 1 unknown"
        );
    }

    #[test]
    fn the_list_keeps_its_state_column_when_the_preview_has_room() {
        assert_eq!(list_width(89), None);
        assert_eq!(list_width(98), Some(60));
        assert_eq!(list_width(103), Some(65));
        assert_eq!(list_width(112), Some(65));
    }
}
