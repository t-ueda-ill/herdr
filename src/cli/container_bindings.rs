use clap::{Arg, ArgAction, ArgMatches, Command};

use crate::api::schema::{
    ContainerListProjection, ContainerOpenParams, ContainerOpenTarget, Method, Request,
};

use super::super::projection::{self, optional_text, Field, Leaf, ParseError};

fn container() -> Field<Option<String>> {
    Field::new(None).input(optional_text("container", "REF").help(
        "Start the shell in a running Docker container: project/service for Compose, otherwise a container name or ID",
    ))
}

/// Adds `--container REF` to a shell-creating command's own arguments.
pub(crate) fn with_container(mut arguments: Vec<Arg>) -> Vec<Arg> {
    arguments.extend(container().arguments());
    arguments
}

/// The `--container REF` value, for callers that combine it with another launch option.
pub(crate) fn read_container(matches: &ArgMatches) -> Result<Option<String>, ParseError> {
    let mut container = container();
    container.read_matches(matches)?;
    container.finish()
}

/// Sends fully parsed create/split params as-is, or nested in `container.open` when
/// `--container REF` was given.
pub(crate) fn into_method<P>(
    matches: &ArgMatches,
    params: P,
    plain: fn(P) -> Method,
    nested: fn(P) -> ContainerOpenTarget,
) -> Result<Method, ParseError> {
    let mut container = container();
    container.read_matches(matches)?;
    // Not a `container` field on the create/split params: those shapes are frozen, and an
    // older server would ignore the field and start a host shell, so the reference travels
    // in `container.open` with the unchanged params nested inside.
    Ok(match container.finish()? {
        Some(container) => Method::ContainerOpen(ContainerOpenParams {
            container,
            ssh_host: None,
            target: nested(params),
        }),
        None => plain(params),
    })
}

fn json_flag() -> Arg {
    Arg::new("json")
        .long("json")
        .action(ArgAction::SetTrue)
        .help("Print the raw API response as JSON")
}

const LEAVES: [Leaf; 1] = [Leaf {
    name: "list",
    about: "List running containers and the workspaces they match",
    id: "cli:container:list",
    arguments: || {
        let mut arguments = ContainerListProjection::arguments();
        arguments.push(json_flag());
        arguments
    },
    method: |matches| ContainerListProjection::from_matches(matches).map(Method::ContainerList),
}];

pub(crate) fn command() -> Command {
    Command::new("container")
        .about("Inspect Docker containers available for container shells")
        .subcommand_required(true)
        .subcommands(LEAVES.iter().map(Leaf::command))
}

/// Parses `args` into the request and whether the raw JSON response was asked for.
pub(crate) fn request(args: &[String]) -> Result<(Request, bool), ParseError> {
    // Not `projection::group_request`: it drops the leaf matches, and `--json` only picks the
    // printer without reaching the API, so it has to be read from the same parse.
    projection::parse(command().bin_name("herdr container"), args, |matches| {
        let (name, matches) = matches
            .subcommand()
            .ok_or_else(|| ParseError::Usage("missing container command".into()))?;
        let leaf = LEAVES
            .iter()
            .find(|leaf| leaf.name == name)
            .ok_or_else(|| ParseError::Usage(format!("unknown container command: {name}")))?;
        let request = Request {
            id: leaf.id.into(),
            method: (leaf.method)(matches)?,
        };
        Ok((request, matches.get_flag("json")))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<(Request, bool), ParseError> {
        request(&args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>())
    }

    #[test]
    fn container_list_defaults_to_all_workspaces_and_text_output() {
        let (request, json) = parse(&["list"]).unwrap();

        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({"id":"cli:container:list","method":"container.list","params":{}})
        );
        assert!(!json);
    }

    #[test]
    fn container_list_parses_workspace_filter_and_json_flag() {
        let (request, json) = parse(&["list", "--workspace", "w2", "--json"]).unwrap();

        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({"id":"cli:container:list","method":"container.list","params":{"workspace_id":"w2"}})
        );
        assert!(json);
    }

    #[test]
    fn container_list_accepts_workspace_equals_form() {
        let (request, _) = parse(&["list", "--workspace=w3"]).unwrap();

        assert_eq!(
            request.method,
            Method::ContainerList(crate::api::schema::ContainerListParams {
                workspace_id: Some("w3".into()),
                ssh_host: None,
            })
        );
    }

    #[test]
    fn container_list_parses_the_ssh_host() {
        for args in [vec!["list", "--ssh", "prod"], vec!["list", "--ssh=prod"]] {
            let (request, _) = parse(&args).unwrap();

            assert_eq!(
                request.method,
                Method::ContainerList(crate::api::schema::ContainerListParams {
                    workspace_id: None,
                    ssh_host: Some("prod".into()),
                })
            );
        }
        let (request, _) = parse(&["list", "--ssh", "prod"]).unwrap();
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({"id":"cli:container:list","method":"container.list","params":{"ssh_host":"prod"}})
        );
    }

    #[test]
    fn container_list_rejects_missing_workspace_value_and_unknown_options() {
        for args in [
            vec!["list", "--workspace"],
            vec!["list", "--ssh"],
            vec!["list", "--all"],
            vec!["list", "extra"],
            vec![],
            vec!["open"],
        ] {
            assert!(
                matches!(parse(&args), Err(ParseError::Clap(error)) if error.exit_code() == 2),
                "{args:?}"
            );
        }
        assert!(
            matches!(parse(&["list", "--help"]), Err(ParseError::Clap(error)) if error.exit_code() == 0)
        );
        command().debug_assert();
    }

    #[test]
    fn container_option_help_describes_the_reference_forms() {
        let arguments = with_container(Vec::new());

        assert_eq!(arguments.len(), 1);
        assert_eq!(arguments[0].get_long(), Some("container"));
        assert!(arguments[0]
            .get_help()
            .is_some_and(|help| help.to_string().contains("project/service")));
    }
}
