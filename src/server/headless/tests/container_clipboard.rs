use super::off_host_clipboard_fixture::{
    cleanup_of, rejection, stage_target_of, staged_event, staged_image, OffHostPaneFixture,
    OffHostShell, CLIENT,
};
#[cfg(not(windows))]
use super::off_host_clipboard_fixture::{staged_file, STAGED_PATH};
use super::*;
#[cfg(not(windows))]
use crate::server::clipboard_image::RemoteDockerHost;
use crate::server::clipboard_image::{test_cleanup, OffHostTarget};

const CONTAINER_ID: &str = "cid-1";

fn launch() -> crate::container::ContainerLaunch {
    crate::container::ContainerLaunch {
        reference: crate::container::ContainerRef::Compose {
            project: "shop".into(),
            service: "db".into(),
        },
        canonical: "shop/db".into(),
        id: CONTAINER_ID.into(),
        workdir: None,
        user: Some("node".into()),
        shell: crate::container::ContainerShell::BashOrSh,
        ssh: None,
    }
}

fn container_pane(running: bool) -> OffHostPaneFixture {
    OffHostPaneFixture::new(OffHostShell::Container(launch()), running)
}

fn container_target(container_id: &str) -> OffHostTarget {
    OffHostTarget::Container {
        id: container_id.into(),
        user: Some("node".into()),
        ssh: None,
    }
}

#[cfg(not(windows))]
const REMOTE_CONTROL_PATH: &str = "/tmp/hssh-1000/0123abcd-%C";

#[cfg(not(windows))]
fn remote_launch(control_path: Option<&str>) -> crate::container::ContainerLaunch {
    crate::container::ContainerLaunch {
        ssh: crate::ssh_host::SshLaunch::new("deploy@prod".into(), control_path.map(Into::into)),
        ..launch()
    }
}

#[cfg(not(windows))]
fn remote_container_target() -> OffHostTarget {
    OffHostTarget::Container {
        id: CONTAINER_ID.into(),
        user: Some("node".into()),
        ssh: Some(RemoteDockerHost {
            host: "deploy@prod".into(),
            control_path: REMOTE_CONTROL_PATH.into(),
        }),
    }
}

// Why not on Windows: container panes are rejected there, so the server
// never treats a shell as running in a container.
#[cfg(not(windows))]
#[tokio::test]
async fn staged_container_image_pastes_the_in_container_path_and_is_tracked_for_cleanup() {
    let mut fixture = container_pane(true);
    let event = fixture.staged(container_target(CONTAINER_ID), Ok(staged_image()));

    assert!(fixture.server.handle_internal_event_with_forwarding(event));

    assert_eq!(
        fixture.input_rx.try_recv().expect("pasted container path"),
        Bytes::from_static(STAGED_PATH.as_bytes())
    );
    let client = fixture.client();
    assert_eq!(
        client.staged_off_host_clipboard_files,
        vec![staged_file(container_target(CONTAINER_ID))]
    );
    assert!(client.staged_clipboard_files.is_empty());
    assert!(test_cleanup::take().is_empty());
    fixture.finish();
}

#[tokio::test]
async fn staged_container_image_for_a_stale_target_is_removed_instead_of_pasted() {
    type Mutation = fn(&mut OffHostPaneFixture) -> Option<AppEvent>;
    let cases: [(&str, Mutation); 4] = [
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
                container_target(CONTAINER_ID),
                Ok(staged_image()),
            ))
        }),
        ("pane now runs in another container", |fixture| {
            Some(fixture.staged(container_target("cid-old"), Ok(staged_image())))
        }),
    ];
    for (case, mutate) in cases {
        let mut fixture = container_pane(true);
        let event = mutate(&mut fixture)
            .unwrap_or_else(|| fixture.staged(container_target(CONTAINER_ID), Ok(staged_image())));
        let stage_target = stage_target_of(&event);

        assert!(
            !fixture.server.handle_internal_event_with_forwarding(event),
            "{case}"
        );

        fixture.assert_nothing_pasted();
        if let Some(client) = fixture.server.clients.get(&CLIENT) {
            assert!(client.staged_off_host_clipboard_files.is_empty(), "{case}");
        }
        assert_eq!(test_cleanup::take(), cleanup_of(stage_target), "{case}");
        fixture.finish();
    }
}

#[tokio::test]
async fn failed_container_stage_notifies_only_the_requesting_client() {
    let mut fixture = container_pane(true);
    let (other_writer, other_control_rx, _other_render_rx) = test_client_writer();
    fixture.server.clients.insert(
        12,
        ClientConnection::new_with_mode(
            ClientConnectionMode::ClientShell,
            (80, 24),
            crate::kitty_graphics::HostCellSize::default(),
            2,
            RenderEncoding::SemanticFrame,
            Some(other_writer),
        ),
    );
    let event = fixture.staged(
        container_target(CONTAINER_ID),
        Err("docker exited with status 126: sh not found".into()),
    );

    assert!(!fixture.server.handle_internal_event_with_forwarding(event));

    assert_eq!(
        rejection(&fixture.control_rx),
        "Paste rejected: Image could not be copied into the container: \
         docker exited with status 126: sh not found"
    );
    assert!(other_control_rx
        .recv_timeout(Duration::from_millis(50))
        .is_err());
    fixture.assert_nothing_pasted();
    assert!(fixture.client().staged_off_host_clipboard_files.is_empty());
    assert!(test_cleanup::take().is_empty());
    fixture.finish();
}

