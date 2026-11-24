//! The ssh config file a session's ssh panes read instead of ssh's defaults,
//! chosen with `herdr --ssh <file>` and remembered per session.
//!
//! The client resolves `--ssh` and hands it to the server it spawns through
//! [`ENV_VAR`]. The server is the only writer of the remembered value, so a
//! later launch without `--ssh` keeps opening hosts through the same file.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Carries the request from the launching client to the server it spawns.
pub(crate) const ENV_VAR: &str = "HERDR_SSH_CONFIG";
/// `--ssh none` returns the session to ssh's own default configs.
const DEFAULT_KEYWORD: &str = "none";
const STATE_FILE: &str = "ssh-config";
const OPTION: &str = "--ssh";

/// `Err` when the server could not tell which config the session uses.
static SESSION_CONFIG: OnceLock<Result<Option<PathBuf>, String>> = OnceLock::new();

/// What a launch asked the session to use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SshConfigRequest {
    /// ssh's own defaults (`~/.ssh/config` and the system config).
    Default,
    /// Only this file, through `ssh -F`.
    File(PathBuf),
}

impl SshConfigRequest {
    fn config(&self) -> Option<&Path> {
        match self {
            Self::Default => None,
            Self::File(path) => Some(path),
        }
    }

    fn env_value(&self) -> OsString {
        match self {
            Self::Default => DEFAULT_KEYWORD.into(),
            Self::File(path) => path.as_os_str().to_owned(),
        }
    }

    fn from_env_value(value: OsString) -> Self {
        if value == DEFAULT_KEYWORD {
            Self::Default
        } else {
            Self::File(PathBuf::from(value))
        }
    }
}

/// Removes the launch option `--ssh <file>` / `--ssh=<file>` from `args`.
///
/// Why only before the first command word: `herdr pane open --ssh <host>`
/// and the container commands already take `--ssh` as a host alias.
pub(crate) fn extract_launch_arg(args: &[String]) -> Result<(Vec<String>, Option<String>), String> {
    let mut cleaned = Vec::with_capacity(args.len());
    cleaned.extend(args.first().cloned());
    let mut value = None;
    let mut index = 1;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--" || !arg.starts_with('-') {
            cleaned.extend_from_slice(&args[index..]);
            break;
        }
        let found = if arg == OPTION {
            let next = args
                .get(index + 1)
                .ok_or_else(|| format!("missing value for {OPTION}"))?;
            index += 2;
            next.clone()
        } else if let Some(inline) = arg.strip_prefix("--ssh=") {
            index += 1;
            inline.to_owned()
        } else {
            cleaned.push(arg.clone());
            index += 1;
            continue;
        };
        if value.replace(found).is_some() {
            return Err(format!("{OPTION} can only be specified once"));
        }
    }
    Ok((cleaned, value))
}

/// Resolves a `--ssh` value: `none`, or a readable file given as an absolute
/// path, a relative path, or a `~` path the shell left unexpanded.
pub(crate) fn resolve_request(raw: &str) -> Result<SshConfigRequest, String> {
    if raw == DEFAULT_KEYWORD {
        return Ok(SshConfigRequest::Default);
    }
    if raw.is_empty() {
        return Err(format!(
            "{OPTION} needs an ssh config file, or `{DEFAULT_KEYWORD}` for ssh's defaults"
        ));
    }
    let path = crate::worktree::expand_tilde_absolute_path(raw);
    // Why open it now: a missing file would otherwise surface only as an ssh
    // pane that exits right after it opens.
    match std::fs::File::open(&path).and_then(|file| file.metadata()) {
        Ok(metadata) if metadata.is_file() => Ok(SshConfigRequest::File(path)),
        Ok(_) => Err(format!("ssh config {} is not a file", path.display())),
        Err(err) => Err(format!("cannot read ssh config {}: {err}", path.display())),
    }
}

/// Refuses `--ssh` when a socket override points this launch at a server
/// whose session this process cannot name.
///
/// Why: the remembered config is read from the selected session's directory,
/// while a socket override from the environment may attach to another
/// session's server. An explicit `--session` replaces the overrides.
pub(crate) fn check_launch_target(
    explicit_session: bool,
    socket_override: bool,
) -> Result<(), String> {
    if socket_override && !explicit_session {
        return Err(format!(
            "{OPTION} needs --session <name> while HERDR_SOCKET_PATH or HERDR_CLIENT_SOCKET_PATH selects the server"
        ));
    }
    Ok(())
}

