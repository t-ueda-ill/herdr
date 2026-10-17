use std::sync::mpsc::Sender;

use crate::api::schema::{
    Method, Request, ResponseResult, SshHostInfoDto, SshHostsParams, SshOpenParams, SshOpenTarget,
};
use crate::app::App;
use crate::events::{AppEvent, SshHostsResult, SshOpenResult};
use crate::pane::PaneLaunchTarget;
use crate::ssh_host::launch::ssh_destination_alias;
use crate::ssh_host::{SshHostError, SshHostInfo, SshLaunch};

use super::api::responses::{encode_error, encode_success};

type ApiFailure = (String, String);

/// Returned intact by `spawn_named_with` when the lookup thread cannot start.
struct SshOpenJob {
    id: String,
    params: SshOpenParams,
    client_local: bool,
    respond_to: Sender<String>,
}

struct SshHostsJob {
    id: String,
    respond_to: Sender<String>,
}

impl App {
    pub(crate) fn handle_deferred_ssh_api_request(
        &mut self,
        request: Request,
        respond_to: Sender<String>,
        client_local: bool,
    ) -> bool {
        match request.method {
            Method::SshOpen(params) => {
                self.start_ssh_open(request.id, params, respond_to, client_local);
                true
            }
            Method::SshHosts(params) => {
                self.start_ssh_hosts(request.id, params, respond_to);
                true
            }
            _ => false,
        }
    }

    fn start_ssh_open(
        &mut self,
        id: String,
        mut params: SshOpenParams,
        respond_to: Sender<String>,
        client_local: bool,
    ) {
        if let Err((code, message)) = validate_ssh_host(&params.host) {
            let _ = respond_to.send(encode_error(id, &code, message));
            return;
        }
        if let Err((code, message)) = self.pin_open_target((&mut params.target).into()) {
            let _ = respond_to.send(encode_error(id, &code, message));
            return;
        }
        let job = SshOpenJob {
            id,
            params,
            client_local,
            respond_to,
        };
        let event_tx = self.event_tx.clone();
        let spawned = crate::thread_spawn::spawn_named_with("herdr-ssh-open", job, move |job| {
            let result = resolve_ssh_launch(&job.params.host);
            let _ = event_tx.blocking_send(AppEvent::SshOpenFinished(Box::new(SshOpenResult {
                id: job.id,
                params: job.params,
                client_local: job.client_local,
                result,
                respond_to: job.respond_to,
            })));
        });
        if let Err((job, err)) = spawned {
            tracing::warn!(err = %err, "failed to spawn ssh host lookup thread");
            let _ = job.respond_to.send(encode_error(
                job.id,
                "ssh_unavailable",
                format!("could not start ssh host lookup: {err}"),
            ));
        }
    }

    pub(crate) fn handle_ssh_open_finished(&mut self, finished: SshOpenResult) {
        let SshOpenResult {
            id,
            params,
            result,
            respond_to,
            ..
        } = finished;
        // A target closed during the lookup takes the handlers' own not-found path.
        let response = match result {
            Err((code, message)) => encode_error(id, &code, message),
            Ok(launch) => {
                let launch_target = PaneLaunchTarget::Ssh(&launch);
                match params.target {
                    SshOpenTarget::Workspace(target) => {
                        self.handle_workspace_create(id, target, launch_target)
                    }
                    SshOpenTarget::Tab(target) => self.handle_tab_create(id, target, launch_target),
                    SshOpenTarget::Split(target) => {
                        self.handle_pane_split(id, target, launch_target)
                    }
                }
            }
        };
        let _ = respond_to.send(response);
    }

