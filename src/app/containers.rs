use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use crate::api::schema::{ContainerInfoDto, ContainerOpenTarget, Method, Request, ResponseResult};
use crate::app::App;
use crate::container::docker::DockerHost;
use crate::container::workdir::{compose_matches_repo_root, containers_for_repo_root};
use crate::container::{ContainerInfo, ContainerLaunch, ContainerListError, ContainerRef};
use crate::events::{AppEvent, ContainerListResult, ContainerOpenResult, WorkspaceRepoRoot};
use crate::pane::PaneLaunchTarget;
use crate::ssh_host::SshLaunch;

use super::api::responses::{encode_error, encode_success};
use super::ssh_hosts::{resolve_ssh_launch, validate_ssh_host};

type ApiFailure = (String, String);

/// Returned intact by `spawn_named_with` when the lookup thread cannot start.
struct ContainerOpenJob {
    id: String,
    target: ContainerOpenTarget,
    ssh_host: Option<String>,
    reference: ContainerRef,
    host_cwd: PathBuf,
    client_local: bool,
    respond_to: Sender<String>,
}

struct WorkspaceRepoProbe {
    workspace_id: String,
    repo_root: Option<PathBuf>,
    identity_cwd: Option<PathBuf>,
}

impl WorkspaceRepoProbe {
    fn resolve(self) -> WorkspaceRepoRoot {
        WorkspaceRepoRoot {
            repo_root: self.repo_root.or_else(|| {
                self.identity_cwd
                    .as_deref()
                    .and_then(crate::workspace::git_space_metadata)
                    .map(|space| space.repo_root)
            }),
            workspace_id: self.workspace_id,
        }
    }
}

struct ContainerListJob {
    id: String,
    /// The validated `Host` value as sent; the worker resolves it to a launch.
    ssh_host: Option<String>,
    filter_workspace_id: Option<String>,
    probes: Vec<WorkspaceRepoProbe>,
    respond_to: Sender<String>,
}

impl App {
    pub(crate) fn handle_deferred_container_api_request(
        &mut self,
        request: Request,
        respond_to: Sender<String>,
        client_local: bool,
    ) -> bool {
        match request.method {
            Method::ContainerOpen(params) => {
                self.start_container_open(
                    request.id,
                    params.container,
                    None,
                    params.target,
                    respond_to,
                    client_local,
                );
                true
            }
            Method::SshContainerOpen(params) => {
                self.start_container_open(
                    request.id,
                    params.container,
                    Some(params.host),
                    params.target,
                    respond_to,
                    client_local,
                );
                true
            }
            Method::ContainerList(params) => {
                self.start_container_list(request.id, None, params.workspace_id, respond_to);
                true
            }
            Method::SshContainerList(params) => {
                self.start_container_list(
                    request.id,
                    Some(params.host),
                    params.workspace_id,
                    respond_to,
                );
                true
            }
            _ => false,
        }
    }

    fn start_container_open(
        &mut self,
        id: String,
        container: String,
        ssh_host: Option<String>,
        mut target: ContainerOpenTarget,
        respond_to: Sender<String>,
        client_local: bool,
    ) {
        let reference = match parse_container_reference(&container) {
            Ok(reference) => reference,
            Err((code, message)) => {
                let _ = respond_to.send(encode_error(id, &code, message));
                return;
            }
        };
        if let Some(host) = &ssh_host {
            if let Err((code, message)) = validate_ssh_host(host) {
                let _ = respond_to.send(encode_error(id, &code, message));
                return;
            }
        }
        let host_cwd = match self.pin_container_open_target(&mut target) {
            Ok(host_cwd) => host_cwd,
            Err((code, message)) => {
                let _ = respond_to.send(encode_error(id, &code, message));
                return;
            }
        };
        let job = ContainerOpenJob {
            id,
            target,
            ssh_host,
            reference,
            host_cwd,
            client_local,
            respond_to,
        };
        let event_tx = self.event_tx.clone();
        let spawned =
            crate::thread_spawn::spawn_named_with("herdr-container-open", job, move |job| {
                let result = resolve_container_launch(
                    job.ssh_host.as_deref(),
                    &job.reference,
                    &job.host_cwd,
                );
                let _ = event_tx.blocking_send(AppEvent::ContainerOpenFinished(Box::new(
                    ContainerOpenResult {
                        id: job.id,
                        target: job.target,
                        client_local: job.client_local,
                        result,
                        respond_to: job.respond_to,
                    },
                )));
            });
        if let Err((job, err)) = spawned {
            tracing::warn!(err = %err, "failed to spawn container lookup thread");
            let _ = job.respond_to.send(encode_error(
                job.id,
                "container_unavailable",
                format!("could not start container lookup: {err}"),
            ));
        }
    }

