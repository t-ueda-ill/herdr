use std::path::PathBuf;

use tracing::{info, warn};

#[cfg(windows)]
use crate::protocol::ClientInputEvent;
use crate::protocol::MAX_CLIPBOARD_IMAGE_PAYLOAD;
use crate::protocol::{ClientClipboardImageTarget, ClientMessage};

use super::{write_to_server, ClientError};

pub(super) fn write_remote_image_to_server(
    stream: &mut impl super::ClientMessageSink,
    target: ClientClipboardImageTarget,
    image: crate::platform::ClipboardImage,
    source: &'static str,
) -> Result<(), ClientError> {
    if image.bytes.len() > MAX_CLIPBOARD_IMAGE_PAYLOAD {
        warn!(
            bytes = image.bytes.len(),
            max = MAX_CLIPBOARD_IMAGE_PAYLOAD,
            source,
            "local image is too large to bridge"
        );
        return Ok(());
    }

    info!(
        bytes = image.bytes.len(),
        extension = image.extension,
        source,
        "bridging local image to remote server"
    );
    write_to_server(
        stream,
        &ClientMessage::ClipboardImage {
            target,
            extension: image.extension.to_owned(),
            data: image.bytes,
        },
    )
    .map_err(ClientError::ConnectionLost)
}

pub(super) fn client_remote_image_paste_key(
    config: &crate::config::Config,
) -> Option<(crossterm::event::KeyCode, crossterm::event::KeyModifiers)> {
    match config.remote_image_paste_key() {
        Ok(key) => key,
        Err(diagnostic) => {
            warn!(diagnostic = %diagnostic, "local remote image paste key config diagnostic");
            None
        }
    }
}

/// Whether local images must be bridged because the receiving shell cannot
/// read this machine's clipboard: the client is remote, the endpoint is not
/// local, or the target pane runs inside a container or as an ssh login
/// (`target_needs_bridge`).
/// Local host panes stay unbridged so agents keep reading the clipboard.
pub(super) fn endpoint_accepts_local_images(
    remote_client_process: bool,
    endpoint_id: &super::endpoint::ClientEndpointId,
    active_surface_available: bool,
    target_needs_bridge: bool,
) -> bool {
    active_surface_available
        && (remote_client_process || !endpoint_id.is_local() || target_needs_bridge)
}

pub(super) fn clipboard_image_target_needs_bridge(
    shell: Option<&super::shell::ClientShellState>,
    target: Option<&ClientClipboardImageTarget>,
) -> bool {
    shell
        .zip(target)
        .is_some_and(|(shell, target)| shell.clipboard_image_target_needs_bridge(target))
}

#[cfg(unix)]
pub(super) fn should_bridge_clipboard_image_paste(
    data: &[u8],
    is_remote_client: bool,
    remote_image_paste_key: Option<(crossterm::event::KeyCode, crossterm::event::KeyModifiers)>,
) -> bool {
    if !is_remote_client {
        return false;
    }
    if data == b"\x1b[200~\x1b[201~" {
        return true;
    }

    let Some(remote_image_paste_key) = remote_image_paste_key else {
        return false;
    };

    let events = crate::raw_input::parse_framed_input(data);
    matches!(
        events.as_slice(),
        [crate::raw_input::RawInputEvent::Key(key)]
            if key.kind == crossterm::event::KeyEventKind::Press
                && crate::config::terminal_key_matches_combo(key, remote_image_paste_key)
    )
}

#[cfg(windows)]
pub(super) fn should_bridge_clipboard_image_events(
    events: &[ClientInputEvent],
    is_remote_client: bool,
    remote_image_paste_key: Option<(crossterm::event::KeyCode, crossterm::event::KeyModifiers)>,
) -> bool {
    if !is_remote_client {
        return false;
    }
    if matches!(events, [ClientInputEvent::Paste { text }] if text.is_empty()) {
        return true;
    }

    let Some(remote_image_paste_key) = remote_image_paste_key else {
        return false;
    };
    matches!(
        events,
        [event]
            if matches!(
                event.to_raw_input_event(),
                crate::raw_input::RawInputEvent::Key(key)
                    if key.kind == crossterm::event::KeyEventKind::Press
                        && crate::config::terminal_key_matches_combo(
                            &key,
                            remote_image_paste_key,
                        )
            )
    )
}

#[cfg(unix)]
pub(super) fn read_image_file_from_terminal_drop(
    data: &[u8],
    is_remote_client: bool,
) -> Option<crate::platform::ClipboardImage> {
    let (path, extension) = image_path_from_terminal_drop(data, is_remote_client)?;
    read_image_file(path, extension)
}

#[cfg(windows)]
pub(super) fn read_image_file_from_client_events(
    events: &[ClientInputEvent],
    is_remote_client: bool,
) -> Option<crate::platform::ClipboardImage> {
    let [ClientInputEvent::Paste { text }] = events else {
        return None;
    };
    let text = normalized_terminal_drop_text(text)?;
    let (path, extension) =
        image_path_from_drop_text(strip_matching_path_quotes(text), is_remote_client)?;
    read_image_file(path, extension)
}