// Why not on Windows: container panes are rejected there, so the server
// never treats a shell as running in a container.
#[cfg(not(windows))]
#[tokio::test]
async fn container_pane_image_never_falls_back_to_a_host_file() {
    let mut fixture = container_pane(true);
    crate::thread_spawn::test_hook::fail_next_spawns(1);

    assert!(!fixture.paste_image());

    assert!(rejection(&fixture.control_rx)
        .starts_with("Paste rejected: Image could not be copied into the container:"));
    fixture.assert_nothing_pasted();
    let client = fixture.client();
    assert!(client.staged_clipboard_files.is_empty());
    assert!(client.staged_off_host_clipboard_files.is_empty());
    fixture.finish();
}

#[tokio::test]
async fn cold_restored_container_pane_stages_the_image_on_the_host() {
    let mut fixture = container_pane(false);

    assert!(fixture.paste_image());

    fixture.assert_host_staged();
    fixture.finish();
}

// Why not on Windows: container panes are rejected there, so the server
// never treats a shell as running in a container.
#[cfg(not(windows))]
#[tokio::test]
async fn client_shell_snapshot_advertises_only_running_container_panes() {
    for (running, expected) in [(true, Some("shop/db")), (false, None)] {
        let fixture = container_pane(running);
        let snapshot =
            crate::server::client_shell::snapshot(&fixture.server.app, "boot", 1, None, None);
        assert_eq!(
            snapshot.panes[0].container.as_deref(),
            expected,
            "running={running}"
        );
        assert_eq!(snapshot.panes[0].ssh_host, None, "running={running}");
        assert_eq!(
            fixture.server.app.session_snapshot().panes[0]
                .container
                .as_deref(),
            Some("shop/db"),
            "public pane metadata keeps the restored container either way"
        );
        fixture.finish();
    }
}

// Why not on Windows: ssh and container panes are rejected there.
#[cfg(not(windows))]
#[tokio::test]
async fn staged_remote_container_image_pastes_the_path_and_tracks_the_remote_target() {
    let mut fixture = OffHostPaneFixture::new(
        OffHostShell::Container(remote_launch(Some(REMOTE_CONTROL_PATH))),
        true,
    );
    let event = fixture.staged(remote_container_target(), Ok(staged_image()));

    assert!(fixture.server.handle_internal_event_with_forwarding(event));

    assert_eq!(
        fixture.input_rx.try_recv().expect("pasted container path"),
        Bytes::from_static(STAGED_PATH.as_bytes())
    );
    assert_eq!(
        fixture.client().staged_off_host_clipboard_files,
        vec![staged_file(remote_container_target())]
    );
    fixture.finish();
}

#[cfg(not(windows))]
#[tokio::test]
async fn staged_remote_container_image_is_removed_when_the_pane_moved_to_the_local_docker() {
    let mut fixture = container_pane(true);
    let event = fixture.staged(remote_container_target(), Ok(staged_image()));
    let stage_target = stage_target_of(&event);

    assert!(!fixture.server.handle_internal_event_with_forwarding(event));

    fixture.assert_nothing_pasted();
    assert_eq!(test_cleanup::take(), cleanup_of(stage_target));
    fixture.finish();
}

#[cfg(not(windows))]
#[tokio::test]
async fn remote_container_pane_without_a_shared_connection_never_reaches_the_local_docker() {
    let mut fixture = OffHostPaneFixture::new(OffHostShell::Container(remote_launch(None)), true);

    assert!(!fixture.paste_image());

    assert_eq!(
        rejection(&fixture.control_rx),
        "Paste rejected: Image could not be copied into the container on the ssh host: \
         the connection is not shared (ControlMaster unavailable)"
    );
    fixture.assert_nothing_pasted();
    let client = fixture.client();
    assert!(client.staged_clipboard_files.is_empty());
    assert!(client.staged_off_host_clipboard_files.is_empty());
    fixture.finish();
}

#[cfg(not(windows))]
#[tokio::test]
async fn client_shell_snapshot_names_the_ssh_host_of_a_remote_container_pane() {
    let fixture = OffHostPaneFixture::new(
        OffHostShell::Container(remote_launch(Some(REMOTE_CONTROL_PATH))),
        true,
    );

    let snapshot =
        crate::server::client_shell::snapshot(&fixture.server.app, "boot", 1, None, None);

    assert_eq!(snapshot.panes[0].container.as_deref(), Some("shop/db"));
    assert_eq!(snapshot.panes[0].ssh_host.as_deref(), Some("prod"));
    assert_eq!(
        fixture.server.app.session_snapshot().panes[0]
            .ssh_host
            .as_deref(),
        Some("prod"),
        "pane info shows the host that runs the container's docker"
    );
    fixture.finish();
}
