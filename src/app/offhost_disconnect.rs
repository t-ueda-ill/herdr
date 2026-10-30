//! Keeps a container or ssh pane whose connection ended with an error, and
//! connects it again.

use crate::layout::PaneId;
use crate::platform::ChildExitReason;
use crate::terminal::Disconnected;

use super::App;

/// Why reconnecting a pane was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReconnectRefusal {
    PaneNotFound,
    NotDisconnected,
}

impl App {
    /// Whether an exited pane stays as a disconnected pane instead of closing.
    ///
    /// Why not the saved launch metadata alone: a pane keeps it while a host
    /// shell runs, and that shell's exit closes the pane as before.
    pub(super) fn keeps_disconnected_pane(
        &self,
        pane_id: PaneId,
        exit_reason: ChildExitReason,
        exit_code: Option<u32>,
    ) -> bool {
        let runs_off_host = self.find_pane(pane_id).is_some_and(|(_, pane)| {
            self.terminal_runtimes
                .get(&pane.attached_terminal_id)
                .is_some_and(crate::terminal::TerminalRuntime::shell_runs_off_host)
        });
        runs_off_host && connection_failed(exit_reason, exit_code)
    }

    /// Keeps the exited runtime: its screen holds what ssh or docker printed.
    pub(super) fn mark_pane_disconnected(&mut self, pane_id: PaneId, exit_code: Option<u32>) {
        let Some(terminal_id) = self
            .find_pane(pane_id)
            .map(|(_, pane)| pane.attached_terminal_id.clone())
        else {
            return;
        };
        if let Some(terminal) = self.state.terminals.get_mut(&terminal_id) {
            terminal.disconnected = Some(Disconnected { exit_code });
            terminal.revision = terminal.revision.saturating_add(1);
        }
        self.render_dirty.request_generic();
        self.render_notify.notify_one();
    }

    pub(crate) fn reconnect_offhost_pane(
        &mut self,
        pane_id: PaneId,
    ) -> Result<(), ReconnectRefusal> {
        self.begin_offhost_reconnect(pane_id)?;
        self.start_pending_offhost_connects();
        self.render_dirty.request_generic();
        self.render_notify.notify_one();
        Ok(())
    }

    /// Closes the pane as `pane.close` does, from the disconnect bar.
    pub(crate) fn close_disconnected_pane(&mut self, pane_id: PaneId) {
        let Some(public_pane_id) = self
            .find_pane(pane_id)
            .and_then(|(ws_idx, _)| self.public_pane_id(ws_idx, pane_id))
        else {
            return;
        };
        let target = crate::api::schema::PaneTarget {
            pane_id: public_pane_id,
        };
        if let Err(response) = self.close_pane("disconnect-bar".into(), &target) {
            tracing::warn!(pane = pane_id.raw(), %response, "disconnect bar could not close the pane");
        }
    }

    /// Hands the pane to the restore path, which looks the container up again
    /// and starts the shell, keeping the exited screen as its history.
    fn begin_offhost_reconnect(&mut self, pane_id: PaneId) -> Result<(), ReconnectRefusal> {
        let terminal_id = self
            .find_pane(pane_id)
            .map(|(_, pane)| pane.attached_terminal_id.clone())
            .ok_or(ReconnectRefusal::PaneNotFound)?;
        let terminal = self
            .state
            .terminals
            .get_mut(&terminal_id)
            .ok_or(ReconnectRefusal::PaneNotFound)?;
        if terminal.disconnected.take().is_none() {
            return Err(ReconnectRefusal::NotDisconnected);
        }
        let history = self
            .terminal_runtimes
            .remove(&terminal_id)
            .and_then(|runtime| {
                let history = runtime.snapshot_history();
                runtime.shutdown();
                history.map(ending_with_a_line_break)
            });
        terminal.pending_offhost_connect = Some(crate::terminal::PendingOffHostConnect {
            initial_history_ansi: history,
            resolving: false,
        });
        terminal.revision = terminal.revision.saturating_add(1);
        Ok(())
    }
}

/// The snapshot ends at the last printed character, so without the break the
/// new connection's first output would overwrite the old last line.
fn ending_with_a_line_break(mut history: String) -> String {
    if !history.ends_with('\n') {
        history.push_str("\r\n");
    }
    history
}

