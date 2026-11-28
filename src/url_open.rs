//! Opening a URL in a browser: each way the platform (or the user's
//! configured command) offers is tried in turn until one works, and the
//! caller learns whether any did.

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::platform::UrlOpener;

/// How long an opener may run before it counts as having opened the URL.
/// Why not wait for it to exit: `xdg-open` and some `$BROWSER` commands stay
/// until the browser they started closes.
const STILL_RUNNING_MEANS_OPENED: Duration = Duration::from_secs(3);
const POLL: Duration = Duration::from_millis(50);

/// What one opener did.
#[derive(Debug)]
pub(crate) enum Attempt {
    /// Opened, possibly by a process still running that the caller should
    /// reap.
    Opened(Option<Child>),
    Failed(String),
}

/// The configured `command` (`{url}` replaced, or the URL added last), or
/// the platform's openers when `command` is empty.
pub(crate) fn openers(url: &str, command: &[String]) -> Vec<UrlOpener> {
    match crate::platform::command_with_url(command, "{url}", url) {
        Some(opener) => vec![opener],
        None => crate::platform::url_openers(url),
    }
}

/// Tries `openers` in order with `run` until one opens the URL. The error
/// names each opener tried and why it failed, never the URL.
pub(crate) fn open_with(
    openers: &[UrlOpener],
    mut run: impl FnMut(&UrlOpener) -> Attempt,
) -> Result<Option<Child>, String> {
    if openers.is_empty() {
        return Err("no way to open a browser on this platform".into());
    }
    let mut failures = Vec::new();
    for opener in openers {
        match run(opener) {
            Attempt::Opened(child) => return Ok(child),
            Attempt::Failed(reason) => failures.push(reason),
        }
    }
    Err(failures.join("; "))
}

/// Runs one opener and waits up to `still_running_means_opened` for its
/// verdict.
pub(crate) fn run(opener: &UrlOpener, still_running_means_opened: Duration) -> Attempt {
    let (program, args, exit_status_reliable) = match opener {
        #[cfg(windows)]
        UrlOpener::Native { url } => {
            return match crate::platform::open_url_natively(url) {
                Ok(()) => Attempt::Opened(None),
                Err(err) => Attempt::Failed(format!("ShellExecuteW: {err}")),
            }
        }
        UrlOpener::Command {
            program,
            args,
            exit_status_reliable,
        } => (program, args, *exit_status_reliable),
    };
    let mut child = match Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => return Attempt::Failed(format!("{program}: {err}")),
    };
    let deadline = Instant::now() + still_running_means_opened;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() || !exit_status_reliable => {
                return Attempt::Opened(None)
            }
            Ok(Some(status)) => return Attempt::Failed(format!("{program} exited with {status}")),
            Ok(None) if Instant::now() >= deadline => return Attempt::Opened(Some(child)),
            Ok(None) => std::thread::sleep(POLL),
            Err(err) => return Attempt::Failed(format!("{program}: {err}")),
        }
    }
}

/// Opens `url` with `command`, or the platform's openers when it is empty.
/// Blocks until an opener has answered, at most a few seconds each.
pub(crate) fn open(url: &str, command: &[String]) -> Result<Option<Child>, String> {
    open_with(&openers(url, command), |opener| {
        run(opener, STILL_RUNNING_MEANS_OPENED)
    })
}

#[cfg(test)]
mod tests;