    fn start_ssh_hosts(&mut self, id: String, _params: SshHostsParams, respond_to: Sender<String>) {
        if let Err((code, message)) = ensure_ssh_platform_supported() {
            let _ = respond_to.send(encode_error(id, &code, message));
            return;
        }
        let job = SshHostsJob { id, respond_to };
        let event_tx = self.event_tx.clone();
        let spawned = crate::thread_spawn::spawn_named_with("herdr-ssh-hosts", job, move |job| {
            let result = crate::ssh_host::hosts::list_configured_hosts().map_err(ssh_unavailable);
            let _ = event_tx.blocking_send(AppEvent::SshHostsFinished(Box::new(SshHostsResult {
                id: job.id,
                result,
                respond_to: job.respond_to,
            })));
        });
        if let Err((job, err)) = spawned {
            tracing::warn!(err = %err, "failed to spawn ssh host listing thread");
            let _ = job.respond_to.send(encode_error(
                job.id,
                "ssh_unavailable",
                format!("could not start ssh host listing: {err}"),
            ));
        }
    }

    pub(crate) fn handle_ssh_hosts_finished(&mut self, finished: SshHostsResult) {
        let SshHostsResult {
            id,
            result,
            respond_to,
        } = finished;
        let response = match result {
            Err((code, message)) => encode_error(id, &code, message),
            Ok(hosts) => encode_success(
                id,
                ResponseResult::SshHosts {
                    hosts: hosts.into_iter().map(ssh_host_info_dto).collect(),
                },
            ),
        };
        let _ = respond_to.send(response);
    }
}

pub(super) fn ensure_ssh_platform_supported() -> Result<(), ApiFailure> {
    // Why not attempt it: Windows OpenSSH has no ControlMaster, so the pane
    // could never carry the side channel ssh panes exist for.
    if cfg!(windows) {
        return Err((
            "platform_unsupported".into(),
            "ssh panes are not supported on Windows hosts".into(),
        ));
    }
    Ok(())
}

pub(super) fn validate_ssh_host(host: &str) -> Result<(), ApiFailure> {
    ensure_ssh_platform_supported()?;
    ssh_destination_alias(host).map(|_| ()).ok_or_else(|| {
        (
            "invalid_ssh_host".into(),
            format!(
                "invalid ssh host {host:?}; expected a Host alias from ~/.ssh/config, optionally as user@alias"
            ),
        )
    })
}

/// Reads the ssh config and may create the control directory, so it must
/// stay off the app thread.
pub(super) fn resolve_ssh_launch(host: &str) -> Result<SshLaunch, ApiFailure> {
    let hosts = crate::ssh_host::hosts::list_configured_hosts().map_err(ssh_unavailable)?;
    launch_for_configured_host(host, &hosts, crate::ssh_host::control::pane_control_path)
}

fn launch_for_configured_host(
    host: &str,
    configured: &[SshHostInfo],
    control_path: impl FnOnce(&str) -> Option<std::path::PathBuf>,
) -> Result<SshLaunch, ApiFailure> {
    let not_found = || {
        (
            "ssh_host_not_found".to_string(),
            format!("no Host alias in ~/.ssh/config matches {host}"),
        )
    };
    let alias = ssh_destination_alias(host).ok_or_else(not_found)?;
    // Why not let ssh resolve unknown names: herdr only opens hosts the user
    // configured, so a typo fails here instead of in a pane that just exits.
    if !configured.iter().any(|info| info.alias == alias) {
        return Err(not_found());
    }
    SshLaunch::new(host.to_owned(), control_path(host)).ok_or_else(not_found)
}

fn ssh_unavailable(err: SshHostError) -> ApiFailure {
    tracing::warn!(err = %err, "ssh host lookup failed");
    ("ssh_unavailable".into(), err.to_string())
}

