use super::harness::*;
use std::os::unix::fs::PermissionsExt;

/// Stands in for the docker CLI: answers `ps` and `inspect` from fixture files
/// and turns `exec` into a local `/bin/sh`, so a container pane really runs.
const DOCKER_SHIM: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> "$HERDR_TEST_DOCKER_LOG"
case "$1" in
  ps) cat "$HERDR_TEST_DOCKER_PS" ;;
  inspect) cat "$HERDR_TEST_DOCKER_INSPECT" ;;
  exec) shift
        while [ $# -gt 0 ]; do case "$1" in -it|-i|-t) shift;; -w|-u|-e) shift 2;; *) break;; esac; done
        shift   # container id
        exec /bin/sh ;;
  *) exit 1 ;;
esac
"#;

/// A docker CLI whose daemon never answers. `exec` keeps the kill on timeout
/// from orphaning a `sleep` that would outlive the test.
const HANGING_DOCKER_SHIM: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> "$HERDR_TEST_DOCKER_LOG"
exec sleep 30
"#;

/// Mirrors `DOCKER_CLI_TIMEOUT`, the budget one container lookup may spend in docker.
const DOCKER_LOOKUP_BUDGET: Duration = Duration::from_secs(5);

const COMPOSE_ID: &str = "c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00";
const RUN_ID: &str = "5c7a7c405c7a7c405c7a7c405c7a7c405c7a7c405c7a7c405c7a7c405c7a7c40";

struct FakeDocker {
    bin_dir: PathBuf,
    log: PathBuf,
    ps: PathBuf,
    inspect: PathBuf,
}

impl FakeDocker {
    fn install(base: &Path, script: &str) -> Self {
        let bin_dir = base.join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let program = bin_dir.join("docker");
        fs::write(&program, script).unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
        let docker = Self {
            bin_dir,
            log: base.join("docker.log"),
            ps: base.join("docker-ps.txt"),
            inspect: base.join("docker-inspect.json"),
        };
        docker.set_running(&[]);
        docker
    }

    /// `ps` lists exactly these containers and `inspect` describes them.
    fn set_running(&self, containers: &[serde_json::Value]) {
        let ids: String = containers
            .iter()
            .map(|container| format!("{}\n", container["Id"].as_str().unwrap()))
            .collect();
        fs::write(&self.ps, ids).unwrap();
        fs::write(
            &self.inspect,
            serde_json::to_string_pretty(&serde_json::Value::from(containers.to_vec())).unwrap(),
        )
        .unwrap();
    }

    fn path_override(&self) -> String {
        let inherited = std::env::var("PATH").unwrap_or_default();
        format!("{}:{inherited}", self.bin_dir.display())
    }

    fn calls(&self) -> Vec<String> {
        fs::read_to_string(&self.log)
            .map(|log| log.lines().map(str::to_owned).collect())
            .unwrap_or_default()
    }

    /// The shim logs `exec` from the pane process, after the API response.
    fn wait_for_last_call_starting_with(&self, prefix: &str) -> String {
        let mut last = None;
        let found = wait_until(Duration::from_secs(5), Duration::from_millis(25), || {
            last = self.calls().pop();
            last.as_deref().is_some_and(|line| line.starts_with(prefix))
        });
        assert!(
            found,
            "docker log never ended with `{prefix}`; calls: {:#?}",
            self.calls()
        );
        last.unwrap()
    }
}

struct ContainerTest {
    base: PathBuf,
    config_home: PathBuf,
    runtime_dir: PathBuf,
    socket_path: PathBuf,
    docker: FakeDocker,
}

impl ContainerTest {
    fn new(shim: &str) -> Self {
        let base = unique_test_dir();
        let runtime_dir = base.join("runtime");
        Self {
            config_home: base.join("config"),
            socket_path: runtime_dir.join("herdr.sock"),
            runtime_dir,
            docker: FakeDocker::install(&base, shim),
            base,
        }
    }

