//! Refuses public API requests that read or drive a pane on a protected ssh
//! host (see `crate::ssh_host::protected`).
//!
//! Only the public socket is checked: the user's own TUI reaches the server
//! through the client endpoint and keeps full access. Listings and metadata
//! (`*.list`, `*.get`, layout geometry) stay readable, so a script can still
//! see that the pane exists; its screen, input, files and lifecycle cannot be
//! reached.

use crate::api::schema::{
    ContainerOpenTarget, Method, PaneMoveDestination, PaneSplitParams, Request, SshOpenTarget,
    Subscription, TabCreateParams, WorkspaceCreateParams,
};
use crate::app::App;
use crate::terminal::TerminalState;

pub(crate) const ERROR_CODE: &str = "protected_host";

/// What a request names, before the app resolves it to panes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ApiRef<'a> {
    Pane(&'a str),
    /// The pane a request without a pane id acts on: the focused pane of the
    /// active workspace.
    FocusedPane,
    /// The active tab, for requests that act on a whole tab by default.
    ActiveTab,
    Tab(&'a str),
    Workspace(&'a str),
    /// A terminal id, pane id or agent name, as agent targets accept.
    Terminal(&'a str),
    Task(&'a str),
    /// A pane id, or the assist that reads it.
    AssistPane(&'a str),
    SshHost(&'a str),
}

fn pane_or_focused(pane_id: Option<&str>) -> ApiRef<'_> {
    pane_id.map_or(ApiRef::FocusedPane, ApiRef::Pane)
}

fn workspace(workspace_id: Option<&str>) -> Option<ApiRef<'_>> {
    workspace_id.map(ApiRef::Workspace)
}

fn split_refs(params: &PaneSplitParams) -> Vec<ApiRef<'_>> {
    [
        Some(pane_or_focused(params.target_pane_id.as_deref())),
        workspace(params.workspace_id.as_deref()),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn workspace_create_refs(params: &WorkspaceCreateParams) -> Vec<ApiRef<'_>> {
    workspace(params.source_workspace_id.as_deref())
        .into_iter()
        .collect()
}

fn tab_create_refs(params: &TabCreateParams) -> Vec<ApiRef<'_>> {
    workspace(params.workspace_id.as_deref())
        .into_iter()
        .collect()
}

/// Where an `ssh.open` or `container.open` puts its new pane.
fn open_target_refs<'a>(
    workspace_params: Option<&'a WorkspaceCreateParams>,
    tab: Option<&'a TabCreateParams>,
    split: Option<&'a PaneSplitParams>,
) -> Vec<ApiRef<'a>> {
    workspace_params
        .map(workspace_create_refs)
        .or_else(|| tab.map(tab_create_refs))
        .or_else(|| split.map(split_refs))
        .unwrap_or_default()
}

fn ssh_open_target_refs(target: &SshOpenTarget) -> Vec<ApiRef<'_>> {
    match target {
        SshOpenTarget::Workspace(params) => open_target_refs(Some(params), None, None),
        SshOpenTarget::Tab(params) => open_target_refs(None, Some(params), None),
        SshOpenTarget::Split(params) => open_target_refs(None, None, Some(params)),
    }
}

fn container_open_target_refs(target: &ContainerOpenTarget) -> Vec<ApiRef<'_>> {
    match target {
        ContainerOpenTarget::Workspace(params) => open_target_refs(Some(params), None, None),
        ContainerOpenTarget::Tab(params) => open_target_refs(None, Some(params), None),
        ContainerOpenTarget::Split(params) => open_target_refs(None, None, Some(params)),
    }
}

fn directory_refs<'a>(workspace_id: Option<&'a str>, pane_id: Option<&'a str>) -> Vec<ApiRef<'a>> {
    match (pane_id, workspace_id) {
        (Some(pane_id), _) => vec![ApiRef::Pane(pane_id)],
        (None, Some(workspace_id)) => vec![ApiRef::Workspace(workspace_id)],
        (None, None) => vec![ApiRef::FocusedPane],
    }
}

