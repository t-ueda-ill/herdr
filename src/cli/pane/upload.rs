//! `herdr pane upload`: one procedure of several requests, so it stands
//! outside the one-command-one-method projection grammar.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use clap::{Arg, ArgAction, ArgMatches, Command};

use crate::api::schema::{
    Method, PaneUploadBeginParams, PaneUploadCancelParams, PaneUploadCollision,
    PaneUploadFinishParams, PaneUploadFinished, PaneUploadStarted, Request, ResponseResult,
};
use crate::upload::reply::result_of;
use crate::upload::source::{scan, DuplicateNames, Writes};

/// Exit status when `--on-conflict fail` met an existing name. Why not 2: the
/// CLI already exits 2 for usage errors.
pub(crate) const CONFLICT_EXIT: i32 = 3;

const COLLISIONS: [(&str, PaneUploadCollision); 3] = [
    ("fail", PaneUploadCollision::Fail),
    ("rename", PaneUploadCollision::Rename),
    ("replace", PaneUploadCollision::Replace),
];

pub(crate) fn command() -> Command {
    Command::new("upload")
        .about("Copy local files into the directory a pane's shell sees")
        .long_about(
            "Copy local files and folders into the environment a pane's shell runs in: this \
             host, a container, or an ssh host. Everything is staged next to the destination and \
             published at once, so a failed or interrupted upload leaves nothing behind. \
             Symlinks are copied as links; FIFOs, sockets, and devices are skipped.",
        )
        .arg(
            Arg::new("pane_id")
                .value_name("PANE_ID")
                .required(true)
                .help("Pane whose environment receives the files"),
        )
        .arg(
            Arg::new("paths")
                .value_name("PATH")
                .required(true)
                .num_args(1..)
                .help("Local files or folders; relative paths start in the current directory"),
        )
        .arg(
            Arg::new("dest")
                .long("dest")
                .value_name("DIR")
                .help("Absolute directory in the pane's environment [default: the pane's working directory, then its home]"),
        )
        .arg(
            Arg::new("on-conflict")
                .long("on-conflict")
                .value_name("POLICY")
                .value_parser(COLLISIONS.map(|(name, _)| name))
                .default_value("fail")
                .help("What to do when a name exists: fail (exit 3), rename to `name (1).ext`, or replace"),
        )
        .arg(
            Arg::new("json")
                .long("json")
                .action(ArgAction::SetTrue)
                .help("Print the finish result as JSON instead of one path per line"),
        )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UploadArgs {
    pub(crate) pane_id: String,
    pub(crate) paths: Vec<PathBuf>,
    pub(crate) destination: Option<String>,
    pub(crate) collision: PaneUploadCollision,
    pub(crate) json: bool,
}

pub(crate) fn parse(args: &[String], cwd: &std::path::Path) -> Result<UploadArgs, clap::Error> {
    let matches: ArgMatches = command().no_binary_name(true).try_get_matches_from(args)?;
    let text = |name: &str| matches.get_one::<String>(name).cloned();
    let collision = text("on-conflict")
        .and_then(|value| COLLISIONS.iter().find(|(name, _)| *name == value))
        .map_or(PaneUploadCollision::Fail, |(_, collision)| *collision);
    Ok(UploadArgs {
        pane_id: crate::cli::normalize_pane_id(&text("pane_id").unwrap_or_default()),
        paths: matches
            .get_many::<String>("paths")
            .into_iter()
            .flatten()
            .map(|path| cwd.join(path))
            .collect(),
        destination: text("dest"),
        collision,
        json: matches.get_flag("json"),
    })
}

/// Transport for the procedure, so tests can stand in for the server.
pub(crate) trait Server {
    fn request(&mut self, request: &Request, first: bool) -> std::io::Result<serde_json::Value>;
}

pub(super) struct SocketServer;

impl Server for SocketServer {
    fn request(&mut self, request: &Request, first: bool) -> std::io::Result<serde_json::Value> {
        // Why check the protocol only once: every write is its own connection,
        // and a second ping per chunk would double the round trips.
        if first {
            crate::cli::send_request(request)
        } else {
            crate::cli::send_request_unchecked(request)
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Finished(PaneUploadFinished),
    /// The server answered with this error code and message.
    Failed(String, String),
    Interrupted,
    /// The local paths could not be read.
    Local(String),
}

/// Begin, write every chunk one at a time, then finish. Why one at a time:
/// each API request is its own connection, and two in flight could reach the
/// server out of order.
pub(crate) fn upload(
    server: &mut dyn Server,
    args: &UploadArgs,
    interrupted: &AtomicBool,
    warn: &mut dyn FnMut(&str),
) -> std::io::Result<Outcome> {
    let scanned = match scan(
        &args.paths,
        DuplicateNames::Reject,
        crate::upload::MAX_UPLOAD_ENTRIES,
    ) {
        Ok(scanned) => scanned,
        Err(message) => return Ok(Outcome::Local(message)),
    };
    for warning in &scanned.warnings {
        warn(warning);
    }
    if scanned.skipped_special > 0 {
        warn(&format!(
            "skipped {} special files (FIFOs, sockets, or devices)",
            scanned.skipped_special
        ));
    }
    let begin = Request {
        id: "cli:pane:upload:begin".into(),
        method: Method::PaneUploadBegin(PaneUploadBeginParams {
            pane_id: Some(args.pane_id.clone()),
            terminal_id: None,
            destination: args.destination.clone(),
            names: scanned.names.clone(),
            entries: scanned.entries.len() as u64,
            bytes: scanned.bytes,
        }),
    };
    let started: PaneUploadStarted = match result_of(server.request(&begin, true)?) {
        Ok(ResponseResult::PaneUploadStarted(started)) => started,
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
                id: "cli:pane:upload:cancel".into(),
                method: Method::PaneUploadCancel(PaneUploadCancelParams {
                    upload_id: started.upload_id.clone(),
                }),
            },
            false,
        );
    };
    let chunk = usize::try_from(started.chunk_bytes)
        .unwrap_or(crate::upload::UPLOAD_CHUNK_BYTES)
        .clamp(1, crate::upload::UPLOAD_CHUNK_BYTES);
    for (index, write) in Writes::new(&scanned, started.upload_id.clone(), chunk).enumerate() {
        if interrupted.load(Ordering::Acquire) {
            cancel(server);
            return Ok(Outcome::Interrupted);
        }
        let write = match write {
            Ok(write) => write,
            Err(message) => {
                cancel(server);
                return Ok(Outcome::Local(message));
            }
        };
        let request = Request {
            id: format!("cli:pane:upload:write:{index}"),
            method: Method::PaneUploadWrite(write),
        };
        if let Err((code, message)) = result_of(server.request(&request, false)?) {
            cancel(server);
            return Ok(Outcome::Failed(code, message));
        }
    }
    let finish = Request {
        id: "cli:pane:upload:finish".into(),
        method: Method::PaneUploadFinish(PaneUploadFinishParams {
            upload_id: started.upload_id.clone(),
            collision: args.collision,
        }),
    };
    match result_of(server.request(&finish, false)?) {
        Ok(ResponseResult::PaneUploadFinished(finished)) => Ok(Outcome::Finished(finished)),
        Ok(other) => Ok(Outcome::Failed(
            "invalid_response".into(),
            format!("unexpected finish result {other:?}"),
        )),
        Err((code, message)) => {
            // A refused publish keeps the stage for another try; the CLI will not retry.
            cancel(server);
            Ok(Outcome::Failed(code, message))
        }
    }
}

pub(crate) fn exit_code(outcome: &Outcome) -> i32 {
    match outcome {
        Outcome::Finished(_) => 0,
        Outcome::Failed(code, _) if code == "upload_conflict" => CONFLICT_EXIT,
        Outcome::Interrupted => 130,
        Outcome::Failed(..) | Outcome::Local(_) => 1,
    }
}

pub(super) fn run(args: &[String]) -> std::io::Result<i32> {
    let cwd = std::env::current_dir()?;
    let parsed = match parse(args, &cwd) {
        Ok(parsed) => parsed,
        Err(error) => return crate::cli::print_clap_error(&error),
    };
    let interrupted = Arc::new(AtomicBool::new(false));
    let flag = interrupted.clone();
    // Best effort: without a handler, an interrupted upload expires on the server.
    let _ = ctrlc::set_handler(move || flag.store(true, Ordering::Release));
    let outcome = upload(&mut SocketServer, &parsed, &interrupted, &mut |warning| {
        eprintln!("warning: {warning}");
    })?;
    match &outcome {
        Outcome::Finished(finished) => {
            for warning in &finished.warnings {
                eprintln!("warning: {warning}");
            }
            if parsed.json {
                println!(
                    "{}",
                    serde_json::to_string(finished).map_err(std::io::Error::other)?
                );
            } else {
                for path in &finished.paths {
                    println!("{path}");
                }
            }
        }
        Outcome::Failed(code, message) => eprintln!("error: {message} ({code})"),
        Outcome::Interrupted => eprintln!("upload cancelled"),
        Outcome::Local(message) => eprintln!("error: {message}"),
    }
    Ok(exit_code(&outcome))
}

// The tests build scratch trees with Unix paths.
#[cfg(all(test, unix))]
mod tests;