/// Hands `request` to a server this launch spawns, or refuses it when the
/// session's running server remembers another config. Call it again with
/// `server_running` once the server is ready: two launches racing to start
/// the session must not attach to the server the other one configured.
///
/// Why refuse instead of switching: open ssh panes and their masters were
/// resolved through the remembered file, and a silent switch would send new
/// panes with the same alias to another machine.
pub(crate) fn apply_for_launch(
    request: &SshConfigRequest,
    server_running: bool,
    state_dir: &Path,
) -> Result<(), String> {
    if server_running {
        let remembered = read_remembered(state_dir)
            .map_err(|err| format!("cannot read this session's remembered ssh config: {err}"))?;
        if !same_config(remembered.as_deref(), request.config()) {
            return Err(format!(
                "this session's server already uses {}; run `herdr server stop` first to switch to {}",
                describe(remembered.as_deref()),
                describe(request.config()),
            ));
        }
        return Ok(());
    }
    std::env::set_var(ENV_VAR, request.env_value());
    Ok(())
}

/// Reads and clears the request the launching client left in the server's
/// environment. Why clear it: pane shells inherit the server's environment,
/// and a nested herdr must not carry this session's choice into its own.
pub(crate) fn take_server_request() -> Option<SshConfigRequest> {
    let value = std::env::var_os(ENV_VAR)?;
    std::env::remove_var(ENV_VAR);
    Some(SshConfigRequest::from_env_value(value))
}

/// Settles the session's config at server start: a request replaces the
/// remembered one, and no request keeps it.
///
/// Why an error instead of falling back to ssh's defaults: a host alias
/// shared by both configs would silently reach another machine, and the
/// launching client compares against the remembered file.
/// On an error the session's config stays unknown, and [`current`] refuses
/// ssh launches instead of reading ssh's defaults.
pub(crate) fn establish(request: Option<SshConfigRequest>, state_dir: &Path) -> io::Result<()> {
    match establish_in(request, state_dir) {
        Ok(config) => {
            if let Some(config) = &config {
                tracing::info!(config = %config.display(), "ssh panes read the session ssh config");
            }
            let _ = SESSION_CONFIG.set(Ok(config));
            Ok(())
        }
        Err(err) => {
            let _ = SESSION_CONFIG.set(Err(format!(
                "the session ssh config is unknown ({err}); restart the server with --ssh <file|none>"
            )));
            Err(err)
        }
    }
}

fn establish_in(
    request: Option<SshConfigRequest>,
    state_dir: &Path,
) -> io::Result<Option<PathBuf>> {
    let state = state_dir.join(STATE_FILE);
    match request {
        None => read_remembered(state_dir),
        Some(SshConfigRequest::Default) => match std::fs::remove_file(&state) {
            Ok(()) => Ok(None),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err),
        },
        Some(SshConfigRequest::File(path)) => {
            std::fs::create_dir_all(state_dir)?;
            let text = path.to_str().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "ssh config path is not UTF-8")
            })?;
            std::fs::write(&state, format!("{text}\n"))?;
            Ok(Some(path))
        }
    }
}

/// The config the session's server uses; `Ok(None)` reads ssh's defaults,
/// as does a process that never settled one.
pub(crate) fn current() -> Result<Option<PathBuf>, String> {
    SESSION_CONFIG.get().cloned().unwrap_or(Ok(None))
}

/// Only a missing file means "no setting". Why every other failure is an
/// error: falling back to ssh's defaults could reach another machine.
/// Why only absolute paths: the state file is user-editable, and a relative
/// path would resolve against whatever directory the server started in.
fn read_remembered(state_dir: &Path) -> io::Result<Option<PathBuf>> {
    let text = match std::fs::read_to_string(state_dir.join(STATE_FILE)) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    let path = PathBuf::from(text.trim_end_matches(['\n', '\r']));
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("remembered ssh config {} is not absolute", path.display()),
        ));
    }
    Ok(Some(path))
}

fn same_config(a: Option<&Path>, b: Option<&Path>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            crate::worktree::canonical_or_original(a) == crate::worktree::canonical_or_original(b)
        }
        _ => false,
    }
}