/// The things `method` would reach. Why no wildcard arm: a new method must
/// be classified here before it compiles, or it would reach protected panes.
pub(crate) fn api_refs(method: &Method) -> Vec<ApiRef<'_>> {
    use ApiRef::*;
    match method {
        Method::Ping(_)
        | Method::ServerStop(_)
        | Method::ServerLiveHandoff(_)
        | Method::ServerReloadConfig(_)
        | Method::ServerSshAgentRegister(_)
        | Method::ServerAgentManifests(_)
        | Method::ServerReloadAgentManifests(_)
        | Method::NotificationShow(_)
        | Method::ProductAnnouncementDismiss(_)
        | Method::ReleaseNotesDismiss(_)
        | Method::ClientWindowTitleSet(_)
        | Method::ClientWindowTitleClear(_)
        | Method::ClientShellSurfaceSet(_)
        | Method::PopupClose(_)
        | Method::IntegrationList(_)
        | Method::IntegrationInstall(_)
        | Method::IntegrationUninstall(_)
        | Method::PluginLink(_)
        | Method::PluginList(_)
        | Method::PluginUnlink(_)
        | Method::PluginEnable(_)
        | Method::PluginDisable(_)
        | Method::PluginActionList(_)
        | Method::PluginLogList(_)
        | Method::AgentViewSet(_)
        | Method::AgentViewClear(_)
        | Method::AssistProducts(_)
        | Method::AssistList(_)
        | Method::PreviewList(_)
        | Method::PreviewStop(_)
        | Method::ContainerList(_)
        | Method::SshHosts(_)
        | Method::WorktreeList(_)
        | Method::EventsWait(_) => Vec::new(),

        // Local filesystem only; a pane's files go through `pane.*` methods.
        Method::FileRead(_)
        | Method::FileStat(_)
        | Method::FilesCancel(_)
        | Method::FilesSearch(_)
        | Method::FilesReplace(_)
        | Method::FilesPlan(_)
        | Method::FilesOperate(_) => Vec::new(),

        // Transfers are refused when they begin; these only continue one.
        Method::PaneUploadCancel(_)
        | Method::PaneUploadFinish(_)
        | Method::PaneUploadWrite(_)
        | Method::PaneDownloadCancel(_)
        | Method::PaneDownloadRead(_) => Vec::new(),

        // Listings and metadata stay readable.
        Method::SessionSnapshot(_)
        | Method::WorkspaceList(_)
        | Method::WorkspaceGet(_)
        | Method::TabList(_)
        | Method::TabGet(_)
        | Method::AgentList(_)
        | Method::AgentGet(_)
        | Method::AgentWait(_)
        | Method::TaskList(_)
        | Method::PaneList(_)
        | Method::PaneCurrent(_)
        | Method::PaneGet(_)
        | Method::PaneLayout(_)
        | Method::PaneNeighbor(_)
        | Method::PaneEdges(_)
        | Method::PaneProcessInfo(_)
        | Method::LayoutExport(_) => Vec::new(),

        Method::CommandInvoke(params) => [
            Some(pane_or_focused(params.pane_id.as_deref())),
            params.tab_id.as_deref().map(Tab),
            workspace(params.workspace_id.as_deref()),
        ]
        .into_iter()
        .flatten()
        .collect(),
        Method::PluginActionInvoke(params) => params
            .context
            .as_ref()
            .map(|context| {
                [
                    context.focused_pane_id.as_deref().map(Pane),
                    context.tab_id.as_deref().map(Tab),
                    workspace(context.workspace_id.as_deref()),
                ]
                .into_iter()
                .flatten()
                .collect()
            })
            .unwrap_or_default(),
        Method::PluginPaneOpen(params) => [
            params.target_pane_id.as_deref().map(Pane),
            workspace(params.workspace_id.as_deref()),
        ]
        .into_iter()
        .flatten()
        .collect(),
        Method::PluginPaneFocus(params) => vec![Pane(&params.pane_id)],
        Method::PluginPaneClose(params) => vec![Pane(&params.pane_id)],

        Method::WorkspaceCreate(params) => workspace_create_refs(params),
        Method::WorkspaceFocus(target) => vec![Workspace(&target.workspace_id)],
        Method::WorkspaceRename(params) => vec![Workspace(&params.workspace_id)],
        Method::WorkspaceMove(params) => vec![Workspace(&params.workspace_id)],
        Method::WorkspaceMoveBlock(params) => params
            .workspace_ids
            .iter()
            .map(|id| Workspace(id))
            .collect(),
        Method::WorkspaceReportMetadata(params) => vec![Workspace(&params.workspace_id)],
        Method::WorkspaceClose(params) => vec![Workspace(&params.workspace_id)],
        Method::WorktreeCreate(params) => workspace(params.workspace_id.as_deref())
            .into_iter()
            .collect(),
        Method::WorktreeOpen(params) => workspace(params.workspace_id.as_deref())
            .into_iter()
            .collect(),
        Method::WorktreeRemove(params) => vec![Workspace(&params.workspace_id)],

        Method::ContainerOpen(params) => container_open_target_refs(&params.target),
        Method::SshOpen(params) => std::iter::once(SshHost(&params.host))
            .chain(ssh_open_target_refs(&params.target))
            .collect(),
        Method::SshContainerOpen(params) => std::iter::once(SshHost(&params.host))
            .chain(container_open_target_refs(&params.target))
            .collect(),
        // Lists containers by running `docker` on the host.
        Method::SshContainerList(params) => vec![SshHost(&params.host)],

        Method::TabCreate(params) => tab_create_refs(params),
        Method::TabCreateInherit(params) => [
            Some(pane_or_focused(params.source_pane_id.as_deref())),
            workspace(params.create.workspace_id.as_deref()),
        ]
        .into_iter()
        .flatten()
        .collect(),
        Method::TabFocus(target) | Method::TabClose(target) => vec![Tab(&target.tab_id)],
        Method::TabRename(params) => vec![Tab(&params.tab_id)],
        Method::TabMove(params) => vec![Tab(&params.tab_id)],

        Method::AgentRead(params) => vec![Terminal(&params.target)],
        // The explanation quotes the screen text detection matched.
        Method::AgentExplain(target) => vec![Terminal(&target.target)],
        Method::AgentSendKeys(params) => vec![Terminal(&params.target)],
        Method::AgentRename(params) => vec![Terminal(&params.target)],
        Method::AgentFocus(target) => vec![Terminal(&target.target)],
        Method::AgentStart(params) => vec![Pane(&params.pane_id)],
        Method::AgentPrompt(params) => vec![Terminal(&params.target)],

        Method::TaskSubmit(params) => vec![Terminal(&params.target)],
        Method::TaskGet(target) | Method::TaskCancel(target) | Method::TaskResult(target) => {
            vec![Task(&target.task_id)]
        }
        Method::TaskWait(params) => vec![Task(&params.task_id)],

        Method::PaneSplit(params) | Method::PaneSplitInherit(params) => split_refs(params),
        Method::PaneSwap(params) => {
            let named: Vec<_> = [
                params.pane_id.as_deref(),
                params.source_pane_id.as_deref(),
                params.target_pane_id.as_deref(),
            ]
            .into_iter()
            .flatten()
            .map(Pane)
            .collect();
            if named.is_empty() {
                vec![FocusedPane]
            } else {
                named
            }
        }
        Method::PaneMove(params) => {
            let destination = match &params.destination {
                PaneMoveDestination::Tab {
                    tab_id,
                    target_pane_id,
                    ..
                } => vec![Some(Tab(tab_id)), target_pane_id.as_deref().map(Pane)],
                PaneMoveDestination::NewTab { workspace_id, .. } => {
                    vec![workspace(workspace_id.as_deref())]
                }
                PaneMoveDestination::NewWorkspace { .. } => Vec::new(),
            };
            std::iter::once(Pane(&params.pane_id))
                .chain(destination.into_iter().flatten())
                .collect()
        }
        Method::PaneZoom(params) => vec![pane_or_focused(params.pane_id.as_deref())],
        Method::PaneFocusDirection(params) => vec![pane_or_focused(params.pane_id.as_deref())],
        Method::PaneResize(params) => vec![pane_or_focused(params.pane_id.as_deref())],
        Method::LayoutApply(params) => {
            let named: Vec<_> = [
                params.tab_id.as_deref().map(Tab),
                workspace(params.workspace_id.as_deref()),
            ]
            .into_iter()
            .flatten()
            .collect();
            if named.is_empty() {
                vec![ActiveTab]
            } else {
                named
            }
        }
        Method::LayoutSetSplitRatio(params) => match (&params.pane_id, &params.tab_id) {
            (Some(pane_id), _) => vec![Pane(pane_id)],
            (None, Some(tab_id)) => vec![Tab(tab_id)],
            (None, None) => vec![ActiveTab],
        },

        Method::PaneScroll(params) => vec![Pane(&params.pane_id)],
        Method::PaneClear(target)
        | Method::PaneReconnect(target)
        | Method::PaneEditScrollback(target)
        | Method::PaneFocus(target)
        | Method::PaneClose(target) => vec![Pane(&target.pane_id)],
        Method::PaneSelectionRead(params) => vec![Pane(&params.pane_id)],
        Method::PaneCopyMotion(params) => vec![Pane(&params.pane_id)],
        Method::PaneCopySearch(params) => vec![Pane(&params.pane_id)],
        Method::PanePromptMotion(params) => vec![Pane(&params.pane_id)],
        Method::PaneCommandOutputRange(params) => vec![Pane(&params.pane_id)],
        Method::PaneInputSet(params) => vec![Pane(&params.pane_id)],
        Method::PaneLinkActivate(params) | Method::PaneLinkResolve(params) => {
            vec![Pane(&params.pane_id)]
        }
        Method::PaneRename(params) => vec![Pane(&params.pane_id)],
        Method::PaneSendText(params) => vec![Pane(&params.pane_id)],
        Method::PaneSendKeys(params) => vec![Pane(&params.pane_id)],
        Method::PaneSendInput(params) => vec![Pane(&params.pane_id)],
        Method::PaneRead(params) => vec![Pane(&params.pane_id)],
        Method::PaneReportAgent(params) => vec![Pane(&params.pane_id)],
        Method::PaneReportAgentSession(params) => vec![Pane(&params.pane_id)],
        Method::PaneReportMetadata(params) => vec![Pane(&params.pane_id)],
        Method::PaneClearAgentAuthority(params) => vec![Pane(&params.pane_id)],
        Method::PaneReleaseAgent(params) => vec![Pane(&params.pane_id)],
        Method::PaneDeclareAgent(params) => vec![Pane(&params.pane_id)],
        Method::PaneWaitForOutput(params) => vec![Pane(&params.pane_id)],
        Method::EventsSubscribe(params) => params
            .subscriptions
            .iter()
            .filter_map(|subscription| match subscription {
                Subscription::PaneOutputMatched { pane_id, .. }
                | Subscription::PaneAgentStatusChanged { pane_id, .. }
                | Subscription::PaneScrollChanged { pane_id } => Some(Pane(pane_id)),
                _ => None,
            })
            .collect(),

        Method::PaneOpenEditor(params) => [
            Some(pane_or_focused(params.pane_id.as_deref())),
            workspace(params.workspace_id.as_deref()),
        ]
        .into_iter()
        .flatten()
        .collect(),
        Method::PaneOpenFile(params) => [
            Some(pane_or_focused(params.pane_id.as_deref())),
            workspace(params.workspace_id.as_deref()),
        ]
        .into_iter()
        .flatten()
        .collect(),
        // An explicit pane wins, then the focused pane of the named or the
        // focused workspace, as `directory_target` resolves it.
        Method::DirectoryList(params) => {
            directory_refs(params.workspace_id.as_deref(), params.pane_id.as_deref())
        }
        Method::DirectoryIndex(params) => {
            directory_refs(params.workspace_id.as_deref(), params.pane_id.as_deref())
        }
        Method::PaneDirectoryList(params) => vec![Pane(&params.pane_id)],
        Method::PaneDirectoryIndex(params) => vec![Pane(&params.pane_id)],
        Method::PaneDirectoryOpenEditor(params) => vec![Pane(&params.pane_id)],
        Method::PaneFileRead(params) => vec![Pane(&params.pane_id)],
        Method::PaneDirectoryOpenFile(params) => vec![Pane(&params.pane_id)],
        Method::PaneFileStat(params) => vec![Pane(&params.pane_id)],
        // Without a pane or terminal the transfer stays on the server's disk.
        Method::PaneUploadBegin(params) => [
            params.pane_id.as_deref().map(Pane),
            params.terminal_id.as_deref().map(Terminal),
        ]
        .into_iter()
        .flatten()
        .collect(),
        Method::PaneDownloadBegin(params) => {
            params.pane_id.as_deref().map(Pane).into_iter().collect()
        }
        Method::PreviewStart(params) => params.pane_id.as_deref().map(Pane).into_iter().collect(),

        Method::AssistStart(params) => vec![Pane(&params.pane_id)],
        Method::AssistRefresh(params) => vec![AssistPane(&params.pane_id)],
    }
}

