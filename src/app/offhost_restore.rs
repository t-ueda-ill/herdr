//! Starts the shells of cold-restored container and ssh panes.

use crate::container::docker::DockerHost;
use crate::container::{ContainerLaunch, ContainerRef};
use crate::events::AppEvent;
use crate::terminal::{TerminalId, TerminalRuntimeRegistry, TerminalState};

use super::{App, AppState};

/// How a waiting pane gets its shell.
#[derive(Debug, Clone, PartialEq, Eq)]
enum OffHostStart {
    /// An ssh login needs no lookup: its launch was resolved when it opened.
    Spawn,
    /// The saved container id may be gone, so docker is asked again first.
    Lookup(Box<ContainerLaunch>),
}

impl App {
    /// Connects every restored container or ssh pane that waits for its shell.
    pub(crate) fn start_pending_offhost_connects(&mut self) {
        for (terminal_id, start) in pending_offhost_starts(&self.state, &self.terminal_runtimes) {
            match start {
                OffHostStart::Spawn => self.spawn_pending_offhost_shell(&terminal_id),
                OffHostStart::Lookup(launch) => self.start_container_lookup(terminal_id, *launch),
            }
        }
    }

    /// Runs docker (over ssh for a remote host), so it stays off the app thread.
    fn start_container_lookup(&mut self, terminal_id: TerminalId, launch: ContainerLaunch) {
        if let Some(pending) = self
            .state
            .terminals
            .get_mut(&terminal_id)
            .and_then(|terminal| terminal.pending_offhost_connect.as_mut())
        {
            pending.resolving = true;
        }
        let event_tx = self.event_tx.clone();
        let job = (terminal_id, launch);
        let spawned = crate::thread_spawn::spawn_named_with(
            "herdr-container-reconnect",
            job,
            move |(terminal_id, launch)| {
                let container_id = running_container_id(&launch);
                let _ = event_tx.blocking_send(AppEvent::OffHostReconnectResolved {
                    terminal_id,
                    container_id,
                });
            },
        );
        if let Err(((terminal_id, _), err)) = spawned {
            tracing::warn!(err = %err, "failed to spawn container reconnect lookup");
            self.handle_offhost_reconnect_resolved(terminal_id, None);
        }
    }

    pub(crate) fn handle_offhost_reconnect_resolved(
        &mut self,
        terminal_id: TerminalId,
        container_id: Option<String>,
    ) {
        let Some(terminal) = self.state.terminals.get_mut(&terminal_id) else {
            return;
        };
        if apply_container_lookup(terminal, container_id) {
            self.spawn_pending_offhost_shell(&terminal_id);
        }
    }

    fn spawn_pending_offhost_shell(&mut self, terminal_id: &TerminalId) {
        if self.terminal_runtimes.get(terminal_id).is_some() {
            return;
        }
        let Some((ws_idx, pane_id)) = pane_of_terminal(&self.state, terminal_id) else {
            return;
        };
        let Some(launch_env) = self.pane_launch_env(ws_idx, pane_id, Vec::new()) else {
            return;
        };
        let Some(terminal) = self.state.terminals.get_mut(terminal_id) else {
            return;
        };
        let Some(pending) = terminal.pending_offhost_connect.take() else {
            return;
        };
        let cwd = terminal.cwd.clone();
        let (rows, cols) = self
            .state
            .new_pane_size(crate::ui::NewPanePlacement::Existing {
                ws_idx,
                pane: pane_id,
            });
        let spawned = crate::terminal::TerminalRuntime::spawn_with_initial_history(
            pane_id,
            rows,
            cols,
            cwd,
            self.state.pane_scrollback_limit_bytes,
            self.state.host_terminal_theme,
            self.state.host_terminal_appearance,
            self.state.pane_shell_config_for_terminal(terminal_id),
            &launch_env,
            pending.initial_history_ansi.as_deref(),
            self.event_tx.clone(),
            self.render_notify.clone(),
            self.render_dirty.clone(),
        );
        match spawned {
            Ok(runtime) => {
                self.terminal_runtimes.insert(terminal_id.clone(), runtime);
            }
            Err(err) => {
                tracing::warn!(
                    pane = pane_id.raw(),
                    terminal = %terminal_id,
                    err = %err,
                    "failed to reconnect restored off-host pane"
                );
                if let Some(terminal) = self.state.terminals.get_mut(terminal_id) {
                    terminal.restore_error = Some(format!(
                        "Could not start the saved connection: {err}. Restart this session to retry."
                    ));
                    terminal.revision = terminal.revision.saturating_add(1);
                }
            }
        }
        self.schedule_session_save();
    }
}

