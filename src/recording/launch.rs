//! `herdr --log <dir>` and `--log-input`: how a launch asks the session's
//! server to record panes, and how the session remembers it.
//!
//! The client resolves the options and hands them to the server it spawns
//! through [`DIR_ENV_VAR`] and [`INPUT_ENV_VAR`]. The server is the only
//! writer of the remembered value, so a later launch without `--log` keeps
//! recording into the same folder.

use std::io;
use std::path::{Path, PathBuf};

use super::RecordingSettings;

const DIR_ENV_VAR: &str = "HERDR_PANE_LOG";
const INPUT_ENV_VAR: &str = "HERDR_PANE_LOG_INPUT";
/// `--log none` stops a session from recording.
const OFF_KEYWORD: &str = "none";
const STATE_FILE: &str = "pane-log.json";
const DIR_OPTION: &str = "--log";
const INPUT_OPTION: &str = "--log-input";
const SSH_OPTION: &str = "--ssh";

/// What a launch asked the session to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RecordingRequest {
    Off,
    On(RecordingSettings),
}

impl RecordingRequest {
    fn settings(&self) -> Option<&RecordingSettings> {
        match self {
            Self::Off => None,
            Self::On(settings) => Some(settings),
        }
    }
}

/// The launch options as typed, before any path is resolved.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct LaunchArgs {
    pub dir: Option<String>,
    pub input: bool,
}

/// Removes `--log <dir>`, `--log=<dir>` and `--log-input` from `args`.
///
/// Why only before the first command word: subcommands own their options,
/// and a later `--log` there must keep meaning what that command says.
pub(crate) fn extract_launch_args(args: &[String]) -> Result<(Vec<String>, LaunchArgs), String> {
    let mut cleaned = Vec::with_capacity(args.len());
    cleaned.extend(args.first().cloned());
    let mut parsed = LaunchArgs::default();
    let mut index = 1;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--" || !arg.starts_with('-') {
            cleaned.extend_from_slice(&args[index..]);
            break;
        }
        index += 1;
        let dir = if arg == DIR_OPTION {
            let value = args
                .get(index)
                .ok_or_else(|| format!("missing value for {DIR_OPTION}"))?;
            index += 1;
            value.clone()
        } else if let Some(inline) = arg.strip_prefix("--log=") {
            inline.to_owned()
        } else if arg == SSH_OPTION {
            // `--ssh <file>` is another launch option; its value is not a
            // command word, and the ssh scanner takes both afterwards.
            cleaned.push(arg.clone());
            if let Some(value) = args.get(index) {
                cleaned.push(value.clone());
                index += 1;
            }
            continue;
        } else if arg == INPUT_OPTION {
            if parsed.input {
                return Err(format!("{INPUT_OPTION} can only be specified once"));
            }
            parsed.input = true;
            continue;
        } else {
            cleaned.push(arg.clone());
            continue;
        };
        if parsed.dir.replace(dir).is_some() {
            return Err(format!("{DIR_OPTION} can only be specified once"));
        }
    }
    Ok((cleaned, parsed))
}

/// Resolves the typed options: `none`, or a folder given as an absolute
/// path, a relative path, or a `~` path the shell left unexpanded.
pub(crate) fn resolve_request(args: LaunchArgs) -> Result<Option<RecordingRequest>, String> {
    let Some(raw) = args.dir else {
        if args.input {
            return Err(format!("{INPUT_OPTION} needs {DIR_OPTION} <dir>"));
        }
        return Ok(None);
    };
    if raw == OFF_KEYWORD {
        if args.input {
            return Err(format!(
                "{INPUT_OPTION} cannot be combined with {DIR_OPTION} {OFF_KEYWORD}"
            ));
        }
        return Ok(Some(RecordingRequest::Off));
    }
    if raw.is_empty() {
        return Err(format!(
            "{DIR_OPTION} needs a folder, or `{OFF_KEYWORD}` to stop recording"
        ));
    }
    let dir = crate::worktree::expand_tilde_absolute_path(&raw);
    // Why check now: a file in the way would otherwise surface only as
    // panes that silently record nothing.
    if dir.exists() && !dir.is_dir() {
        return Err(format!("log folder {} is not a folder", dir.display()));
    }
    Ok(Some(RecordingRequest::On(RecordingSettings {
        dir,
        input: args.input,
    })))
}