    fn pin_container_open_target(
        &self,
        target: &mut ContainerOpenTarget,
    ) -> Result<PathBuf, ApiFailure> {
        self.pin_open_target(target.into())
    }

    pub(crate) fn handle_container_open_finished(&mut self, finished: ContainerOpenResult) {
        let ContainerOpenResult {
            id,
            target,
            result,
            respond_to,
            ..
        } = finished;
        // A target closed during the lookup takes the handlers' own not-found path.
        let response = match result {
            Err((code, message)) => encode_error(id, &code, message),
            Ok(launch) => match target {
                ContainerOpenTarget::Workspace(target) => {
                    self.handle_workspace_create(id, target, PaneLaunchTarget::Container(&launch))
                }
                ContainerOpenTarget::Tab(target) => {
                    self.handle_tab_create(id, target, PaneLaunchTarget::Container(&launch))
                }
                ContainerOpenTarget::Split(target) => {
                    self.handle_pane_split(id, target, PaneLaunchTarget::Container(&launch))
                }
            },
        };
        let _ = respond_to.send(response);
    }

    fn start_container_list(
        &mut self,
        id: String,
        ssh_host: Option<String>,
        workspace_id: Option<String>,
        respond_to: Sender<String>,
    ) {
        if let Err((code, message)) = ensure_container_platform_supported() {
            let _ = respond_to.send(encode_error(id, &code, message));
            return;
        }
        if let Some(host) = &ssh_host {
            if let Err((code, message)) = validate_ssh_host(host) {
                let _ = respond_to.send(encode_error(id, &code, message));
                return;
            }
        }
        let (filter_workspace_id, probes) =
            match self.container_list_probes(workspace_id.as_deref()) {
                Ok(captured) => captured,
                Err((code, message)) => {
                    let _ = respond_to.send(encode_error(id, &code, message));
                    return;
                }
            };
        let job = ContainerListJob {
            id,
            ssh_host,
            filter_workspace_id,
            probes,
            respond_to,
        };
        let event_tx = self.event_tx.clone();
        let spawned =
            crate::thread_spawn::spawn_named_with("herdr-container-list", job, move |job| {
                let remote = job.ssh_host.is_some();
                let workspaces = job
                    .probes
                    .into_iter()
                    .map(|probe| {
                        // Why not run Git discovery for a remote listing:
                        // its containers are never matched to local repos.
                        if remote {
                            WorkspaceRepoRoot {
                                workspace_id: probe.workspace_id,
                                repo_root: None,
                            }
                        } else {
                            probe.resolve()
                        }
                    })
                    .collect();
                let ssh_alias = job
                    .ssh_host
                    .as_deref()
                    .and_then(crate::ssh_host::launch::ssh_destination_alias)
                    .map(str::to_owned);
                let result = list_containers(job.ssh_host.as_deref());
                let _ = event_tx.blocking_send(AppEvent::ContainerListFinished(Box::new(
                    ContainerListResult {
                        id: job.id,
                        filter_workspace_id: job.filter_workspace_id,
                        workspaces,
                        ssh_host: ssh_alias,
                        result,
                        respond_to: job.respond_to,
                    },
                )));
            });
        if let Err((job, err)) = spawned {
            tracing::warn!(err = %err, "failed to spawn container list thread");
            let _ = job.respond_to.send(encode_error(
                job.id,
                "container_unavailable",
                format!("could not start container listing: {err}"),
            ));
        }
    }

