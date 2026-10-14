use std::collections::HashMap;

use crate::api::schema::{
    Method, WorkspaceCreateParams, WorkspaceRenameParams, WorkspaceReportMetadataParams,
};

use super::container::WithContainer;

pub(super) fn run_workspace_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_workspace_help();
        return Ok(2);
    };

    match subcommand {
        "list" => workspace_list(&args[1..]),
        "create" => workspace_create(&args[1..]),
        "get" => workspace_get(&args[1..]),
        "focus" => workspace_focus(&args[1..]),
        "rename" => workspace_rename(&args[1..]),
        "report-metadata" => workspace_report_metadata(&args[1..]),
        "close" => workspace_close(&args[1..]),
        "help" | "--help" | "-h" => {
            print_workspace_help();
            Ok(0)
        }
        _ => {
            print_workspace_help();
            Ok(2)
        }
    }
}

fn workspace_list(args: &[String]) -> std::io::Result<i32> {
    if !args.is_empty() {
        eprintln!("usage: herdr workspace list");
        return Ok(2);
    }

    super::runtime::workspace_list()
}

fn workspace_create(args: &[String]) -> std::io::Result<i32> {
    match parse_workspace_create_args(args) {
        Ok(launch) => super::runtime::workspace_create(launch),
        Err(message) => {
            eprintln!("{message}");
            Ok(2)
        }
    }
}

fn parse_workspace_create_args(
    args: &[String],
) -> Result<WithContainer<WorkspaceCreateParams>, String> {
    let args = super::expand_equals_args(args, &["--container"]);
    let mut cwd = None;
    let mut focus = false;
    let mut label = None;
    let mut env = HashMap::new();
    let mut container = None;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--cwd" => {
                let Some(value) = args.get(index + 1) else {
                    return Err("missing value for --cwd".into());
                };
                cwd = Some(value.clone());
                index += 2;
            }
            "--label" => {
                let Some(value) = args.get(index + 1) else {
                    return Err("missing value for --label".into());
                };
                label = Some(value.clone());
                index += 2;
            }
            "--container" => {
                let Some(value) = args.get(index + 1) else {
                    return Err("missing value for --container".into());
                };
                container = Some(value.clone());
                index += 2;
            }
            "--focus" => {
                focus = true;
                index += 1;
            }
            "--no-focus" => {
                focus = false;
                index += 1;
            }
            "--env" => {
                let Some(value) = args.get(index + 1) else {
                    return Err("missing value for --env".into());
                };
                let (key, value) = super::parse_env_assignment(value)?;
                env.insert(key, value);
                index += 2;
            }
            other => return Err(format!("unknown option: {other}")),
        }
    }

    Ok(WithContainer {
        params: WorkspaceCreateParams {
            source_workspace_id: None,
            cwd,
            focus,
            label,
            env,
        },
        container,
    })
}

fn workspace_get(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_workspace_id) = args.first() else {
        eprintln!("usage: herdr workspace get <workspace_id>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr workspace get <workspace_id>");
        return Ok(2);
    }

    super::runtime::workspace_get(super::normalize_workspace_id(raw_workspace_id))
}

fn workspace_focus(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_workspace_id) = args.first() else {
        eprintln!("usage: herdr workspace focus <workspace_id>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr workspace focus <workspace_id>");
        return Ok(2);
    }

    super::runtime::workspace_focus(super::normalize_workspace_id(raw_workspace_id))
}

fn workspace_rename(args: &[String]) -> std::io::Result<i32> {
    if args.len() < 2 {
        eprintln!("usage: herdr workspace rename <workspace_id> <label>");
        return Ok(2);
    }

    super::runtime::workspace_rename(WorkspaceRenameParams {
        workspace_id: super::normalize_workspace_id(&args[0]),
        label: args[1..].join(" "),
    })
}

