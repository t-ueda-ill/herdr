#[path = "ssh_bindings.rs"]
pub(crate) mod bindings;

pub(crate) use bindings::command;

use crate::api::schema::{ResponseResult, SshHostInfoDto, SuccessResponse};

use super::projection::ParseError;

pub(super) fn run_ssh_command(args: &[String]) -> std::io::Result<i32> {
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
    print_ssh_hosts_human(&response)
}

fn print_ssh_hosts_human(response: &serde_json::Value) -> std::io::Result<i32> {
    // Not folded into the empty-table message: `ssh_unavailable` stays a JSON error on
    // stderr so scripts can tell a missing ssh client apart from an empty config.
    if response.get("error").is_some() {
        return super::print_response(response);
    }
    let parsed: SuccessResponse =
        serde_json::from_value(response.clone()).map_err(std::io::Error::other)?;
    let ResponseResult::SshHosts { hosts } = parsed.result else {
        return super::print_response(response);
    };
    print!("{}", format_ssh_host_table(&hosts));
    Ok(0)
}

fn format_ssh_host_table(hosts: &[SshHostInfoDto]) -> String {
    if hosts.is_empty() {
        return "No ssh hosts in ~/.ssh/config.\n".into();
    }
    let header = ["alias", "hostname", "user"].map(String::from);
    let rows = std::iter::once(header)
        .chain(hosts.iter().map(|host| {
            [
                host.alias.clone(),
                host.hostname.clone().unwrap_or_else(|| "-".into()),
                host.user.clone().unwrap_or_else(|| "-".into()),
            ]
        }))
        .collect::<Vec<_>>();
    let mut widths = [0; 3];
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_host_table_reports_an_empty_config() {
        assert_eq!(
            format_ssh_host_table(&[]),
            "No ssh hosts in ~/.ssh/config.\n"
        );
    }

    #[test]
    fn ssh_host_table_aligns_columns_and_marks_unknown_fields() {
        let hosts = [
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
        ];

        assert_eq!(
            format_ssh_host_table(&hosts),
            concat!(
                "alias  hostname          user\n",
                "prod   prod.example.com  deploy\n",
                "lab    -                 -\n",
            )
        );
    }
}