    /// Takes repo roots already known on the app thread. Why not run Git
    /// discovery here: it touches the filesystem, so it waits for the worker.
    fn container_list_probes(
        &self,
        filter: Option<&str>,
    ) -> Result<(Option<String>, Vec<WorkspaceRepoProbe>), ApiFailure> {
        let indices: Vec<usize> = match filter {
            Some(workspace_id) => vec![self.parse_workspace_id(workspace_id).ok_or_else(|| {
                (
                    "workspace_not_found".to_string(),
                    format!("workspace {workspace_id} not found"),
                )
            })?],
            None => (0..self.state.workspaces.len()).collect(),
        };
        // Aliases such as `1` become the stable id the result is matched against.
        let filter_workspace_id = filter.and_then(|_| {
            indices
                .first()
                .map(|&ws_idx| self.public_workspace_id(ws_idx))
        });
        let probes = indices
            .into_iter()
            .map(|ws_idx| {
                let ws = &self.state.workspaces[ws_idx];
                let repo_root = ws
                    .worktree_space()
                    .map(|membership| membership.repo_root.clone())
                    .or_else(|| ws.git_space().map(|space| space.repo_root.clone()));
                let identity_cwd = if repo_root.is_none() {
                    ws.resolved_identity_cwd_from(&self.state.terminals, &self.terminal_runtimes)
                } else {
                    None
                };
                WorkspaceRepoProbe {
                    workspace_id: ws.id.clone(),
                    repo_root,
                    identity_cwd,
                }
            })
            .collect();
        Ok((filter_workspace_id, probes))
    }

    pub(crate) fn handle_container_list_finished(&mut self, finished: ContainerListResult) {
        let ContainerListResult {
            id,
            filter_workspace_id,
            workspaces,
            ssh_host,
            result,
            respond_to,
        } = finished;
        let response = match result {
            Err((code, message)) => encode_error(id, &code, message),
            Ok(containers) => {
                // Why not report the captured ids as-is: a workspace closed
                // during the docker call must not appear as an owner.
                let open: Vec<WorkspaceRepoRoot> = workspaces
                    .into_iter()
                    .filter(|captured| {
                        self.state
                            .workspaces
                            .iter()
                            .any(|ws| ws.id == captured.workspace_id)
                    })
                    .collect();
                match filter_workspace_id {
                    Some(workspace_id)
                        if !open.iter().any(|ws| ws.workspace_id == workspace_id) =>
                    {
                        encode_error(
                            id,
                            "workspace_not_found",
                            format!("workspace {workspace_id} not found"),
                        )
                    }
                    filter => encode_success(
                        id,
                        ResponseResult::ContainerList {
                            containers: container_list_dtos(
                                &containers,
                                &open,
                                filter.as_deref(),
                                ssh_host.as_deref(),
                            ),
                        },
                    ),
                }
            }
        };
        let _ = respond_to.send(response);
    }
}

fn ensure_container_platform_supported() -> Result<(), ApiFailure> {
    // Why not attempt it: docker exec login flags and cwd normalization are
    // unverified on Windows hosts, so a pane could start in the wrong shell.
    if cfg!(windows) {
        return Err((
            "platform_unsupported".into(),
            "container panes are not supported on Windows hosts".into(),
        ));
    }
    Ok(())
}

fn parse_container_reference(text: &str) -> Result<ContainerRef, ApiFailure> {
    ensure_container_platform_supported()?;
    ContainerRef::parse(text).ok_or_else(|| {
        (
            "invalid_container".into(),
            format!("invalid container reference {text:?}; expected project/service or a container name"),
        )
    })
}

/// Resolves the ssh host's control path and lists its docker, so it must stay
/// off the app thread.
fn list_containers(ssh_host: Option<&str>) -> Result<Vec<ContainerInfo>, ApiFailure> {
    let ssh = ssh_host.map(resolve_ssh_launch).transpose()?;
    crate::container::docker::list_running_containers(&DockerHost::for_launch(ssh.as_ref()))
        .map_err(container_unavailable)
}

/// Runs docker (over ssh for a remote host), so it must stay off the app thread.
fn resolve_container_launch(
    ssh_host: Option<&str>,
    reference: &ContainerRef,
    host_cwd: &Path,
) -> Result<ContainerLaunch, ApiFailure> {
    let ssh = ssh_host.map(resolve_ssh_launch).transpose()?;
    let host = DockerHost::for_launch(ssh.as_ref());
    match crate::container::docker::inspect_reference(&host, reference) {
        // `docker inspect <name>` also reports stopped containers, which
        // `docker exec` would reject after the pane already exists.
        Ok(Some(info)) if info.running => Ok(launch_for_info(&info, ssh, host_cwd)),
        Ok(_) => Err((
            "container_not_found".into(),
            match ssh_host {
                Some(host) => format!(
                    "no running container matches {} on {host}",
                    reference.canonical()
                ),
                None => format!("no running container matches {}", reference.canonical()),
            },
        )),
        Err(err) => Err(container_unavailable(err)),
    }
}

