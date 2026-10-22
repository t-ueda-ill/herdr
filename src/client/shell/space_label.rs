//! Spaces-row label for workspaces whose shell runs off-host.
//!
//! Why not bake this into the server's workspace name: `WorkspaceInfo.label`,
//! notifications and the rename editor would then show (and persist) the
//! prefix. It is presentation, so it is derived from the snapshot here.

use std::borrow::Cow;

use crate::protocol::{ClientShellPane, ClientShellSnapshot, ClientShellWorkspace};

/// Label the spaces rows show for `workspace`: the stored label for host
/// workspaces, a `[c]`/`[s]` form for container and ssh ones.
pub(super) fn space_display_label<'a>(
    snapshot: &'a ClientShellSnapshot,
    workspace: &'a ClientShellWorkspace,
) -> Cow<'a, str> {
    off_host_space_label(snapshot, workspace).map_or(Cow::Borrowed(&workspace.label), Cow::Owned)
}

/// `Some` only for off-host workspaces. Why not infer it from the `Cow`
/// variant of `space_display_label`: callers branch on "is off-host" and that
/// must not silently depend on which variant the helper happens to return.
pub(super) fn off_host_space_label(
    snapshot: &ClientShellSnapshot,
    workspace: &ClientShellWorkspace,
) -> Option<String> {
    off_host_label(workspace, representative_pane(snapshot, workspace)?)
}

/// Pane that stands for the workspace: the focused one of its active tab, else
/// the tab's first. Why not `pane.focused` alone: it is the global focus, so
/// other workspaces fall back to their active tab's first pane.
fn representative_pane<'a>(
    snapshot: &'a ClientShellSnapshot,
    workspace: &ClientShellWorkspace,
) -> Option<&'a ClientShellPane> {
    let mut panes = snapshot.panes.iter().filter(|pane| {
        pane.workspace_id == workspace.workspace_id && pane.tab_id == workspace.active_tab_id
    });
    let first = panes.next()?;
    if first.focused {
        return Some(first);
    }
    panes.clone().find(|pane| pane.focused).or(Some(first))
}

fn off_host_label(workspace: &ClientShellWorkspace, pane: &ClientShellPane) -> Option<String> {
    // Why not `pane.cwd`: for an off-host pane without OSC 7 it falls back to
    // the host launch directory, which would read as a remote path.
    let pwd = pane.foreground_cwd.as_deref();
    if let Some(container) = pane.container.as_deref() {
        let host = pane.ssh_host.as_deref();
        // The picker titles a Compose container by its Docker name
        // (`shop-web-1`) while `pane.container` is `project/service`.
        let stem = host
            .and_then(|host| workspace.label.strip_suffix(host)?.strip_suffix('@'))
            .unwrap_or(&workspace.label);
        let shown = if stem == container {
            Some(container)
        } else if is_compose_name(container, stem) {
            Some(stem)
        } else {
            None
        };
        let shown = shown.or((!workspace.custom_label).then_some(container));
        return Some(if let Some(shown) = shown {
            format!("[c] {}@{shown}{}", host.unwrap_or("local"), PwdSuffix(pwd))
        } else {
            format!("[c] {}", workspace.label)
        });
    }
    let alias = pane.ssh_host.as_deref()?;
    Some(if !workspace.custom_label || workspace.label == alias {
        format!("[s] {alias}{}", PwdSuffix(pwd))
    } else {
        format!("[s] {}", workspace.label)
    })
}

/// Docker Compose container name for canonical `project/service`:
/// `{project}-{service}-{n}` (v2) or `{project}_{service}_{n}` (v1).
fn is_compose_name(canonical: &str, name: &str) -> bool {
    let Some((project, service)) = canonical.split_once('/') else {
        return false;
    };
    ['-', '_'].into_iter().any(|sep| {
        name.strip_prefix(project)
            .and_then(|rest| rest.strip_prefix(sep))
            .and_then(|rest| rest.strip_prefix(service))
            .and_then(|rest| rest.strip_prefix(sep))
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    })
}

/// `:pwd`, or nothing; displayed in place so no intermediate String is built.
struct PwdSuffix<'a>(Option<&'a str>);

