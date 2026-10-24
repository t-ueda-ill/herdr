use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::offhost::shell_integration::{launcher_argv, needs_launcher, LauncherShell};

/// Fully resolved ssh pane target, kept so respawns never re-read the config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SshLaunch {
    /// Destination handed to `ssh`: a config alias, optionally prefixed with `user@`.
    pub(crate) host: String,
    /// The config alias alone. Computed once so metadata paths only clone it.
    pub(crate) canonical: String,
    /// Herdr-owned `ControlPath` template; `None` when multiplexing is unavailable.
    pub(crate) control_path: Option<PathBuf>,
    /// Directory the remote shell is asked to start in, applied by the
    /// launcher so a missing one falls back to the login directory.
    pub(crate) start_dir: Option<PathBuf>,
    /// True when `~/.ssh/config` sets `RemoteCommand` for the host, or when
    /// that was not checked. Why default to true: a configured command owns the
    /// session, and injecting a launcher there would replace it.
    pub(crate) has_remote_command: bool,
}

impl SshLaunch {
    /// `None` for a destination `ssh_destination_alias` rejects.
    pub(crate) fn new(host: String, control_path: Option<PathBuf>) -> Option<Self> {
        let canonical = ssh_destination_alias(&host)?.to_owned();
        Some(Self {
            host,
            canonical,
            control_path,
            start_dir: None,
            has_remote_command: true,
        })
    }

    pub(crate) fn with_remote_command(mut self, has_remote_command: bool) -> Self {
        self.has_remote_command = has_remote_command;
        self
    }

    #[cfg(test)]
    pub(crate) fn with_start_dir(mut self, start_dir: Option<PathBuf>) -> Self {
        self.start_dir = start_dir;
        self
    }
}

/// True for names `ssh` can only read as a destination.
///
/// Why not accept every config alias: the value lands in `ssh`'s argv, so a
/// leading `-` would become an option and shell or `%` metacharacters would
/// reach token expansion.
pub(crate) fn is_valid_ssh_host(text: &str) -> bool {
    !text.is_empty()
        && !text.starts_with('-')
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'@' | b'-'))
}

/// The config alias of `alias` or `user@alias`; `None` when the destination is invalid.
pub(crate) fn ssh_destination_alias(host: &str) -> Option<&str> {
    if !is_valid_ssh_host(host) {
        return None;
    }
    // `ssh` itself splits the user off at the last `@`.
    let alias = match host.rsplit_once('@') {
        Some((user, alias)) if !user.is_empty() => alias,
        Some(_) => return None,
        None => host,
    };
    is_valid_ssh_host(alias).then_some(alias)
}

/// `ssh -C [-S <control> -o ControlMaster=auto -o ControlPersist=600] -t <host>`.
///
/// Why not `-F` with a generated config like `herdr --remote`: the pane is the
/// user's own login, so their `~/.ssh/config` must apply unchanged. Why not
/// `BatchMode`: the pane is interactive and owns password and host key prompts.
pub(crate) fn ssh_argv(launch: &SshLaunch) -> Vec<OsString> {
    let mut argv: Vec<OsString> = vec!["ssh".into(), "-C".into()];
    if let Some(control_path) = &launch.control_path {
        argv.extend([
            "-S".into(),
            control_path.as_os_str().to_owned(),
            "-o".into(),
            "ControlMaster=auto".into(),
            "-o".into(),
            "ControlPersist=600".into(),
        ]);
    }
    argv.push("-t".into());
    argv.push((&launch.host).into());
    argv
}

/// [`ssh_argv`] running `command` on the remote, with the config's
/// `RemoteCommand` disabled.
///
/// Why not put `-o RemoteCommand=none` in `ssh_argv`: plain panes are the
/// user's own login and must keep their configured `RemoteCommand`. Why it is
/// needed here: OpenSSH refuses a command line together with a configured
/// `RemoteCommand` and exits 255.
pub(crate) fn ssh_command_argv(launch: &SshLaunch, command: OsString) -> Vec<OsString> {
    let mut argv = ssh_argv(launch);
    // `ssh_argv` ends with `-t <host>`; the override must precede them.
    let before_tty = argv.len() - 2;
    argv.splice(
        before_tty..before_tty,
        ["-o".into(), "RemoteCommand=none".into()],
    );
    argv.push(command);
    argv
}

