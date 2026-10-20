use std::ffi::OsString;
use std::path::Path;

use super::types::{ContainerInfo, ContainerLaunch, ContainerShell};
use super::workdir::resolve_workdir;
use crate::ssh_host::launch::{shell_quote, ssh_command_argv};

/// `host_cwd` is `None` when docker runs on another machine, see `resolve_workdir`.
/// The caller sets `ssh` afterwards: this only reads container facts.
pub(crate) fn plan_launch(info: &ContainerInfo, host_cwd: Option<&Path>) -> ContainerLaunch {
    let reference = info.reference();
    ContainerLaunch {
        canonical: reference.canonical(),
        reference,
        id: info.id.clone(),
        workdir: resolve_workdir(info, host_cwd),
        user: info.herdr.user.clone().or_else(|| info.user.clone()),
        shell: match &info.herdr.shell {
            Some(shell) => ContainerShell::Explicit(shell.clone()),
            None => ContainerShell::BashOrSh,
        },
        ssh: None,
    }
}

pub(crate) const BASH_OR_SH_PROBE: &str =
    "if command -v bash >/dev/null 2>&1; then exec bash \"$@\"; else exec sh \"$@\"; fi";

/// `-it` is required: `-i` wires the pane PTY to stdin and `-t` gives the
/// container side a TTY. Env goes in as `-e` because the docker CLI process
/// environment never reaches the exec'd shell.
pub(crate) fn docker_exec_argv(
    launch: &ContainerLaunch,
    login: bool,
    env: &[(String, String)],
) -> Vec<OsString> {
    let mut argv: Vec<OsString> = vec!["docker".into(), "exec".into(), "-it".into()];
    if let Some(workdir) = &launch.workdir {
        argv.push("-w".into());
        argv.push(workdir.as_os_str().to_owned());
    }
    if let Some(user) = &launch.user {
        argv.push("-u".into());
        argv.push(user.into());
    }
    for (key, value) in env {
        argv.push("-e".into());
        argv.push(format!("{key}={value}").into());
    }
    argv.push((&launch.id).into());
    match &launch.shell {
        ContainerShell::Explicit(shell) => argv.push(shell.into()),
        ContainerShell::BashOrSh => {
            argv.extend([
                "sh".into(),
                "-c".into(),
                BASH_OR_SH_PROBE.into(),
                "sh".into(),
            ]);
        }
    }
    if login {
        argv.push("-l".into());
    }
    argv
}