fn pending_offhost_starts(
    state: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> Vec<(TerminalId, OffHostStart)> {
    let mut starts = Vec::new();
    for ws in &state.workspaces {
        for tab in &ws.tabs {
            for pane in tab.panes.values() {
                let terminal_id = &pane.attached_terminal_id;
                if terminal_runtimes.get(terminal_id).is_some() {
                    continue;
                }
                let Some(terminal) = state.terminals.get(terminal_id) else {
                    continue;
                };
                let Some(pending) = &terminal.pending_offhost_connect else {
                    continue;
                };
                if pending.resolving {
                    continue;
                }
                let start = match &terminal.container {
                    Some(launch) => OffHostStart::Lookup(Box::new(launch.clone())),
                    None => OffHostStart::Spawn,
                };
                starts.push((terminal_id.clone(), start));
            }
        }
    }
    starts
}

/// Records a finished container lookup. `false` when the pane no longer waits,
/// for example because it was closed or connected during the lookup.
///
/// Why keep the saved launch when the lookup finds nothing: a host that needs
/// a password cannot answer a background lookup, but the pane's own `ssh` can
/// still ask for it.
fn apply_container_lookup(terminal: &mut TerminalState, container_id: Option<String>) -> bool {
    let Some(pending) = terminal.pending_offhost_connect.as_mut() else {
        return false;
    };
    pending.resolving = false;
    if let (Some(launch), Some(id)) = (terminal.container.as_mut(), container_id) {
        launch.id = id;
    }
    true
}

/// The running container the pane should enter: the saved one while it runs,
/// otherwise whatever its name or compose service runs now.
///
/// Why not plan the workdir again like `container.open`: the pane keeps the
/// directory and user it was opened with.
fn running_container_id(launch: &ContainerLaunch) -> Option<String> {
    let host = DockerHost::for_launch(launch.ssh.as_ref());
    let running = |reference: &ContainerRef| {
        crate::container::docker::inspect_reference(&host, reference)
            .ok()
            .flatten()
            .filter(|info| info.running)
            .map(|info| info.id)
    };
    // `docker inspect` takes an id wherever it takes a name.
    running(&ContainerRef::Name(launch.id.clone())).or_else(|| running(&launch.reference))
}

fn pane_of_terminal(
    state: &AppState,
    terminal_id: &TerminalId,
) -> Option<(usize, crate::layout::PaneId)> {
    state
        .workspaces
        .iter()
        .enumerate()
        .find_map(|(ws_idx, ws)| {
            ws.tabs.iter().find_map(|tab| {
                tab.panes
                    .iter()
                    .find(|(_, pane)| &pane.attached_terminal_id == terminal_id)
                    .map(|(pane_id, _)| (ws_idx, *pane_id))
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn container_launch(id: &str) -> ContainerLaunch {
        ContainerLaunch {
            reference: ContainerRef::Name("scratch".into()),
            canonical: "scratch".into(),
            id: id.into(),
            workdir: Some("/work".into()),
            start_dir: Some("/work/sub".into()),
            user: None,
            shell: crate::container::ContainerShell::BashOrSh,
            ssh: None,
        }
    }

    fn state_with_panes(count: usize) -> (AppState, Vec<TerminalId>) {
        let mut state = AppState::test_new();
        let mut ws = crate::workspace::Workspace::test_new("offhost");
        for _ in 1..count {
            ws.test_split(ratatui::layout::Direction::Horizontal);
        }
        state.workspaces = vec![ws];
        state.active = Some(0);
        state.ensure_test_terminals();
        let ids = state.workspaces[0].tabs[0]
            .layout
            .pane_ids()
            .into_iter()
            .map(|pane| {
                state.workspaces[0].tabs[0].panes[&pane]
                    .attached_terminal_id
                    .clone()
            })
            .collect();
        (state, ids)
    }

    #[test]
    fn waiting_ssh_panes_start_at_once_and_container_panes_look_up_first() {
        let (mut state, ids) = state_with_panes(3);
        let ssh = crate::ssh_host::SshLaunch::new("prod".into(), None).unwrap();
        let ssh_terminal = state.terminals.get_mut(&ids[0]).unwrap();
        ssh_terminal.ssh = Some(ssh);
        ssh_terminal.pending_offhost_connect = Some(Default::default());
        let container_terminal = state.terminals.get_mut(&ids[1]).unwrap();
        container_terminal.container = Some(container_launch("old-id"));
        container_terminal.pending_offhost_connect = Some(Default::default());

        let mut starts = pending_offhost_starts(&state, &TerminalRuntimeRegistry::new());
        starts.sort_by_key(|(id, _)| ids.iter().position(|known| known == id));

        assert_eq!(
            starts,
            vec![
                (ids[0].clone(), OffHostStart::Spawn),
                (
                    ids[1].clone(),
                    OffHostStart::Lookup(Box::new(container_launch("old-id")))
                ),
            ],
            "a host pane that does not wait starts nothing"
        );
        state.assert_invariants_for_test();
    }

    #[test]
    fn a_container_pane_whose_lookup_runs_is_not_looked_up_again() {
        let (mut state, ids) = state_with_panes(1);
        let terminal = state.terminals.get_mut(&ids[0]).unwrap();
        terminal.container = Some(container_launch("old-id"));
        terminal.pending_offhost_connect = Some(crate::terminal::PendingOffHostConnect {
            initial_history_ansi: None,
            resolving: true,
        });

        assert!(pending_offhost_starts(&state, &TerminalRuntimeRegistry::new()).is_empty());
    }

    #[test]
    fn a_finished_lookup_takes_the_running_container_id() {
        let mut terminal = TerminalState::new(TerminalId::alloc(), "/host".into())
            .with_container(container_launch("old-id"))
            .with_pending_offhost_connect(Some("history".into()));
        terminal.pending_offhost_connect.as_mut().unwrap().resolving = true;

        assert!(apply_container_lookup(&mut terminal, Some("new-id".into())));

        assert_eq!(terminal.container.as_ref().unwrap().id, "new-id");
        assert_eq!(
            terminal.container.as_ref().unwrap().start_dir,
            Some("/work/sub".into()),
            "the pane keeps the directory it opens in"
        );
        assert_eq!(
            terminal.pending_offhost_connect,
            Some(crate::terminal::PendingOffHostConnect {
                initial_history_ansi: Some("history".into()),
                resolving: false,
            })
        );
    }

    #[test]
    fn a_lookup_that_finds_nothing_keeps_the_saved_container_id() {
        let mut terminal = TerminalState::new(TerminalId::alloc(), "/host".into())
            .with_container(container_launch("old-id"))
            .with_pending_offhost_connect(None);

        assert!(apply_container_lookup(&mut terminal, None));

        assert_eq!(terminal.container.as_ref().unwrap().id, "old-id");
    }

    #[test]
    fn a_lookup_for_a_pane_that_stopped_waiting_starts_nothing() {
        let mut terminal = TerminalState::new(TerminalId::alloc(), "/host".into())
            .with_container(container_launch("old-id"));

        assert!(!apply_container_lookup(
            &mut terminal,
            Some("new-id".into())
        ));

        assert_eq!(terminal.container.as_ref().unwrap().id, "old-id");
    }
}
