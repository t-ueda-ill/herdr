use crate::api::schema::{
    ContainerInfoDto, ContainerListParams, ContainerOpenParams, ContainerOpenTarget, Method,
    Request, ResponseResult, SuccessResponse,
};

const CONTAINER_LIST_USAGE: &str = "usage: herdr container list [--workspace ID] [--json]";

/// Fully parsed create/split params plus the `--container REF` that may reroute them.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct WithContainer<P> {
    pub(super) params: P,
    pub(super) container: Option<String>,
}

impl<P> WithContainer<P> {
    // Not a `container` field on the create/split params: those shapes are frozen, and an
    // older server would ignore the field and start a host shell, so the reference travels
    // in `container.open` with the unchanged params nested inside.
    pub(super) fn into_method(
        self,
        plain: fn(P) -> Method,
        nested: fn(P) -> ContainerOpenTarget,
    ) -> Method {
        match self.container {
            Some(container) => Method::ContainerOpen(ContainerOpenParams {
                container,
                target: nested(self.params),
            }),
            None => plain(self.params),
        }
    }
}

pub(super) fn run_container_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(String::as_str) else {
        print_container_help();
        return Ok(2);
    };

    match subcommand {
        "list" => container_list(&args[1..]),
        "help" | "--help" | "-h" => {
            print_container_help();
            Ok(0)
        }
        _ => {
            print_container_help();
            Ok(2)
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct ContainerListArgs {
    params: ContainerListParams,
    json: bool,
}

fn parse_container_list_args(args: &[String]) -> Result<ContainerListArgs, String> {
    let args = super::expand_equals_args(args, &["--workspace"]);
    let mut workspace_id = None;
    let mut json = false;

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
            "--json" => {
                json = true;
                index += 1;
            }
            other => return Err(format!("unknown option: {other}")),
        }
    }

    Ok(ContainerListArgs {
        params: ContainerListParams { workspace_id },
        json,
    })
}

fn container_list(args: &[String]) -> std::io::Result<i32> {
    let ContainerListArgs { params, json } = match parse_container_list_args(args) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("{message}");
            eprintln!("{CONTAINER_LIST_USAGE}");
            return Ok(2);
        }
    };
    let response = super::send_request(&Request {
        id: "cli:container:list".into(),
        method: Method::ContainerList(params),
    })?;
    if json {
        return super::print_response(&response);
    }
    print_container_list_human(&response)
}

fn print_container_list_human(response: &serde_json::Value) -> std::io::Result<i32> {
    // Not folded into the empty-table message: `container_unavailable` stays a JSON error on
    // stderr so scripts can tell a missing docker daemon apart from zero running containers.
    if response.get("error").is_some() {
        return super::print_response(response);
    }
    let parsed: SuccessResponse =
        serde_json::from_value(response.clone()).map_err(std::io::Error::other)?;
    let ResponseResult::ContainerList { containers } = parsed.result else {
        return super::print_response(response);
    };
    print!("{}", format_container_table(&containers));
    Ok(0)
}

fn format_container_table(containers: &[ContainerInfoDto]) -> String {
    if containers.is_empty() {
        return "No running containers.\n".into();
    }
    let header = [
        "reference",
        "name",
        "project/service",
        "workspace",
        "default",
    ]
    .map(String::from);
    let rows = std::iter::once(header)
        .chain(containers.iter().map(|container| {
            [
                container.reference.clone(),
                container.name.clone(),
                match (&container.project, &container.service) {
                    (Some(project), Some(service)) => format!("{project}/{service}"),
                    _ => "-".into(),
                },
                container.workspace_id.clone().unwrap_or_else(|| "-".into()),
                if container.is_default { "*" } else { "" }.into(),
            ]
        }))
        .collect::<Vec<_>>();
    let mut widths = [0; 5];
    for row in &rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }

    let mut table = String::new();
    for row in &rows {
        let line = row
            .iter()
            .zip(widths)
            .map(|(cell, width)| format!("{cell:<width$}"))
            .collect::<Vec<_>>()
            .join("  ");
        table.push_str(line.trim_end());
        table.push('\n');
    }
    table
}