/// The argv of the pane process: `docker exec ...` locally, or `ssh -t <host>`
/// followed by one command string when the container's docker is remote.
///
/// Why one pre-quoted string: ssh joins its trailing arguments with spaces and
/// hands them to the remote login shell, which would re-split `-c` scripts and
/// env values. Every element is quoted because `-e KEY=VALUE` is arbitrary.
pub(crate) fn container_shell_argv(
    launch: &ContainerLaunch,
    login: bool,
    env: &[(String, String)],
) -> Vec<OsString> {
    let docker = docker_exec_argv(launch, login, env);
    let Some(ssh) = &launch.ssh else {
        return docker;
    };
    let command = docker
        .iter()
        .map(|arg| shell_quote(&arg.to_string_lossy()))
        .collect::<Vec<_>>()
        .join(" ");
    ssh_command_argv(ssh, command.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::container::{ContainerRef, HerdrLabels};

    fn launch(shell: ContainerShell) -> ContainerLaunch {
        ContainerLaunch {
            reference: ContainerRef::Name("scratch".into()),
            canonical: "scratch".into(),
            id: "cid".into(),
            workdir: Some("/work".into()),
            user: Some("dev".into()),
            shell,
            ssh: None,
        }
    }

    fn strings(argv: Vec<OsString>) -> Vec<String> {
        argv.into_iter().map(|a| a.into_string().unwrap()).collect()
    }

    const PROBE: &str =
        "if command -v bash >/dev/null 2>&1; then exec bash \"$@\"; else exec sh \"$@\"; fi";

    #[test]
    fn docker_exec_argv_wraps_bash_or_sh_probe_with_login_flag() {
        let argv = strings(docker_exec_argv(
            &launch(ContainerShell::BashOrSh),
            true,
            &[],
        ));
        assert_eq!(
            argv,
            vec![
                "docker", "exec", "-it", "-w", "/work", "-u", "dev", "cid", "sh", "-c", PROBE,
                "sh", "-l"
            ]
        );
        let argv = strings(docker_exec_argv(
            &launch(ContainerShell::BashOrSh),
            false,
            &[],
        ));
        assert_eq!(argv.last().map(String::as_str), Some("sh"));
    }

    #[test]
    fn docker_exec_argv_uses_explicit_shell_label_without_probe() {
        let shell = ContainerShell::Explicit("/bin/zsh".into());
        let argv = strings(docker_exec_argv(&launch(shell.clone()), false, &[]));
        assert_eq!(argv[argv.len() - 2..], ["cid", "/bin/zsh"]);
        assert!(!argv.iter().any(|a| a == "-c"));
        let argv = strings(docker_exec_argv(&launch(shell), true, &[]));
        assert_eq!(argv[argv.len() - 3..], ["cid", "/bin/zsh", "-l"]);
    }

    #[test]
    fn docker_exec_argv_places_env_and_workdir_before_container_id() {
        let env = [
            ("TERM".to_string(), "xterm-256color".to_string()),
            ("HERDR_PANE_ID".to_string(), "p1".to_string()),
        ];
        let argv = strings(docker_exec_argv(
            &launch(ContainerShell::BashOrSh),
            false,
            &env,
        ));
        let id = argv.iter().position(|a| a == "cid").unwrap();
        let before = &argv[..id];
        for expected in [
            "-w",
            "/work",
            "-e",
            "TERM=xterm-256color",
            "HERDR_PANE_ID=p1",
        ] {
            assert!(before.iter().any(|a| a == expected), "{expected}");
        }
        assert_eq!(&argv[..3], ["docker", "exec", "-it"]);
    }

    #[test]
    fn docker_exec_argv_omits_user_and_workdir_when_unknown() {
        let mut bare = launch(ContainerShell::BashOrSh);
        bare.workdir = None;
        bare.user = None;
        let argv = strings(docker_exec_argv(&bare, false, &[]));
        assert_eq!(&argv[..4], ["docker", "exec", "-it", "cid"]);
    }

    fn info() -> ContainerInfo {
        ContainerInfo {
            id: "cid".into(),
            name: "scratch".into(),
            running: true,
            compose: None,
            working_dir: Some("/image".into()),
            user: Some("1000".into()),
            bind_mounts: Vec::new(),
            herdr: HerdrLabels::default(),
        }
    }

    #[test]
    fn plan_launch_resolves_defaults_from_container_config() {
        let planned = plan_launch(&info(), Some(Path::new("/host")));
        assert_eq!(planned.id, "cid");
        assert_eq!(planned.canonical, "scratch");
        assert_eq!(planned.reference, ContainerRef::Name("scratch".into()));
        assert_eq!(planned.workdir, Some("/image".into()));
        assert_eq!(planned.user.as_deref(), Some("1000"));
        assert_eq!(planned.shell, ContainerShell::BashOrSh);
    }

    #[test]
    fn plan_launch_prefers_herdr_labels_over_container_config() {
        let mut labeled = info();
        labeled.herdr.shell = Some("/bin/zsh".into());
        labeled.herdr.user = Some("dev".into());
        labeled.herdr.workdir = Some("/srv".into());
        let planned = plan_launch(&labeled, Some(Path::new("/host")));
        assert_eq!(planned.shell, ContainerShell::Explicit("/bin/zsh".into()));
        assert_eq!(planned.user.as_deref(), Some("dev"));
        assert_eq!(planned.workdir, Some("/srv".into()));
    }

    #[test]
    fn plan_launch_leaves_ssh_unset_and_ignores_bind_mounts_without_a_host_cwd() {
        let mut mounted = info();
        mounted.bind_mounts = vec![crate::container::types::BindMount {
            source: "/host".into(),
            destination: "/mnt".into(),
        }];
        let local = plan_launch(&mounted, Some(Path::new("/host/src")));
        assert_eq!(local.workdir, Some("/mnt/src".into()));
        assert_eq!(local.ssh, None);

        let remote = plan_launch(&mounted, None);
        assert_eq!(remote.workdir, Some("/image".into()));
    }

    fn remote(shell: ContainerShell) -> ContainerLaunch {
        ContainerLaunch {
            ssh: crate::ssh_host::SshLaunch::new("deploy@prod".into(), Some("/tmp/ctl-%C".into())),
            ..launch(shell)
        }
    }

    #[test]
    fn container_shell_argv_is_plain_docker_for_a_local_container() {
        let local = launch(ContainerShell::BashOrSh);
        assert_eq!(
            container_shell_argv(&local, true, &[]),
            docker_exec_argv(&local, true, &[])
        );
    }

    #[test]
    fn container_shell_argv_runs_the_quoted_docker_command_over_ssh() {
        let argv = strings(container_shell_argv(
            &remote(ContainerShell::BashOrSh),
            true,
            &[("TERM".into(), "xterm-256color".into())],
        ));
        assert_eq!(
            &argv[..12],
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
                "deploy@prod"
            ]
        );
        assert_eq!(argv.len(), 13, "docker travels as one ssh argument");
        assert_eq!(
            argv[12],
            format!(
                "'docker' 'exec' '-it' '-w' '/work' '-u' 'dev' '-e' 'TERM=xterm-256color' \
                 'cid' 'sh' '-c' '{PROBE}' 'sh' '-l'"
            )
        );
    }

    #[test]
    fn remote_container_argv_overrides_remote_command_but_local_docker_has_none() {
        let remote_argv = strings(container_shell_argv(
            &remote(ContainerShell::BashOrSh),
            true,
            &[],
        ));
        let override_at = remote_argv
            .iter()
            .position(|a| a == "RemoteCommand=none")
            .expect("remote argv overrides RemoteCommand");
        let tty_at = remote_argv.iter().position(|a| a == "-t").unwrap();
        assert_eq!(remote_argv[override_at - 1], "-o");
        assert!(override_at < tty_at);

        let local_argv = strings(container_shell_argv(
            &launch(ContainerShell::BashOrSh),
            true,
            &[],
        ));
        assert!(local_argv.iter().all(|a| !a.contains("RemoteCommand")));
    }

    #[test]
    fn container_shell_argv_quotes_env_values_the_remote_shell_would_expand() {
        let env = [("MSG".to_string(), "it's $HOME; id".to_string())];
        let argv = strings(container_shell_argv(
            &remote(ContainerShell::Explicit("/bin/zsh".into())),
            false,
            &env,
        ));
        let command = argv.last().unwrap();
        assert!(command.contains(r"'MSG=it'\''s $HOME; id'"), "{command}");
        assert!(command.ends_with("'cid' '/bin/zsh'"), "{command}");
    }
}