/// The argv of an ssh pane: plain [`ssh_argv`], or the launcher when the pane
/// must enter `start_dir` or load the shell integration.
///
/// The launcher always runs a login shell, because a plain `ssh -t host` is
/// answered with one and a command line is not. It names no shell, so `$SHELL`
/// decides, as it does for the plain login. A host with a configured
/// `RemoteCommand` keeps the plain argv: that command owns the session.
pub(crate) fn ssh_pane_argv(launch: &SshLaunch, shell_integration: bool) -> Vec<OsString> {
    if launch.has_remote_command || !needs_launcher(shell_integration, launch.start_dir.as_deref())
    {
        return ssh_argv(launch);
    }
    let command = launcher_argv(
        launch.start_dir.as_deref(),
        LauncherShell::Account,
        true,
        shell_integration,
    )
    .iter()
    .map(|arg| shell_quote(&arg.to_string_lossy()))
    .collect::<Vec<_>>()
    .join(" ");
    ssh_command_argv(launch, portable_remote_command(&command).into())
}

/// Wraps a POSIX command line so any login shell passes it through intact.
///
/// sshd runs the string with the account's login shell. In fish a backslash
/// is an escape inside single quotes, and in csh a newline inside them is a
/// syntax error and `!` starts history expansion. So every byte outside a
/// small safe set is written as an octal escape, which `printf` decodes and
/// `eval` runs under `sh`. `%` is escaped too because `printf` reads the text
/// as its format. Why not `base64`: it is not guaranteed on a remote machine.
pub(crate) fn portable_remote_command(posix_command: &str) -> String {
    let mut escaped = String::with_capacity(posix_command.len() * 2);
    for byte in posix_command.bytes() {
        if byte.is_ascii_alphanumeric() || b"_./,:=+- ".contains(&byte) {
            escaped.push(char::from(byte));
        } else {
            escaped.push_str(&format!("\\{byte:03o}"));
        }
    }
    format!("exec sh -c 'eval \"$(printf \"{escaped}\")\"'")
}

/// `ssh [-S <control> -o ControlMaster=auto -o ControlPersist=600] <batch options> <host> <command>`.
///
/// Why `ControlMaster=auto` and not `no` like the clipboard side channel: a
/// listing may be the first contact with the host, and the master it creates
/// is the one a pane opened afterwards joins through the same `ControlPath`.
/// Why `BatchMode`: nothing here can answer a password or host key prompt, so
/// an unauthenticated host must fail fast and send the user to an ssh pane.
/// Why not `-F`: the user's own config must resolve `host` exactly as it does
/// for ssh panes, or `%C` in the control path would name another socket.
/// The remaining options match the clipboard side channel: no tty (stdin may
/// carry raw bytes), no `RemoteCommand` (ssh refuses a second command), and
/// no forwardings or local commands.
/// `command` is one string the remote login shell parses; the caller quotes it.
pub(crate) fn ssh_batch_argv(
    host: &str,
    control_path: Option<&Path>,
    command: &str,
) -> Vec<OsString> {
    let mut argv: Vec<OsString> = Vec::new();
    if let Some(control_path) = control_path {
        argv.extend([
            "-S".into(),
            control_path.as_os_str().to_owned(),
            "-o".into(),
            "ControlMaster=auto".into(),
            "-o".into(),
            "ControlPersist=600".into(),
        ]);
    }
    for option in [
        "BatchMode=yes",
        "ConnectTimeout=5",
        "RequestTTY=no",
        "RemoteCommand=none",
        "ClearAllForwardings=yes",
        "PermitLocalCommand=no",
        "ForwardX11=no",
        "ForwardAgent=no",
    ] {
        argv.push("-o".into());
        argv.push(option.into());
    }
    argv.push(host.into());
    argv.push(command.into());
    argv
}

/// Exit status 255 is ssh's own failure; these are the ones a prompt-free
/// run cannot get past but an interactive ssh pane can.
pub(crate) fn is_ssh_login_failure(stderr: &str) -> bool {
    [
        "Permission denied",
        "Host key verification failed",
        "REMOTE HOST IDENTIFICATION HAS CHANGED",
        "Could not resolve",
        "Too many authentication failures",
    ]
    .iter()
    .any(|marker| stderr.contains(marker))
}

