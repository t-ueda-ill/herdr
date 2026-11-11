//! Shell command boundaries, read from the OSC 133 prompt marks the terminal
//! keeps on each row. A shell that sends no marks has no boundaries, and every
//! lookup here then reports [`CommandMarkError::Unmarked`].

use crate::ghostty::{RowPrompt, Terminal};

use super::{PaneTerminal, TerminalTextPoint};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PromptMotion {
    Previous,
    Next,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommandMarkError {
    /// No row carries a prompt mark: the shell does not send OSC 133.
    Unmarked,
    /// The shell sends marks, but there is no prompt or output to return.
    NotFound,
}

/// Screen rows whose output decides what "the output at this row" is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OutputProbe {
    /// A row of the command the cursor is on.
    own: Option<usize>,
    /// The row directly above the prompt that waits for input, tried when
    /// `own` has nothing written.
    previous: Option<usize>,
}

/// Start row of the nearest prompt strictly above or below `row`.
fn prompt_start_row(
    total_rows: usize,
    row: usize,
    motion: PromptMotion,
    prompt_at: impl Fn(usize) -> RowPrompt,
) -> Option<usize> {
    let is_start = |candidate: &usize| prompt_at(*candidate) == RowPrompt::Start;
    match motion {
        PromptMotion::Previous => (0..row.min(total_rows)).rev().find(is_start),
        PromptMotion::Next => (row.saturating_add(1)..total_rows).find(is_start),
    }
}

/// On an output row the probe is that row. On a prompt row it is the first row
/// after the prompt and the command typed at it.
///
/// Why the last prompt also probes the row above it: that prompt is the one
/// waiting for input, where the cursor sits right after a command ends, and it
/// has no output of its own yet. Why only the row directly above, and only for
/// the last prompt: a command that printed nothing leaves its prompt there, and
/// stepping over it would hand back an older command's output unnoticed.
fn output_probe(
    total_rows: usize,
    row: usize,
    prompt_at: impl Fn(usize) -> RowPrompt,
) -> OutputProbe {
    if row >= total_rows {
        return OutputProbe {
            own: None,
            previous: None,
        };
    }
    if prompt_at(row) == RowPrompt::None {
        return OutputProbe {
            own: Some(row),
            previous: None,
        };
    }
    let own = (row + 1..total_rows)
        .find(|candidate| prompt_at(*candidate) != RowPrompt::Continuation)
        .filter(|candidate| prompt_at(*candidate) == RowPrompt::None);
    let is_last_prompt =
        prompt_start_row(total_rows, row, PromptMotion::Next, &prompt_at).is_none();
    let previous = is_last_prompt
        .then(|| {
            // The first row of this prompt, or the row above it when the
            // prompt's first row already left the scrollback.
            let first = (0..=row)
                .rev()
                .find(|candidate| prompt_at(*candidate) != RowPrompt::Continuation)?;
            if prompt_at(first) == RowPrompt::Start {
                first.checked_sub(1)
            } else {
                Some(first)
            }
        })
        .flatten()
        .filter(|candidate| prompt_at(*candidate) == RowPrompt::None);
    OutputProbe { own, previous }
}

fn row_prompt(terminal: &Terminal, row: usize) -> RowPrompt {
    u32::try_from(row)
        .ok()
        .and_then(|row| terminal.screen_row_prompt(row).ok())
        .unwrap_or(RowPrompt::None)
}

fn missing_mark_error(terminal: &Terminal, total_rows: usize) -> CommandMarkError {
    if (0..total_rows).any(|row| row_prompt(terminal, row) == RowPrompt::Start) {
        CommandMarkError::NotFound
    } else {
        CommandMarkError::Unmarked
    }
}

