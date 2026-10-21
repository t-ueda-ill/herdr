use crate::api::schema::{
    ContainerOpenTarget, Method, PaneSplitParams, TabCreateInheritParams, TabCreateParams,
};
use crate::app::App;
use crate::container::{ContainerLaunch, ContainerRef};
use crate::layout::PaneId;
use crate::pane::PaneLaunchTarget;
use crate::server::client_shell::{running_pane_off_host_shell, RunningOffHostShell};
use crate::ssh_host::SshLaunch;

use super::responses::encode_error;

/// An owned copy of a live off-host launch.
///
/// Why not borrow it: the create handlers take `&mut self`, which a borrow of
/// the source terminal's state would outlive.
enum InheritedLaunch {
    Container(ContainerLaunch),
    Ssh(SshLaunch),
}

impl InheritedLaunch {
    fn target(&self) -> PaneLaunchTarget<'_> {
        match self {
            Self::Container(launch) => PaneLaunchTarget::Container(launch),
            Self::Ssh(launch) => PaneLaunchTarget::Ssh(launch),
        }
    }
}

impl From<RunningOffHostShell<'_>> for InheritedLaunch {
    fn from(running: RunningOffHostShell<'_>) -> Self {
        match running {
            RunningOffHostShell::Container(launch) => Self::Container(launch.clone()),
            RunningOffHostShell::Ssh(launch) => Self::Ssh(launch.clone()),
        }
    }
}

impl App {
    /// Reuses the stored launch instead of resolving docker or the ssh config
    /// again: this runs on the app thread. Why not the stored metadata alone
    /// to decide: a cold-restored pane keeps it while running a host shell.
    fn inherited_launch(&self, ws_idx: usize, pane_id: PaneId) -> Option<InheritedLaunch> {
        running_pane_off_host_shell(self, ws_idx, pane_id).map(InheritedLaunch::from)
    }

    /// The pane a new tab inherits from, pinning the workspace when the
    /// request names the source pane.
    fn tab_inherit_source(
        &self,
        create: &mut TabCreateParams,
        source_pane_id: Option<&str>,
    ) -> Result<Option<(usize, PaneId)>, (String, String)> {
        match source_pane_id {
            Some(source_pane_id) => {
                let Some((ws_idx, pane_id)) = self.parse_pane_id(source_pane_id) else {
                    return Err(("pane_not_found".into(), "pane not found".into()));
                };
                if create.workspace_id.is_none() {
                    create.workspace_id = Some(self.public_workspace_id(ws_idx));
                }
                Ok(Some((ws_idx, pane_id)))
            }
            None => {
                let (ws_idx, _) = self.tab_create_location(create.workspace_id.as_deref(), None)?;
                Ok(self
                    .state
                    .workspaces
                    .get(ws_idx)
                    .and_then(|ws| Some((ws_idx, ws.focused_pane_id()?))))
            }
        }
    }

    /// A local container source with an explicit host `cwd` needs its workdir
    /// planned again through the container's bind mounts, like `container.open`.
    /// Why not do it inline: that needs `docker inspect`, which must stay off
    /// the app thread, so the caller runs the returned open request instead.
    /// Why not a remote container: its bind mounts are another machine's
    /// paths, so the stored workdir stays.
    pub(crate) fn container_reopen_for_inherit(
        &self,
        method: &Method,
    ) -> Option<(ContainerRef, ContainerOpenTarget)> {
        let (ws_idx, pane_id, target) = match method {
            Method::TabCreateInherit(params) => {
                params.create.cwd.as_ref()?;
                let mut create = params.create.clone();
                let (ws_idx, pane_id) = self
                    .tab_inherit_source(&mut create, params.source_pane_id.as_deref())
                    .ok()??;
                (ws_idx, pane_id, ContainerOpenTarget::Tab(create))
            }
            Method::PaneSplitInherit(params) => {
                params.cwd.as_ref()?;
                let (ws_idx, pane_id, _) = self.pane_split_location(params).ok()?;
                (ws_idx, pane_id, ContainerOpenTarget::Split(params.clone()))
            }
            _ => return None,
        };
        match self.inherited_launch(ws_idx, pane_id)? {
            InheritedLaunch::Container(launch) if launch.ssh.is_none() => {
                Some((launch.reference, target))
            }
            _ => None,
        }
    }

