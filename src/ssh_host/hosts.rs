use std::ffi::OsStr;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use super::config::{list_host_aliases, FsConfigLoader, SshConfigLoader, SshHostInfo};
use super::launch::ssh_destination_alias;

#[derive(Debug)]
pub(crate) enum SshHostError {
    SshNotFound,
    NoHomeDirectory,
    ConfigUnreadable { path: PathBuf, source: io::Error },
}

impl fmt::Display for SshHostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SshNotFound => write!(f, "ssh executable not found on PATH"),
            Self::NoHomeDirectory => {
                write!(f, "no home directory to locate the ssh config in")
            }
            Self::ConfigUnreadable { path, source } => {
                write!(f, "could not read {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for SshHostError {}

/// Lists the hosts an ssh pane can open. Reads `~/.ssh/config` and its
/// includes, so it must stay off the app thread.
pub(crate) fn list_configured_hosts() -> Result<Vec<SshHostInfo>, SshHostError> {
    if find_on_path(std::env::var_os("PATH").as_deref(), "ssh").is_none() {
        return Err(SshHostError::SshNotFound);
    }
    let user_config = crate::platform::remote_ssh_config_paths()
        .user_config
        .ok_or(SshHostError::NoHomeDirectory)?;
    configured_hosts_in(&FsConfigLoader, &user_config)
}

/// Why not list the system `ssh_config`: its hosts are rare and shared by
/// every account, while the picker offers the user's own aliases.
pub(crate) fn configured_hosts_in(
    loader: &dyn SshConfigLoader,
    user_config: &Path,
) -> Result<Vec<SshHostInfo>, SshHostError> {
    let ssh_dir = user_config.parent().unwrap_or_else(|| Path::new("."));
    let hosts =
        list_host_aliases(loader, user_config, ssh_dir, ssh_dir.parent()).map_err(|source| {
            SshHostError::ConfigUnreadable {
                path: user_config.to_owned(),
                source,
            }
        })?;
    // An alias `ssh` cannot be handed safely (or one with `user@`, which a
    // `Host` pattern never matches) would only fail after the pane opened.
    Ok(hosts
        .into_iter()
        .filter(|host| ssh_destination_alias(&host.alias) == Some(host.alias.as_str()))
        .collect())
}

fn find_on_path(path_var: Option<&OsStr>, program: &str) -> Option<PathBuf> {
    std::env::split_paths(path_var?)
        .map(|dir| dir.join(program))
        .find(|candidate| crate::pane::is_executable_file(candidate))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct Files(HashMap<PathBuf, io::Result<String>>);

    impl SshConfigLoader for Files {
        fn read_to_string(&self, path: &Path) -> io::Result<String> {
            match self.0.get(path) {
                Some(Ok(text)) => Ok(text.clone()),
                Some(Err(err)) => Err(io::Error::new(err.kind(), err.to_string())),
                None => Err(io::ErrorKind::NotFound.into()),
            }
        }

        fn list_dir(&self, dir: &Path) -> io::Result<Vec<String>> {
            Ok(self
                .0
                .keys()
                .filter(|path| path.parent() == Some(dir))
                .filter_map(|path| Some(path.file_name()?.to_str()?.to_owned()))
                .collect())
        }
    }

    fn config(text: &str) -> Files {
        Files(HashMap::from([(
            PathBuf::from("/home/u/.ssh/config"),
            Ok(text.to_owned()),
        )]))
    }

    fn aliases(hosts: &[SshHostInfo]) -> Vec<&str> {
        hosts.iter().map(|host| host.alias.as_str()).collect()
    }

    #[test]
    fn configured_hosts_drop_aliases_ssh_could_misread() {
        let hosts = configured_hosts_in(
            &config("Host prod -oProxyCommand=sh deploy@prod \"two words\" ok.example_1\n"),
            Path::new("/home/u/.ssh/config"),
        )
        .unwrap();

        assert_eq!(aliases(&hosts), ["prod", "ok.example_1"]);
    }

    #[test]
    fn configured_hosts_follow_includes_relative_to_the_ssh_dir() {
        let files = Files(HashMap::from([
            (
                PathBuf::from("/home/u/.ssh/config"),
                Ok("Include conf.d/*\nHost main\n".to_owned()),
            ),
            (
                PathBuf::from("/home/u/.ssh/conf.d/work"),
                Ok("Host work\n  HostName work.example.com\n  User me\n".to_owned()),
            ),
        ]));

        let hosts = configured_hosts_in(&files, Path::new("/home/u/.ssh/config")).unwrap();

        assert_eq!(aliases(&hosts), ["work", "main"]);
        assert_eq!(hosts[0].hostname.as_deref(), Some("work.example.com"));
        assert_eq!(hosts[0].user.as_deref(), Some("me"));
    }

    #[test]
    fn configured_hosts_report_an_unreadable_config() {
        let files = Files(HashMap::from([(
            PathBuf::from("/home/u/.ssh/config"),
            Err(io::ErrorKind::PermissionDenied.into()),
        )]));

        let err = configured_hosts_in(&files, Path::new("/home/u/.ssh/config")).unwrap_err();

        assert!(matches!(
            err,
            SshHostError::ConfigUnreadable { ref path, .. } if path == Path::new("/home/u/.ssh/config")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn find_on_path_requires_an_executable_file() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("herdr-ssh-path-{}", std::process::id()));
        let plain = dir.join("plain");
        let bin = dir.join("bin");
        std::fs::create_dir_all(&plain).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(plain.join("ssh"), "").unwrap();
        std::fs::write(bin.join("ssh"), "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(bin.join("ssh"), std::fs::Permissions::from_mode(0o755)).unwrap();
        let path_var = std::env::join_paths([&plain, &bin]).unwrap();

        let found = find_on_path(Some(&path_var), "ssh");
        let missing = find_on_path(Some(&std::env::join_paths([&plain]).unwrap()), "ssh");
        std::fs::remove_dir_all(&dir).unwrap();

        assert_eq!(found, Some(bin.join("ssh")));
        assert_eq!(missing, None);
        assert_eq!(find_on_path(None, "ssh"), None);
    }
}
