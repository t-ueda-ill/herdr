//! Whether `~/.ssh/config` sets `RemoteCommand` for a host.

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

const SSH_G_TIMEOUT: Duration = Duration::from_secs(5);

/// `ssh -G` prints one lowercase `key value` line per option, and prints no
/// `remotecommand` line at all when none is configured.
pub(crate) fn config_sets_remote_command(ssh_g_output: &str) -> bool {
    ssh_g_output
        .lines()
        .any(|line| line.split_whitespace().next() == Some("remotecommand"))
}

/// Blocks on a child process, so it must stay off the app thread.
///
/// Why true on any failure: an unreadable answer must not inject a launcher
/// over a command the user configured, and the plain login still works.
pub(crate) fn host_has_remote_command(host: &str) -> bool {
    let Ok(mut child) = Command::new("ssh")
        .arg("-G")
        .arg(host)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return true;
    };
    let Some(mut stdout) = child.stdout.take() else {
        return true;
    };
    let (tx, rx) = mpsc::channel();
    // Why a reader thread: `ssh -G` can block on a `Match exec` command, and
    // only a timeout on the receive keeps this lookup from hanging forever.
    let spawned = crate::thread_spawn::spawn_named("herdr-ssh-config-lookup", move || {
        let mut output = String::new();
        let read = stdout.read_to_string(&mut output).map(|_| output);
        let _ = tx.send(read);
    });
    if spawned.is_err() {
        let _ = child.kill();
        let _ = child.wait();
        return true;
    }
    match rx.recv_timeout(SSH_G_TIMEOUT) {
        Ok(Ok(output)) => {
            let success = child.wait().is_ok_and(|status| status.success());
            !success || config_sets_remote_command(&output)
        }
        _ => {
            let _ = child.kill();
            let _ = child.wait();
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_command_line_marks_the_host_as_configured() {
        let output = "host prod\nremotecommand tmux attach\nport 22\n";
        assert!(config_sets_remote_command(output));
    }

    #[test]
    fn output_without_a_remote_command_line_is_unconfigured() {
        let output = "host prod\nport 22\nrequesttty auto\n";
        assert!(!config_sets_remote_command(output));
    }

    #[test]
    fn a_remote_command_line_with_no_value_still_counts() {
        assert!(config_sets_remote_command("remotecommand\n"));
    }

    #[test]
    fn option_names_that_merely_contain_the_key_do_not_count() {
        assert!(!config_sets_remote_command("localcommand remotecommand\n"));
    }
}