fn describe(config: Option<&Path>) -> String {
    match config {
        Some(path) => format!("ssh config {}", path.display()),
        None => "ssh's default configs".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| (*arg).to_owned()).collect()
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-ssh-session-config-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn launch_option_is_taken_in_both_spellings() {
        for argv in [
            args(&["herdr", "--ssh", "~/.ssh/prod"]),
            args(&["herdr", "--ssh=~/.ssh/prod"]),
        ] {
            let (cleaned, value) = extract_launch_arg(&argv).unwrap();
            assert_eq!(cleaned, ["herdr"]);
            assert_eq!(value.as_deref(), Some("~/.ssh/prod"));
        }
    }

    #[test]
    fn pane_and_container_commands_keep_their_own_ssh_host_option() {
        let argv = args(&["herdr", "pane", "open", "--ssh", "prod"]);

        let (cleaned, value) = extract_launch_arg(&argv).unwrap();

        assert_eq!(cleaned, argv);
        assert_eq!(value, None);
    }

    #[test]
    fn launch_option_rejects_a_missing_value_and_a_repeat() {
        assert!(extract_launch_arg(&args(&["herdr", "--ssh"])).is_err());
        assert!(extract_launch_arg(&args(&["herdr", "--ssh=a", "--ssh", "b"])).is_err());
    }

    #[test]
    fn other_launch_flags_stay_in_place() {
        let (cleaned, value) =
            extract_launch_arg(&args(&["herdr", "--version", "--ssh=/x"])).unwrap();

        assert_eq!(cleaned, ["herdr", "--version"]);
        assert_eq!(value.as_deref(), Some("/x"));
    }

    #[test]
    fn request_accepts_none_and_a_readable_file_and_rejects_the_rest() {
        let dir = temp_dir("resolve");
        let file = dir.join("prod-config");
        std::fs::write(&file, "Host prod\n").unwrap();

        assert_eq!(resolve_request("none"), Ok(SshConfigRequest::Default));
        assert_eq!(
            resolve_request(file.to_str().unwrap()),
            Ok(SshConfigRequest::File(file.clone()))
        );
        assert!(resolve_request("").is_err());
        assert!(resolve_request(dir.join("missing").to_str().unwrap()).is_err());
        assert!(resolve_request(dir.to_str().unwrap()).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn server_remembers_a_requested_file_until_a_default_request() {
        let dir = temp_dir("remember");
        let file = dir.join("prod-config");

        let chosen = establish_in(Some(SshConfigRequest::File(file.clone())), &dir).unwrap();
        let kept = establish_in(None, &dir).unwrap();
        let reset = establish_in(Some(SshConfigRequest::Default), &dir).unwrap();
        let after_reset = establish_in(None, &dir).unwrap();

        assert_eq!(chosen.as_ref(), Some(&file));
        assert_eq!(kept.as_ref(), Some(&file));
        assert_eq!(reset, None);
        assert_eq!(after_reset, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_request_that_cannot_be_remembered_is_an_error_not_the_defaults() {
        let dir = temp_dir("unwritable");
        std::fs::create_dir_all(dir.join(STATE_FILE)).unwrap();

        let result = establish_in(Some(SshConfigRequest::File(dir.join("prod-config"))), &dir);

        assert!(result.is_err(), "{result:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_relative_or_unreadable_remembered_config_is_an_error_not_the_defaults() {
        let relative = temp_dir("relative");
        std::fs::write(relative.join(STATE_FILE), "prod-config\n").unwrap();
        let unreadable = temp_dir("unreadable");
        std::fs::create_dir_all(unreadable.join(STATE_FILE)).unwrap();
        let missing = temp_dir("missing");

        assert!(establish_in(None, &relative).is_err());
        assert!(establish_in(None, &unreadable).is_err());
        assert!(apply_for_launch(&SshConfigRequest::Default, true, &unreadable).is_err());
        assert_eq!(establish_in(None, &missing).unwrap(), None);
        for dir in [relative, unreadable, missing] {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn ssh_option_needs_an_explicit_session_under_a_socket_override() {
        assert!(check_launch_target(false, true).is_err());
        assert_eq!(check_launch_target(true, true), Ok(()));
        assert_eq!(check_launch_target(false, false), Ok(()));
    }

    #[test]
    fn running_server_refuses_a_different_config_and_accepts_the_same_one() {
        let dir = temp_dir("running");
        let file = dir.join("prod-config");
        establish_in(Some(SshConfigRequest::File(file.clone())), &dir).unwrap();

        let same = apply_for_launch(&SshConfigRequest::File(file.clone()), true, &dir);
        let other = apply_for_launch(&SshConfigRequest::File(dir.join("dev-config")), true, &dir);
        let default = apply_for_launch(&SshConfigRequest::Default, true, &dir);

        assert_eq!(same, Ok(()));
        let other = other.unwrap_err();
        assert!(other.contains(&file.display().to_string()), "{other}");
        assert!(other.contains("herdr server stop"), "{other}");
        assert!(default.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn request_reaches_the_server_through_the_environment_and_is_cleared() {
        let dir = temp_dir("env");
        let file = dir.join("prod-config");
        std::env::remove_var(ENV_VAR);

        apply_for_launch(&SshConfigRequest::File(file.clone()), false, &dir).unwrap();
        let taken = take_server_request();

        assert_eq!(taken, Some(SshConfigRequest::File(file)));
        assert_eq!(std::env::var_os(ENV_VAR), None);
        assert_eq!(take_server_request(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
