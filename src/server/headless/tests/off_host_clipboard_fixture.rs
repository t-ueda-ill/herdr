//! A focused pane whose shell runs off the server host, shared by the
//! container and ssh clipboard image tests.

use super::*;
use crate::server::clipboard_image::{
    test_cleanup, OffHostCleanupBatch, OffHostClipboardFile, OffHostTarget, StagedClipboardImage,
};

pub(super) const CLIENT: u64 = 11;
pub(super) const STAGED_PATH: &str =
    "/tmp/herdr-clipboard-image-aB3dE9/client-11-clipboard-1-0.png";

/// The launch metadata a pane carries; whether it runs is a separate fact.
#[derive(Clone)]
pub(super) enum OffHostShell {
    Container(crate::container::ContainerLaunch),
    Ssh(crate::ssh_host::SshLaunch),
}

pub(super) struct OffHostPaneFixture {
    pub(super) server: HeadlessServer,
    pub(super) input_rx: tokio::sync::mpsc::Receiver<Bytes>,
    pub(super) control_rx: std::sync::mpsc::Receiver<Vec<u8>>,
    shell: OffHostShell,
    terminal_id: crate::terminal::TerminalId,
    public_pane_id: String,
}

impl OffHostPaneFixture {
    /// `running` is the runtime fact: false models a cold restore that kept
    /// the metadata but started a host shell.
    pub(super) fn new(shell: OffHostShell, running: bool) -> Self {
        let mut server = test_headless_server();
        let workspace = crate::workspace::Workspace::test_new("off-host");
        let pane_id = workspace.tabs[0].root_pane;
        server.app.state.workspaces = vec![workspace];
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        server.app.state.mode = crate::app::Mode::Terminal;
        server.app.state.ensure_test_terminals();
        let terminal_id = server.app.state.workspaces[0]
            .terminal_id(pane_id)
            .expect("root pane terminal")
            .clone();
        // In the registry rather than the workspace so direct terminal
        // attaches resolve the same runtime as pane targets.
        let (runtime, input_rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        server
            .app
            .terminal_runtimes
            .insert(terminal_id.clone(), runtime);
        let (writer, control_rx, _render_rx) = test_client_writer();
        server.clients.insert(
            CLIENT,
            ClientConnection::new_with_mode(
                ClientConnectionMode::ClientShell,
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                1,
                RenderEncoding::SemanticFrame,
                Some(writer),
            ),
        );
        let public_pane_id = server
            .app
            .session_snapshot()
            .focused_pane_id
            .expect("focused pane");
        let mut fixture = Self {
            server,
            input_rx,
            control_rx,
            shell: shell.clone(),
            terminal_id,
            public_pane_id,
        };
        fixture.set_launch(shell);
        fixture.set_running(running);
        fixture
    }

    pub(super) fn target(&self) -> crate::protocol::ClientClipboardImageTarget {
        crate::protocol::ClientClipboardImageTarget::Pane(self.public_pane_id.clone())
    }

    pub(super) fn staged(
        &self,
        stage_target: OffHostTarget,
        result: Result<StagedClipboardImage, String>,
    ) -> AppEvent {
        staged_event(self.target(), stage_target, result)
    }

    /// Replaces the stored launch, as a respawn into a new target would.
    pub(super) fn set_launch(&mut self, shell: OffHostShell) {
        let terminal = self
            .server
            .app
            .state
            .terminals
            .get_mut(&self.terminal_id)
            .expect("root pane terminal state");
        match &shell {
            OffHostShell::Container(launch) => terminal.container = Some(launch.clone()),
            OffHostShell::Ssh(launch) => terminal.ssh = Some(launch.clone()),
        }
        self.shell = shell;
    }

    pub(super) fn set_running(&mut self, running: bool) {
        let registry = &mut self.server.app.terminal_runtimes;
        let mut runtime = registry.remove(&self.terminal_id).expect("test runtime");
        match self.shell {
            OffHostShell::Container(_) => runtime.test_set_in_container(running),
            OffHostShell::Ssh(_) => runtime.test_set_in_ssh(running),
        }
        registry.insert(self.terminal_id.clone(), runtime);
    }

    /// Connects a second client attached directly to the pane's terminal.
    pub(super) fn attach_terminal_client(
        &mut self,
        client_id: u64,
    ) -> std::sync::mpsc::Receiver<Vec<u8>> {
        let (writer, control_rx, _render_rx) = test_client_writer();
        self.server.clients.insert(
            client_id,
            ClientConnection::new_with_mode(
                ClientConnectionMode::TerminalAttach {
                    terminal_id: self.terminal_id.to_string(),
                },
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                2,
                RenderEncoding::SemanticFrame,
                Some(writer),
            ),
        );
        control_rx
    }

    pub(super) fn client(&mut self) -> &mut ClientConnection {
        self.server.clients.get_mut(&CLIENT).expect("client")
    }

    pub(super) fn assert_nothing_pasted(&mut self) {
        assert!(self.input_rx.try_recv().is_err(), "nothing may be pasted");
    }

    pub(super) fn paste_image(&mut self) -> bool {
        let target = self.target();
        self.server
            .handle_server_event(ServerEvent::ClientClipboardImage {
                client_id: CLIENT,
                target,
                extension: "png".into(),
                data: b"image".to_vec(),
            })
    }

    /// Asserts a host file was staged and its path pasted, then removes it.
    pub(super) fn assert_host_staged(&mut self) {
        let staged = std::mem::take(&mut self.client().staged_clipboard_files);
        assert_eq!(staged.len(), 1);
        assert!(self.client().staged_off_host_clipboard_files.is_empty());
        assert_eq!(
            std::fs::read(&staged[0]).expect("host staged file"),
            b"image"
        );
        assert_eq!(
            self.input_rx.try_recv().expect("pasted host path"),
            Bytes::from(staged[0].to_string_lossy().into_owned())
        );
        crate::server::clipboard_image::remove_files(staged);
    }

    pub(super) fn finish(mut self) {
        // Drop would otherwise request off-host cleanups for tracked files.
        if let Some(client) = self.server.clients.get_mut(&CLIENT) {
            client.staged_off_host_clipboard_files.clear();
        }
        shutdown_test_runtimes(&mut self.server);
        let _ = test_cleanup::take();
    }
}

pub(super) fn staged_event(
    target: crate::protocol::ClientClipboardImageTarget,
    stage_target: OffHostTarget,
    result: Result<StagedClipboardImage, String>,
) -> AppEvent {
    AppEvent::ClipboardImageStaged(Box::new(crate::events::ClipboardImageStagedResult {
        client_id: CLIENT,
        target,
        stage_target,
        result,
    }))
}

pub(super) fn staged_image() -> StagedClipboardImage {
    StagedClipboardImage {
        path: STAGED_PATH.into(),
        paste_text: STAGED_PATH.into(),
    }
}

pub(super) fn staged_file(target: OffHostTarget) -> OffHostClipboardFile {
    OffHostClipboardFile {
        target,
        path: STAGED_PATH.into(),
    }
}

pub(super) fn cleanup_of(target: OffHostTarget) -> Vec<OffHostCleanupBatch> {
    vec![OffHostCleanupBatch {
        target,
        paths: vec![STAGED_PATH.into()],
    }]
}

pub(super) fn stage_target_of(event: &AppEvent) -> OffHostTarget {
    match event {
        AppEvent::ClipboardImageStaged(staged) => staged.stage_target.clone(),
        _ => unreachable!("not a staged clipboard image event"),
    }
}

pub(super) fn rejection(control_rx: &std::sync::mpsc::Receiver<Vec<u8>>) -> String {
    match read_server_message(
        control_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("client-local rejection"),
    ) {
        ServerMessage::ClientShellError { message } => message,
        other => panic!("expected client shell error, got {other:?}"),
    }
}