    pub(in crate::app) fn handle_tab_create_inherit(
        &mut self,
        id: String,
        params: TabCreateInheritParams,
    ) -> String {
        let TabCreateInheritParams {
            mut create,
            source_pane_id,
        } = params;
        let source = match self.tab_inherit_source(&mut create, source_pane_id.as_deref()) {
            Ok(source) => source,
            Err((code, message)) => return encode_error(id, &code, message),
        };
        let inherited = source.and_then(|(ws_idx, pane_id)| self.inherited_launch(ws_idx, pane_id));
        match inherited {
            Some(launch) => self.handle_tab_create(id, create, launch.target()),
            None => self.handle_tab_create(id, create, PaneLaunchTarget::Host),
        }
    }

    pub(in crate::app) fn handle_pane_split_inherit(
        &mut self,
        id: String,
        params: PaneSplitParams,
    ) -> String {
        let (ws_idx, pane_id, _) = match self.pane_split_location(&params) {
            Ok(location) => location,
            Err((code, message)) => return encode_error(id, &code, message),
        };
        match self.inherited_launch(ws_idx, pane_id) {
            Some(launch) => self.handle_pane_split(id, params, launch.target()),
            None => self.handle_pane_split(id, params, PaneLaunchTarget::Host),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::api::schema::{
        ContainerOpenTarget, Method, PaneSplitParams, ResponseResult, SplitDirection,
        SuccessResponse, TabCreateInheritParams, TabCreateParams,
    };
    use crate::app::api::test_support::shutdown_test_runtimes;
    use crate::app::App;
    use crate::config::Config;
    use crate::container::{ContainerLaunch, ContainerRef, ContainerShell};
    use crate::ssh_host::SshLaunch;
    use crate::terminal::{TerminalId, TerminalRuntime};
    use crate::workspace::Workspace;

    #[derive(Clone)]
    enum Shell {
        Host,
        Ssh(SshLaunch),
        Container(ContainerLaunch),
        /// Launch metadata kept by a cold restore while the shell runs on the host.
        ColdRestoredSsh(SshLaunch),
    }

    fn ssh_launch() -> SshLaunch {
        SshLaunch::new("deploy@prod".into(), None).expect("valid destination")
    }

    fn container_launch(ssh: Option<SshLaunch>) -> ContainerLaunch {
        ContainerLaunch {
            reference: ContainerRef::Name("db".into()),
            canonical: "db".into(),
            id: "cid".into(),
            workdir: Some("/srv/app".into()),
            user: Some("app".into()),
            shell: ContainerShell::BashOrSh,
            ssh,
        }
    }

    /// One workspace whose first pane is focused and runs `shell`.
    fn app_with_focused_shell(shell: Shell) -> (App, String) {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.default_shell = crate::app::api::test_support::exiting_test_command().into();
        app.state.workspaces = vec![Workspace::test_new("inherit")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        let pane = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id: TerminalId = app.state.workspaces[0]
            .terminal_id(pane)
            .cloned()
            .expect("terminal id");
        let mut runtime = TerminalRuntime::test_with_scrollback_bytes(20, 5, 1000, b"");
        let terminal = app.state.terminals.get_mut(&terminal_id).expect("terminal");
        match shell {
            Shell::Host => {}
            Shell::Ssh(launch) => {
                terminal.ssh = Some(launch);
                runtime.test_set_in_ssh(true);
            }
            Shell::ColdRestoredSsh(launch) => terminal.ssh = Some(launch),
            Shell::Container(launch) => {
                terminal.container = Some(launch);
                runtime.test_set_in_container(true);
            }
        }
        app.terminal_runtimes.insert(terminal_id, runtime);
        let public_pane_id = app.public_pane_id(0, pane).expect("public pane id");
        (app, public_pane_id)
    }

    fn tab_params(source_pane_id: Option<String>) -> TabCreateInheritParams {
        TabCreateInheritParams {
            create: TabCreateParams {
                workspace_id: None,
                cwd: None,
                focus: false,
                label: None,
                env: Default::default(),
            },
            source_pane_id,
        }
    }

    fn split_params(target_pane_id: Option<String>) -> PaneSplitParams {
        PaneSplitParams {
            workspace_id: None,
            target_pane_id,
            direction: SplitDirection::Right,
            ratio: None,
            cwd: None,
            focus: false,
            right_click: Default::default(),
            env: Default::default(),
        }
    }

    /// Terminals other than the source pane's, whatever created them.
    fn new_terminals<'a>(
        app: &'a App,
        source: &TerminalId,
    ) -> Vec<&'a crate::terminal::TerminalState> {
        app.state
            .terminals
            .values()
            .filter(|terminal| &terminal.id != source)
            .collect()
    }

    fn source_terminal(app: &App) -> TerminalId {
        let pane = app.state.workspaces[0].tabs[0].root_pane;
        app.state.workspaces[0]
            .terminal_id(pane)
            .cloned()
            .expect("terminal id")
    }

    fn is_success(response: &str) -> bool {
        matches!(
            serde_json::from_str::<SuccessResponse>(response),
            Ok(SuccessResponse {
                result: ResponseResult::TabCreated { .. } | ResponseResult::PaneInfo { .. },
                ..
            })
        )
    }

    /// Why not require success: spawning `docker` fails on hosts without it,
    /// and the contract is that a failed remote launch leaves no host shell.
    fn assert_launch_or_failure_without_host_fallback(
        app: &App,
        response: &str,
        source: &TerminalId,
        check: impl Fn(&crate::terminal::TerminalState),
    ) {
        let created = new_terminals(app, source);
        if is_success(response) {
            assert_eq!(created.len(), 1, "{response}");
            check(created[0]);
        } else {
            assert!(created.is_empty(), "{response}");
        }
    }

    #[tokio::test]
    async fn new_tab_from_a_live_ssh_pane_launches_the_same_ssh_host() {
        let launch = ssh_launch();
        let (mut app, _) = app_with_focused_shell(Shell::Ssh(launch.clone()));
        let source = source_terminal(&app);

        let response = app.handle_tab_create_inherit("req".into(), tab_params(None));

        assert_launch_or_failure_without_host_fallback(&app, &response, &source, |terminal| {
            assert_eq!(terminal.ssh.as_ref(), Some(&launch));
            assert_eq!(terminal.container, None);
        });
        shutdown_test_runtimes(&mut app);
    }

    #[tokio::test]
    async fn new_tab_from_a_live_container_pane_launches_the_same_container() {
        let launch = container_launch(None);
        let (mut app, _) = app_with_focused_shell(Shell::Container(launch.clone()));
        let source = source_terminal(&app);

        let response = app.handle_tab_create_inherit("req".into(), tab_params(None));

        assert_launch_or_failure_without_host_fallback(&app, &response, &source, |terminal| {
            assert_eq!(terminal.container.as_ref(), Some(&launch));
            assert_eq!(terminal.ssh, None);
        });
        shutdown_test_runtimes(&mut app);
    }

    #[tokio::test]
    async fn new_tab_from_a_remote_docker_container_pane_keeps_its_ssh_host() {
        let launch = container_launch(Some(ssh_launch()));
        let (mut app, _) = app_with_focused_shell(Shell::Container(launch.clone()));
        let source = source_terminal(&app);

        let response = app.handle_tab_create_inherit("req".into(), tab_params(None));

        assert_launch_or_failure_without_host_fallback(&app, &response, &source, |terminal| {
            assert_eq!(terminal.container.as_ref(), Some(&launch));
        });
        shutdown_test_runtimes(&mut app);
    }

    #[tokio::test]
    async fn new_tab_from_a_host_pane_matches_plain_tab_create() {
        let (mut app, _) = app_with_focused_shell(Shell::Host);
        let source = source_terminal(&app);

        let response = app.handle_tab_create_inherit("req".into(), tab_params(None));

        assert!(is_success(&response), "{response}");
        let created = new_terminals(&app, &source);
        assert_eq!(created.len(), 1);
        assert_eq!((&created[0].ssh, &created[0].container), (&None, &None));
        shutdown_test_runtimes(&mut app);
    }

    #[tokio::test]
    async fn new_tab_from_a_cold_restored_ssh_pane_opens_a_host_shell() {
        let (mut app, _) = app_with_focused_shell(Shell::ColdRestoredSsh(ssh_launch()));
        let source = source_terminal(&app);

        let response = app.handle_tab_create_inherit("req".into(), tab_params(None));

        assert!(is_success(&response), "{response}");
        let created = new_terminals(&app, &source);
        assert_eq!(created.len(), 1);
        assert_eq!(created[0].ssh, None);
        shutdown_test_runtimes(&mut app);
    }

    #[tokio::test]
    async fn new_tab_copies_the_launch_of_an_explicit_source_pane() {
        let launch = ssh_launch();
        let (mut app, ssh_pane) = app_with_focused_shell(Shell::Ssh(launch.clone()));
        let source = source_terminal(&app);
        // Focus a host pane so only `source_pane_id` can select the ssh one.
        let host_pane = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces[0].tabs[0].layout.focus_pane(host_pane);
        app.state.ensure_test_terminals();

        let response = app.handle_tab_create_inherit("req".into(), tab_params(Some(ssh_pane)));

        let created: Vec<_> = new_terminals(&app, &source)
            .into_iter()
            .filter(|terminal| terminal.ssh.is_some())
            .collect();
        if is_success(&response) {
            assert_eq!(created.len(), 1, "{response}");
            assert_eq!(created[0].ssh.as_ref(), Some(&launch));
        }
        shutdown_test_runtimes(&mut app);
    }

    #[tokio::test]
    async fn explicit_cwd_on_a_local_container_source_is_replanned_through_container_open() {
        let (app, pane) = app_with_focused_shell(Shell::Container(container_launch(None)));
        let mut params = tab_params(None);
        params.create.cwd = Some("/home/me/proj/sub".into());

        let (reference, target) = app
            .container_reopen_for_inherit(&Method::TabCreateInherit(params))
            .expect("container reopen");
        assert_eq!(reference, ContainerRef::Name("db".into()));
        assert!(matches!(target, ContainerOpenTarget::Tab(_)));

        let mut split = split_params(Some(pane));
        split.cwd = Some("/home/me/proj/sub".into());
        let (_, target) = app
            .container_reopen_for_inherit(&Method::PaneSplitInherit(split))
            .expect("container reopen");
        assert!(matches!(target, ContainerOpenTarget::Split(_)));
    }

    #[tokio::test]
    async fn container_reopen_is_limited_to_a_local_container_source_with_a_cwd() {
        let with_cwd = |mut params: TabCreateInheritParams| {
            params.create.cwd = Some("/home/me/proj".into());
            Method::TabCreateInherit(params)
        };
        let without_cwd = Method::TabCreateInherit(tab_params(None));
        let (container, _) = app_with_focused_shell(Shell::Container(container_launch(None)));
        assert!(container
            .container_reopen_for_inherit(&without_cwd)
            .is_none());
        for shell in [
            Shell::Host,
            Shell::Ssh(ssh_launch()),
            Shell::Container(container_launch(Some(ssh_launch()))),
        ] {
            let (app, _) = app_with_focused_shell(shell);
            assert!(app
                .container_reopen_for_inherit(&with_cwd(tab_params(None)))
                .is_none());
        }
    }

    #[tokio::test]
    async fn new_tab_with_an_unknown_source_pane_is_pane_not_found() {
        let (mut app, _) = app_with_focused_shell(Shell::Host);

        let response =
            app.handle_tab_create_inherit("req".into(), tab_params(Some("p_missing".into())));

        assert!(response.contains("pane_not_found"), "{response}");
        shutdown_test_runtimes(&mut app);
    }

    #[tokio::test]
    async fn split_of_a_live_ssh_pane_launches_the_same_ssh_host() {
        let launch = ssh_launch();
        let (mut app, pane) = app_with_focused_shell(Shell::Ssh(launch.clone()));
        let source = source_terminal(&app);

        let response = app.handle_pane_split_inherit("req".into(), split_params(Some(pane)));

        assert_launch_or_failure_without_host_fallback(&app, &response, &source, |terminal| {
            assert_eq!(terminal.ssh.as_ref(), Some(&launch));
        });
        shutdown_test_runtimes(&mut app);
    }

    #[tokio::test]
    async fn split_of_a_live_container_pane_launches_the_same_container() {
        let launch = container_launch(Some(ssh_launch()));
        let (mut app, pane) = app_with_focused_shell(Shell::Container(launch.clone()));
        let source = source_terminal(&app);

        let response = app.handle_pane_split_inherit("req".into(), split_params(Some(pane)));

        assert_launch_or_failure_without_host_fallback(&app, &response, &source, |terminal| {
            assert_eq!(terminal.container.as_ref(), Some(&launch));
        });
        shutdown_test_runtimes(&mut app);
    }

    #[tokio::test]
    async fn split_of_a_host_pane_matches_plain_pane_split() {
        let (mut app, pane) = app_with_focused_shell(Shell::Host);
        let source = source_terminal(&app);

        let response = app.handle_pane_split_inherit("req".into(), split_params(Some(pane)));

        assert!(is_success(&response), "{response}");
        let created = new_terminals(&app, &source);
        assert_eq!(created.len(), 1);
        assert_eq!((&created[0].ssh, &created[0].container), (&None, &None));
        shutdown_test_runtimes(&mut app);
    }

    #[tokio::test]
    async fn split_of_an_unknown_pane_is_pane_not_found() {
        let (mut app, _) = app_with_focused_shell(Shell::Host);

        let response =
            app.handle_pane_split_inherit("req".into(), split_params(Some("p_missing".into())));

        assert!(response.contains("pane_not_found"), "{response}");
        shutdown_test_runtimes(&mut app);
    }
}