pub(crate) fn terminal_is_protected(terminal: &TerminalState) -> bool {
    terminal.ssh.as_ref().is_some_and(|ssh| ssh.protected)
        || terminal
            .container
            .as_ref()
            .and_then(|container| container.ssh.as_ref())
            .is_some_and(|ssh| ssh.protected)
}

impl App {
    /// The error response for a public request that would reach a pane on a
    /// protected ssh host, or `None` when it may run.
    pub(crate) fn protected_host_denial(&self, request: &Request) -> Option<String> {
        let denied = api_refs(&request.method)
            .into_iter()
            .find(|reference| self.reaches_protected_host(reference))?;
        let what = match denied {
            ApiRef::SshHost(host) => format!("ssh host {host} is protected"),
            ApiRef::Pane(id) | ApiRef::AssistPane(id) => {
                format!("pane {id} is on a protected ssh host")
            }
            ApiRef::Terminal(id) => format!("target {id} is on a protected ssh host"),
            ApiRef::Task(id) => format!("task {id} is on a protected ssh host"),
            ApiRef::Tab(id) => format!("tab {id} holds a pane on a protected ssh host"),
            ApiRef::Workspace(id) => {
                format!("workspace {id} holds a pane on a protected ssh host")
            }
            ApiRef::FocusedPane => "the focused pane is on a protected ssh host".to_owned(),
            ApiRef::ActiveTab => "the active tab holds a pane on a protected ssh host".to_owned(),
        };
        Some(super::responses::encode_error(
            request.id.clone(),
            ERROR_CODE,
            format!("{what}; the herdr CLI and API cannot reach it. Use the herdr window instead."),
        ))
    }

