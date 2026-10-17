use clap::{Arg, ArgAction, ArgMatches, Command};

use crate::api::schema::{
    ContainerOpenParams, ContainerOpenTarget, Method, Request, SshHostsParams, SshOpenParams,
    SshOpenTarget,
};

use super::super::projection::{self, optional_text, Field, Leaf, ParseError};

fn ssh() -> Field<Option<String>> {
    Field::new(None).input(
        optional_text("ssh", "HOST")
            .help("Start the shell over ssh to a Host alias from ~/.ssh/config, optionally as user@alias; with --container, the container is looked up in the docker on that host"),
    )
}

/// Adds `--container REF` and `--ssh HOST` to a shell-creating command's own arguments.
pub(crate) fn with_launch_options(arguments: Vec<Arg>) -> Vec<Arg> {
    let mut arguments = super::super::container::bindings::with_container(arguments);
    arguments.extend(ssh().arguments());
    arguments
}

/// Sends fully parsed create/split params as-is, nested in `container.open` for
/// `--container REF` (with `ssh_host` when `--ssh HOST` names where its docker runs),
/// or nested in `ssh.open` for `--ssh HOST` alone.
pub(crate) fn into_method<P>(
    matches: &ArgMatches,
    params: P,
    plain: fn(P) -> Method,
    in_container: fn(P) -> ContainerOpenTarget,
    over_ssh: fn(P) -> SshOpenTarget,
) -> Result<Method, ParseError> {
    let mut ssh = ssh();
    ssh.read_matches(matches)?;
    // Not an `ssh` field on the create/split params: those shapes are frozen, and an
    // older server would ignore the field and start a host shell.
    let host = ssh.finish()?;
    // Not a clap conflict: `--ssh` alone logs in to the host, but next to
    // `--container` it only says where that container's docker runs.
    match (
        host,
        super::super::container::bindings::read_container(matches)?,
    ) {
        (Some(host), Some(container)) => Ok(Method::ContainerOpen(ContainerOpenParams {
            container,
            ssh_host: Some(host),
            target: in_container(params),
        })),
        (Some(host), None) => Ok(Method::SshOpen(SshOpenParams {
            host,
            target: over_ssh(params),
        })),
        (None, _) => {
            super::super::container::bindings::into_method(matches, params, plain, in_container)
        }
    }
}

fn json_flag() -> Arg {
    Arg::new("json")
        .long("json")
        .action(ArgAction::SetTrue)
        .help("Print the raw API response as JSON")
}

const LEAVES: [Leaf; 1] = [Leaf {
    name: "hosts",
    about: "List Host aliases from ~/.ssh/config that ssh panes can open",
    id: "cli:ssh:hosts",
    arguments: || vec![json_flag()],
    method: |_| Ok(Method::SshHosts(SshHostsParams::default())),
}];

pub(crate) fn command() -> Command {
    Command::new("ssh")
        .about("Inspect ssh hosts available for ssh panes")
        .subcommand_required(true)
        .subcommands(LEAVES.iter().map(Leaf::command))
}

/// Parses `args` into the request and whether the raw JSON response was asked for.
pub(crate) fn request(args: &[String]) -> Result<(Request, bool), ParseError> {
    // Not `projection::group_request`: it drops the leaf matches, and `--json` only picks the
    // printer without reaching the API, so it has to be read from the same parse.
    projection::parse(command().bin_name("herdr ssh"), args, |matches| {
        let (name, matches) = matches
            .subcommand()
            .ok_or_else(|| ParseError::Usage("missing ssh command".into()))?;
        let leaf = LEAVES
            .iter()
            .find(|leaf| leaf.name == name)
            .ok_or_else(|| ParseError::Usage(format!("unknown ssh command: {name}")))?;
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
    fn ssh_hosts_defaults_to_text_output() {
        let (request, json) = parse(&["hosts"]).unwrap();

        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({"id":"cli:ssh:hosts","method":"ssh.hosts","params":{}})
        );
        assert!(!json);
    }

    #[test]
    fn ssh_hosts_json_flag_selects_the_raw_printer() {
        let (_, json) = parse(&["hosts", "--json"]).unwrap();

        assert!(json);
    }

    #[test]
    fn ssh_command_rejects_unknown_commands_and_options() {
        for args in [
            vec!["hosts", "--all"],
            vec!["hosts", "extra"],
            vec![],
            vec!["open"],
        ] {
            assert!(
                matches!(parse(&args), Err(ParseError::Clap(error)) if error.exit_code() == 2),
                "{args:?}"
            );
        }
        assert!(
            matches!(parse(&["hosts", "--help"]), Err(ParseError::Clap(error)) if error.exit_code() == 0)
        );
        command().debug_assert();
    }

    #[test]
    fn ssh_option_help_names_the_ssh_config_alias() {
        let arguments = with_launch_options(Vec::new());

        let ssh = arguments
            .iter()
            .find(|argument| argument.get_long() == Some("ssh"))
            .expect("--ssh is offered next to --container");
        assert!(ssh
            .get_help()
            .is_some_and(|help| help.to_string().contains("~/.ssh/config")));
        assert!(arguments
            .iter()
            .any(|argument| argument.get_long() == Some("container")));
    }

    #[test]
    fn ssh_option_next_to_container_says_where_the_containers_docker_runs() {
        use crate::api::schema::WorkspaceCreateParams;
        let command = Command::new("create").args(with_launch_options(Vec::new()));
        let matches = command
            .try_get_matches_from(["create", "--container", "app/web", "--ssh", "prod"])
            .unwrap();
        let params = WorkspaceCreateParams {
            source_workspace_id: None,
            cwd: None,
            focus: false,
            label: None,
            env: Default::default(),
        };

        let method = into_method(
            &matches,
            params.clone(),
            Method::WorkspaceCreate,
            ContainerOpenTarget::Workspace,
            SshOpenTarget::Workspace,
        )
        .unwrap();

        assert_eq!(
            method,
            Method::ContainerOpen(ContainerOpenParams {
                container: "app/web".into(),
                ssh_host: Some("prod".into()),
                target: ContainerOpenTarget::Workspace(params),
            })
        );
    }
}