fn read_image_file(
    path: PathBuf,
    extension: &'static str,
) -> Option<crate::platform::ClipboardImage> {
    let metadata = std::fs::metadata(&path).ok()?;
    if !metadata.is_file() {
        return None;
    }

    let file = std::fs::File::open(&path).ok()?;
    let bytes =
        match crate::platform::read_limited_reader(file, MAX_CLIPBOARD_IMAGE_PAYLOAD).ok()? {
            crate::platform::LimitedRead::Complete(bytes) => bytes,
            crate::platform::LimitedRead::Empty => return None,
            crate::platform::LimitedRead::Oversized => {
                warn!(
                    max = MAX_CLIPBOARD_IMAGE_PAYLOAD,
                    "local image file drop is too large to bridge"
                );
                return None;
            }
        };

    Some(crate::platform::ClipboardImage { bytes, extension })
}

#[cfg(unix)]
pub(super) fn image_path_from_terminal_drop(
    data: &[u8],
    is_remote_client: bool,
) -> Option<(PathBuf, &'static str)> {
    let bytes = bracketed_paste_payload(data).unwrap_or(data);
    let text = std::str::from_utf8(bytes).ok()?;
    let raw = text;
    let text = normalized_terminal_drop_text(text)?;
    let text = unescape_terminal_drop_path(strip_matching_path_quotes(text));
    image_path_from_drop_text(&text, is_remote_client).or_else(|| {
        // Why also the upload tokenizer: under WSL a Windows terminal drops
        // `C:\…`, which only its translation turns into a local path, and one
        // image must keep its clipboard staging path (spec Q19).
        is_remote_client
            .then(|| {
                translated_image_path(
                    raw,
                    crate::platform::drop_path_syntax(),
                    crate::platform::dropped_path,
                )
            })
            .flatten()
    })
}

/// The one image a drop names, read the way an upload drop reads paths.
#[cfg(unix)]
pub(super) fn translated_image_path(
    text: &str,
    syntax: crate::platform::DropPathSyntax,
    dropped_path: impl Fn(&str) -> Option<PathBuf>,
) -> Option<(PathBuf, &'static str)> {
    let drop = super::upload::drop_text::split_drop_text(text, syntax)?;
    let [token] = drop.tokens.as_slice() else {
        return None;
    };
    let path = dropped_path(token)?;
    let extension = recognized_image_extension(path.extension()?.to_str()?)?;
    Some((path, extension))
}

fn normalized_terminal_drop_text(text: &str) -> Option<&str> {
    let text = text.trim_end_matches(['\r', '\n']);
    (!text.is_empty() && !text.contains(['\r', '\n'])).then_some(text)
}

fn image_path_from_drop_text(
    text: &str,
    is_remote_client: bool,
) -> Option<(PathBuf, &'static str)> {
    if !is_remote_client {
        return None;
    }
    let path = PathBuf::from(text);
    if !path.is_absolute() {
        return None;
    }
    let extension = recognized_image_extension(path.extension()?.to_str()?)?;
    Some((path, extension))
}

/// The text of one bracketed paste, the only input offered as a file drop.
/// Why not bare input too: typed keys arrive unbracketed, and a lone `/`
/// is an absolute path that exists.
#[cfg(unix)]
pub(super) fn bracketed_drop_text(data: &[u8]) -> Option<&str> {
    std::str::from_utf8(bracketed_paste_payload(data)?).ok()
}

#[cfg(unix)]
fn bracketed_paste_payload(data: &[u8]) -> Option<&[u8]> {
    const START: &[u8] = b"\x1b[200~";
    const END: &[u8] = b"\x1b[201~";
    data.strip_prefix(START)?.strip_suffix(END)
}

fn strip_matching_path_quotes(text: &str) -> &str {
    if text.len() < 2 {
        return text;
    }

    let bytes = text.as_bytes();
    match (bytes.first(), bytes.last()) {
        (Some(b'\''), Some(b'\'')) | (Some(b'"'), Some(b'"')) => &text[1..text.len() - 1],
        _ => text,
    }
}

#[cfg(unix)]
fn unescape_terminal_drop_path(text: &str) -> String {
    let mut unescaped = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(escaped) = chars.next() {
                unescaped.push(escaped);
            } else {
                unescaped.push(ch);
            }
        } else {
            unescaped.push(ch);
        }
    }
    unescaped
}

fn recognized_image_extension(extension: &str) -> Option<&'static str> {
    if extension.eq_ignore_ascii_case("png") {
        Some("png")
    } else if extension.eq_ignore_ascii_case("jpg") || extension.eq_ignore_ascii_case("jpeg") {
        Some("jpg")
    } else if extension.eq_ignore_ascii_case("gif") {
        Some("gif")
    } else if extension.eq_ignore_ascii_case("webp") {
        Some("webp")
    } else if extension.eq_ignore_ascii_case("bmp") {
        Some("bmp")
    } else {
        None
    }
}