    fn start_server(&self) -> SpawnedHerdr {
        let path_override = self.docker.path_override();
        let herdr = spawn_herdr_with_env(
            &self.config_home,
            &self.runtime_dir,
            &self.socket_path,
            Some(Path::new(&path_override)),
            // Why integration off: the shim logs one line per call, and the
            // launcher's multi-line script would split the `exec` entry.
            "onboarding = false\n[offhost]\nshell_integration = false\n",
            &[
                ("HERDR_TEST_DOCKER_LOG", &self.docker.log),
                ("HERDR_TEST_DOCKER_PS", &self.docker.ps),
                ("HERDR_TEST_DOCKER_INSPECT", &self.docker.inspect),
            ],
        );
        wait_for_socket(&self.socket_path, Duration::from_secs(5));
        herdr
    }

    fn dir(&self, name: &str) -> PathBuf {
        let dir = self.base.join(name);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn cli(&self, args: &[&str]) -> std::process::Output {
        run_cli(&self.socket_path, args)
    }

    fn cli_json(&self, args: &[&str]) -> serde_json::Value {
        run_cli_json(&self.socket_path, args)
    }

    fn session_file(&self) -> PathBuf {
        self.config_home.join(app_dir_name()).join("session.json")
    }

    fn finish(self, herdr: SpawnedHerdr) {
        cleanup_spawned_herdr(herdr, self.base);
    }
}

fn inspect_entry(
    id: &str,
    name: &str,
    labels: serde_json::Value,
    mounts: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "Id": id,
        "Name": format!("/{name}"),
        "State": { "Status": "running" },
        "Config": { "Labels": labels, "WorkingDir": "/", "User": "" },
        "Mounts": mounts,
    })
}

/// The host directory is bind-mounted at `/work`, so it maps to that workdir.
fn bind_work(host_dir: &Path) -> serde_json::Value {
    serde_json::json!([{
        "Type": "bind",
        "Source": host_dir,
        "Destination": "/work",
        "Mode": "rw",
        "RW": true,
        "Propagation": "rprivate",
    }])
}

fn compose_container(project: &str, service: &str, project_dir: &Path) -> serde_json::Value {
    inspect_entry(
        COMPOSE_ID,
        &format!("{project}-{service}-1"),
        serde_json::json!({
            "com.docker.compose.project": project,
            "com.docker.compose.service": service,
            "com.docker.compose.container-number": "1",
            "com.docker.compose.project.working_dir": project_dir,
            "com.docker.compose.project.config_files": project_dir.join("compose.yml"),
        }),
        bind_work(project_dir),
    )
}

fn run_container(name: &str, host_dir: &Path) -> serde_json::Value {
    inspect_entry(RUN_ID, name, serde_json::json!({}), bind_work(host_dir))
}

fn compose_ps_call(project: &str, service: &str) -> String {
    format!(
        "ps -q --no-trunc --filter status=running \
         --filter label=com.docker.compose.project={project} \
         --filter label=com.docker.compose.service={service}"
    )
}

