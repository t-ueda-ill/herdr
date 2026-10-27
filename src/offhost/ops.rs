//! The three read operations behind `pane.directory.list`,
//! `pane.directory.index`, and `pane.file.read`, independent of transport.

use crate::api::schema::{
    FileDocument, FileStat, PaneDirectoryIndexing, PaneDirectoryListing, PaneFileContent,
    PaneFileStatus, PaneLocation,
};

use super::exec::ExecError;
use super::parse::{
    parse_index, parse_listing, parse_read, parse_stat, ParseFailure, ParsedRead, ScriptStatus,
    MAX_INDEX_ENTRIES, MAX_LIST_ENTRIES,
};
use super::scripts::{index_script, list_script, read_script, stat_script};

/// API error code and message, as `encode_error` takes them.
pub(crate) type Failure = (&'static str, String);

/// Runs a fixed script with positional arguments and returns raw stdout.
pub(crate) trait ScriptRunner {
    fn run(&self, script: &str, args: &[&str]) -> Result<Vec<u8>, ExecError>;

    /// Whole seconds left before the shared deadline, so a script can bound
    /// its own long-running tools. `None` when there is no deadline.
    fn seconds_left(&self) -> Option<u64> {
        None
    }
}

/// Accepts only absolute paths without `..`: the server cannot know which
/// directory a relative path was meant against, and `..` would let a
/// containment check on the client be bypassed.
pub(crate) fn validate_path(path: &str) -> Result<(), Failure> {
    if path.is_empty() || !path.starts_with('/') || path.contains('\0') {
        return Err(("invalid_params", format!("path must be absolute: {path:?}")));
    }
    if path.split('/').any(|component| component == "..") {
        return Err((
            "invalid_params",
            format!("path must not contain `..`: {path:?}"),
        ));
    }
    Ok(())
}

fn script_failure(failure: ParseFailure, path: &str) -> Failure {
    match failure {
        ParseFailure::Script(ScriptStatus::NotFound) => ("not_found", format!("not found: {path}")),
        ParseFailure::Script(ScriptStatus::NotDirectory) => {
            ("not_a_directory", format!("not a directory: {path}"))
        }
        ParseFailure::Script(ScriptStatus::PermissionDenied) => {
            ("permission_denied", format!("permission denied: {path}"))
        }
        ParseFailure::Script(ScriptStatus::NotFile) => {
            ("file_error", "not a regular file".to_owned())
        }
        // Same wording as the host `file.read`, so clients show one message.
        ParseFailure::Script(ScriptStatus::TooLarge) => {
            ("file_error", crate::files::TOO_LARGE_MESSAGE.to_owned())
        }
        ParseFailure::Malformed(detail) => (
            "directory_read_failed",
            format!("unexpected output from the environment: {detail}"),
        ),
    }
}

fn trust_flag(trust_repository: bool) -> &'static str {
    if trust_repository {
        "1"
    } else {
        "0"
    }
}

/// `root` is where the pane's shell is; `path` is the directory to list.
/// Both are `None` for an ssh pane that has not reported its directory, which
/// the script resolves to the login directory.
pub(crate) fn list(
    runner: &dyn ScriptRunner,
    location: PaneLocation,
    root: Option<&str>,
    path: Option<&str>,
    trust_repository: bool,
) -> Result<PaneDirectoryListing, Failure> {
    let target = path.or(root);
    if let Some(path) = path {
        validate_path(path)?;
    }
    let output = runner
        .run(
            &list_script(),
            &[target.unwrap_or(""), trust_flag(trust_repository)],
        )
        .map_err(|err| err.failure())?;
    let parsed = parse_listing(&output, MAX_LIST_ENTRIES)
        .map_err(|failure| script_failure(failure, target.unwrap_or("~")))?;
    let nonzero = |count: usize| u64::try_from(count).ok().filter(|count| *count > 0);
    Ok(PaneDirectoryListing {
        location,
        // Why not `canonical`: the root is the pane's directory whichever
        // directory is listed, so it must not follow a client's expansion.
        root: root.map_or_else(|| parsed.home.clone(), str::to_owned),
        path: target.map_or_else(|| parsed.home.clone(), str::to_owned),
        canonical: parsed.canonical,
        entries: parsed.entries,
        truncated: parsed.truncated,
        git_error: parsed.git_error,
        omitted: nonzero(parsed.omitted),
        unlistable: nonzero(parsed.unlistable),
    })
}

