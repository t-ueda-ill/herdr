use std::ffi::OsString;
use std::path::Path;

use super::types::{ContainerInfo, ContainerLaunch, ContainerShell};
use super::workdir::resolve_workdir;

pub(crate) fn plan_launch(info: &ContainerInfo, host_cwd: &Path) -> ContainerLaunch {
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
    }
}

const BASH_OR_SH_PROBE: &str =
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
        let planned = plan_launch(&info(), Path::new("/host"));
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
        let planned = plan_launch(&labeled, Path::new("/host"));
        assert_eq!(planned.shell, ContainerShell::Explicit("/bin/zsh".into()));
        assert_eq!(planned.user.as_deref(), Some("dev"));
        assert_eq!(planned.workdir, Some("/srv".into()));
    }
}
