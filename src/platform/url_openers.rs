//! The ways a platform can open a URL in a browser, tried in order by
//! `crate::url_open`.

/// One way to open a URL. Commands run without a shell: the URL may come
/// from a pane, so it is only ever one argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UrlOpener {
    Command {
        program: String,
        args: Vec<String>,
        /// False for a program that reports failure even when it opened the
        /// URL, such as `explorer.exe`; then only a failed start counts.
        exit_status_reliable: bool,
    },
    /// The platform's own call, `ShellExecuteW`.
    #[cfg(windows)]
    Native { url: String },
}

impl UrlOpener {
    pub(crate) fn command(program: &str, args: Vec<String>) -> Self {
        Self::Command {
            program: program.to_owned(),
            args,
            exit_status_reliable: true,
        }
    }

    /// The program's name, for messages and logs. Never the arguments: they
    /// hold the URL, and a preview URL carries its token.
    pub(crate) fn name(&self) -> &str {
        match self {
            Self::Command { program, .. } => program,
            #[cfg(windows)]
            Self::Native { .. } => "ShellExecuteW",
        }
    }
}

/// `words` with `{url}` replaced by `url`, or `url` added last when no word
/// is `{url}`. `None` when `words` is empty.
pub(crate) fn command_with_url(
    words: &[String],
    placeholder: &str,
    url: &str,
) -> Option<UrlOpener> {
    let (program, rest) = words.split_first()?;
    let mut args: Vec<String> = rest
        .iter()
        .map(|word| word.replace(placeholder, url))
        .collect();
    if !rest.iter().any(|word| word.contains(placeholder)) {
        args.push(url.to_owned());
    }
    Some(UrlOpener::command(program, args))
}

// Why allowed on Windows: it opens URLs with ShellExecuteW, not commands.
#[cfg_attr(windows, allow(dead_code))]
/// The commands of a `$BROWSER` value: `:`-separated, each split at
/// whitespace, with `%s` standing for the URL as in the old convention that
/// `xdg-open` and Python's `webbrowser` follow.
pub(crate) fn browser_env_openers(value: &str, url: &str) -> Vec<UrlOpener> {
    value
        .split(':')
        .filter_map(|command| {
            let words: Vec<String> = command.split_whitespace().map(str::to_owned).collect();
            command_with_url(&words, "%s", url)
        })
        .collect()
}

// Why allowed on Windows: it opens URLs with ShellExecuteW, not commands.
#[cfg_attr(windows, allow(dead_code))]
/// `openers` without a later one that runs the same program as an earlier
/// one, which would only fail the same way again.
pub(crate) fn dedup_by_program(openers: Vec<UrlOpener>) -> Vec<UrlOpener> {
    let mut kept: Vec<UrlOpener> = Vec::with_capacity(openers.len());
    for opener in openers {
        if !kept.iter().any(|known| known.name() == opener.name()) {
            kept.push(opener);
        }
    }
    kept
}

#[cfg(test)]
mod tests;