fn error_code(output: &std::process::Output) -> serde_json::Value {
    let body: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap_or_else(|err| {
        panic!(
            "stderr is not a JSON error ({err}): {}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    body["error"]["code"].clone()
}

/// Proves the pane runs a live shell rather than an exited `docker` process.
fn assert_pane_runs_shell(test: &ContainerTest, pane_id: &str, marker: &str) {
    let typed = test.cli(&["pane", "run", pane_id, &format!("echo {marker}-$((40+2))")]);
    assert!(
        typed.status.success(),
        "pane run failed: {}",
        String::from_utf8_lossy(&typed.stderr)
    );
    assert!(
        wait_until(Duration::from_secs(5), Duration::from_millis(25), || {
            pane_read_recent_contains(&test.socket_path, pane_id, &format!("{marker}-42"))
        }),
        "pane {pane_id} never evaluated the typed command"
    );
}

fn pane_containers_in_session(session: &serde_json::Value) -> Vec<serde_json::Value> {
    session["workspaces"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|workspace| workspace["tabs"].as_array().into_iter().flatten())
        .flat_map(|tab| {
            tab["panes"]
                .as_object()
                .into_iter()
                .flat_map(|panes| panes.values())
        })
        .filter_map(|pane| pane.get("container").cloned())
        .collect()
}

#[test]
fn workspace_create_with_container_runs_docker_exec_with_workdir() {
    let test = ContainerTest::new(DOCKER_SHIM);
    let project = test.dir("project");
    test.docker
        .set_running(&[run_container("scratch", &project)]);
    let herdr = test.start_server();

    let created = test.cli_json(&[
        "workspace",
        "create",
        "--cwd",
        project.to_str().unwrap(),
        "--container",
        "scratch",
    ]);

    assert_eq!(created["result"]["type"], "workspace_created");
    assert_eq!(created["result"]["root_pane"]["container"], "scratch");
    let exec = test.docker.wait_for_last_call_starting_with("exec ");
    assert!(
        exec.starts_with("exec -it -w /work "),
        "the bind mount of the host cwd must become the exec workdir: {exec}"
    );
    assert!(
        exec.contains(&format!(" {RUN_ID} sh -c ")),
        "the shell must start in the resolved container id via the bash-or-sh probe: {exec}"
    );
    let calls = test.docker.calls();
    assert_eq!(
        &calls[..calls.len() - 1],
        &["inspect --type container scratch"],
        "a plain name is resolved with a single inspect before exec"
    );
    let pane_id = created["result"]["root_pane"]["pane_id"].as_str().unwrap();
    assert_pane_runs_shell(&test, pane_id, "workspace-container");

    test.finish(herdr);
}

#[test]
fn pane_split_with_container_records_reference_in_pane_get() {
    let test = ContainerTest::new(DOCKER_SHIM);
    let project = test.dir("shop");
    test.docker
        .set_running(&[compose_container("shop", "web", &project)]);
    let herdr = test.start_server();
    let created = test.cli_json(&["workspace", "create", "--cwd", project.to_str().unwrap()]);
    let host_pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();

    let split = test.cli_json(&[
        "pane",
        "split",
        &host_pane_id,
        "--direction",
        "right",
        "--container",
        "shop/web",
    ]);
    let container_pane_id = split["result"]["pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();

    let container_pane = test.cli_json(&["pane", "get", &container_pane_id]);
    assert_eq!(container_pane["result"]["pane"]["container"], "shop/web");
    let host_pane = test.cli_json(&["pane", "get", &host_pane_id]);
    assert!(
        host_pane["result"]["pane"].get("container").is_none(),
        "the split source stays a host pane: {host_pane}"
    );
    let calls = test.docker.calls();
    assert_eq!(
        calls.first(),
        Some(&compose_ps_call("shop", "web")),
        "a compose reference is resolved through label filters: {calls:#?}"
    );
    assert_eq!(
        calls.get(1),
        Some(&format!("inspect --type container {COMPOSE_ID}"))
    );
    let exec = test.docker.wait_for_last_call_starting_with("exec ");
    assert!(exec.contains(&format!(" {COMPOSE_ID} ")), "{exec}");
    assert_pane_runs_shell(&test, &container_pane_id, "split-container");

    test.finish(herdr);
}

#[test]
fn tab_create_with_unknown_container_returns_container_not_found() {
    let test = ContainerTest::new(DOCKER_SHIM);
    let herdr = test.start_server();
    let created = test.cli_json(&["workspace", "create", "--cwd", test.base.to_str().unwrap()]);
    let workspace_id = created["result"]["workspace"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();
    let tabs_before = test.cli_json(&["tab", "list", "--workspace", &workspace_id]);

    let rejected = test.cli(&[
        "tab",
        "create",
        "--workspace",
        &workspace_id,
        "--container",
        "ghost/web",
    ]);

    assert_eq!(rejected.status.code(), Some(1));
    assert_eq!(error_code(&rejected), "container_not_found");
    assert_eq!(
        test.cli_json(&["tab", "list", "--workspace", &workspace_id]),
        tabs_before,
        "a failed lookup must not leave a tab behind"
    );
    assert_eq!(test.docker.calls(), vec![compose_ps_call("ghost", "web")]);

    test.finish(herdr);
}

#[test]
fn container_launch_survives_session_snapshot() {
    let test = ContainerTest::new(DOCKER_SHIM);
    let project = test.dir("shop");
    test.docker
        .set_running(&[compose_container("shop", "web", &project)]);
    let mut herdr = test.start_server();
    test.cli_json(&[
        "workspace",
        "create",
        "--cwd",
        project.to_str().unwrap(),
        "--container",
        "shop/web",
    ]);
    test.docker.wait_for_last_call_starting_with("exec ");

    assert!(test.cli(&["server", "stop"]).status.success());
    let pid = herdr.child.process_id();
    assert!(herdr.child.wait().unwrap().success());
    unregister_spawned_herdr_pid(pid);
    drop(herdr);

    let session: serde_json::Value =
        serde_json::from_slice(&fs::read(test.session_file()).unwrap()).unwrap();
    assert_eq!(
        pane_containers_in_session(&session),
        vec![serde_json::json!({
            "reference": { "compose": { "project": "shop", "service": "web" } },
            "id": COMPOSE_ID,
            "workdir": "/work",
            "shell": "bash_or_sh",
        })]
    );

    let restarted = test.start_server();
    let panes = test.cli_json(&["pane", "list"]);
    let restored: Vec<&serde_json::Value> = panes["result"]["panes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|pane| &pane["container"])
        .collect();
    assert_eq!(
        restored,
        vec!["shop/web"],
        "the restored pane keeps its container reference"
    );

    test.finish(restarted);
}

#[test]
fn container_list_reports_workspace_match() {
    let test = ContainerTest::new(DOCKER_SHIM);
    let repo = test.base.join("shop");
    create_committed_repo(&repo);
    let elsewhere = test.dir("elsewhere");
    test.docker.set_running(&[
        compose_container("shop", "web", &repo),
        run_container("scratch", &elsewhere),
    ]);
    let herdr = test.start_server();
    let created = test.cli_json(&["workspace", "create", "--cwd", repo.to_str().unwrap()]);
    let workspace_id = created["result"]["workspace"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_string();

    let listed = test.cli_json(&["container", "list", "--json"]);

    assert_eq!(listed["result"]["type"], "container_list");
    let owners: Vec<(&serde_json::Value, &serde_json::Value)> = listed["result"]["containers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|container| (&container["reference"], &container["workspace_id"]))
        .collect();
    assert_eq!(
        owners,
        vec![
            (
                &serde_json::json!("shop/web"),
                &serde_json::json!(workspace_id)
            ),
            (&serde_json::json!("scratch"), &serde_json::Value::Null),
        ],
        "only the compose project inside the workspace repo is attributed to it"
    );

    let filtered = test.cli_json(&["container", "list", "--workspace", &workspace_id, "--json"]);
    let references: Vec<&serde_json::Value> = filtered["result"]["containers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|container| &container["reference"])
        .collect();
    assert_eq!(references, vec!["shop/web"]);

    test.finish(herdr);
}

#[test]
fn container_lookup_times_out_without_blocking_server() {
    let test = ContainerTest::new(HANGING_DOCKER_SHIM);
    let herdr = test.start_server();
    let started = Instant::now();
    let create = Command::new(env!("CARGO_BIN_EXE_herdr"))
        .args([
            "workspace",
            "create",
            "--cwd",
            test.base.to_str().unwrap(),
            "--container",
            "shop/web",
        ])
        .env("HERDR_SOCKET_PATH", &test.socket_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    assert!(
        wait_until(Duration::from_secs(5), Duration::from_millis(25), || {
            !test.docker.calls().is_empty()
        }),
        "the container lookup never reached docker"
    );

    let ping_started = Instant::now();
    let pong = send_request(
        &test.socket_path,
        r#"{"id":"1","method":"ping","params":{}}"#,
    );
    let ping_elapsed = ping_started.elapsed();

    assert_eq!(pong["result"]["type"], "pong");
    assert!(
        ping_elapsed < DOCKER_LOOKUP_BUDGET / 2,
        "ping waited {ping_elapsed:?} behind a hanging docker lookup"
    );
    let rejected = create.wait_with_output().unwrap();
    let lookup_elapsed = started.elapsed();
    assert_eq!(error_code(&rejected), "container_unavailable");
    assert!(
        lookup_elapsed < DOCKER_LOOKUP_BUDGET + Duration::from_secs(5),
        "the lookup must give up at its budget, not when docker exits: {lookup_elapsed:?}"
    );

    test.finish(herdr);
}
