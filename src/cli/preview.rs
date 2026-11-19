use clap::{Arg, ArgAction, ArgMatches, Command};

use crate::api::schema::{
    EmptyParams, Method, PreviewInfo, PreviewStartParams, PreviewStopParams, Request,
    ResponseResult, SuccessResponse,
};

use super::projection::{Leaf, ParseError};

fn json_flag() -> Arg {
    Arg::new("json")
        .long("json")
        .action(ArgAction::SetTrue)
        .help("Print the raw API response as JSON")
}

/// A host path made absolute against the current directory. Paths in an ssh or
/// container pane are passed through: only that environment can resolve them.
fn absolute(path: &str, in_pane: bool) -> Result<String, ParseError> {
    if in_pane || std::path::Path::new(path).is_absolute() {
        return Ok(path.to_owned());
    }
    let joined = std::env::current_dir().map_err(ParseError::Io)?.join(path);
    Ok(joined.to_string_lossy().into_owned())
}

fn start(matches: &ArgMatches) -> Result<Method, ParseError> {
    let pane_id = matches
        .get_one::<String>("pane")
        .map(|pane| crate::cli::normalize_pane_id(pane));
    let in_pane = pane_id.is_some();
    let root = matches
        .get_one::<String>("root")
        .ok_or_else(|| ParseError::Usage("missing ROOT".into()))?;
    let entry = matches
        .get_one::<String>("entry")
        .map(|entry| absolute(entry, in_pane))
        .transpose()?;
    Ok(Method::PreviewStart(PreviewStartParams {
        pane_id,
        root: absolute(root, in_pane)?,
        entry,
    }))
}

const LEAVES: [Leaf; 3] = [
    Leaf {
        name: "start",
        about:
            "Serve a folder to the browser on this machine and reload open pages when files change",
        id: "cli:preview:start",
        arguments: || {
            vec![
                Arg::new("root")
                    .value_name("ROOT")
                    .required(true)
                    .help("Folder served as the site root"),
                Arg::new("entry")
                    .long("entry")
                    .value_name("PATH")
                    .help("File or folder under ROOT the URL opens; defaults to ROOT"),
                Arg::new("pane")
                    .long("pane")
                    .value_name("PANE")
                    .help("Read ROOT where this pane's shell runs: its container or ssh host"),
                Arg::new("open")
                    .long("open")
                    .action(ArgAction::SetTrue)
                    .help("Open the URL in the default browser"),
                json_flag(),
            ]
        },
        method: start,
    },
    Leaf {
        name: "list",
        about: "List running previews",
        id: "cli:preview:list",
        arguments: || vec![json_flag()],
        method: |_| Ok(Method::PreviewList(EmptyParams::default())),
    },
    Leaf {
        name: "stop",
        about: "Stop a preview",
        id: "cli:preview:stop",
        arguments: || {
            vec![
                Arg::new("preview_id")
                    .value_name("PREVIEW_ID")
                    .required(true)
                    .help("Id printed by preview start or preview list"),
                json_flag(),
            ]
        },
        method: |matches| {
            Ok(Method::PreviewStop(PreviewStopParams {
                preview_id: matches
                    .get_one::<String>("preview_id")
                    .cloned()
                    .unwrap_or_default(),
            }))
        },
    },
];

pub(crate) fn command() -> Command {
    Command::new("preview")
        .about("Preview folders in the browser with live reload")
        .subcommand_required(true)
        .subcommands(LEAVES.iter().map(Leaf::command))
}

struct Parsed {
    request: Request,
    json: bool,
    open: bool,
}

fn request(args: &[String]) -> Result<Parsed, ParseError> {
    super::projection::parse(command().bin_name("herdr preview"), args, |matches| {
        let (name, matches) = matches
            .subcommand()
            .ok_or_else(|| ParseError::Usage("missing preview command".into()))?;
        let leaf = LEAVES
            .iter()
            .find(|leaf| leaf.name == name)
            .ok_or_else(|| ParseError::Usage(format!("unknown preview command: {name}")))?;
        Ok(Parsed {
            request: Request {
                id: leaf.id.into(),
                method: (leaf.method)(matches)?,
            },
            json: matches.get_flag("json"),
            open: matches.try_get_one::<bool>("open").ok().flatten().copied() == Some(true),
        })
    })
}

pub(super) fn run_preview_command(args: &[String]) -> std::io::Result<i32> {
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
        ResponseResult::PreviewStarted { preview } => {
            println!("{}  {}", preview.preview_id, preview.url);
            if parsed.open {
                if let Err(err) = crate::platform::open_url(&preview.url) {
                    eprintln!("could not open the browser: {err}");
                    return Ok(1);
                }
            }
        }
        ResponseResult::PreviewList { previews } => print!("{}", format_previews(&previews)),
        ResponseResult::PreviewStopped { preview_id } => println!("stopped {preview_id}"),
        _ => return super::print_response(&response),
    }
    Ok(0)
}

fn place(preview: &PreviewInfo) -> String {
    let Some(location) = &preview.location else {
        return "this host".into();
    };
    match (&location.container, &location.ssh_host) {
        (Some(container), Some(host)) => format!("{container} on {host}"),
        (Some(container), None) => container.clone(),
        (None, Some(host)) => host.clone(),
        (None, None) => "-".into(),
    }
}

fn format_previews(previews: &[PreviewInfo]) -> String {
    if previews.is_empty() {
        return "No previews running.\n".into();
    }
    let rows: Vec<[String; 4]> = std::iter::once(["id", "port", "place", "root"].map(String::from))
        .chain(previews.iter().map(|preview| {
            [
                preview.preview_id.clone(),
                preview.port.to_string(),
                place(preview),
                preview.root.clone(),
            ]
        }))
        .collect();
    let mut widths = [0; 4];
    for row in &rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    rows.iter()
        .map(|row| {
            let line = row
                .iter()
                .zip(widths)
                .map(|(cell, width)| format!("{cell:<width$}"))
                .collect::<Vec<_>>()
                .join("  ");
            format!("{}\n", line.trim_end())
        })
        .collect()
}

#[cfg(test)]
mod tests;
