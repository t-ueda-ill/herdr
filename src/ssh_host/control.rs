use std::io;
use std::path::PathBuf;

/// The `ControlPath` an ssh pane to `host` multiplexes through, or `None`
/// when this platform's OpenSSH cannot multiplex or the path is unsafe.
///
/// Why not `create_remote_ssh_config_dir`: it mints a fresh per-process
/// directory removed by a drop guard, while a pane has no owner object and its
/// respawns and later file transfers must reach the same master.
pub(crate) fn pane_control_path(host: &str) -> Option<PathBuf> {
    control_path_with(
        crate::platform::remote_ssh_config_paths().multiplexing,
        || crate::platform::shared_ssh_control_path(&control_namespace(), host),
    )
}

/// Why not the `herdr --remote` namespace: herdr stops that master when it
/// retires a remote connection, which must not reach a pane's login.
fn control_namespace() -> PathBuf {
    crate::config::config_dir().join("ssh-panes")
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

#[cfg(test)]
mod tests {
    use super::*;

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
        let first = pane_control_path("prod").expect("unix multiplexes");

        assert_eq!(pane_control_path("prod"), Some(first.clone()));
        assert_ne!(pane_control_path("staging"), Some(first.clone()));
        assert_ne!(
            crate::platform::shared_ssh_control_path(&crate::config::config_path(), "prod").ok(),
            Some(first.clone())
        );
        assert!(first.to_string_lossy().ends_with("-%C"));
    }
}