    fn reaches_protected_host(&self, reference: &ApiRef<'_>) -> bool {
        match *reference {
            ApiRef::SshHost(host) => crate::ssh_host::protected::current().covers(host),
            // Why the terminal fallback: handlers read pane ids today, but an
            // id they may learn to accept must not pass unchecked.
            ApiRef::Pane(id) => match self.parse_pane_id(id) {
                Some((ws_idx, pane_id)) => self.pane_is_protected(ws_idx, pane_id),
                None => self.terminal_target_is_protected(id),
            },
            ApiRef::FocusedPane => self.state.active.is_some_and(|ws_idx| {
                self.state
                    .workspaces
                    .get(ws_idx)
                    .and_then(|ws| ws.focused_pane_id())
                    .is_some_and(|pane_id| self.pane_is_protected(ws_idx, pane_id))
            }),
            ApiRef::ActiveTab => self.state.active.is_some_and(|ws_idx| {
                self.state
                    .workspaces
                    .get(ws_idx)
                    .is_some_and(|ws| self.tab_is_protected(ws_idx, ws.active_tab_index()))
            }),
            ApiRef::Tab(id) => self
                .parse_tab_id(id)
                .is_some_and(|(ws_idx, tab_idx)| self.tab_is_protected(ws_idx, tab_idx)),
            ApiRef::Workspace(id) => self.parse_workspace_id(id).is_some_and(|ws_idx| {
                (0..self.state.workspaces[ws_idx].tabs.len())
                    .any(|tab_idx| self.tab_is_protected(ws_idx, tab_idx))
            }),
            ApiRef::Terminal(target) => self.terminal_target_is_protected(target),
            ApiRef::Task(id) => crate::tasks::TaskId::parse(id)
                .and_then(|task_id| self.state.tasks.get(task_id))
                .is_some_and(|task| {
                    self.state.terminals.get(&task.terminal).map_or_else(
                        || {
                            task.location.ssh_host.as_deref().is_some_and(|host| {
                                crate::ssh_host::protected::current().covers(host)
                            })
                        },
                        terminal_is_protected,
                    )
                }),
            ApiRef::AssistPane(id) => self.parse_pane_id(id).is_some_and(|(ws_idx, pane_id)| {
                self.pane_is_protected(ws_idx, pane_id)
                    || self
                        .state
                        .terminal_id_for_pane(ws_idx, pane_id)
                        .and_then(|terminal| self.state.assists.for_either(&terminal))
                        .and_then(|link| link.target.as_ref())
                        .and_then(|target| self.state.terminals.get(target))
                        .is_some_and(terminal_is_protected)
            }),
        }
    }

