use std::collections::HashMap;

use crate::api::schema::{TabCreateParams, TabListParams, TabRenameParams};

use super::container::WithContainer;

pub(super) fn run_tab_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_tab_help();
        return Ok(2);
    };

    match subcommand {
        "list" => tab_list(&args[1..]),
        "create" => tab_create(&args[1..]),
        "get" => tab_get(&args[1..]),
        "focus" => tab_focus(&args[1..]),
        "rename" => tab_rename(&args[1..]),
        "close" => tab_close(&args[1..]),
        "help" | "--help" | "-h" => {
            print_tab_help();
            Ok(0)
        }
        _ => {
            print_tab_help();
            Ok(2)
        }
    }
}

fn tab_list(args: &[String]) -> std::io::Result<i32> {
    let mut workspace_id = None;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--workspace" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --workspace");
                    return Ok(2);
                };
                workspace_id = Some(super::normalize_workspace_id(value));
                index += 2;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    super::runtime::tab_list(TabListParams { workspace_id })
}

fn tab_create(args: &[String]) -> std::io::Result<i32> {
    match parse_tab_create_args(args) {
        Ok(launch) => super::runtime::tab_create(launch),
        Err(message) => {
            eprintln!("{message}");
            Ok(2)
        }
    }
}

fn parse_tab_create_args(args: &[String]) -> Result<WithContainer<TabCreateParams>, String> {
    let args = super::expand_equals_args(args, &["--container"]);
    let mut workspace_id = None;
    let mut cwd = None;
    let mut focus = false;
    let mut label = None;
    let mut env = HashMap::new();
    let mut container = None;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--workspace" => {
                let Some(value) = args.get(index + 1) else {
                    return Err("missing value for --workspace".into());
                };
                workspace_id = Some(super::normalize_workspace_id(value));
                index += 2;
            }
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
        params: TabCreateParams {
            workspace_id,
            cwd,
            focus,
            label,
            env,
        },
        container,
    })
}

fn tab_get(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_tab_id) = args.first() else {
        eprintln!("usage: herdr tab get <tab_id>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr tab get <tab_id>");
        return Ok(2);
    }

    super::runtime::tab_get(super::normalize_tab_id(raw_tab_id))
}

fn tab_focus(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_tab_id) = args.first() else {
        eprintln!("usage: herdr tab focus <tab_id>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr tab focus <tab_id>");
        return Ok(2);
    }

    super::runtime::tab_focus(super::normalize_tab_id(raw_tab_id))
}

fn tab_rename(args: &[String]) -> std::io::Result<i32> {
    if args.len() < 2 {
        eprintln!("usage: herdr tab rename <tab_id> <label>");
        return Ok(2);
    }

    super::runtime::tab_rename(TabRenameParams {
        tab_id: super::normalize_tab_id(&args[0]),
        label: args[1..].join(" "),
    })
}

fn tab_close(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_tab_id) = args.first() else {
        eprintln!("usage: herdr tab close <tab_id>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr tab close <tab_id>");
        return Ok(2);
    }

    super::runtime::tab_close(super::normalize_tab_id(raw_tab_id))
}

fn print_tab_help() {
    eprintln!("herdr tab commands:");
    eprintln!("  herdr tab list [--workspace <workspace_id>]");
    eprintln!(
        "  herdr tab create [--workspace <workspace_id>] [--cwd PATH] [--label TEXT] [--env KEY=VALUE] [--container REF] [--focus] [--no-focus]"
    );
    eprintln!("  herdr tab get <tab_id>");
    eprintln!("  herdr tab focus <tab_id>");
    eprintln!("  herdr tab rename <tab_id> <label>");
    eprintln!("  herdr tab close <tab_id>");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn tab_create_parses_container_option() {
        let launch = parse_tab_create_args(&args(&[
            "--workspace",
            "w1",
            "--container",
            "worker",
            "--cwd",
            "/repo",
        ]))
        .unwrap();

        assert_eq!(launch.container.as_deref(), Some("worker"));
        assert_eq!(launch.params.workspace_id.as_deref(), Some("w1"));
        assert_eq!(launch.params.cwd.as_deref(), Some("/repo"));
    }

    #[test]
    fn tab_create_accepts_container_equals_form() {
        let launch = parse_tab_create_args(&args(&["--container=app/web"])).unwrap();

        assert_eq!(launch.container.as_deref(), Some("app/web"));
    }

    #[test]
    fn tab_create_without_container_stays_a_plain_create() {
        let launch = parse_tab_create_args(&args(&["--label", "logs"])).unwrap();

        assert_eq!(launch.container, None);
        assert_eq!(launch.params.label.as_deref(), Some("logs"));
    }

    #[test]
    fn tab_create_rejects_container_without_value() {
        assert_eq!(
            parse_tab_create_args(&args(&["--container"])),
            Err("missing value for --container".into())
        );
    }
}