fn ssh_host_info_dto(info: SshHostInfo) -> SshHostInfoDto {
    SshHostInfoDto {
        alias: info.alias,
        hostname: info.hostname,
        user: info.user,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::api::schema::{
        ErrorResponse, PaneSplitParams, SplitDirection, SuccessResponse, TabCreateParams,
        WorkspaceCreateParams,
    };
    use crate::config::Config;
    use crate::workspace::Workspace;

    fn test_app() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.default_shell = crate::app::exiting_test_command().into();
        app
    }

    fn error_code(response: &str) -> String {
        serde_json::from_str::<ErrorResponse>(response)
            .unwrap_or_else(|err| panic!("expected error response, got {response}: {err}"))
            .error
            .code
    }

    fn platform_or(code: &str) -> &str {
        if cfg!(windows) {
            "platform_unsupported"
        } else {
            code
        }
    }

    fn workspace_target() -> SshOpenTarget {
        SshOpenTarget::Workspace(WorkspaceCreateParams {
            source_workspace_id: None,
            cwd: None,
            focus: false,
            label: None,
            env: Default::default(),
        })
    }

    fn ssh_open(host: &str, target: SshOpenTarget) -> Request {
        Request {
            id: "open".into(),
            method: Method::SshOpen(SshOpenParams {
                host: host.into(),
                target,
            }),
        }
    }

    fn split_params(target_pane_id: Option<&str>) -> PaneSplitParams {
        PaneSplitParams {
            target_pane_id: target_pane_id.map(str::to_owned),
            workspace_id: None,
            direction: SplitDirection::Right,
            ratio: None,
            cwd: None,
            focus: false,
            env: Default::default(),
            right_click: Default::default(),
        }
    }

    fn tab_params(workspace_id: Option<&str>, cwd: Option<&str>) -> TabCreateParams {
        TabCreateParams {
            workspace_id: workspace_id.map(str::to_owned),
            cwd: cwd.map(str::to_owned),
            focus: false,
            label: None,
            env: Default::default(),
        }
    }

    fn host(alias: &str) -> SshHostInfo {
        SshHostInfo {
            alias: alias.into(),
            hostname: None,
            user: None,
        }
    }

    #[test]
    fn ssh_methods_are_rejected_by_synchronous_dispatch() {
        let mut app = test_app();

        for request in [
            ssh_open("prod", workspace_target()),
            Request {
                id: "hosts".into(),
                method: Method::SshHosts(SshHostsParams::default()),
            },
        ] {
            assert_eq!(
                error_code(&app.handle_api_request(request)),
                "invalid_request"
            );
        }
    }

    #[test]
    fn ssh_open_rejects_invalid_host_before_reading_config() {
        for bad in ["-oProxyCommand=sh", "prod;id", "", "@prod"] {
            let mut app = test_app();
            let (respond_to, response_rx) = std::sync::mpsc::channel();

            assert!(app.handle_deferred_ssh_api_request(
                ssh_open(bad, workspace_target()),
                respond_to,
                false,
            ));

            let response = response_rx
                .try_recv()
                .expect("an invalid host is rejected without a config lookup");
            assert_eq!(
                error_code(&response),
                platform_or("invalid_ssh_host"),
                "{bad:?}"
            );
            assert!(app.state.workspaces.is_empty());
            assert!(app.event_rx.try_recv().is_err());
        }
    }

    #[test]
    fn ssh_open_rejects_missing_split_target_before_reading_config() {
        let mut app = test_app();
        app.state.workspaces = vec![Workspace::test_new("only")];
        app.state.active = Some(0);
        app.state.selected = 0;
        let (respond_to, response_rx) = std::sync::mpsc::channel();

        assert!(app.handle_deferred_ssh_api_request(
            ssh_open(
                "prod",
                SshOpenTarget::Split(split_params(Some("w_missing:p_9")))
            ),
            respond_to,
            false,
        ));

        let response = response_rx
            .try_recv()
            .expect("a missing target is rejected without a config lookup");
        assert_eq!(error_code(&response), platform_or("pane_not_found"));
        assert!(app.event_rx.try_recv().is_err());
    }

    #[test]
    fn configured_alias_resolves_to_a_launch_with_its_control_path() {
        let configured = [host("lab"), host("prod")];

        let launch = launch_for_configured_host("deploy@prod", &configured, |destination| {
            Some(PathBuf::from(format!("/tmp/ctl-{destination}")))
        })
        .unwrap();

        assert_eq!(launch.host, "deploy@prod");
        assert_eq!(launch.canonical, "prod");
        assert_eq!(
            launch.control_path,
            Some(PathBuf::from("/tmp/ctl-deploy@prod"))
        );
    }

    #[test]
    fn unconfigured_alias_is_not_found_without_computing_a_control_path() {
        let configured = [host("lab")];

        let err = launch_for_configured_host("prod", &configured, |_| {
            panic!("an unknown host must not reach the control path")
        })
        .unwrap_err();

        assert_eq!(err.0, "ssh_host_not_found");
    }

    #[test]
    fn ssh_host_errors_map_to_ssh_unavailable() {
        for err in [
            SshHostError::SshNotFound,
            SshHostError::NoHomeDirectory,
            SshHostError::ConfigUnreadable {
                path: PathBuf::from("/home/u/.ssh/config"),
                source: std::io::ErrorKind::PermissionDenied.into(),
            },
        ] {
            assert_eq!(ssh_unavailable(err).0, "ssh_unavailable");
        }
    }

    fn finish_ssh_open(
        app: &mut App,
        target: SshOpenTarget,
        result: Result<SshLaunch, ApiFailure>,
    ) -> String {
        let (respond_to, response_rx) = std::sync::mpsc::channel();
        app.handle_ssh_open_finished(SshOpenResult {
            id: "open".into(),
            params: SshOpenParams {
                host: "prod".into(),
                target,
            },
            client_local: false,
            result,
            respond_to,
        });
        response_rx
            .try_recv()
            .expect("completion responds on the app thread")
    }

    fn resolved_launch() -> SshLaunch {
        SshLaunch::new("prod".into(), None).expect("valid destination")
    }

    #[test]
    fn ssh_open_finish_reports_lookup_failure_without_creating() {
        let mut app = test_app();

        let response = finish_ssh_open(
            &mut app,
            workspace_target(),
            Err((
                "ssh_host_not_found".into(),
                "no Host alias in ~/.ssh/config matches prod".into(),
            )),
        );

        assert_eq!(error_code(&response), "ssh_host_not_found");
        assert!(app.state.workspaces.is_empty());
        assert!(app.state.terminals.is_empty());
    }

    #[test]
    fn ssh_open_finish_takes_not_found_path_when_target_closed_during_lookup() {
        let mut app = test_app();

        let response = finish_ssh_open(
            &mut app,
            SshOpenTarget::Tab(tab_params(Some("w_closed"), Some("/"))),
            Ok(resolved_launch()),
        );

        assert_eq!(error_code(&response), "workspace_not_found");
        assert!(app.state.terminals.is_empty());
    }

    #[test]
    fn ssh_hosts_finish_reports_configured_hosts_in_order() {
        let mut app = test_app();
        let (respond_to, response_rx) = std::sync::mpsc::channel();

        app.handle_ssh_hosts_finished(SshHostsResult {
            id: "hosts".into(),
            result: Ok(vec![
                SshHostInfo {
                    alias: "prod".into(),
                    hostname: Some("prod.example.com".into()),
                    user: Some("deploy".into()),
                },
                host("lab"),
            ]),
            respond_to,
        });

        let success: SuccessResponse =
            serde_json::from_str(&response_rx.try_recv().unwrap()).unwrap();
        assert_eq!(
            success.result,
            ResponseResult::SshHosts {
                hosts: vec![
                    SshHostInfoDto {
                        alias: "prod".into(),
                        hostname: Some("prod.example.com".into()),
                        user: Some("deploy".into()),
                    },
                    SshHostInfoDto {
                        alias: "lab".into(),
                        hostname: None,
                        user: None,
                    },
                ],
            }
        );
    }
}
