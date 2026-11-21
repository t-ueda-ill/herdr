use serde::{Deserialize, Serialize};

use super::PaneLocationKind;

/// Where a pane's shell runs: the fields of `PaneLocation` plus one comparable id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Location {
    /// `local`, `ssh:<alias>`, `container:<ref>`, or `ssh:<alias>/container:<ref>`.
    pub id: String,
    pub kind: PaneLocationKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
}

impl Location {
    pub fn new(ssh_host: Option<&str>, container: Option<&str>) -> Self {
        let parts: Vec<String> = ssh_host
            .map(|host| format!("ssh:{host}"))
            .into_iter()
            .chain(container.map(|container| format!("container:{container}")))
            .collect();
        Self {
            id: if parts.is_empty() {
                "local".to_string()
            } else {
                parts.join("/")
            },
            kind: match (ssh_host, container) {
                (_, Some(_)) => PaneLocationKind::Container,
                (Some(_), None) => PaneLocationKind::Ssh,
                (None, None) => PaneLocationKind::Host,
            },
            ssh_host: ssh_host.map(str::to_string),
            container: container.map(str::to_string),
        }
    }

    /// Exact match, or `filter` names an enclosing location (`ssh:a` covers
    /// `ssh:a/container:b`).
    pub fn id_matches(id: &str, filter: &str) -> bool {
        // Why not `starts_with(filter)` alone: `ssh:devbox` must not cover `ssh:devbox2`.
        id.strip_prefix(filter)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with("/container:"))
    }
}

#[cfg(test)]
mod tests {
    use super::Location;
    use crate::api::schema::PaneLocationKind;

    fn covers(location: Location, filter: &str) -> bool {
        Location::id_matches(&location.id, filter)
    }

    #[test]
    fn a_pane_without_ssh_or_container_is_local() {
        let location = Location::new(None, None);
        assert_eq!(location.id, "local");
        assert_eq!(location.kind, PaneLocationKind::Host);
        assert_eq!(location.ssh_host, None);
        assert_eq!(location.container, None);
    }

    #[test]
    fn an_ssh_pane_is_named_by_its_alias() {
        let location = Location::new(Some("devbox"), None);
        assert_eq!(location.id, "ssh:devbox");
        assert_eq!(location.kind, PaneLocationKind::Ssh);
        assert_eq!(location.ssh_host.as_deref(), Some("devbox"));
    }

    #[test]
    fn a_local_container_is_named_by_its_reference() {
        let location = Location::new(None, Some("shop/web"));
        assert_eq!(location.id, "container:shop/web");
        assert_eq!(location.container.as_deref(), Some("shop/web"));
    }

    #[test]
    fn a_container_behind_ssh_nests_under_the_host() {
        let location = Location::new(Some("devbox"), Some("etl"));
        assert_eq!(location.id, "ssh:devbox/container:etl");
        assert_eq!(location.kind, PaneLocationKind::Container);
        assert_eq!(location.ssh_host.as_deref(), Some("devbox"));
        assert_eq!(location.container.as_deref(), Some("etl"));
    }

    #[test]
    fn a_host_filter_matches_the_host_and_its_containers() {
        let filter = "ssh:devbox";
        assert!(covers(Location::new(Some("devbox"), None), filter));
        assert!(covers(Location::new(Some("devbox"), Some("etl")), filter));
    }

    #[test]
    fn a_filter_does_not_match_a_host_that_only_shares_a_prefix() {
        assert!(!covers(Location::new(Some("devbox2"), None), "ssh:devbox"));
    }

    #[test]
    fn a_container_filter_does_not_match_the_host_itself() {
        assert!(!covers(
            Location::new(Some("devbox"), None),
            "ssh:devbox/container:etl"
        ));
    }

    #[test]
    fn local_filter_matches_only_local_panes() {
        assert!(covers(Location::new(None, None), "local"));
        assert!(!covers(Location::new(Some("devbox"), None), "local"));
    }
}