fn launch_for_info(
    info: &ContainerInfo,
    ssh: Option<SshLaunch>,
    host_cwd: &Path,
) -> ContainerLaunch {
    // Why not map the local cwd on a remote host: its bind mounts are paths of
    // another machine.
    let mut launch = crate::container::launch::plan_launch(info, ssh.is_none().then_some(host_cwd));
    launch.ssh = ssh;
    launch
}

fn container_unavailable(err: ContainerListError) -> ApiFailure {
    tracing::warn!(err = %err, "docker lookup failed");
    ("container_unavailable".into(), err.to_string())
}

fn container_list_dtos(
    containers: &[ContainerInfo],
    workspaces: &[WorkspaceRepoRoot],
    filter_workspace_id: Option<&str>,
    ssh_host: Option<&str>,
) -> Vec<ContainerInfoDto> {
    if let Some(alias) = ssh_host {
        // Why not match workspaces: compose directories on another machine
        // say nothing about the repos opened here, and a coincidental path
        // match would mark an unrelated container as a workspace's own.
        if filter_workspace_id.is_some() {
            return Vec::new();
        }
        let mut remote: Vec<&ContainerInfo> = containers.iter().collect();
        remote.sort_by_key(|info| !info.herdr.default);
        return remote
            .into_iter()
            .map(|info| ContainerInfoDto {
                ssh_host: Some(alias.to_owned()),
                ..container_info_dto(info, None)
            })
            .collect();
    }
    if let Some(filter) = filter_workspace_id {
        let Some(repo_root) = workspaces
            .iter()
            .find(|ws| ws.workspace_id == filter)
            .and_then(|ws| ws.repo_root.as_deref())
        else {
            return Vec::new();
        };
        return containers_for_repo_root(containers, repo_root)
            .into_iter()
            .map(|info| container_info_dto(info, Some(filter)))
            .collect();
    }
    containers
        .iter()
        .map(|info| {
            // The DTO names one owner, so the earliest workspace on the repo
            // wins instead of the result depending on later duplicates.
            let owner = info.compose.as_ref().and_then(|compose| {
                workspaces
                    .iter()
                    .find(|ws| {
                        ws.repo_root
                            .as_deref()
                            .is_some_and(|root| compose_matches_repo_root(compose, root))
                    })
                    .map(|ws| ws.workspace_id.as_str())
            });
            container_info_dto(info, owner)
        })
        .collect()
}

