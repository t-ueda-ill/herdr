#[path = "container_bindings.rs"]
pub(crate) mod bindings;

pub(crate) use bindings::command;

use crate::api::schema::{ContainerInfoDto, ResponseResult, SuccessResponse};

use super::projection::ParseError;

pub(super) fn run_container_command(args: &[String]) -> std::io::Result<i32> {
    let (request, json) = match bindings::request(args) {
        Ok(parsed) => parsed,
        Err(ParseError::Usage(message)) => {
            eprintln!("{message}");
            return Ok(2);
        }
        Err(ParseError::Io(error)) => return Err(error),
        Err(ParseError::Clap(error)) => return super::print_clap_error(&error),
    };
    let response = super::send_request(&request)?;
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
    // Not always shown: a local listing keeps its familiar columns.
    let show_host = containers
        .iter()
        .any(|container| container.ssh_host.is_some());
    let mut header = vec![
        "reference",
        "name",
        "project/service",
        "workspace",
        "default",
    ];
    if show_host {
        header.push("host");
    }
    let rows = std::iter::once(header.into_iter().map(String::from).collect::<Vec<_>>())
        .chain(containers.iter().map(|container| {
            let mut row = vec![
                container.reference.clone(),
                container.name.clone(),
                match (&container.project, &container.service) {
                    (Some(project), Some(service)) => format!("{project}/{service}"),
                    _ => "-".into(),
                },
                container.workspace_id.clone().unwrap_or_else(|| "-".into()),
                if container.is_default { "*" } else { "" }.into(),
            ];
            if show_host {
                row.push(container.ssh_host.clone().unwrap_or_else(|| "-".into()));
            }
            row
        }))
        .collect::<Vec<_>>();
    let mut widths = vec![0; rows[0].len()];
    for row in &rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }

    let mut table = String::new();
    for row in &rows {
        let line = row
            .iter()
            .zip(&widths)
            .map(|(cell, width)| format!("{cell:<width$}"))
            .collect::<Vec<_>>()
            .join("  ");
        table.push_str(line.trim_end());
        table.push('\n');
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    fn container(reference: &str, name: &str) -> ContainerInfoDto {
        ContainerInfoDto {
            reference: reference.into(),
            id: format!("{name}-id"),
            name: name.into(),
            project: None,
            service: None,
            working_dir: None,
            workspace_id: None,
            ssh_host: None,
            is_default: false,
        }
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
            ssh_host: None,
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

    #[test]
    fn container_table_adds_a_host_column_for_containers_of_an_ssh_host() {
        let remote = ContainerInfoDto {
            ssh_host: Some("prod".into()),
            is_default: true,
            ..container("scratch", "scratch")
        };
        let local = container("cache", "cache");

        assert_eq!(
            format_container_table(&[remote, local]),
            concat!(
                "reference  name     project/service  workspace  default  host\n",
                "scratch    scratch  -                -          *        prod\n",
                "cache      cache    -                -                   -\n",
            )
        );
    }
}
