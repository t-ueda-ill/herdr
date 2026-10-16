use super::off_host_clipboard_fixture::{
    cleanup_of, rejection, stage_target_of, staged_event, staged_file, staged_image,
    OffHostPaneFixture, OffHostShell, CLIENT, STAGED_PATH,
};
use super::*;
use crate::server::clipboard_image::{test_cleanup, OffHostTarget};

const HOST: &str = "deploy@prod";
const CONTROL_PATH: &str = "/tmp/hssh-1000/0123abcd-%C";
const NO_SHARED_CONNECTION: &str = "Paste rejected: Image could not be copied to the ssh host: \
     the pane's ssh connection is not shared (ControlMaster unavailable)";

fn launch(host: &str, control_path: Option<&str>) -> crate::ssh_host::SshLaunch {
    crate::ssh_host::SshLaunch::new(host.into(), control_path.map(Into::into))
        .expect("valid ssh destination")
}

fn ssh_pane(running: bool) -> OffHostPaneFixture {
    OffHostPaneFixture::new(OffHostShell::Ssh(launch(HOST, Some(CONTROL_PATH))), running)
}

fn ssh_target(host: &str, control_path: &str) -> OffHostTarget {
    OffHostTarget::Ssh {
        host: host.into(),
        control_path: control_path.into(),
    }
}

#[tokio::test]
async fn staged_ssh_image_pastes_the_remote_path_and_is_tracked_for_cleanup() {
    let mut fixture = ssh_pane(true);
    let event = fixture.staged(ssh_target(HOST, CONTROL_PATH), Ok(staged_image()));

    assert!(fixture.server.handle_internal_event_with_forwarding(event));

    assert_eq!(
        fixture.input_rx.try_recv().expect("pasted remote path"),
        Bytes::from_static(STAGED_PATH.as_bytes())
    );
    let client = fixture.client();
    assert_eq!(
        client.staged_off_host_clipboard_files,
        vec![staged_file(ssh_target(HOST, CONTROL_PATH))]
    );
    assert!(client.staged_clipboard_files.is_empty());
    assert!(test_cleanup::take().is_empty());
    fixture.finish();
}

#[tokio::test]
async fn staged_ssh_image_for_a_stale_target_is_removed_instead_of_pasted() {
    type Mutation = fn(&mut OffHostPaneFixture) -> Option<AppEvent>;
    let cases: [(&str, Mutation); 6] = [
        ("pane respawned on the host", |fixture| {
            fixture.set_running(false);
            None
        }),
        ("client disconnected", |fixture| {
            fixture.server.clients.remove(&CLIENT);
            None
        }),
        ("pane closed", |_fixture| {
            Some(staged_event(
                crate::protocol::ClientClipboardImageTarget::Pane("w_closed:p_closed".into()),
                ssh_target(HOST, CONTROL_PATH),
                Ok(staged_image()),
            ))
        }),
        ("pane now logs in to another host", |fixture| {
            fixture.set_launch(OffHostShell::Ssh(launch("staging", Some(CONTROL_PATH))));
            None
        }),
        ("pane now multiplexes through another master", |fixture| {
            fixture.set_launch(OffHostShell::Ssh(launch(
                HOST,
                Some("/tmp/hssh-1000/other-%C"),
            )));
            None
        }),
        ("pane lost its shared connection", |fixture| {
            fixture.set_launch(OffHostShell::Ssh(launch(HOST, None)));
            None
        }),
    ];
    for (case, mutate) in cases {
        let mut fixture = ssh_pane(true);
        let event = mutate(&mut fixture)
            .unwrap_or_else(|| fixture.staged(ssh_target(HOST, CONTROL_PATH), Ok(staged_image())));
        let stage_target = stage_target_of(&event);

        assert!(
            !fixture.server.handle_internal_event_with_forwarding(event),
            "{case}"
        );

        fixture.assert_nothing_pasted();
        if let Some(client) = fixture.server.clients.get(&CLIENT) {
            assert!(client.staged_off_host_clipboard_files.is_empty(), "{case}");
            assert!(client.staged_clipboard_files.is_empty(), "{case}");
        }
        assert_eq!(test_cleanup::take(), cleanup_of(stage_target), "{case}");
        fixture.finish();
    }
}

