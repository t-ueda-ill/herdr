//! The Markdown file an assist reads: where the pane runs, and what it shows.

use crate::pane::CommandText;

/// What the assist reads about one pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AssistContext {
    pub(crate) pane_id: String,
    pub(crate) label: String,
    /// `local`, `ssh:<alias>`, `container:<ref>`, ...
    pub(crate) location: String,
    pub(crate) cwd: Option<String>,
    pub(crate) agent: Option<String>,
    pub(crate) read: ContextRead,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ContextRead {
    /// From a shell that marks its prompts (OSC 133).
    Commands {
        commands: Vec<CommandText>,
        waiting_prompt: Option<String>,
    },
    /// The bottom of the screen, for a shell without marks or an agent.
    Screen(String),
}

/// The context as Markdown. Each output keeps at most `max_lines` of its last lines.
pub(crate) fn render_context(context: &AssistContext, max_lines: usize) -> String {
    let mut out = format!(
        "# herdr pane \"{}\" ({})\n\n- location: {}\n",
        context.label, context.pane_id, context.location
    );
    if let Some(cwd) = &context.cwd {
        out.push_str(&format!("- cwd: {cwd}\n"));
    }
    if let Some(agent) = &context.agent {
        out.push_str(&format!("- agent: {agent}\n"));
    }
    match &context.read {
        ContextRead::Commands {
            commands,
            waiting_prompt,
        } => {
            out.push_str("\n## Recent commands (oldest first)\n");
            if commands.is_empty() {
                out.push_str("\nNone yet.\n");
            }
            for command in commands {
                out.push('\n');
                out.push_str(&fenced("command", &command.command));
                if !command.output.is_empty() {
                    out.push_str(&fenced("output", &last_lines(&command.output, max_lines)));
                }
            }
            if let Some(prompt) = waiting_prompt {
                out.push_str(
                    "\n## Prompt waiting for input\n\nText after the prompt is typed but not run yet.\n\n",
                );
                out.push_str(&fenced("prompt", prompt));
            }
        }
        ContextRead::Screen(text) => {
            out.push_str("\n## Screen (bottom)\n\n");
            out.push_str(&fenced("screen", &last_lines(text, max_lines)));
        }
    }
    out
}

/// A fenced block whose fence is longer than any backtick run in `text`, so
/// output that holds Markdown cannot close it early.
fn fenced(info: &str, text: &str) -> String {
    let longest_run = text
        .split(|character| character != '`')
        .map(str::len)
        .max()
        .unwrap_or(0);
    let fence = "`".repeat(longest_run.max(2) + 1);
    format!("{fence}{info}\n{text}\n{fence}\n")
}

fn last_lines(text: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= max_lines {
        return text.to_owned();
    }
    let dropped = lines.len() - max_lines;
    format!(
        "[{dropped} earlier lines left out]\n{}",
        lines[dropped..].join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(read: ContextRead) -> AssistContext {
        AssistContext {
            pane_id: "p_2".into(),
            label: "shell".into(),
            location: "ssh:dev".into(),
            cwd: Some("/home/u/app".into()),
            agent: None,
            read,
        }
    }

    #[test]
    fn the_header_names_the_pane_and_where_it_runs() {
        let text = render_context(&context(ContextRead::Screen(String::new())), 10);

        assert!(text.starts_with("# herdr pane \"shell\" (p_2)\n"));
        assert!(text.contains("- location: ssh:dev\n"));
        assert!(text.contains("- cwd: /home/u/app\n"));
        assert!(!text.contains("- agent:"));
    }

    #[test]
    fn commands_are_written_with_their_output_and_the_waiting_prompt_last() {
        let text = render_context(
            &context(ContextRead::Commands {
                commands: vec![CommandText {
                    command: "$ make".into(),
                    output: "error: x".into(),
                }],
                waiting_prompt: Some("$ git rebase main".into()),
            }),
            10,
        );

        let make = text.find("```command\n$ make\n```").unwrap();
        let output = text.find("```output\nerror: x\n```").unwrap();
        let prompt = text.find("```prompt\n$ git rebase main\n```").unwrap();
        assert!(make < output && output < prompt);
    }

    #[test]
    fn a_command_without_output_gets_no_output_block() {
        let text = render_context(
            &context(ContextRead::Commands {
                commands: vec![CommandText {
                    command: "$ cd src".into(),
                    output: String::new(),
                }],
                waiting_prompt: None,
            }),
            10,
        );

        assert!(!text.contains("```output"));
        assert!(!text.contains("Prompt waiting"));
    }

    #[test]
    fn long_output_keeps_its_last_lines_and_says_how_many_were_left_out() {
        let text = render_context(&context(ContextRead::Screen("1\n2\n3\n4\n5".into())), 2);

        assert!(text.contains("```screen\n[3 earlier lines left out]\n4\n5\n```"));
    }

    #[test]
    fn a_fence_inside_the_output_cannot_close_the_block() {
        let text = render_context(&context(ContextRead::Screen("a\n````\nb".into())), 10);

        assert!(text.contains("`````screen\na\n````\nb\n`````\n"));
    }
}
