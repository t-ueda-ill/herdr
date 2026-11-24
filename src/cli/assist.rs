use clap::{Arg, ArgAction, ArgMatches, Command};

use crate::api::schema::{
    AssistInfo, AssistPlacement, AssistProduct, AssistRefreshParams, AssistStartParams,
    EmptyParams, Method, Request, ResponseResult, SuccessResponse,
};

use super::projection::{Leaf, ParseError};

fn json_flag() -> Arg {
    Arg::new("json")
        .long("json")
        .action(ArgAction::SetTrue)
        .help("Print the raw API response as JSON")
}

fn pane_arg(help: &'static str) -> Arg {
    Arg::new("pane").value_name("PANE").help(help)
}

/// The pane named on the command line, or the pane the command runs in.
fn pane(matches: &ArgMatches) -> Result<String, ParseError> {
    matches
        .get_one::<String>("pane")
        .map(|pane| crate::cli::normalize_pane_id(pane))
        .or_else(super::target::caller_pane_id)
        .ok_or_else(|| ParseError::Usage("missing PANE: pass it or run inside a herdr pane".into()))
}

fn product(name: &str) -> Option<AssistProduct> {
    match name {
        "claude" => Some(AssistProduct::Claude),
        "codex" => Some(AssistProduct::Codex),
        "copilot" => Some(AssistProduct::Copilot),
        _ => None,
    }
}

fn start(matches: &ArgMatches) -> Result<Method, ParseError> {
    let placement = if matches.get_flag("right") {
        AssistPlacement::Right
    } else if matches.get_flag("below") {
        AssistPlacement::Below
    } else {
        AssistPlacement::Auto
    };
    Ok(Method::AssistStart(AssistStartParams {
        pane_id: pane(matches)?,
        product: matches
            .get_one::<String>("with")
            .and_then(|name| product(name)),
        placement,
        focus: !matches.get_flag("no_focus"),
    }))
}

const LEAVES: [Leaf; 4] = [
    Leaf {
        name: "start",
        about: "Open a coding-agent CLI beside a pane, to read what the pane shows and advise",
        id: "cli:assist:start",
        arguments: || {
            vec![
                pane_arg("Pane the assist reads; defaults to the pane this runs in"),
                Arg::new("with")
                    .long("with")
                    .value_name("PRODUCT")
                    .value_parser(["claude", "codex", "copilot"])
                    .help("CLI to open; defaults to the first found on the server's PATH"),
                Arg::new("right")
                    .long("right")
                    .action(ArgAction::SetTrue)
                    .conflicts_with("below")
                    .help("Open on the right of the pane"),
                Arg::new("below")
                    .long("below")
                    .action(ArgAction::SetTrue)
                    .help("Open in the lower half of the pane"),
                Arg::new("no_focus")
                    .long("no-focus")
                    .action(ArgAction::SetTrue)
                    .help("Keep the focus where it is"),
                json_flag(),
            ]
        },
        method: start,
    },
    Leaf {
        name: "refresh",
        about: "Write the pane's latest output for its assist and tell the assist to read it",
        id: "cli:assist:refresh",
        arguments: || {
            vec![
                pane_arg("The assist or the pane it reads; defaults to the pane this runs in"),
                json_flag(),
            ]
        },
        method: |matches| {
            Ok(Method::AssistRefresh(AssistRefreshParams {
                pane_id: pane(matches)?,
            }))
        },
    },
    Leaf {
        name: "list",
        about: "List open assists and the panes they read",
        id: "cli:assist:list",
        arguments: || vec![json_flag()],
        method: |_| Ok(Method::AssistList(EmptyParams::default())),
    },
    Leaf {
        name: "products",
        about: "List the coding-agent CLIs found on the server's PATH",
        id: "cli:assist:products",
        arguments: || vec![json_flag()],
        method: |_| Ok(Method::AssistProducts(EmptyParams::default())),
    },
];

pub(crate) fn command() -> Command {
    Command::new("assist")
        .about("Get advice from a coding-agent CLI on what a pane shows")
        .subcommand_required(true)
        .subcommands(LEAVES.iter().map(Leaf::command))
}

struct Parsed {
    request: Request,
    json: bool,
}

fn request(args: &[String]) -> Result<Parsed, ParseError> {
    super::projection::parse(command().bin_name("herdr assist"), args, |matches| {
        let (name, matches) = matches
            .subcommand()
            .ok_or_else(|| ParseError::Usage("missing assist command".into()))?;
        let leaf = LEAVES
            .iter()
            .find(|leaf| leaf.name == name)
            .ok_or_else(|| ParseError::Usage(format!("unknown assist command: {name}")))?;
        Ok(Parsed {
            request: Request {
                id: leaf.id.into(),
                method: (leaf.method)(matches)?,
            },
            json: matches.get_flag("json"),
        })
    })
}

pub(super) fn run_assist_command(args: &[String]) -> std::io::Result<i32> {
    let parsed = match request(args) {
        Ok(parsed) => parsed,
        Err(ParseError::Usage(message)) => {
            eprintln!("{message}");
            return Ok(2);
        }
        Err(ParseError::Io(error)) => return Err(error),
        Err(ParseError::Clap(error)) => return super::print_clap_error(&error),
    };
    let response = super::send_request(&parsed.request)?;
    if parsed.json || response.get("error").is_some() {
        return super::print_response(&response);
    }
    let result = serde_json::from_value::<SuccessResponse>(response.clone())
        .map_err(std::io::Error::other)?
        .result;
    match result {
        ResponseResult::AssistStarted { assist, created } => {
            let verb = if created { "opened" } else { "already open" };
            println!("{verb} {}", describe(&assist));
        }
        ResponseResult::AssistRefreshed { assist } => println!("refreshed {}", describe(&assist)),
        ResponseResult::AssistList { assists } => print!("{}", format_assists(&assists)),
        ResponseResult::AssistProducts { products } => {
            if products.is_empty() {
                println!("No claude, codex or copilot on the server's PATH.");
            }
            for found in products {
                println!("{}  {}", product_name(found.product), found.path);
            }
        }
        _ => return super::print_response(&response),
    }
    Ok(0)
}

fn product_name(product: AssistProduct) -> &'static str {
    match product {
        AssistProduct::Claude => "claude",
        AssistProduct::Codex => "codex",
        AssistProduct::Copilot => "copilot",
        AssistProduct::Unknown => "unknown",
    }
}

fn describe(assist: &AssistInfo) -> String {
    format!(
        "{} ({}) for {}",
        assist.pane_id,
        product_name(assist.product),
        assist.target_pane_id.as_deref().unwrap_or("a closed pane")
    )
}

fn format_assists(assists: &[AssistInfo]) -> String {
    if assists.is_empty() {
        return "No assists open.\n".into();
    }
    assists
        .iter()
        .map(|assist| format!("{}\n", describe(assist)))
        .collect()
}

#[cfg(test)]
mod tests;