#[tokio::test]
async fn failed_ssh_stage_notifies_only_the_requesting_client() {
    let mut fixture = ssh_pane(true);
    let other_control_rx = fixture.attach_terminal_client(12);
    let event = fixture.staged(
        ssh_target(HOST, CONTROL_PATH),
        Err("ssh exited with status 255: Permission denied (publickey,password).".into()),
    );

    assert!(!fixture.server.handle_internal_event_with_forwarding(event));

    assert_eq!(
        rejection(&fixture.control_rx),
        "Paste rejected: Image could not be copied to the ssh host: \
         ssh exited with status 255: Permission denied (publickey,password)."
    );
    assert!(other_control_rx
        .recv_timeout(Duration::from_millis(50))
        .is_err());
    fixture.assert_nothing_pasted();
    assert!(fixture.client().staged_off_host_clipboard_files.is_empty());
    assert!(test_cleanup::take().is_empty());
    fixture.finish();
}

#[tokio::test]
async fn ssh_pane_image_never_falls_back_to_a_host_file() {
    let mut fixture = ssh_pane(true);
    crate::thread_spawn::test_hook::fail_next_spawns(1);

    assert!(!fixture.paste_image());

    assert!(rejection(&fixture.control_rx)
        .starts_with("Paste rejected: Image could not be copied to the ssh host:"));
    fixture.assert_nothing_pasted();
    let client = fixture.client();
    assert!(client.staged_clipboard_files.is_empty());
    assert!(client.staged_off_host_clipboard_files.is_empty());
    fixture.finish();
}

#[tokio::test]
async fn ssh_pane_without_a_shared_connection_rejects_instead_of_staging_on_the_host() {
    let mut fixture = OffHostPaneFixture::new(OffHostShell::Ssh(launch(HOST, None)), true);

    assert!(!fixture.paste_image());

    // The reason is known up front, so no stage worker is started.
    assert_eq!(rejection(&fixture.control_rx), NO_SHARED_CONNECTION);
    fixture.assert_nothing_pasted();
    let client = fixture.client();
    assert!(client.staged_clipboard_files.is_empty());
    assert!(client.staged_off_host_clipboard_files.is_empty());
    fixture.finish();
}

#[tokio::test]
async fn direct_terminal_attach_to_an_ssh_pane_uses_the_ssh_backend() {
    let mut fixture = OffHostPaneFixture::new(OffHostShell::Ssh(launch(HOST, None)), true);
    let attach_rx = fixture.attach_terminal_client(12);

    assert!(!fixture
        .server
        .handle_server_event(ServerEvent::ClientClipboardImage {
            client_id: 12,
            target: crate::protocol::ClientClipboardImageTarget::DirectTerminal,
            extension: "png".into(),
            data: b"image".to_vec(),
        }));

    match read_server_message(
        attach_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("client-local rejection"),
    ) {
        ServerMessage::Notify { message, body, .. } => {
            assert_eq!(message, "Paste rejected");
            assert_eq!(
                body.as_deref(),
                NO_SHARED_CONNECTION.strip_prefix("Paste rejected: ")
            );
        }
        other => panic!("expected a toast, got {other:?}"),
    }
    fixture.assert_nothing_pasted();
    assert!(fixture
        .server
        .clients
        .get(&12)
        .is_some_and(|client| client.staged_clipboard_files.is_empty()));
    fixture.finish();
}

#[tokio::test]
async fn cold_restored_ssh_pane_stages_the_image_on_the_host() {
    for control_path in [Some(CONTROL_PATH), None] {
        let mut fixture =
            OffHostPaneFixture::new(OffHostShell::Ssh(launch(HOST, control_path)), false);

        assert!(fixture.paste_image(), "control_path={control_path:?}");

        fixture.assert_host_staged();
        fixture.finish();
    }
}

#[tokio::test]
async fn client_shell_snapshot_advertises_only_running_ssh_panes() {
    for (running, expected) in [(true, Some("prod")), (false, None)] {
        let fixture = ssh_pane(running);
        let snapshot =
            crate::server::client_shell::snapshot(&fixture.server.app, "boot", 1, None, None);
        assert_eq!(
            snapshot.panes[0].ssh_host.as_deref(),
            expected,
            "running={running}"
        );
        assert_eq!(snapshot.panes[0].container, None, "running={running}");
        assert_eq!(
            fixture.server.app.session_snapshot().panes[0]
                .ssh_host
                .as_deref(),
            Some("prod"),
            "public pane metadata keeps the restored ssh host either way"
        );
        fixture.finish();
    }
}