/// The output around `row`, or `None` when nothing is written there: the
/// terminal answers with a single blank cell for output that has no text.
fn written_output_range(
    terminal: &Terminal,
    row: usize,
) -> Option<(TerminalTextPoint, TerminalTextPoint)> {
    let row = u32::try_from(row).ok()?;
    let ((start_col, start_row), (end_col, end_row)) =
        terminal.screen_command_output_range(0, row).ok()??;
    let (_, graphemes) = terminal.screen_cell(end_col, end_row).ok()?;
    let has_text = graphemes
        .iter()
        .filter_map(|codepoint| char::from_u32(*codepoint))
        .any(|character| !character.is_whitespace());
    has_text.then_some((
        TerminalTextPoint {
            row: start_row,
            col: start_col,
        },
        TerminalTextPoint {
            row: end_row,
            col: end_col,
        },
    ))
}

impl PaneTerminal {
    /// First cell of the nearest prompt above or below `row`.
    pub(crate) fn prompt_motion_target(
        &self,
        row: u32,
        motion: PromptMotion,
    ) -> Result<TerminalTextPoint, CommandMarkError> {
        let core = self
            .ghostty
            .core
            .lock()
            .map_err(|_| CommandMarkError::NotFound)?;
        let terminal = &core.terminal;
        let total_rows = terminal
            .total_rows()
            .map_err(|_| CommandMarkError::NotFound)?;
        let target = prompt_start_row(total_rows, row as usize, motion, |candidate| {
            row_prompt(terminal, candidate)
        })
        .and_then(|target| u32::try_from(target).ok());
        match target {
            Some(row) => Ok(TerminalTextPoint { row, col: 0 }),
            None => Err(missing_mark_error(terminal, total_rows)),
        }
    }