pub(crate) fn index(
    runner: &dyn ScriptRunner,
    location: PaneLocation,
    root: Option<&str>,
    path: Option<&str>,
    trust_repository: bool,
) -> Result<PaneDirectoryIndexing, Failure> {
    let target = path.or(root);
    if let Some(path) = path {
        validate_path(path)?;
    }
    // One second less than the host's own deadline, so the tool is gone
    // inside the environment by the time the host stops waiting.
    let find_budget = runner
        .seconds_left()
        .map(|left| left.saturating_sub(1).max(1).to_string())
        .unwrap_or_default();
    let output = runner
        .run(
            &index_script(),
            &[
                target.unwrap_or(""),
                trust_flag(trust_repository),
                &find_budget,
            ],
        )
        .map_err(|err| err.failure())?;
    let parsed = parse_index(&output, MAX_INDEX_ENTRIES)
        .map_err(|failure| script_failure(failure, target.unwrap_or("~")))?;
    Ok(PaneDirectoryIndexing {
        location,
        // Why the searched directory when git is absent: the `find` paths are
        // relative to it, so the client still needs a base to join them with.
        repo_root: parsed.git_root.or(Some(parsed.canonical)),
        entries: parsed.entries,
        truncated: parsed.truncated,
        git_error: parsed.git_error,
    })
}

pub(crate) fn read(
    runner: &dyn ScriptRunner,
    location: PaneLocation,
    path: &str,
    revision: Option<&str>,
) -> Result<PaneFileContent, Failure> {
    validate_path(path)?;
    let output = runner
        .run(&read_script(), &[path, revision.unwrap_or("")])
        .map_err(|err| err.failure())?;
    let document = match parse_read(&output).map_err(|failure| script_failure(failure, path))? {
        ParsedRead::Unchanged { revision } => FileDocument {
            path: path.to_owned(),
            revision,
            text: String::new(),
            unchanged: true,
        },
        ParsedRead::Content { revision, bytes } => FileDocument {
            path: path.to_owned(),
            revision,
            text: crate::files::validate_text(bytes)?,
            unchanged: false,
        },
    };
    Ok(PaneFileContent { location, document })
}

pub(crate) fn stat(
    runner: &dyn ScriptRunner,
    location: PaneLocation,
    path: &str,
) -> Result<PaneFileStatus, Failure> {
    validate_path(path)?;
    let output = runner
        .run(&stat_script(), &[path])
        .map_err(|err| err.failure())?;
    let parsed = parse_stat(&output).map_err(|failure| script_failure(failure, path))?;
    Ok(PaneFileStatus {
        location,
        stat: FileStat {
            path: path.to_owned(),
            kind: Some(parsed.kind),
            size: parsed.size,
            mtime: parsed
                .mtime_seconds
                .and_then(|seconds| seconds.checked_mul(1000)),
            mode: parsed.mode,
            // Why the uid: resolving names inside the environment costs a
            // process, and BusyBox `ls -l` cuts names at eight characters.
            owner: parsed.uid,
            children: parsed.children,
            children_limited: parsed.children_limited,
            link_target: parsed.link_target,
        },
    })
}

/// Refuses a directory, which neither an editor nor a pager can open, and a
/// missing file for the pager: only an editor can create one. A path behind
/// an unsearchable folder passes, so the program reports the real error.
pub(crate) fn probe_openable(
    runner: &dyn ScriptRunner,
    path: &str,
    mode: crate::api::schema::OpenFileMode,
) -> Result<(), Failure> {
    let output = runner
        .run(PROBE_OPEN_SCRIPT, &[path])
        .map_err(|err| err.failure())?;
    let output =
        super::parse::after_marker(&output).map_err(|failure| script_failure(failure, path))?;
    if output.starts_with(b"dir\0") {
        return Err(("invalid_params", format!("path is a directory: {path}")));
    }
    if mode == crate::api::schema::OpenFileMode::Pager && output.starts_with(b"missing\0") {
        return Err(("not_found", format!("not found: {path}")));
    }
    Ok(())
}

const PROBE_OPEN_SCRIPT: &str = r#"
printf 'HERDR-OFFHOST-1\0'
parent=${1%/*}
if [ -d "$1" ]; then printf 'dir\0'
elif [ -e "$1" ] || { [ -d "${parent:-/}" ] && [ ! -x "${parent:-/}" ]; }; then printf 'other\0'
else printf 'missing\0'; fi
"#;