    fn pane_is_protected(&self, ws_idx: usize, pane_id: crate::layout::PaneId) -> bool {
        self.state
            .terminal_id_for_pane(ws_idx, pane_id)
            .and_then(|terminal| self.state.terminals.get(&terminal))
            .is_some_and(terminal_is_protected)
    }

    fn tab_is_protected(&self, ws_idx: usize, tab_idx: usize) -> bool {
        self.state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.tabs.get(tab_idx))
            .is_some_and(|tab| {
                tab.panes.values().any(|pane| {
                    self.state
                        .terminals
                        .get(&pane.attached_terminal_id)
                        .is_some_and(terminal_is_protected)
                })
            })
    }

    /// Why both resolutions and every ambiguous candidate: the handler picks
    /// one rule per method, and a name shared with a protected pane must not
    /// slip through because this check chose another.
    fn terminal_target_is_protected(&self, target: &str) -> bool {
        use super::super::terminal_targets::TerminalTargetError;
        [
            self.resolve_terminal_target(target),
            self.resolve_agent_target(target),
        ]
        .into_iter()
        .any(|resolved| match resolved {
            Ok(resolved) => self
                .state
                .terminals
                .get(resolved.terminal_id.as_str())
                .is_some_and(terminal_is_protected),
            Err(TerminalTargetError::Ambiguous { candidates, .. }) => {
                candidates.iter().any(|candidate| {
                    self.state
                        .terminals
                        .get(candidate.terminal_id.as_str())
                        .is_some_and(terminal_is_protected)
                })
            }
            Err(TerminalTargetError::NotFound { .. }) => false,
        })
    }
}

#[cfg(test)]
mod tests;
