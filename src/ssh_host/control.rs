use std::io;
use std::path::{Path, PathBuf};

use super::SshLaunch;

/// The `ControlPath` an ssh pane to `host` multiplexes through, or `None`
/// when this platform's OpenSSH cannot multiplex or the path is unsafe.
///
/// Why not `create_remote_ssh_config_dir`: it mints a fresh per-process
/// directory removed by a drop guard, while a pane has no owner object and its
/// respawns and later file transfers must reach the same master.
pub(crate) fn pane_control_path(host: &str, config: Option<&Path>) -> Option<PathBuf> {
    control_path_with(
        crate::platform::remote_ssh_config_paths().multiplexing,
        || crate::platform::shared_ssh_control_path(&control_namespace(config), host),
    )
}

/// Why not the `herdr --remote` namespace: herdr stops that master when it
/// retires a remote connection, which must not reach a pane's login.
/// Why `config` joins the namespace: `%C` covers only the resolved host,
/// port, and user, so two configs that differ in `IdentityFile` or
/// `ProxyCommand` would otherwise share one master and its credentials.
/// Without a config the namespace stays as before, so saved panes keep theirs.
fn control_namespace(config: Option<&Path>) -> PathBuf {
    let base = crate::config::config_dir().join("ssh-panes");
    match config {
        None => base,
        Some(config) => {
            let mut namespace = base.into_os_string();
            namespace.push("#");
            namespace.push(config);
            PathBuf::from(namespace)
        }
    }
}

pub(crate) fn control_path_with(
    multiplexing: bool,
    control_path: impl FnOnce() -> io::Result<PathBuf>,
) -> Option<PathBuf> {
    if !multiplexing {
        return None;
    }
    match control_path() {
        Ok(path) => Some(path),
        Err(err) => {
            // The pane still opens; only multiplexed side channels are lost.
            tracing::warn!(%err, "ssh pane control path unavailable; connecting without multiplexing");
            None
        }
    }
}

/// Recreates the directory of `launch`'s control path if needed, and returns
/// the launch to spawn now: the control path is kept once its directory exists,
/// and dropped when that directory cannot be made safe.
///
/// Why not reuse the saved path as is: a `/tmp` cleaner or a reboot may have
/// removed the directory since the pane opened, and OpenSSH then fails to bind
/// the master socket and exits 255 on every reconnect.
pub(crate) fn prepare_launch(launch: &SshLaunch) -> SshLaunch {
    let mut ready = launch.clone();
    ready.control_path = prepare_control_path(launch.control_path.as_deref());
    ready
}

/// [`prepare_launch`] for the ssh host a container runs on.
pub(crate) fn prepare_container_launch(
    launch: &crate::container::ContainerLaunch,
) -> crate::container::ContainerLaunch {
    let mut ready = launch.clone();
    ready.ssh = launch.ssh.as_ref().map(prepare_launch);
    ready
}

/// [`prepare_launch`] for a bare control path.
pub(crate) fn prepare_control_path(control_path: Option<&Path>) -> Option<PathBuf> {
    prepare_control_path_with(control_path, crate::platform::ensure_shared_ssh_control_dir)
}

fn prepare_control_path_with(
    control_path: Option<&Path>,
    ensure_dir: impl FnOnce(&Path) -> io::Result<()>,
) -> Option<PathBuf> {
    let control_path = control_path?;
    match ensure_dir(control_path) {
        Ok(()) => Some(control_path.to_owned()),
        Err(err) => {
            tracing::warn!(%err, "ssh control directory unavailable; connecting without multiplexing");
            None
        }
    }
}

/// A control path the spawn paths keep, for tests that run a fake `ssh`.
#[cfg(all(test, unix))]
pub(crate) fn test_control_path(host: &str) -> PathBuf {
    pane_control_path(host, None).expect("this platform multiplexes ssh")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_session_ssh_config_gets_its_own_control_namespace() {
        let default = control_namespace(None);
        let prod = control_namespace(Some(Path::new("/cfg/prod")));

        assert_eq!(default, crate::config::config_dir().join("ssh-panes"));
        assert_ne!(prod, default);
        assert_ne!(prod, control_namespace(Some(Path::new("/cfg/dev"))));
        assert_eq!(prod, control_namespace(Some(Path::new("/cfg/prod"))));
    }

    #[test]
    fn control_path_is_absent_without_multiplexing() {
        let path = control_path_with(false, || -> io::Result<PathBuf> {
            panic!("no control path may be computed without multiplexing")
        });

        assert_eq!(path, None);
    }

    #[test]
    fn control_path_uses_the_computed_template_when_multiplexing() {
        let path = control_path_with(true, || Ok(PathBuf::from("/tmp/hssh-1/abc-%C")));

        assert_eq!(path, Some(PathBuf::from("/tmp/hssh-1/abc-%C")));
    }

    #[test]
    fn control_path_falls_back_to_a_plain_session_when_unsafe() {
        let path = control_path_with(true, || {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "unsafe SSH control directory parent",
            ))
        });

        assert_eq!(path, None);
    }

    #[cfg(unix)]
    #[test]
    fn pane_control_path_is_stable_per_host_and_distinct_from_remote_attach() {
        let first = pane_control_path("prod", None).expect("unix multiplexes");

        assert_eq!(pane_control_path("prod", None), Some(first.clone()));
        assert_ne!(pane_control_path("staging", None), Some(first.clone()));
        assert_ne!(
            crate::platform::shared_ssh_control_path(&crate::config::config_path(), "prod").ok(),
            Some(first.clone())
        );
        assert!(first.to_string_lossy().ends_with("-%C"));
    }

    #[test]
    fn a_control_path_whose_directory_is_ready_is_kept() {
        let path = prepare_control_path_with(Some(Path::new("/tmp/hssh-1/abc-%C")), |_| Ok(()));

        assert_eq!(path, Some(PathBuf::from("/tmp/hssh-1/abc-%C")));
    }

    #[test]
    fn a_control_path_whose_directory_cannot_be_made_safe_is_dropped() {
        let path = prepare_control_path_with(Some(Path::new("/tmp/hssh-1/abc-%C")), |_| {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "SSH control directory must be owned by the current user",
            ))
        });

        assert_eq!(path, None);
    }

    #[test]
    fn no_control_path_needs_no_directory() {
        let path = prepare_control_path_with(None, |_| -> io::Result<()> {
            panic!("no directory may be prepared without a control path")
        });

        assert_eq!(path, None);
    }

    #[cfg(unix)]
    #[test]
    fn a_pane_launch_keeps_the_control_path_herdr_computed_for_it() {
        let launch = SshLaunch::new("prod".into(), pane_control_path("prod", None)).unwrap();

        assert_eq!(prepare_launch(&launch), launch);
    }

    #[test]
    fn a_launch_saved_with_a_foreign_control_path_connects_without_multiplexing() {
        let launch =
            SshLaunch::new("prod".into(), Some("/nonexistent/hssh-1/abc-%C".into())).unwrap();

        assert_eq!(prepare_launch(&launch).control_path, None);
    }
}