/// Refuses `--log` when a socket override points this launch at a server
/// whose session this process cannot name.
///
/// Why: the remembered choice is read from the selected session's folder,
/// while a socket override from the environment may attach to another
/// session's server. An explicit `--session` replaces the overrides.
pub(crate) fn check_launch_target(
    explicit_session: bool,
    socket_override: bool,
) -> Result<(), String> {
    if socket_override && !explicit_session {
        return Err(format!(
            "{DIR_OPTION} needs --session <name> while HERDR_SOCKET_PATH or HERDR_CLIENT_SOCKET_PATH selects the server"
        ));
    }
    Ok(())
}

/// Hands `request` to a server this launch spawns, or refuses it when the
/// session's running server remembers something else.
///
/// Why refuse instead of switching: the running panes already record (or
/// not) under the remembered choice, and a silent mismatch would leave an
/// audit trail the user believes exists but does not.
/// `stop_command` stops this session's server, e.g. `herdr session stop prod`.
pub(crate) fn apply_for_launch(
    request: &RecordingRequest,
    server_running: bool,
    state_dir: &Path,
    stop_command: &str,
) -> Result<(), String> {
    if server_running {
        let remembered = read_remembered(state_dir)
            .map_err(|err| format!("cannot read this session's pane recording choice: {err}"))?;
        if !same_settings(remembered.as_ref(), request.settings()) {
            return Err(format!(
                "this session's server is {}; run `{stop_command}` first to switch to {}",
                describe(remembered.as_ref()),
                describe(request.settings()),
            ));
        }
        return Ok(());
    }
    match request {
        RecordingRequest::Off => std::env::set_var(DIR_ENV_VAR, OFF_KEYWORD),
        RecordingRequest::On(settings) => {
            check_writable(&settings.dir.join(super::session_folder_name()))?;
            std::env::set_var(DIR_ENV_VAR, &settings.dir);
            if settings.input {
                std::env::set_var(INPUT_ENV_VAR, "1");
            } else {
                std::env::remove_var(INPUT_ENV_VAR);
            }
        }
    }
    Ok(())
}

/// Creates the folder and proves a file can be made in it. Why at launch:
/// the server finds out only when a pane opens, and then reports it only on
/// that pane, so the user would believe the session is being recorded.
fn check_writable(dir: &Path) -> Result<(), String> {
    let cannot = |err: io::Error| format!("cannot record into {}: {err}", dir.display());
    super::sink::ensure_private_dir(dir).map_err(cannot)?;
    let probe = dir.join(format!(".herdr-log-check-{}", std::process::id()));
    crate::platform::create_config_temporary(&probe, true).map_err(cannot)?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

/// Reads and clears the request the launching client left in the server's
/// environment. Why clear it: pane shells inherit the server's environment,
/// and a nested herdr must not record into this session's folder.
pub(crate) fn take_server_request() -> Option<RecordingRequest> {
    let dir = std::env::var_os(DIR_ENV_VAR);
    let input = std::env::var_os(INPUT_ENV_VAR).is_some_and(|value| value == "1");
    std::env::remove_var(DIR_ENV_VAR);
    std::env::remove_var(INPUT_ENV_VAR);
    let dir = dir?;
    if dir == OFF_KEYWORD {
        return Some(RecordingRequest::Off);
    }
    let dir = PathBuf::from(dir);
    // Why only absolute paths: the server starts in another directory than
    // the shell that typed a relative path.
    dir.is_absolute()
        .then_some(RecordingRequest::On(RecordingSettings { dir, input }))
}

/// Settles the session's choice at server start: a request replaces the
/// remembered one, and no request keeps it.
pub(crate) fn establish(
    request: Option<RecordingRequest>,
    state_dir: &Path,
) -> io::Result<Option<RecordingSettings>> {
    let state = state_dir.join(STATE_FILE);
    match request {
        None => read_remembered(state_dir),
        Some(RecordingRequest::Off) => match std::fs::remove_file(&state) {
            Ok(()) => Ok(None),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err),
        },
        Some(RecordingRequest::On(settings)) => {
            std::fs::create_dir_all(state_dir)?;
            let json = serde_json::to_string(&settings).map_err(io::Error::other)?;
            std::fs::write(&state, format!("{json}\n"))?;
            Ok(Some(settings))
        }
    }
}