fn workspace_report_metadata(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_workspace_id) = args.first() else {
        eprintln!("usage: herdr workspace report-metadata <workspace_id> --source ID [--token NAME=VALUE] [--clear-token NAME] [--seq N] [--ttl-ms N]");
        return Ok(2);
    };
    let workspace_id = super::normalize_workspace_id(raw_workspace_id);
    let mut source = None;
    let mut tokens = HashMap::new();
    let mut seq = None;
    let mut ttl_ms = None;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--source" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --source");
                    return Ok(2);
                };
                source = Some(value.clone());
                index += 2;
            }
            "--token" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --token");
                    return Ok(2);
                };
                let (key, value) = match super::parse_token_assignment(value) {
                    Ok(token) => token,
                    Err(message) => {
                        eprintln!("{message}");
                        return Ok(2);
                    }
                };
                tokens.insert(key, value);
                index += 2;
            }
            "--clear-token" => {
                let Some(key) = args.get(index + 1) else {
                    eprintln!("missing value for --clear-token");
                    return Ok(2);
                };
                tokens.insert(key.clone(), None);
                index += 2;
            }
            "--seq" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --seq");
                    return Ok(2);
                };
                seq = Some(super::parse_u64_flag("--seq", value)?);
                index += 2;
            }
            "--ttl-ms" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --ttl-ms");
                    return Ok(2);
                };
                ttl_ms = Some(super::parse_u64_flag("--ttl-ms", value)?);
                index += 2;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }
    let Some(source) = source.filter(|source| !source.trim().is_empty()) else {
        eprintln!("missing required --source");
        return Ok(2);
    };
    if tokens.is_empty() {
        eprintln!("missing token to set or clear");
        return Ok(2);
    }
    super::send_ok_request(Method::WorkspaceReportMetadata(
        WorkspaceReportMetadataParams {
            workspace_id,
            source,
            tokens,
            seq,
            ttl_ms,
        },
    ))
}

fn workspace_close(args: &[String]) -> std::io::Result<i32> {
    let (raw_workspace_id, close_group) = match args {
        [workspace_id] => (workspace_id, false),
        [workspace_id, flag] if flag == "--group" => (workspace_id, true),
        _ => {
            eprintln!("usage: herdr workspace close <workspace_id> [--group]");
            return Ok(2);
        }
    };

    super::runtime::workspace_close(crate::api::schema::WorkspaceCloseParams {
        workspace_id: super::normalize_workspace_id(raw_workspace_id),
        close_group,
    })
}

fn print_workspace_help() {
    eprintln!("herdr workspace commands:");
    eprintln!("  herdr workspace list");
    eprintln!("  herdr workspace create [--cwd PATH] [--label TEXT] [--env KEY=VALUE] [--container REF] [--focus] [--no-focus]");
    eprintln!("  herdr workspace get <workspace_id>");
    eprintln!("  herdr workspace focus <workspace_id>");
    eprintln!("  herdr workspace rename <workspace_id> <label>");
    eprintln!("  herdr workspace report-metadata <workspace_id> --source ID [--token NAME=VALUE] [--clear-token NAME] [--seq N] [--ttl-ms N]");
    eprintln!("  herdr workspace close <workspace_id> [--group]");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn workspace_create_parses_container_option() {
        let launch = parse_workspace_create_args(&args(&[
            "--container",
            "app/web",
            "--label",
            "api",
            "--env",
            "MODE=dev",
            "--focus",
        ]))
        .unwrap();

        assert_eq!(launch.container.as_deref(), Some("app/web"));
        assert_eq!(launch.params.label.as_deref(), Some("api"));
        assert_eq!(
            launch.params.env.get("MODE").map(String::as_str),
            Some("dev")
        );
        assert!(launch.params.focus);
    }

    #[test]
    fn workspace_create_accepts_container_equals_form() {
        let launch = parse_workspace_create_args(&args(&["--container=app/web"])).unwrap();

        assert_eq!(launch.container.as_deref(), Some("app/web"));
    }

    #[test]
    fn workspace_create_without_container_stays_a_plain_create() {
        let launch = parse_workspace_create_args(&args(&["--cwd", "/repo"])).unwrap();

        assert_eq!(launch.container, None);
        assert_eq!(launch.params.cwd.as_deref(), Some("/repo"));
    }

    #[test]
    fn workspace_create_rejects_container_without_value() {
        assert_eq!(
            parse_workspace_create_args(&args(&["--container"])),
            Err("missing value for --container".into())
        );
    }
}