    /// First and last cell of the output of the command at `row`, without
    /// trailing blanks. A command that still runs ends at its last written row.
    pub(crate) fn command_output_range(
        &self,
        row: u32,
    ) -> Result<(TerminalTextPoint, TerminalTextPoint), CommandMarkError> {
        let core = self
            .ghostty
            .core
            .lock()
            .map_err(|_| CommandMarkError::NotFound)?;
        let terminal = &core.terminal;
        let total_rows = terminal
            .total_rows()
            .map_err(|_| CommandMarkError::NotFound)?;
        let probe = output_probe(total_rows, row as usize, |candidate| {
            row_prompt(terminal, candidate)
        });
        [probe.own, probe.previous]
            .into_iter()
            .flatten()
            .find_map(|candidate| written_output_range(terminal, candidate))
            .ok_or_else(|| missing_mark_error(terminal, total_rows))
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc;

    use super::super::GhosttyPaneTerminal;
    use super::*;

    const PROMPT: &[u8] = b"\x1b]133;A\x07$ \x1b]133;B\x07";

    /// A shell with OSC 133 marks: every command is a prompt, the typed
    /// command and its output. The prompt waiting for input comes last.
    fn marked_shell(cols: u16, rows: u16, commands: &[(&str, &[&str])]) -> PaneTerminal {
        let mut terminal = Terminal::new(cols, rows, 1000).unwrap();
        for (command, output) in commands {
            terminal.write(PROMPT);
            terminal.write(command.as_bytes());
            terminal.write(b"\r\n\x1b]133;C\x07");
            for line in *output {
                terminal.write(line.as_bytes());
                terminal.write(b"\r\n");
            }
        }
        terminal.write(PROMPT);
        pane(terminal)
    }

    fn pane(terminal: Terminal) -> PaneTerminal {
        let (tx, _rx) = mpsc::channel(4);
        PaneTerminal::new(GhosttyPaneTerminal::new(terminal, tx).unwrap())
    }

    fn point(row: u32, col: u16) -> TerminalTextPoint {
        TerminalTextPoint { row, col }
    }

    #[test]
    fn previous_prompt_from_output_is_the_prompt_of_that_command() {
        let shell = marked_shell(
            20,
            4,
            &[("first", &["a1", "a2"]), ("second", &["b1", "b2"])],
        );

        assert_eq!(
            shell.prompt_motion_target(5, PromptMotion::Previous),
            Ok(point(3, 0))
        );
    }

    #[test]
    fn previous_prompt_from_a_prompt_is_the_command_before_it() {
        let shell = marked_shell(
            20,
            4,
            &[("first", &["a1", "a2"]), ("second", &["b1", "b2"])],
        );

        assert_eq!(
            shell.prompt_motion_target(3, PromptMotion::Previous),
            Ok(point(0, 0))
        );
    }

    #[test]
    fn next_prompt_skips_the_rest_of_the_current_command() {
        let shell = marked_shell(
            20,
            4,
            &[("first", &["a1", "a2"]), ("second", &["b1", "b2"])],
        );

        assert_eq!(
            shell.prompt_motion_target(1, PromptMotion::Next),
            Ok(point(3, 0))
        );
    }

    #[test]
    fn prompt_motion_past_the_first_or_last_prompt_finds_nothing() {
        let shell = marked_shell(20, 4, &[("first", &["a1"])]);

        assert_eq!(
            shell.prompt_motion_target(0, PromptMotion::Previous),
            Err(CommandMarkError::NotFound)
        );
        assert_eq!(
            shell.prompt_motion_target(2, PromptMotion::Next),
            Err(CommandMarkError::NotFound)
        );
    }

    #[test]
    fn a_shell_without_marks_is_reported_as_unmarked() {
        let mut terminal = Terminal::new(20, 4, 100).unwrap();
        terminal.write(b"$ first\r\na1\r\n$ ");
        let shell = pane(terminal);

        assert_eq!(
            shell.prompt_motion_target(2, PromptMotion::Previous),
            Err(CommandMarkError::Unmarked)
        );
        assert_eq!(
            shell.command_output_range(1),
            Err(CommandMarkError::Unmarked)
        );
    }

    #[test]
    fn output_range_from_an_output_row_covers_that_command_only() {
        let shell = marked_shell(
            20,
            4,
            &[("first", &["a1", "a2 tail"]), ("second", &["b1", "b2"])],
        );

        assert_eq!(
            shell.command_output_range(2),
            Ok((point(1, 0), point(2, 6)))
        );
    }

    #[test]
    fn output_range_from_a_prompt_row_is_the_output_of_its_command() {
        let shell = marked_shell(
            20,
            4,
            &[("first", &["a1", "a2"]), ("second", &["b1", "b2"])],
        );

        assert_eq!(
            shell.command_output_range(0),
            Ok((point(1, 0), point(2, 1)))
        );
    }

    #[test]
    fn output_range_from_a_wrapped_command_line_starts_after_the_whole_command() {
        let shell = marked_shell(10, 6, &[("0123456789abc", &["out"])]);

        assert_eq!(
            shell.command_output_range(0),
            Ok((point(2, 0), point(2, 2)))
        );
        assert_eq!(
            shell.command_output_range(1),
            Ok((point(2, 0), point(2, 2)))
        );
    }

    #[test]
    fn output_range_from_the_waiting_prompt_is_the_output_directly_above_it() {
        let shell = marked_shell(20, 8, &[("first", &["a1"]), ("second", &["b1", "b2"])]);

        assert_eq!(
            shell.command_output_range(5),
            Ok((point(3, 0), point(4, 1)))
        );
    }

    #[test]
    fn output_range_from_the_waiting_prompt_on_the_last_row_is_the_output_directly_above_it() {
        let shell = marked_shell(20, 3, &[("first", &["a1", "a2", "a3"])]);

        assert_eq!(
            shell.command_output_range(4),
            Ok((point(1, 0), point(3, 1)))
        );
    }

    #[test]
    fn output_range_of_an_earlier_command_that_printed_nothing_is_not_found() {
        let shell = marked_shell(
            20,
            8,
            &[("first", &["a1"]), ("quiet", &[]), ("third", &["c1"])],
        );

        assert_eq!(
            shell.command_output_range(2),
            Err(CommandMarkError::NotFound)
        );
    }

    #[test]
    fn output_range_of_a_running_command_ends_at_its_last_written_row() {
        let mut terminal = Terminal::new(20, 6, 100).unwrap();
        terminal.write(PROMPT);
        terminal.write(b"slow\r\n\x1b]133;C\x07partial 1\r\npartial 2\r\n");
        let shell = pane(terminal);

        assert_eq!(
            shell.command_output_range(3),
            Ok((point(1, 0), point(2, 8)))
        );
    }

    #[test]
    fn output_range_survives_the_prompt_leaving_the_scrollback() {
        let mut terminal = Terminal::new(20, 3, 0).unwrap();
        terminal.write(PROMPT);
        terminal.write(b"long\r\n\x1b]133;C\x07l1\r\nl2\r\nl3\r\nl4\r\n");
        terminal.write(PROMPT);
        let shell = pane(terminal);

        assert_eq!(
            shell.command_output_range(2),
            Ok((point(0, 0), point(1, 1)))
        );
    }

    #[test]
    fn output_range_ignores_marks_of_the_primary_screen_while_the_alternate_screen_is_shown() {
        let mut terminal = Terminal::new(20, 4, 100).unwrap();
        terminal.write(PROMPT);
        terminal.write(b"vim\r\n\x1b]133;C\x07\x1b[?1049hfull screen");
        let shell = pane(terminal);

        assert_eq!(
            shell.command_output_range(0),
            Err(CommandMarkError::Unmarked)
        );
    }

    /// The marks herdr's own shell code sends: a prompt hook cannot mark
    /// where the typed command starts, so there is no `B`.
    const HOOK_PROMPT: &[u8] = b"\x1b]133;A\x07$ ";

    #[test]
    fn output_range_works_with_only_prompt_start_and_output_start_marks() {
        let mut terminal = Terminal::new(20, 6, 100).unwrap();
        for (command, output) in [("first", "a1"), ("second", "b1")] {
            terminal.write(HOOK_PROMPT);
            terminal.write(command.as_bytes());
            terminal.write(b"\r\n\x1b]133;C\x07");
            terminal.write(output.as_bytes());
            terminal.write(b"\r\n");
        }
        terminal.write(HOOK_PROMPT);
        let shell = pane(terminal);

        assert_eq!(
            shell.command_output_range(4),
            Ok((point(3, 0), point(3, 1)))
        );
        assert_eq!(
            shell.prompt_motion_target(4, PromptMotion::Previous),
            Ok(point(2, 0))
        );
    }

    #[test]
    fn output_that_ends_without_a_newline_stays_with_the_command_that_printed_it() {
        for output in ["tail", "t1\r\nt2\r\ntail"] {
            let mut terminal = Terminal::new(20, 10, 100).unwrap();
            terminal.write(HOOK_PROMPT);
            terminal.write(b"echo first\r\n\x1b]133;C\x07first\r\n");
            terminal.write(HOOK_PROMPT);
            terminal.write(b"printf\r\n\x1b]133;C\x07");
            terminal.write(output.as_bytes());
            terminal.write(HOOK_PROMPT);
            let last_row = terminal.total_rows().unwrap() as u32 - 1;
            let waiting_prompt = (0..=last_row)
                .rev()
                .find(|row| terminal.screen_row_prompt(*row).unwrap() == RowPrompt::Start)
                .unwrap();
            let shell = pane(terminal);

            let (start, end) = shell.command_output_range(waiting_prompt).unwrap();

            assert_eq!(start, point(3, 0), "{output:?}");
            assert_eq!(end, point(waiting_prompt - 1, 3), "{output:?}");
        }
    }

    #[test]
    fn the_waiting_prompt_after_a_command_without_output_finds_nothing() {
        let shell = marked_shell(20, 8, &[("first", &["a1"]), ("cd /tmp", &[])]);

        assert_eq!(
            shell.command_output_range(3),
            Err(CommandMarkError::NotFound)
        );
    }

    #[test]
    fn the_waiting_prompt_after_an_empty_command_line_finds_nothing() {
        let mut terminal = Terminal::new(20, 8, 100).unwrap();
        terminal.write(HOOK_PROMPT);
        terminal.write(b"first\r\n\x1b]133;C\x07a1\r\n");
        terminal.write(HOOK_PROMPT);
        terminal.write(b"\r\n");
        terminal.write(HOOK_PROMPT);
        let shell = pane(terminal);

        assert_eq!(
            shell.command_output_range(3),
            Err(CommandMarkError::NotFound)
        );
    }
}