/// A nonzero code or a signal counts as failed; a clean `exit` reports 0.
///
/// Why not an end without a code as failed: a pane carried over by a live
/// handoff has no wait handle, so its clean `exit` would show the bar too.
fn connection_failed(exit_reason: ChildExitReason, exit_code: Option<u32>) -> bool {
    match exit_reason {
        ChildExitReason::Exited => exit_code != Some(0),
        ChildExitReason::Interrupted => true,
        #[cfg(unix)]
        ChildExitReason::Handoff => false,
        ChildExitReason::WaitFailed => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::AppEvent;

    struct Fixture {
        app: App,
        pane_id: PaneId,
        terminal_id: crate::terminal::TerminalId,
    }

    /// An ssh pane next to a host pane, so closing it keeps the workspace.
    fn ssh_pane(runs_off_host: bool) -> Fixture {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let mut workspace = crate::workspace::Workspace::test_new("offhost");
        let pane_id = workspace.test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        let terminal_id = app.state.workspaces[0]
            .terminal_id(pane_id)
            .unwrap()
            .clone();
        app.state.terminals.get_mut(&terminal_id).unwrap().ssh =
            crate::ssh_host::SshLaunch::new("prod".into(), None);
        let (mut runtime, _input_rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        runtime.test_set_in_ssh(runs_off_host);
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);
        Fixture {
            app,
            pane_id,
            terminal_id,
        }
    }

    fn exit(fixture: &mut Fixture, exit_reason: ChildExitReason, exit_code: Option<u32>) {
        fixture.app.handle_internal_event(AppEvent::PaneDied {
            pane_id: fixture.pane_id,
            exit_reason,
            exit_code,
        });
    }

    #[tokio::test]
    async fn an_ssh_pane_whose_connection_fails_stays_with_its_screen() {
        for (exit_reason, exit_code) in [
            (ChildExitReason::Exited, Some(255)),
            (ChildExitReason::Interrupted, None),
        ] {
            let mut fixture = ssh_pane(true);

            exit(&mut fixture, exit_reason, exit_code);

            assert!(fixture.app.find_pane(fixture.pane_id).is_some());
            assert_eq!(
                fixture.app.state.terminals[&fixture.terminal_id].disconnected,
                Some(Disconnected { exit_code })
            );
            assert!(
                fixture
                    .app
                    .terminal_runtimes
                    .get(&fixture.terminal_id)
                    .is_some(),
                "the exited screen shows what ssh printed"
            );
            fixture.app.state.assert_invariants_for_test();
        }
    }

    #[tokio::test]
    async fn a_clean_exit_an_unknown_end_or_a_host_shell_closes_the_pane() {
        for (runs_off_host, exit_reason, exit_code) in [
            (true, ChildExitReason::Exited, Some(0)),
            (true, ChildExitReason::WaitFailed, None),
            #[cfg(unix)]
            (true, ChildExitReason::Handoff, None),
            (false, ChildExitReason::Exited, Some(1)),
        ] {
            let mut fixture = ssh_pane(runs_off_host);

            exit(&mut fixture, exit_reason, exit_code);

            assert!(fixture.app.find_pane(fixture.pane_id).is_none());
            fixture.app.state.assert_invariants_for_test();
        }
    }

    #[tokio::test]
    async fn reconnecting_hands_the_pane_to_the_restore_path_with_its_screen() {
        let mut fixture = ssh_pane(true);
        exit(&mut fixture, ChildExitReason::Exited, Some(255));
        fixture
            .app
            .terminal_runtimes
            .get(&fixture.terminal_id)
            .unwrap()
            .test_process_pty_bytes(b"Connection to prod closed.");

        fixture
            .app
            .begin_offhost_reconnect(fixture.pane_id)
            .unwrap();

        let terminal = &fixture.app.state.terminals[&fixture.terminal_id];
        assert_eq!(terminal.disconnected, None);
        assert!(fixture
            .app
            .terminal_runtimes
            .get(&fixture.terminal_id)
            .is_none());
        let history = terminal
            .pending_offhost_connect
            .as_ref()
            .and_then(|pending| pending.initial_history_ansi.as_deref())
            .unwrap_or_default();
        assert!(
            history.contains("Connection to prod closed."),
            "{history:?}"
        );
        assert!(
            history.ends_with('\n'),
            "the next connection starts below the last one's output: {history:?}"
        );
    }

    #[tokio::test]
    async fn a_connected_pane_is_not_reconnected() {
        let mut fixture = ssh_pane(true);

        assert_eq!(
            fixture.app.reconnect_offhost_pane(fixture.pane_id),
            Err(ReconnectRefusal::NotDisconnected)
        );
        assert!(fixture
            .app
            .terminal_runtimes
            .get(&fixture.terminal_id)
            .is_some());
        assert_eq!(
            fixture.app.reconnect_offhost_pane(PaneId::from_raw(9999)),
            Err(ReconnectRefusal::PaneNotFound)
        );
    }
}