fn print_container_help() {
    eprintln!("herdr container commands:");
    eprintln!("  {}", CONTAINER_LIST_USAGE.trim_start_matches("usage: "));
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::api::schema::{PaneRightClickTarget, PaneSplitParams, SplitDirection};

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    fn split_params() -> PaneSplitParams {
        PaneSplitParams {
            workspace_id: None,
            target_pane_id: Some("w1:p1".into()),
            direction: SplitDirection::Right,
            ratio: None,
            cwd: None,
            focus: false,
            right_click: PaneRightClickTarget::Herdr,
            env: HashMap::new(),
        }
    }

    fn container(reference: &str, name: &str) -> ContainerInfoDto {
        ContainerInfoDto {
            reference: reference.into(),
            id: format!("{name}-id"),
            name: name.into(),
            project: None,
            service: None,
            working_dir: None,
            workspace_id: None,
            is_default: false,
        }
    }

    #[test]
    fn container_reference_nests_unchanged_params_in_container_open() {
        let method = WithContainer {
            params: split_params(),
            container: Some("app/web".into()),
        }
        .into_method(Method::PaneSplit, ContainerOpenTarget::Split);

        assert_eq!(
            method,
            Method::ContainerOpen(ContainerOpenParams {
                container: "app/web".into(),
                target: ContainerOpenTarget::Split(split_params()),
            })
        );
    }

    #[test]
    fn missing_container_reference_keeps_the_plain_method() {
        let method = WithContainer {
            params: split_params(),
            container: None,
        }
        .into_method(Method::PaneSplit, ContainerOpenTarget::Split);

        assert_eq!(method, Method::PaneSplit(split_params()));
    }

    #[test]
    fn container_list_defaults_to_all_workspaces_and_text_output() {
        let parsed = parse_container_list_args(&[]).unwrap();

        assert_eq!(
            parsed,
            ContainerListArgs {
                params: ContainerListParams { workspace_id: None },
                json: false,
            }
        );
    }

    #[test]
    fn container_list_parses_workspace_filter_and_json_flag() {
        let parsed = parse_container_list_args(&args(&["--workspace", "w2", "--json"])).unwrap();

        assert_eq!(parsed.params.workspace_id.as_deref(), Some("w2"));
        assert!(parsed.json);
    }

    #[test]
    fn container_list_accepts_workspace_equals_form() {
        let parsed = parse_container_list_args(&args(&["--workspace=w3"])).unwrap();

        assert_eq!(parsed.params.workspace_id.as_deref(), Some("w3"));
    }

    #[test]
    fn container_list_rejects_missing_workspace_value_and_unknown_options() {
        assert_eq!(
            parse_container_list_args(&args(&["--workspace"])),
            Err("missing value for --workspace".into())
        );
        assert_eq!(
            parse_container_list_args(&args(&["--all"])),
            Err("unknown option: --all".into())
        );
    }

    #[test]
    fn container_table_reports_when_nothing_is_running() {
        assert_eq!(format_container_table(&[]), "No running containers.\n");
    }

    #[test]
    fn container_table_aligns_columns_and_marks_the_workspace_default() {
        let compose = ContainerInfoDto {
            project: Some("app".into()),
            service: Some("web".into()),
            workspace_id: Some("w1".into()),
            is_default: true,
            ..container("app/web", "app-web-1")
        };
        let plain = container("scratch", "scratch");

        assert_eq!(
            format_container_table(&[compose, plain]),
            concat!(
                "reference  name       project/service  workspace  default\n",
                "app/web    app-web-1  app/web          w1         *\n",
                "scratch    scratch    -                -\n",
            )
        );
    }
}
