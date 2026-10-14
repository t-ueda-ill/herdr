use std::path::{Path, PathBuf};

use super::types::{BindMount, ComposeInfo, ContainerInfo};

/// Maps a host directory to its container path through the bind mount whose
/// source is the deepest ancestor of it.
pub(crate) fn container_path_for_host(mounts: &[BindMount], host_cwd: &Path) -> Option<PathBuf> {
    mounts
        .iter()
        .filter(|mount| host_cwd.starts_with(&mount.source))
        .max_by_key(|mount| mount.source.components().count())
        .and_then(|mount| {
            let relative = host_cwd.strip_prefix(&mount.source).ok()?;
            // Why not join unconditionally: joining an empty path appends a
            // trailing slash, which leaks into `-w` and the session file.
            if relative.as_os_str().is_empty() {
                return Some(mount.destination.clone());
            }
            Some(mount.destination.join(relative))
        })
}

/// Order: `herdr.workdir` label, bind mount of the host cwd, image WorkingDir.
pub(crate) fn resolve_workdir(info: &ContainerInfo, host_cwd: &Path) -> Option<PathBuf> {
    info.herdr
        .workdir
        .clone()
        .or_else(|| container_path_for_host(&info.bind_mounts, host_cwd))
        .or_else(|| info.working_dir.clone())
}

pub(crate) fn compose_matches_repo_root(compose: &ComposeInfo, repo_root: &Path) -> bool {
    let config_dirs = compose.config_files.iter().filter_map(|file| file.parent());
    compose
        .working_dir
        .as_deref()
        .into_iter()
        .chain(config_dirs)
        .any(|dir| dir.starts_with(repo_root))
}

/// Compose containers living inside the repo, `herdr.default` ones first.
pub(crate) fn containers_for_repo_root<'a>(
    infos: &'a [ContainerInfo],
    repo_root: &Path,
) -> Vec<&'a ContainerInfo> {
    let mut matched: Vec<&ContainerInfo> = infos
        .iter()
        .filter(|info| {
            info.compose
                .as_ref()
                .is_some_and(|compose| compose_matches_repo_root(compose, repo_root))
        })
        .collect();
    matched.sort_by_key(|info| !info.herdr.default);
    matched
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::container::HerdrLabels;

    fn mount(source: &str, destination: &str) -> BindMount {
        BindMount {
            source: source.into(),
            destination: destination.into(),
        }
    }

    fn compose(working_dir: Option<&str>, config_files: &[&str]) -> ComposeInfo {
        ComposeInfo {
            project: "p".into(),
            service: "s".into(),
            working_dir: working_dir.map(Into::into),
            config_files: config_files.iter().map(Into::into).collect(),
        }
    }

    fn info(name: &str) -> ContainerInfo {
        ContainerInfo {
            id: name.into(),
            name: name.into(),
            running: true,
            compose: None,
            working_dir: None,
            user: None,
            bind_mounts: Vec::new(),
            herdr: HerdrLabels::default(),
        }
    }

    #[test]
    fn container_path_for_host_uses_longest_matching_bind_mount() {
        let mounts = [mount("/home/dev", "/all"), mount("/home/dev/app", "/work")];
        assert_eq!(
            container_path_for_host(&mounts, Path::new("/home/dev/app/src")),
            Some(PathBuf::from("/work/src"))
        );
        assert_eq!(
            container_path_for_host(&mounts, Path::new("/home/dev/app")),
            Some(PathBuf::from("/work"))
        );
        assert_eq!(
            container_path_for_host(&mounts, Path::new("/home/dev/other")),
            Some(PathBuf::from("/all/other"))
        );
    }

    #[test]
    fn container_path_for_host_maps_mount_root_to_destination_verbatim() {
        let mounts = [mount("/home/dev/app", "/work")];
        let mapped = container_path_for_host(&mounts, Path::new("/home/dev/app")).unwrap();
        // PathBuf equality ignores a trailing slash; the `-w` argument and the
        // session file carry the text itself.
        assert_eq!(mapped.as_os_str(), "/work");
    }

    #[test]
    fn container_path_for_host_returns_none_outside_mounts() {
        let mounts = [mount("/home/dev/app", "/work")];
        assert_eq!(container_path_for_host(&mounts, Path::new("/tmp")), None);
        assert_eq!(
            container_path_for_host(&mounts, Path::new("/home/dev/application")),
            None
        );
    }

    #[test]
    fn resolve_workdir_prefers_herdr_label_then_bind_then_working_dir() {
        let mut container = info("c");
        container.working_dir = Some("/image-default".into());
        let cwd = Path::new("/home/dev/app/src");
        assert_eq!(
            resolve_workdir(&container, cwd),
            Some(PathBuf::from("/image-default"))
        );

        container.bind_mounts = vec![mount("/home/dev/app", "/work")];
        assert_eq!(
            resolve_workdir(&container, cwd),
            Some(PathBuf::from("/work/src"))
        );

        container.herdr.workdir = Some("/srv".into());
        assert_eq!(
            resolve_workdir(&container, cwd),
            Some(PathBuf::from("/srv"))
        );

        assert_eq!(resolve_workdir(&info("bare"), cwd), None);
    }

    #[test]
    fn compose_matches_repo_root_by_working_dir_or_config_file() {
        let root = Path::new("/home/dev/src/shop");
        assert!(compose_matches_repo_root(
            &compose(Some("/home/dev/src/shop/db"), &[]),
            root
        ));
        assert!(compose_matches_repo_root(
            &compose(None, &["/home/dev/src/shop/compose.yml"]),
            root
        ));
        assert!(!compose_matches_repo_root(
            &compose(Some("/home/dev/src/shopping"), &["/elsewhere/compose.yml"]),
            root
        ));
        assert!(!compose_matches_repo_root(&compose(None, &[]), root));
    }

    #[test]
    fn containers_for_repo_root_puts_herdr_default_first() {
        let root = Path::new("/home/dev/src/shop");
        let mut first = info("a");
        first.compose = Some(compose(Some("/home/dev/src/shop"), &[]));
        let mut second = info("b");
        second.compose = Some(compose(Some("/home/dev/src/shop/db"), &[]));
        second.herdr.default = true;
        let mut unrelated = info("c");
        unrelated.compose = Some(compose(Some("/other"), &[]));
        let plain = info("d");

        let infos = [first, second, unrelated, plain];
        let names: Vec<_> = containers_for_repo_root(&infos, root)
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(names, vec!["b", "a"]);
    }
}