fn container_info_dto(info: &ContainerInfo, workspace_id: Option<&str>) -> ContainerInfoDto {
    let compose = info.compose.as_ref();
    ContainerInfoDto {
        reference: info.reference().canonical(),
        id: info.id.clone(),
        name: info.name.clone(),
        project: compose.map(|compose| compose.project.clone()),
        service: compose.map(|compose| compose.service.clone()),
        working_dir: compose
            .and_then(|compose| compose.working_dir.as_deref())
            .map(|dir| dir.display().to_string()),
        workspace_id: workspace_id.map(str::to_owned),
        ssh_host: None,
        is_default: info.herdr.default,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::api::schema::{
        ContainerListParams, ContainerOpenParams, ContainerOpenTarget, ErrorResponse, Method,
        PaneSplitParams, SplitDirection, SshContainerListParams, SshContainerOpenParams,
        TabCreateParams, WorkspaceCreateParams,
    };
    use crate::config::Config;
    use crate::container::{ComposeInfo, ContainerShell, HerdrLabels};
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

    fn compose_container(
        name: &str,
        project: &str,
        service: &str,
        working_dir: &str,
        default: bool,
    ) -> ContainerInfo {
        ContainerInfo {
            id: format!("{name}-id"),
            name: name.into(),
            running: true,
            compose: Some(ComposeInfo {
                project: project.into(),
                service: service.into(),
                working_dir: Some(PathBuf::from(working_dir)),
                config_files: Vec::new(),
            }),
            working_dir: Some(PathBuf::from("/srv/app")),
            user: None,
            bind_mounts: Vec::new(),
            herdr: HerdrLabels {
                default,
                ..HerdrLabels::default()
            },
        }
    }

    fn plain_container(name: &str) -> ContainerInfo {
        ContainerInfo {
            id: format!("{name}-id"),
            name: name.into(),
            running: true,
            compose: None,
            working_dir: Some(PathBuf::from("/work")),
            user: None,
            bind_mounts: Vec::new(),
            herdr: HerdrLabels::default(),
        }
    }

    fn workspace_root(workspace_id: &str, repo_root: Option<&str>) -> WorkspaceRepoRoot {
        WorkspaceRepoRoot {
            workspace_id: workspace_id.into(),
            repo_root: repo_root.map(PathBuf::from),
        }
    }

    fn listed_containers() -> Vec<ContainerInfo> {
        vec![
            compose_container("shop-web-1", "shop", "web", "/repo/shop", false),
            compose_container("shop-db-1", "shop", "db", "/repo/shop/docker", true),
            compose_container("other-api-1", "other", "api", "/elsewhere/other", false),
            plain_container("scratch"),
        ]
    }

    #[test]
    fn container_list_marks_containers_matching_workspace_repo_root() {
        let workspaces = [
            workspace_root("w1", Some("/repo/shop")),
            workspace_root("w2", None),
            workspace_root("w3", Some("/repo/shop")),
        ];

        let dtos = container_list_dtos(&listed_containers(), &workspaces, None, None);

        assert_eq!(
            dtos,
            vec![
                ContainerInfoDto {
                    reference: "shop/web".into(),
                    id: "shop-web-1-id".into(),
                    name: "shop-web-1".into(),
                    project: Some("shop".into()),
                    service: Some("web".into()),
                    working_dir: Some("/repo/shop".into()),
                    workspace_id: Some("w1".into()),
                    ssh_host: None,
                    is_default: false,
                },
                ContainerInfoDto {
                    reference: "shop/db".into(),
                    id: "shop-db-1-id".into(),
                    name: "shop-db-1".into(),
                    project: Some("shop".into()),
                    service: Some("db".into()),
                    working_dir: Some("/repo/shop/docker".into()),
                    workspace_id: Some("w1".into()),
                    ssh_host: None,
                    is_default: true,
                },
                ContainerInfoDto {
                    reference: "other/api".into(),
                    id: "other-api-1-id".into(),
                    name: "other-api-1".into(),
                    project: Some("other".into()),
                    service: Some("api".into()),
                    working_dir: Some("/elsewhere/other".into()),
                    workspace_id: None,
                    ssh_host: None,
                    is_default: false,
                },
                ContainerInfoDto {
                    reference: "scratch".into(),
                    id: "scratch-id".into(),
                    name: "scratch".into(),
                    project: None,
                    service: None,
                    working_dir: None,
                    workspace_id: None,
                    ssh_host: None,
                    is_default: false,
                },
            ]
        );
    }

    #[test]
    fn container_list_filter_keeps_only_that_workspace_with_default_first() {
        let workspaces = [
            workspace_root("w1", Some("/repo/shop")),
            workspace_root("w2", None),
            workspace_root("w3", Some("/repo/shop")),
        ];

        let scoped = container_list_dtos(&listed_containers(), &workspaces, Some("w3"), None);
        let summary: Vec<_> = scoped
            .iter()
            .map(|dto| {
                (
                    dto.reference.as_str(),
                    dto.workspace_id.as_deref(),
                    dto.is_default,
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                ("shop/db", Some("w3"), true),
                ("shop/web", Some("w3"), false)
            ]
        );

        assert!(
            container_list_dtos(&listed_containers(), &workspaces, Some("w2"), None).is_empty()
        );
    }

    #[test]
    fn remote_container_list_names_the_host_and_never_matches_local_workspaces() {
        let workspaces = [workspace_root("w1", Some("/repo/shop"))];

        let dtos = container_list_dtos(&listed_containers(), &workspaces, None, Some("prod"));

        let summary: Vec<_> = dtos
            .iter()
            .map(|dto| {
                (
                    dto.reference.as_str(),
                    dto.workspace_id.as_deref(),
                    dto.ssh_host.as_deref(),
                    dto.is_default,
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                ("shop/db", None, Some("prod"), true),
                ("shop/web", None, Some("prod"), false),
                ("other/api", None, Some("prod"), false),
                ("scratch", None, Some("prod"), false),
            ]
        );
        assert!(
            container_list_dtos(&listed_containers(), &workspaces, Some("w1"), Some("prod"))
                .is_empty(),
            "a workspace filter cannot select containers of another machine"
        );
    }

    #[test]
    fn remote_launch_keeps_the_ssh_host_and_skips_bind_mount_mapping() {
        let mut info = plain_container("scratch");
        info.bind_mounts = vec![crate::container::types::BindMount {
            source: PathBuf::from("/home/dev/app"),
            destination: PathBuf::from("/work"),
        }];
        let cwd = Path::new("/home/dev/app/src");
        let ssh = SshLaunch::new("deploy@prod".into(), Some("/tmp/ctl-%C".into()));

        let remote = launch_for_info(&info, ssh.clone(), cwd);
        assert_eq!(remote.ssh, ssh);
        assert_eq!(
            remote.workdir,
            Some(PathBuf::from("/work")),
            "image WorkingDir"
        );

        let local = launch_for_info(&info, None, cwd);
        assert_eq!(local.ssh, None);
        assert_eq!(local.workdir, Some(PathBuf::from("/work/src")));
    }

    #[test]
    fn container_open_rejects_invalid_ssh_host_before_spawning() {
        for host in ["-oProxyCommand=sh", "prod;id", ""] {
            let mut app = test_app();
            let (respond_to, response_rx) = std::sync::mpsc::channel();
            let request = Request {
                id: "open".into(),
                method: Method::SshContainerOpen(SshContainerOpenParams {
                    host: host.into(),
                    container: "shop/db".into(),
                    target: workspace_target(),
                }),
            };

            assert!(app.handle_deferred_container_api_request(request, respond_to, false));

            let response = response_rx
                .try_recv()
                .expect("an invalid host is rejected without an ssh lookup");
            assert_eq!(
                error_code(&response),
                platform_or("invalid_ssh_host"),
                "{host:?}"
            );
            assert!(app.state.workspaces.is_empty());
            assert!(app.event_rx.try_recv().is_err());
        }
    }

    #[test]
    fn container_list_rejects_invalid_ssh_host_before_spawning() {
        let mut app = test_app();
        let (respond_to, response_rx) = std::sync::mpsc::channel();

        assert!(app.handle_deferred_container_api_request(
            Request {
                id: "list".into(),
                method: Method::SshContainerList(SshContainerListParams {
                    host: "-oProxyCommand=sh".into(),
                    workspace_id: None,
                }),
            },
            respond_to,
            false,
        ));

        let response = response_rx
            .try_recv()
            .expect("an invalid host is rejected without an ssh lookup");
        assert_eq!(error_code(&response), platform_or("invalid_ssh_host"));
        assert!(app.event_rx.try_recv().is_err());
    }

    fn error_code(response: &str) -> String {
        serde_json::from_str::<ErrorResponse>(response)
            .unwrap_or_else(|err| panic!("expected error response, got {response}: {err}"))
            .error
            .code
    }

    fn container_open(container: &str, target: ContainerOpenTarget) -> Request {
        Request {
            id: "open".into(),
            method: Method::ContainerOpen(ContainerOpenParams {
                container: container.into(),
                target,
            }),
        }
    }

    fn workspace_target() -> ContainerOpenTarget {
        ContainerOpenTarget::Workspace(WorkspaceCreateParams {
            source_workspace_id: None,
            cwd: None,
            focus: false,
            label: None,
            env: Default::default(),
        })
    }

    #[test]
    fn container_methods_are_rejected_by_synchronous_dispatch() {
        let mut app = test_app();

        for request in [
            container_open("shop/db", workspace_target()),
            Request {
                id: "list".into(),
                method: Method::ContainerList(ContainerListParams::default()),
            },
        ] {
            assert_eq!(
                error_code(&app.handle_api_request(request)),
                "invalid_request"
            );
        }
    }

    fn platform_or(code: &str) -> &str {
        if cfg!(windows) {
            "platform_unsupported"
        } else {
            code
        }
    }

    #[test]
    fn workspace_create_rejects_invalid_container_before_spawning() {
        let mut app = test_app();
        let (respond_to, response_rx) = std::sync::mpsc::channel();

        assert!(app.handle_deferred_container_api_request(
            container_open("a/b/c", workspace_target()),
            respond_to,
            false,
        ));

        let response = response_rx
            .try_recv()
            .expect("an invalid reference is rejected without a docker lookup");
        assert_eq!(error_code(&response), platform_or("invalid_container"));
        assert!(app.state.workspaces.is_empty());
        assert!(app.event_rx.try_recv().is_err());
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

    #[test]
    fn container_open_rejects_missing_split_target_before_docker() {
        let mut app = test_app();
        app.state.workspaces = vec![Workspace::test_new("only")];
        app.state.active = Some(0);
        app.state.selected = 0;
        let (respond_to, response_rx) = std::sync::mpsc::channel();

        assert!(app.handle_deferred_container_api_request(
            container_open(
                "shop/db",
                ContainerOpenTarget::Split(split_params(Some("w_missing:p_9"))),
            ),
            respond_to,
            false,
        ));

        let response = response_rx
            .try_recv()
            .expect("a missing target is rejected without a docker lookup");
        assert_eq!(error_code(&response), platform_or("pane_not_found"));
        assert!(app.event_rx.try_recv().is_err());
    }

    #[test]
    fn container_list_rejects_unknown_workspace_before_docker() {
        let mut app = test_app();
        let (respond_to, response_rx) = std::sync::mpsc::channel();

        assert!(app.handle_deferred_container_api_request(
            Request {
                id: "list".into(),
                method: Method::ContainerList(ContainerListParams {
                    workspace_id: Some("w_missing".into()),
                }),
            },
            respond_to,
            false,
        ));

        let response = response_rx
            .try_recv()
            .expect("an unknown workspace is rejected without a docker lookup");
        assert_eq!(error_code(&response), platform_or("workspace_not_found"));
    }

    #[test]
    fn container_open_pins_target_ids_and_cwd_before_lookup() {
        let cwd = std::env::temp_dir();
        let mut app = test_app();
        let mut workspace = Workspace::test_new("pinned");
        workspace.identity_cwd = cwd.clone();
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.new_terminal_cwd = crate::config::NewTerminalCwdConfig::Follow;
        app.state.ensure_test_terminals();
        let workspace_id = app.public_workspace_id(0);
        let focused_pane = app.state.workspaces[0].focused_pane_id().unwrap();
        let public_pane_id = app.public_pane_id(0, focused_pane).unwrap();
        let expected_cwd = Some(cwd.display().to_string());

        let mut tab = ContainerOpenTarget::Tab(tab_params(None, None));
        assert_eq!(app.pin_container_open_target(&mut tab).unwrap(), cwd);
        let ContainerOpenTarget::Tab(tab) = tab else {
            panic!("pinning must keep the target kind");
        };
        assert_eq!(tab.workspace_id.as_deref(), Some(workspace_id.as_str()));
        assert_eq!(tab.cwd, expected_cwd);

        let mut split = ContainerOpenTarget::Split(split_params(None));
        assert_eq!(app.pin_container_open_target(&mut split).unwrap(), cwd);
        let ContainerOpenTarget::Split(split) = split else {
            panic!("pinning must keep the target kind");
        };
        assert_eq!(
            split.target_pane_id.as_deref(),
            Some(public_pane_id.as_str())
        );
        assert_eq!(split.cwd, expected_cwd);
    }

    fn resolved_launch() -> ContainerLaunch {
        ContainerLaunch {
            reference: ContainerRef::Compose {
                project: "shop".into(),
                service: "db".into(),
            },
            canonical: "shop/db".into(),
            id: "abc123".into(),
            workdir: None,
            user: None,
            shell: ContainerShell::BashOrSh,
            ssh: None,
        }
    }

    fn finish_container_open(
        app: &mut App,
        target: ContainerOpenTarget,
        result: Result<ContainerLaunch, ApiFailure>,
    ) -> String {
        let (respond_to, response_rx) = std::sync::mpsc::channel();
        app.handle_container_open_finished(ContainerOpenResult {
            id: "open".into(),
            target,
            client_local: false,
            result,
            respond_to,
        });
        response_rx
            .try_recv()
            .expect("completion responds on the app thread")
    }

    #[test]
    fn container_open_finish_reports_lookup_failure_without_creating() {
        let mut app = test_app();

        let response = finish_container_open(
            &mut app,
            workspace_target(),
            Err((
                "container_not_found".into(),
                "no running container matches shop/db".into(),
            )),
        );

        assert_eq!(error_code(&response), "container_not_found");
        assert!(app.state.workspaces.is_empty());
        assert!(app.state.terminals.is_empty());
    }

    #[test]
    fn container_open_finish_takes_not_found_path_when_target_closed_during_lookup() {
        let mut app = test_app();

        let response = finish_container_open(
            &mut app,
            ContainerOpenTarget::Tab(tab_params(Some("w_closed"), Some("/"))),
            Ok(resolved_launch()),
        );

        assert_eq!(error_code(&response), "workspace_not_found");
        assert!(app.state.terminals.is_empty());
    }
}