/// Only a missing file means "not recording". Why not treat a broken one the
/// same: a restart would then silently stop an audit trail.
fn read_remembered(state_dir: &Path) -> io::Result<Option<RecordingSettings>> {
    let path = state_dir.join(STATE_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    let invalid = |reason: String| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}: {reason}", path.display()),
        )
    };
    let settings: RecordingSettings =
        serde_json::from_str(&text).map_err(|err| invalid(err.to_string()))?;
    // Why only absolute paths: the state file is user-editable, and a
    // relative path would resolve against wherever the server started.
    if !settings.dir.is_absolute() {
        return Err(invalid("the log folder must be an absolute path".into()));
    }
    Ok(Some(settings))
}

fn same_settings(a: Option<&RecordingSettings>, b: Option<&RecordingSettings>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            a.input == b.input
                && crate::worktree::canonical_or_original(&a.dir)
                    == crate::worktree::canonical_or_original(&b.dir)
        }
        _ => false,
    }
}

fn describe(settings: Option<&RecordingSettings>) -> String {
    match settings {
        None => "not recording panes".into(),
        Some(settings) if settings.input => format!(
            "recording pane output and input into {}",
            settings.dir.display()
        ),
        Some(settings) => format!("recording pane output into {}", settings.dir.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| (*arg).to_owned()).collect()
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-recording-launch-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn on(dir: &Path, input: bool) -> RecordingRequest {
        RecordingRequest::On(RecordingSettings {
            dir: dir.to_path_buf(),
            input,
        })
    }

    #[test]
    fn launch_options_are_taken_out_and_the_rest_is_kept_in_order() {
        let (rest, parsed) = extract_launch_args(&args(&[
            "herdr",
            "--handoff",
            "--log",
            "/logs",
            "--log-input",
        ]))
        .unwrap();
        assert_eq!(rest, args(&["herdr", "--handoff"]));
        assert_eq!(
            parsed,
            LaunchArgs {
                dir: Some("/logs".into()),
                input: true
            }
        );
    }

    #[test]
    fn an_ssh_config_before_or_after_log_stays_for_the_ssh_scanner() {
        for order in [
            ["herdr", "--ssh", "none", "--log", "/logs"],
            ["herdr", "--log", "/logs", "--ssh", "none"],
        ] {
            let (rest, parsed) = extract_launch_args(&args(&order)).unwrap();
            assert_eq!(rest, args(&["herdr", "--ssh", "none"]), "{order:?}");
            assert_eq!(parsed.dir.as_deref(), Some("/logs"), "{order:?}");
        }
    }

    #[test]
    fn a_broken_remembered_choice_is_an_error_and_a_missing_one_is_not() {
        let state = scratch("broken");
        assert_eq!(establish(None, &state).unwrap(), None);
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(state.join(STATE_FILE), "{not json").unwrap();
        assert_eq!(
            establish(None, &state).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        std::fs::write(
            state.join(STATE_FILE),
            r#"{"dir":"relative","input":false}"#,
        )
        .unwrap();
        assert!(establish(None, &state).is_err());
        assert!(
            apply_for_launch(&RecordingRequest::Off, true, &state, "herdr server stop").is_err()
        );
        std::fs::remove_dir_all(&state).unwrap();
    }

    #[test]
    fn the_inline_form_is_accepted() {
        let (_, parsed) = extract_launch_args(&args(&["herdr", "--log=/logs"])).unwrap();
        assert_eq!(parsed.dir.as_deref(), Some("/logs"));
    }

    #[test]
    fn options_after_the_first_command_word_belong_to_that_command() {
        let original = args(&["herdr", "pane", "run", "--log", "x"]);
        let (rest, parsed) = extract_launch_args(&original).unwrap();
        assert_eq!(rest, original);
        assert_eq!(parsed, LaunchArgs::default());
    }

    #[test]
    fn a_repeated_or_valueless_option_is_refused() {
        assert!(extract_launch_args(&args(&["herdr", "--log", "a", "--log", "b"])).is_err());
        assert!(extract_launch_args(&args(&["herdr", "--log-input", "--log-input"])).is_err());
        assert!(extract_launch_args(&args(&["herdr", "--log"])).is_err());
    }

    #[test]
    fn log_input_alone_or_with_none_is_refused() {
        let input_only = LaunchArgs {
            dir: None,
            input: true,
        };
        assert!(resolve_request(input_only).is_err());
        let off_with_input = LaunchArgs {
            dir: Some("none".into()),
            input: true,
        };
        assert!(resolve_request(off_with_input).is_err());
    }

    #[test]
    fn none_turns_recording_off_and_a_relative_folder_becomes_absolute() {
        let off = LaunchArgs {
            dir: Some("none".into()),
            input: false,
        };
        assert_eq!(resolve_request(off).unwrap(), Some(RecordingRequest::Off));
        let relative = LaunchArgs {
            dir: Some("logs".into()),
            input: false,
        };
        let Some(RecordingRequest::On(settings)) = resolve_request(relative).unwrap() else {
            panic!("expected recording on");
        };
        assert!(settings.dir.is_absolute());
        assert!(settings.dir.ends_with("logs"));
    }

    #[test]
    fn a_file_in_place_of_the_folder_is_refused() {
        let dir = scratch("file");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("taken");
        std::fs::write(&file, b"").unwrap();
        let request = LaunchArgs {
            dir: Some(file.to_string_lossy().into_owned()),
            input: false,
        };
        assert!(resolve_request(request).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_server_remembers_a_request_and_keeps_it_when_a_later_start_has_none() {
        let state = scratch("remember");
        let logs = state.join("logs");
        assert_eq!(
            establish(Some(on(&logs, true)), &state).unwrap(),
            Some(RecordingSettings {
                dir: logs.clone(),
                input: true
            })
        );
        assert_eq!(
            establish(None, &state)
                .unwrap()
                .map(|settings| settings.dir),
            Some(logs)
        );
        assert_eq!(
            establish(Some(RecordingRequest::Off), &state).unwrap(),
            None
        );
        assert_eq!(establish(None, &state).unwrap(), None);
        std::fs::remove_dir_all(state).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_that_cannot_be_written_is_refused_at_launch() {
        use std::os::unix::fs::PermissionsExt;
        let root = scratch("read-only");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o500)).unwrap();
        let refused = check_writable(&root.join("logs"));
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        // Why skip when it passed: root ignores directory permissions.
        if refused.is_ok() {
            return;
        }
        assert!(refused.unwrap_err().contains("cannot record into"));
    }

    #[test]
    fn a_new_folder_is_created_and_left_without_the_check_file() {
        let root = scratch("writable");
        let logs = root.join("logs");
        check_writable(&logs).unwrap();
        assert!(logs.is_dir());
        assert_eq!(std::fs::read_dir(&logs).unwrap().count(), 0);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_remembered_choice_that_cannot_be_cleared_is_an_error() {
        use std::os::unix::fs::PermissionsExt;
        let state = scratch("cannot-clear");
        establish(Some(on(&state.join("logs"), true)), &state).unwrap();
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o500)).unwrap();
        let cleared = establish(Some(RecordingRequest::Off), &state);
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::remove_dir_all(&state).unwrap();
        // Why skip when it passed: root ignores directory permissions.
        if cleared.is_ok() {
            return;
        }
        assert_eq!(cleared.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn a_socket_override_needs_an_explicit_session() {
        assert!(check_launch_target(false, true).is_err());
        assert!(check_launch_target(true, true).is_ok());
        assert!(check_launch_target(false, false).is_ok());
    }

    #[test]
    fn a_running_server_refuses_a_launch_that_asks_for_something_else() {
        let state = scratch("running");
        let logs = state.join("logs");
        establish(Some(on(&logs, false)), &state).unwrap();

        assert!(apply_for_launch(&on(&logs, false), true, &state, "herdr server stop").is_ok());
        let err =
            apply_for_launch(&on(&logs, true), true, &state, "herdr server stop").unwrap_err();
        assert!(err.contains("herdr server stop"), "{err}");
        assert!(
            apply_for_launch(&RecordingRequest::Off, true, &state, "herdr server stop").is_err()
        );
        assert!(apply_for_launch(
            &on(&state.join("other"), false),
            true,
            &state,
            "herdr server stop"
        )
        .is_err());
        std::fs::remove_dir_all(state).unwrap();
    }
}