impl std::fmt::Display for PwdSuffix<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Some(pwd) => write!(f, ":{pwd}"),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::snapshot;
    use super::*;
    use crate::api::schema::AgentStatus;

    fn with_pane(
        label: &str,
        custom: bool,
        container: Option<&str>,
        ssh: Option<&str>,
        pwd: Option<&str>,
    ) -> ClientShellSnapshot {
        let mut snapshot = snapshot();
        snapshot.workspaces[0].label = label.into();
        snapshot.workspaces[0].custom_label = custom;
        let pane = &mut snapshot.panes[0];
        pane.container = container.map(str::to_owned);
        pane.ssh_host = ssh.map(str::to_owned);
        pane.foreground_cwd = pwd.map(str::to_owned);
        pane.cwd = Some("/host/launch".into());
        snapshot
    }

    fn label(snapshot: &ClientShellSnapshot) -> String {
        space_display_label(snapshot, &snapshot.workspaces[0]).into_owned()
    }

    #[test]
    fn local_container_shows_local_host_and_pwd() {
        let s = with_pane("tueda-aec", true, Some("tueda-aec"), None, Some("/work"));
        assert_eq!(label(&s), "[c] local@tueda-aec:/work");
    }

    #[test]
    fn container_without_reported_pwd_omits_the_path() {
        let s = with_pane("tueda-aec", true, Some("tueda-aec"), None, None);
        assert_eq!(label(&s), "[c] local@tueda-aec");
    }

    #[test]
    fn remote_container_shows_its_docker_host() {
        let s = with_pane(
            "web-1@prod",
            true,
            Some("web-1"),
            Some("prod"),
            Some("/srv"),
        );
        assert_eq!(label(&s), "[c] prod@web-1:/srv");
    }

    #[test]
    fn compose_container_from_the_picker_uses_the_generated_form() {
        let local = with_pane("shop-web-1", true, Some("shop/web"), None, Some("/app"));
        assert_eq!(label(&local), "[c] local@shop-web-1:/app");
        let remote = with_pane(
            "shop-web-1@prod",
            true,
            Some("shop/web"),
            Some("prod"),
            Some("/app"),
        );
        assert_eq!(label(&remote), "[c] prod@shop-web-1:/app");
        let v1 = with_pane("shop_web_2", true, Some("shop/web"), None, None);
        assert_eq!(label(&v1), "[c] local@shop_web_2");
    }

    #[test]
    fn renamed_compose_container_keeps_the_user_name() {
        let s = with_pane("shop-web-x", true, Some("shop/web"), None, Some("/app"));
        assert_eq!(label(&s), "[c] shop-web-x");
    }

    #[test]
    fn ssh_shows_alias_and_pwd() {
        let s = with_pane("dev_host", true, None, Some("dev_host"), Some("/home/u"));
        assert_eq!(label(&s), "[s] dev_host:/home/u");
    }

    #[test]
    fn renamed_container_and_ssh_keep_the_user_name_with_prefix() {
        let c = with_pane("my db", true, Some("db"), None, Some("/x"));
        assert_eq!(label(&c), "[c] my db");
        let s = with_pane("my box", true, None, Some("dev_host"), Some("/x"));
        assert_eq!(label(&s), "[s] my box");
    }

    #[test]
    fn automatic_name_off_host_uses_the_generated_form() {
        let s = with_pane("repo", false, None, Some("dev_host"), None);
        assert_eq!(label(&s), "[s] dev_host");
    }

    #[test]
    fn host_and_cold_restored_workspaces_are_unchanged() {
        let s = with_pane("client-shell", true, None, None, Some("/repo"));
        assert_eq!(label(&s), "client-shell");
    }

    #[test]
    fn workspace_without_panes_is_unchanged() {
        let mut s = with_pane("x", true, None, Some("h"), None);
        s.panes.clear();
        assert_eq!(label(&s), "x");
    }

    #[test]
    fn other_workspace_uses_its_active_tab_first_pane() {
        let mut s = with_pane("other", true, None, None, None);
        s.workspaces[0].workspace_id = "ws_2".into();
        s.workspaces[0].active_tab_id = "tab_2".into();
        let mut inactive = s.panes[0].clone();
        inactive.workspace_id = "ws_2".into();
        inactive.tab_id = "tab_1".into();
        inactive.ssh_host = Some("hidden".into());
        let mut active = s.panes[0].clone();
        active.workspace_id = "ws_2".into();
        active.tab_id = "tab_2".into();
        active.focused = false;
        active.ssh_host = Some("dev_host".into());
        s.panes = vec![inactive, active];
        assert_eq!(label(&s), "[s] other");
        s.workspaces[0].custom_label = false;
        assert_eq!(label(&s), "[s] dev_host");
    }

    #[test]
    fn focused_pane_that_is_not_first_decides_the_label() {
        let mut s = with_pane("repo", false, None, None, None);
        let mut second = s.panes[0].clone();
        second.focused = true;
        second.ssh_host = Some("dev_host".into());
        s.panes[0].focused = false;
        s.panes.push(second);
        assert_eq!(label(&s), "[s] dev_host");
    }

    #[test]
    fn indented_rows_show_branch_for_host_and_label_for_off_host() {
        let render = |s: &ClientShellSnapshot| {
            let mut ws = s.workspaces[0].clone();
            ws.custom_label = false;
            ws.branch = Some("worktree/feat".into());
            format!(
                "{:?}",
                crate::client::shell::sidebar::workspace_rows(
                    s,
                    &ws,
                    AgentStatus::Idle,
                    true,
                    &crate::config::SpacesSidebarConfig::default(),
                )
            )
        };
        let mut off = with_pane("repo", false, None, Some("dev_host"), None);
        off.workspaces[0].custom_label = false;
        assert!(render(&off).contains("[s] dev_host"));
        let host = with_pane("repo", false, None, None, None);
        let text = render(&host);
        assert!(text.contains("feat") && !text.contains("[s]"), "{text}");
    }

    #[test]
    fn sidebar_rows_carry_the_prefixed_label() {
        let s = with_pane("dev_host", true, None, Some("dev_host"), Some("/home/u"));
        let rows = crate::client::shell::sidebar::workspace_rows(
            &s,
            &s.workspaces[0],
            AgentStatus::Idle,
            false,
            &crate::config::SpacesSidebarConfig::default(),
        );
        let text = format!("{rows:?}");
        assert!(text.contains("[s] dev_host:/home/u"), "{text}");
    }
}
