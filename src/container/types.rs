use std::fmt;
use std::path::PathBuf;

/// User-supplied and persisted container identifier: `project/service` for
/// compose containers, a bare name (or id prefix) otherwise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ContainerRef {
    Compose { project: String, service: String },
    Name(String),
}

impl ContainerRef {
    pub(crate) fn parse(text: &str) -> Option<Self> {
        if text.is_empty() || text.chars().any(char::is_control) {
            return None;
        }
        match text.split_once('/') {
            None => Some(Self::Name(text.to_string())),
            Some((project, service))
                if !project.is_empty() && !service.is_empty() && !service.contains('/') =>
            {
                Some(Self::Compose {
                    project: project.to_string(),
                    service: service.to_string(),
                })
            }
            Some(_) => None,
        }
    }

    pub(crate) fn canonical(&self) -> String {
        match self {
            Self::Compose { project, service } => format!("{project}/{service}"),
            Self::Name(name) => name.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ComposeInfo {
    pub(crate) project: String,
    pub(crate) service: String,
    pub(crate) working_dir: Option<PathBuf>,
    pub(crate) config_files: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BindMount {
    pub(crate) source: PathBuf,
    pub(crate) destination: PathBuf,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct HerdrLabels {
    pub(crate) shell: Option<String>,
    pub(crate) workdir: Option<PathBuf>,
    pub(crate) user: Option<String>,
    pub(crate) default: bool,
}

/// One normalized `docker inspect` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContainerInfo {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) running: bool,
    pub(crate) compose: Option<ComposeInfo>,
    pub(crate) working_dir: Option<PathBuf>,
    pub(crate) user: Option<String>,
    pub(crate) bind_mounts: Vec<BindMount>,
    pub(crate) herdr: HerdrLabels,
}

impl ContainerInfo {
    pub(crate) fn reference(&self) -> ContainerRef {
        match &self.compose {
            Some(compose) => ContainerRef::Compose {
                project: compose.project.clone(),
                service: compose.service.clone(),
            },
            None => ContainerRef::Name(self.name.clone()),
        }
    }

    // Why not compiled into the server: container.open resolves references
    // through docker's own lookup, so nothing outside tests compares them yet.
    #[cfg(test)]
    pub(crate) fn matches(&self, reference: &ContainerRef) -> bool {
        match reference {
            ContainerRef::Compose { project, service } => self
                .compose
                .as_ref()
                .is_some_and(|c| &c.project == project && &c.service == service),
            ContainerRef::Name(name) => &self.name == name || self.id.starts_with(name.as_str()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ContainerShell {
    Explicit(String),
    /// Probed inside the container: bash when present, sh otherwise.
    BashOrSh,
}

/// Fully resolved launch target, kept so respawns never re-query docker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContainerLaunch {
    pub(crate) reference: ContainerRef,
    /// Computed once at resolve time so metadata paths only clone it.
    pub(crate) canonical: String,
    pub(crate) id: String,
    pub(crate) workdir: Option<PathBuf>,
    pub(crate) user: Option<String>,
    pub(crate) shell: ContainerShell,
    /// The host whose docker CLI runs the shell; `None` for the local docker.
    /// Why not `TerminalState.ssh`: the pane process is the local ssh client
    /// running `docker exec`, and a pane never carries both launch kinds.
    pub(crate) ssh: Option<crate::ssh_host::SshLaunch>,
}

#[derive(Debug)]
pub(crate) enum ContainerListError {
    DockerNotFound,
    /// Remote docker is reached through the local `ssh` client.
    SshNotFound,
    /// Batch-mode ssh could not log in without a prompt.
    SshLoginRequired {
        host: String,
    },
    Timeout,
    Failed {
        status: Option<i32>,
        stderr: String,
    },
    InvalidOutput(String),
}

impl fmt::Display for ContainerListError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DockerNotFound => write!(f, "docker executable not found"),
            Self::SshNotFound => write!(f, "ssh executable not found"),
            Self::SshLoginRequired { host } => write!(
                f,
                "cannot reach {host} without a prompt; open an ssh pane to {host} first, then retry"
            ),
            Self::Timeout => write!(f, "docker did not respond in time"),
            Self::Failed { status, stderr } => match status {
                Some(code) => write!(f, "docker exited with status {code}: {stderr}"),
                None => write!(f, "docker was terminated: {stderr}"),
            },
            Self::InvalidOutput(detail) => write!(f, "unexpected docker output: {detail}"),
        }
    }
}

impl std::error::Error for ContainerListError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_ref_parses_project_slash_service_and_plain_name() {
        assert_eq!(
            ContainerRef::parse("shop/db"),
            Some(ContainerRef::Compose {
                project: "shop".into(),
                service: "db".into()
            })
        );
        assert_eq!(
            ContainerRef::parse("scratch"),
            Some(ContainerRef::Name("scratch".into()))
        );
    }

    #[test]
    fn container_ref_rejects_empty_and_control_characters() {
        for bad in ["", "a\0b", "a\nb", "/db", "shop/", "a/b/c", "\t"] {
            assert_eq!(ContainerRef::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn container_ref_canonical_round_trips() {
        for text in ["shop/db", "scratch"] {
            let parsed = ContainerRef::parse(text).unwrap();
            assert_eq!(parsed.canonical(), text);
            assert_eq!(ContainerRef::parse(&parsed.canonical()), Some(parsed));
        }
    }

    fn info(name: &str, id: &str, compose: Option<(&str, &str)>) -> ContainerInfo {
        ContainerInfo {
            id: id.into(),
            name: name.into(),
            running: true,
            compose: compose.map(|(p, s)| ComposeInfo {
                project: p.into(),
                service: s.into(),
                working_dir: None,
                config_files: Vec::new(),
            }),
            working_dir: None,
            user: None,
            bind_mounts: Vec::new(),
            herdr: HerdrLabels::default(),
        }
    }

    #[test]
    fn container_info_reference_prefers_compose_identity() {
        assert_eq!(
            info("shop-db-1", "abc", Some(("shop", "db"))).reference(),
            ContainerRef::Compose {
                project: "shop".into(),
                service: "db".into()
            }
        );
        assert_eq!(
            info("scratch", "abc", None).reference(),
            ContainerRef::Name("scratch".into())
        );
    }

    #[test]
    fn container_info_matches_name_id_prefix_and_compose_pair() {
        let compose = info("shop-db-1", "abcdef", Some(("shop", "db")));
        assert!(compose.matches(&ContainerRef::parse("shop/db").unwrap()));
        assert!(!compose.matches(&ContainerRef::parse("shop/web").unwrap()));
        assert!(compose.matches(&ContainerRef::parse("shop-db-1").unwrap()));
        assert!(compose.matches(&ContainerRef::parse("abc").unwrap()));
        assert!(!compose.matches(&ContainerRef::parse("zzz").unwrap()));
        assert!(!info("scratch", "abc", None).matches(&ContainerRef::parse("shop/db").unwrap()));
    }
}