/// POSIX single quoting: the remote shell reads `text` back unchanged,
/// including spaces, `$`, and embedded `'`.
pub(crate) fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(argv: Vec<OsString>) -> Vec<String> {
        argv.into_iter()
            .map(|arg| arg.into_string().unwrap())
            .collect()
    }

    fn resolved(host: &str) -> SshLaunch {
        SshLaunch::new(host.into(), None)
            .unwrap()
            .with_remote_command(false)
    }

    #[test]
    fn ssh_pane_argv_is_plain_ssh_when_nothing_needs_the_launcher() {
        let launch = resolved("prod");
        assert_eq!(ssh_pane_argv(&launch, false), ssh_argv(&launch));
    }

    #[test]
    fn ssh_pane_argv_runs_the_quoted_login_launcher_with_integration() {
        let launch = resolved("prod").with_start_dir(Some("/srv/it's".into()));
        let argv = strings(ssh_pane_argv(&launch, true));
        assert_eq!(argv[..5], ["ssh", "-C", "-o", "RemoteCommand=none", "-t"]);
        assert_eq!(argv[5], "prod");
        assert_eq!(argv.len(), 7, "the launcher travels as one ssh argument");
        assert!(argv[6].starts_with("exec sh -c 'eval \"$(printf \""));
    }

    #[test]
    fn ssh_pane_argv_launches_only_for_the_start_dir_when_integration_is_off() {
        let launch = resolved("prod").with_start_dir(Some("/srv".into()));
        let argv = strings(ssh_pane_argv(&launch, false));
        assert!(argv[6].starts_with("exec sh -c 'eval"), "{}", argv[6]);
    }

    // Why unix only: the command only ever runs on a POSIX remote, and on
    // Windows `bash` can resolve to the WSL launcher instead of a shell.
    #[cfg(unix)]
    const TRICKY: [&str; 5] = ["it's", "a\\b", "100%", "!x", "line1\nline2"];

    /// The command, run by `shell -c` as sshd does, then decoded by `sh`.
    #[cfg(unix)]
    fn run_through(shell: &str) -> Option<String> {
        let inner = format!(
            "printf '%s|' {}",
            TRICKY
                .iter()
                .map(|arg| shell_quote(arg))
                .collect::<Vec<_>>()
                .join(" ")
        );
        let output = std::process::Command::new(shell)
            .args(["-c", &portable_remote_command(&inner)])
            .stdin(std::process::Stdio::null())
            .output()
            .ok()?;
        Some(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    #[test]
    fn portable_remote_command_keeps_safe_bytes_and_escapes_the_rest() {
        let command = portable_remote_command("ls -l /a_b.c,d:e=f+g");
        assert!(command.contains("ls -l /a_b.c,d:e=f+g"));
        let command = portable_remote_command("'\\\n%!$`\"");
        for (byte, octal) in [
            ("'", "047"),
            ("\\", "134"),
            ("\n", "012"),
            ("%", "045"),
            ("!", "041"),
            ("$", "044"),
            ("`", "140"),
            ("\"", "042"),
        ] {
            assert!(command.contains(&format!("\\{octal}")), "{byte:?}");
        }
    }

    #[test]
    fn portable_remote_command_has_no_byte_a_login_shell_could_reinterpret() {
        let launch = resolved("prod").with_start_dir(Some("/srv/it's a%b".into()));
        let argv = strings(ssh_pane_argv(&launch, true));
        let command = argv.last().unwrap();
        let wrapper = "exec sh -c 'eval \"$(printf \"\")\"'";
        let body = command
            .strip_prefix("exec sh -c 'eval \"$(printf \"")
            .and_then(|rest| rest.strip_suffix("\")\"'"))
            .unwrap_or_else(|| panic!("unexpected wrapper in {wrapper}"));
        assert!(body
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_./,:=+- \\".contains(&b)));
        assert!(!body.contains("\\\\"));
    }

    #[test]
    fn launcher_command_for_a_real_pane_stays_far_below_the_argument_limit() {
        let launch = resolved("prod").with_start_dir(Some("/srv".into()));
        let argv = strings(ssh_pane_argv(&launch, true));
        assert!(argv.last().unwrap().len() < 64 * 1024);
    }

    #[cfg(unix)]
    #[test]
    fn portable_remote_command_survives_sh_and_bash() {
        for shell in ["sh", "bash", "dash"] {
            if let Some(out) = run_through(shell) {
                assert_eq!(out, "it's|a\\b|100%|!x|line1\nline2|", "{shell}");
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn portable_remote_command_survives_other_login_shells_when_installed() {
        for shell in ["zsh", "fish", "tcsh", "csh"] {
            if let Some(out) = run_through(shell) {
                assert_eq!(out, "it's|a\\b|100%|!x|line1\nline2|", "{shell}");
            }
        }
    }

    #[test]
    fn ssh_pane_argv_keeps_plain_ssh_for_a_configured_remote_command() {
        let launch = resolved("prod")
            .with_start_dir(Some("/srv".into()))
            .with_remote_command(true);
        assert_eq!(ssh_pane_argv(&launch, true), ssh_argv(&launch));
    }

    #[test]
    fn ssh_launch_without_a_config_lookup_counts_as_having_a_remote_command() {
        assert!(
            SshLaunch::new("prod".into(), None)
                .unwrap()
                .has_remote_command
        );
    }

    #[test]
    fn ssh_command_argv_disables_remote_command_before_the_destination() {
        let launch = SshLaunch::new("deploy@prod".into(), Some("/tmp/ctl-%C".into())).unwrap();
        let argv = strings(ssh_command_argv(&launch, "docker ps".into()));
        assert_eq!(
            argv,
            [
                "ssh",
                "-C",
                "-S",
                "/tmp/ctl-%C",
                "-o",
                "ControlMaster=auto",
                "-o",
                "ControlPersist=600",
                "-o",
                "RemoteCommand=none",
                "-t",
                "deploy@prod",
                "docker ps"
            ]
        );
    }

    #[test]
    fn ssh_command_argv_without_control_path_has_no_control_options() {
        let launch = SshLaunch::new("prod".into(), None).unwrap();
        let argv = strings(ssh_command_argv(&launch, "true".into()));
        assert_eq!(
            argv,
            [
                "ssh",
                "-C",
                "-o",
                "RemoteCommand=none",
                "-t",
                "prod",
                "true"
            ]
        );
    }

    #[test]
    fn ssh_argv_keeps_the_users_remote_command() {
        let launch = SshLaunch::new("prod".into(), Some("/tmp/ctl-%C".into())).unwrap();
        assert!(strings(ssh_argv(&launch))
            .iter()
            .all(|arg| !arg.contains("RemoteCommand")));
    }

    #[test]
    fn ssh_argv_with_control_path_multiplexes_through_herdr_master() {
        let launch = SshLaunch::new("prod".into(), Some("/tmp/hssh-1/abc-%C".into())).unwrap();

        assert_eq!(
            strings(ssh_argv(&launch)),
            [
                "ssh",
                "-C",
                "-S",
                "/tmp/hssh-1/abc-%C",
                "-o",
                "ControlMaster=auto",
                "-o",
                "ControlPersist=600",
                "-t",
                "prod"
            ]
        );
    }

    #[test]
    fn ssh_argv_without_control_path_opens_a_plain_interactive_session() {
        let launch = SshLaunch::new("deploy@prod".into(), None).unwrap();

        assert_eq!(
            strings(ssh_argv(&launch)),
            ["ssh", "-C", "-t", "deploy@prod"]
        );
    }

    #[test]
    fn ssh_argv_never_uses_a_generated_config_or_batch_mode() {
        let launch = SshLaunch::new("prod".into(), Some("/tmp/ctl".into())).unwrap();
        let argv = strings(ssh_argv(&launch));

        assert!(!argv.iter().any(|arg| arg == "-F"));
        assert!(!argv.iter().any(|arg| arg.contains("BatchMode")));
        assert_eq!(argv.last().map(String::as_str), Some("prod"));
    }

    #[test]
    fn ssh_host_validation_accepts_aliases_and_user_prefixed_aliases() {
        for host in ["prod", "web-1.example.com", "db_2", "deploy@prod", "a@b@c"] {
            assert!(is_valid_ssh_host(host), "{host}");
            assert!(ssh_destination_alias(host).is_some(), "{host}");
        }
        assert_eq!(ssh_destination_alias("deploy@prod"), Some("prod"));
        assert_eq!(ssh_destination_alias("prod"), Some("prod"));
        assert_eq!(ssh_destination_alias("a@b@c"), Some("c"));
    }

    #[test]
    fn ssh_host_validation_rejects_option_injection_and_metacharacters() {
        for host in [
            "",
            "-oProxyCommand=sh",
            "-p",
            "prod;id",
            "prod host",
            "prod\n",
            "%h",
            "$(id)",
            "*.corp",
            "!bastion",
            "@prod",
            "deploy@",
            "deploy@-oProxyCommand",
        ] {
            assert_eq!(ssh_destination_alias(host), None, "{host:?}");
            assert_eq!(SshLaunch::new(host.into(), None), None, "{host:?}");
        }
    }

    #[test]
    fn ssh_launch_canonical_is_the_alias_without_the_user() {
        let launch = SshLaunch::new("deploy@prod".into(), None).unwrap();

        assert_eq!(launch.host, "deploy@prod");
        assert_eq!(launch.canonical, "prod");
    }
}
