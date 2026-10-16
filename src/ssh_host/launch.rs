use std::ffi::OsString;
use std::path::PathBuf;

/// Fully resolved ssh pane target, kept so respawns never re-read the config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SshLaunch {
    /// Destination handed to `ssh`: a config alias, optionally prefixed with `user@`.
    pub(crate) host: String,
    /// The config alias alone. Computed once so metadata paths only clone it.
    pub(crate) canonical: String,
    /// Herdr-owned `ControlPath` template; `None` when multiplexing is unavailable.
    pub(crate) control_path: Option<PathBuf>,
}

impl SshLaunch {
    /// `None` for a destination `ssh_destination_alias` rejects.
    pub(crate) fn new(host: String, control_path: Option<PathBuf>) -> Option<Self> {
        let canonical = ssh_destination_alias(&host)?.to_owned();
        Some(Self {
            host,
            canonical,
            control_path,
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(argv: Vec<OsString>) -> Vec<String> {
        argv.into_iter()
            .map(|arg| arg.into_string().unwrap())
            .collect()
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
