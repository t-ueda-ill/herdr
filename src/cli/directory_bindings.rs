use clap::{Arg, ArgAction, Command};

use crate::api::schema::directory::{DirectoryIndexProjection, DirectoryListProjection};
use crate::api::schema::{Method, Request};

use super::super::projection::{self, Input, Leaf, ParseError};

pub(crate) fn workspace() -> Input<Option<String>> {
    crate::api::schema::tabs::workspace_option("ID")
        .help("Use the focused pane of this workspace instead of the focused workspace")
}

pub(crate) fn pane() -> Input<Option<String>> {
    Input::value("pane", "ID", |slot, value| {
        *slot = Some(crate::cli::normalize_pane_id(value));
        Ok(())
    })
    .help("Use this pane instead of the focused pane")
}

pub(crate) fn path(help: &'static str) -> Input<Option<String>> {
    Input::value("path", "PATH", |slot, value| {
        *slot = Some(normalize_path(value)?);
        Ok(())
    })
    .path()
    .help(help)
}

pub(crate) fn required_path(help: &'static str) -> Input<String> {
    Input::value("path", "PATH", |slot, value| {
        *slot = normalize_path(value)?;
        Ok(())
    })
    .path()
    .required()
    .help(help)
}

pub(crate) fn trust_repository() -> Input<bool> {
    projection::boolean("trust-repository", true)
        .help("Let Git read a repository owned by another user (adds safe.directory)")
}

/// The server accepts only absolute paths, so a local argument is resolved against
/// this process's working directory before it is sent.
fn normalize_path(value: &str) -> std::io::Result<String> {
    if super::super::target::is_remote() {
        // Why not expand `~` or a relative path here: they would resolve
        // against this machine, not the remote server that reads the path.
        if super::super::target::remote_path_is_absolute(value) {
            return Ok(value.to_owned());
        }
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "remote paths must be absolute",
        ));
    }
    let path = crate::worktree::expand_tilde_path(value);
    let absolute = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    };
    Ok(absolute.display().to_string())
}

fn json_flag() -> Arg {
    Arg::new("json")
        .long("json")
        .action(ArgAction::SetTrue)
        .help("Print the raw API response as JSON")
}

const LEAVES: [Leaf; 2] = [
    Leaf {
        name: "list",
        about: "List one directory, starting at the focused pane's working directory",
        id: "cli:directory:list",
        arguments: || {
            let mut arguments = DirectoryListProjection::arguments();
            arguments.push(json_flag());
            arguments
        },
        method: |matches| DirectoryListProjection::from_matches(matches).map(Method::DirectoryList),
    },
    Leaf {
        name: "index",
        about:
            "List the files of the Git repository containing the focused pane's working directory",
        id: "cli:directory:index",
        arguments: || {
            let mut arguments = DirectoryIndexProjection::arguments();
            arguments.push(json_flag());
            arguments
        },
        method: |matches| {
            DirectoryIndexProjection::from_matches(matches).map(Method::DirectoryIndex)
        },
    },
];

pub(crate) fn command() -> Command {
    Command::new("directory")
        .about("Browse files from a pane's working directory")
        .subcommand_required(true)
        .subcommands(LEAVES.iter().map(Leaf::command))
}

/// Parses `args` into the request and whether the raw JSON response was asked for.
pub(crate) fn request(args: &[String]) -> Result<(Request, bool), ParseError> {
    // Not `projection::group_request`: it drops the leaf matches, and `--json` only
    // picks the printer, so it has to be read from the same parse.
    projection::parse(command().bin_name("herdr directory"), args, |matches| {
        let (name, matches) = matches
            .subcommand()
            .ok_or_else(|| ParseError::Usage("missing directory command".into()))?;
        let leaf = LEAVES
            .iter()
            .find(|leaf| leaf.name == name)
            .ok_or_else(|| ParseError::Usage(format!("unknown directory command: {name}")))?;
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
    use crate::api::schema::{DirectoryIndexParams, DirectoryListParams};

    fn parse(args: &[&str]) -> Result<(Request, bool), ParseError> {
        request(&args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>())
    }

    /// Absolute on every platform, unlike a literal `/srv/app`.
    fn absolute_dir() -> String {
        std::env::temp_dir().join("app").display().to_string()
    }

    #[test]
    fn directory_list_without_options_targets_the_focused_pane() {
        let (request, json) = parse(&["list"]).unwrap();

        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({"id":"cli:directory:list","method":"directory.list","params":{}})
        );
        assert!(!json);
    }

    #[test]
    fn directory_list_parses_pane_workspace_path_trust_and_json() {
        let dir = absolute_dir();
        let (request, json) = parse(&[
            "list",
            "--workspace",
            "w2",
            "--pane",
            "w2:p3",
            "--path",
            &dir,
            "--trust-repository",
            "--json",
        ])
        .unwrap();

        assert_eq!(
            request.method,
            Method::DirectoryList(DirectoryListParams {
                workspace_id: Some("w2".into()),
                pane_id: Some("w2:p3".into()),
                path: Some(dir),
                trust_repository: true,
            })
        );
        assert!(json);
    }

    #[test]
    fn directory_index_parses_the_same_target_options() {
        let dir = absolute_dir();
        let (request, json) =
            parse(&["index", &format!("--path={dir}"), "--trust-repository"]).unwrap();

        assert_eq!(request.id, "cli:directory:index");
        assert_eq!(
            request.method,
            Method::DirectoryIndex(DirectoryIndexParams {
                path: Some(dir),
                trust_repository: true,
                ..Default::default()
            })
        );
        assert!(!json);
    }

    #[test]
    fn directory_path_is_resolved_against_the_cli_working_directory() {
        let (request, _) = parse(&["list", "--path", "src"]).unwrap();

        let Method::DirectoryList(params) = request.method else {
            panic!("expected directory.list");
        };
        assert_eq!(
            params.path,
            Some(
                std::env::current_dir()
                    .unwrap()
                    .join("src")
                    .display()
                    .to_string()
            )
        );
    }

    #[test]
    fn directory_path_must_be_absolute_for_a_remote_machine() {
        crate::cli::target::with_test_client(crate::api::client::ApiClient::local(), || {
            assert!(parse(&["list", "--path", "relative"]).is_err());
            assert!(parse(&["index", "--path", "~/repo"]).is_err());
            let (request, _) = parse(&["list", "--path", "/srv/app"]).unwrap();
            assert!(matches!(
                request.method,
                Method::DirectoryList(DirectoryListParams { path: Some(path), .. }) if path == "/srv/app"
            ));
        });
    }

    #[test]
    fn directory_rejects_missing_values_and_unknown_commands() {
        for args in [
            vec!["list", "--path"],
            vec!["list", "--pane"],
            vec!["list", "--all"],
            vec!["list", "extra"],
            vec![],
            vec!["read"],
        ] {
            assert!(
                matches!(parse(&args), Err(ParseError::Clap(error)) if error.exit_code() == 2),
                "{args:?}"
            );
        }
        command().debug_assert();
    }
}
