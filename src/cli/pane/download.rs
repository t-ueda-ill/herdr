//! `herdr pane download`: one procedure of several requests, so it stands
//! outside the one-command-one-method projection grammar.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use clap::{Arg, ArgAction, ArgMatches, Command};

use super::upload::{Server, SocketServer, CONFLICT_EXIT};
use crate::api::schema::{
    Method, PaneDownloadBeginParams, PaneDownloadCancelParams, PaneDownloadReadParams,
    PaneDownloadStarted, PaneUploadCollision, Request, ResponseResult,
};
use crate::download::local::{check_selection, LocalWriter};
use crate::download::notice_text;
use crate::upload::reply::result_of;
use crate::upload::sink::Committed;

const COLLISIONS: [(&str, PaneUploadCollision); 3] = [
    ("fail", PaneUploadCollision::Fail),
    ("rename", PaneUploadCollision::Rename),
    ("replace", PaneUploadCollision::Replace),
];

pub(crate) fn command() -> Command {
    Command::new("download")
        .about("Copy files out of the environment a pane's shell sees to this machine")
        .long_about(
            "Copy files and folders out of the environment a pane's shell runs in (this host, a \
             container, or an ssh host) into a local folder. Everything is staged next to the \
             destination and published at once, so a failed or interrupted download leaves \
             nothing behind. A symlink named on the command line is followed; symlinks inside \
             folders are copied as links. FIFOs, sockets, and devices are skipped.",
        )
        .arg(
            Arg::new("pane_id")
                .value_name("PANE_ID")
                .required(true)
                .help("Pane whose environment holds the files"),
        )
        .arg(
            Arg::new("paths")
                .value_name("PATH")
                .required(true)
                .num_args(1..)
                .help("Absolute paths in the pane's environment"),
        )
        .arg(
            Arg::new("dest")
                .long("dest")
                .value_name("DIR")
                .help("Local directory that receives the files [default: the current directory]"),
        )
        .arg(
            Arg::new("on-conflict")
                .long("on-conflict")
                .value_name("POLICY")
                .value_parser(COLLISIONS.map(|(name, _)| name))
                .default_value("rename")
                .help("What to do when a name exists: rename to `name (1).ext`, fail (exit 3), or replace"),
        )
        .arg(
            Arg::new("json")
                .long("json")
                .action(ArgAction::SetTrue)
                .help("Print the result as JSON instead of one path per line"),
        )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DownloadArgs {
    pub(crate) pane_id: String,
    /// Paths in the pane's environment, which are not this machine's: they are
    /// never resolved against the current directory.
    pub(crate) paths: Vec<String>,
    pub(crate) destination: PathBuf,
    pub(crate) collision: PaneUploadCollision,
    pub(crate) json: bool,
}

pub(crate) fn parse(args: &[String], cwd: &std::path::Path) -> Result<DownloadArgs, clap::Error> {
    let matches: ArgMatches = command().no_binary_name(true).try_get_matches_from(args)?;
    let text = |name: &str| matches.get_one::<String>(name).cloned();
    let collision = text("on-conflict")
        .and_then(|value| COLLISIONS.iter().find(|(name, _)| *name == value))
        .map_or(PaneUploadCollision::Rename, |(_, collision)| *collision);
    Ok(DownloadArgs {
        pane_id: crate::cli::normalize_pane_id(&text("pane_id").unwrap_or_default()),
        paths: matches
            .get_many::<String>("paths")
            .into_iter()
            .flatten()
            .cloned()
            .collect(),
        // Why the current directory and not ~/Downloads: like cp and scp, a
        // command line puts files where it is run.
        destination: text("dest").map_or_else(|| cwd.to_path_buf(), |dest| cwd.join(dest)),
        collision,
        json: matches.get_flag("json"),
    })
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Finished {
        destination: String,
        paths: Vec<String>,
        warnings: Vec<String>,
    },
    /// `--on-conflict fail` met these existing names.
    Conflict(Vec<String>),
    /// An error code and message, from the server or from writing here.
    Failed(String, String),
    Interrupted,
    /// The selection or the destination was refused before the server was asked.
    Local(String),
}

/// Whether the server's session is still there after this error on a read.
/// Why only these two: every other error ends the session on the server, and a
/// cancel would only be answered with `download_not_found`.
fn session_survives(code: &str) -> bool {
    matches!(code, "download_busy" | "invalid_params")
}

fn failed(failure: (&'static str, String)) -> Outcome {
    Outcome::Failed(failure.0.into(), failure.1)
}

/// Begin, read every answer one at a time, then publish. Why one at a time:
/// each API request is its own connection, and two in flight could reach the
/// server out of order.
pub(crate) fn download(
    server: &mut dyn Server,
    args: &DownloadArgs,
    interrupted: &AtomicBool,
) -> std::io::Result<Outcome> {
    if let Err(message) = check_selection(&args.paths) {
        return Ok(Outcome::Local(message));
    }
    match std::fs::metadata(&args.destination) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => {
            return Ok(Outcome::Local(format!(
                "not a folder: {}",
                args.destination.display()
            )))
        }
        Err(error) => {
            return Ok(Outcome::Local(format!(
                "cannot use {}: {error}",
                args.destination.display()
            )))
        }
    }
    let begin = Request {
        id: "cli:pane:download:begin".into(),
        method: Method::PaneDownloadBegin(PaneDownloadBeginParams {
            pane_id: Some(args.pane_id.clone()),
            paths: args.paths.clone(),
            max_bytes: Some(crate::download::DOWNLOAD_CHUNK_BYTES as u64),
        }),
    };
    let started: PaneDownloadStarted = match result_of(server.request(&begin, true)?) {
        Ok(ResponseResult::PaneDownloadStarted(started)) => started,
        Ok(other) => {
            return Ok(Outcome::Failed(
                "invalid_response".into(),
                format!("unexpected begin result {other:?}"),
            ))
        }
        Err((code, message)) => return Ok(Outcome::Failed(code, message)),
    };
    let cancel = |server: &mut dyn Server| {
        let _ = server.request(
            &Request {
                id: "cli:pane:download:cancel".into(),
                method: Method::PaneDownloadCancel(PaneDownloadCancelParams {
                    download_id: started.download_id.clone(),
                }),
            },
            false,
        );
    };
    // Why before the first read: a destination that cannot take a stage should
    // end the download before the server reads anything.
    let (mut writer, _kept) = match LocalWriter::open(args.destination.clone(), &started.names) {
        Ok(opened) => opened,
        Err(failure) => {
            cancel(server);
            return Ok(failed(failure));
        }
    };
    let mut seq = 0;
    loop {
        if interrupted.load(Ordering::Acquire) {
            cancel(server);
            writer.abort();
            return Ok(Outcome::Interrupted);
        }
        let read = Request {
            id: format!("cli:pane:download:read:{seq}"),
            method: Method::PaneDownloadRead(PaneDownloadReadParams {
                download_id: started.download_id.clone(),
                seq,
            }),
        };
        let answer = match server.request(&read, false) {
            Ok(answer) => answer,
            Err(error) => {
                // Why cancel although the read never arrived: each request is
                // its own connection, so the server may still hold the session
                // and would otherwise keep it until it stalls out.
                cancel(server);
                writer.abort();
                return Err(error);
            }
        };
        let chunk = match result_of(answer) {
            Ok(ResponseResult::PaneDownloadChunk(chunk)) if chunk.seq == seq => chunk,
            Ok(other) => {
                cancel(server);
                writer.abort();
                return Ok(Outcome::Failed(
                    "invalid_response".into(),
                    format!("unexpected read result {other:?}"),
                ));
            }
            Err((code, message)) => {
                if session_survives(&code) {
                    cancel(server);
                }
                writer.abort();
                return Ok(Outcome::Failed(code, message));
            }
        };
        for record in &chunk.records {
            if let Err(failure) = writer.apply(record) {
                cancel(server);
                writer.abort();
                return Ok(failed(failure));
            }
        }
        if !chunk.done {
            // Not done: ask for the next answer. One without records was a
            // heartbeat from a slow source.
            seq += 1;
            continue;
        }
        // Why no cancel after this: `done` already ended the session.
        return Ok(match writer.publish(args.collision) {
            Ok(Committed::Published { paths, warnings }) => Outcome::Finished {
                // Why the server's first: they are about the source, and the
                // local ones about what was written.
                destination: crate::platform::file_path_display(
                    &std::fs::canonicalize(&args.destination)
                        .unwrap_or_else(|_| args.destination.clone()),
                ),
                paths: paths.into_iter().map(|(_, path)| path).collect(),
                warnings: chunk.warnings.into_iter().chain(warnings).collect(),
            },
            Ok(Committed::Conflict(names)) => Outcome::Conflict(names),
            Err(failure) => failed(failure),
        });
    }
}

pub(crate) fn exit_code(outcome: &Outcome) -> i32 {
    match outcome {
        Outcome::Finished { .. } => 0,
        Outcome::Conflict(_) => CONFLICT_EXIT,
        Outcome::Interrupted => 130,
        Outcome::Failed(..) | Outcome::Local(_) => 1,
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Rendered {
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

/// What to print. Every name comes from the other side's filesystem, so each
/// is shown with control characters escaped; JSON escapes them on its own.
pub(crate) fn render(outcome: &Outcome, json: bool) -> Rendered {
    let mut out = Rendered::default();
    match outcome {
        Outcome::Finished {
            destination,
            paths,
            warnings,
        } => {
            for warning in warnings {
                out.stderr
                    .push_str(&format!("warning: {}\n", notice_text(warning)));
            }
            if json {
                let value = serde_json::json!({
                    "destination": destination,
                    "paths": paths,
                    "warnings": warnings,
                });
                out.stdout = format!("{value}\n");
            } else {
                for path in paths {
                    out.stdout.push_str(&format!("{}\n", notice_text(path)));
                }
            }
        }
        Outcome::Conflict(names) => {
            out.stderr.push_str(&format!(
                "error: already exists: {} (use --on-conflict rename or replace)\n",
                names
                    .iter()
                    .map(|name| notice_text(name))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        Outcome::Failed(code, message) => out
            .stderr
            .push_str(&format!("error: {} ({code})\n", notice_text(message))),
        Outcome::Interrupted => out.stderr.push_str("download cancelled\n"),
        Outcome::Local(message) => out
            .stderr
            .push_str(&format!("error: {}\n", notice_text(message))),
    }
    out
}

pub(super) fn run(args: &[String]) -> std::io::Result<i32> {
    let cwd = std::env::current_dir()?;
    let parsed = match parse(args, &cwd) {
        Ok(parsed) => parsed,
        Err(error) => return crate::cli::print_clap_error(&error),
    };
    let interrupted = Arc::new(AtomicBool::new(false));
    let flag = interrupted.clone();
    // Best effort: without a handler, an interrupted download expires on the server.
    let _ = ctrlc::set_handler(move || flag.store(true, Ordering::Release));
    let outcome = download(&mut SocketServer, &parsed, &interrupted)?;
    let rendered = render(&outcome, parsed.json);
    eprint!("{}", rendered.stderr);
    print!("{}", rendered.stdout);
    Ok(exit_code(&outcome))
}

// The tests build scratch trees with Unix paths.
#[cfg(all(test, unix))]
mod tests;
