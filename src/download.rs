#![allow(dead_code)] // Why: wired in S3; remove there.

//! Streams files out of the environment a pane's shell sees (this host, a
//! container, or an ssh host) to a client that pulls the stream with ordered
//! reads. Everything that touches a filesystem or a process runs on a worker
//! thread, never on the app thread or in render/layout loops.

pub(crate) mod frame;
pub(crate) mod sender_script;
pub(crate) mod source;
pub(crate) mod source_host;
pub(crate) mod source_remote;

/// Largest decoded data one read answer may carry. Why 512 KiB: base64 makes it
/// 699,052 bytes, which leaves room for names and records under one frame.
pub(crate) const DOWNLOAD_CHUNK_BYTES: usize = 512 * 1024;

/// Reads in flight on one ordered stream. Why not more: each answer can be
/// large, and two hide one round trip without queueing behind rendering.
pub(crate) const DOWNLOAD_WINDOW: u32 = 2;

/// Server caps, independent of anything the client asks for.
pub(crate) const MAX_DOWNLOAD_ENTRIES: u64 = 100_000;
pub(crate) const MAX_DOWNLOAD_BYTES: u64 = 16 * 1024 * 1024 * 1024;

/// Raw bytes of paths and link targets one read answer may carry.
pub(crate) const MAX_READ_META_BYTES: usize = 256 * 1024;
/// Records one read answer may carry. Why not more: at [`RECORD_OVERHEAD_BYTES`]
/// each, 4,096 records would already be 512 KiB of JSON next to the other budgets.
pub(crate) const MAX_READ_RECORDS: usize = 2_048;
/// JSON a record needs besides its path, link target and data (about 95 bytes
/// for a directory header), rounded up.
pub(crate) const RECORD_OVERHEAD_BYTES: usize = 128;
/// Warnings in a finishing answer, control characters unescaped.
pub(crate) const MAX_WARNING_BYTES: usize = 32 * 1024;
/// One warning is cut here, so a single long name cannot use the whole budget.
pub(crate) const MAX_WARNING_LINE_BYTES: usize = 512;

/// The most JSON a read answer may be. Why not `MAX_FRAME_SIZE`: the answer is
/// wrapped in an endpoint control message whose own envelope adds a few dozen
/// bytes, and the wrapper must still fit one frame.
pub(crate) const MAX_DOWNLOAD_REPLY_JSON: usize = 1_900_000;

/// Frames a source may run ahead of the worker. Why bounded: a full queue
/// stops the source, which stops the helper, so the stream cannot outrun the
/// client that pulls it.
pub(crate) const FRAME_QUEUE: usize = 32;

/// Base64 of `bytes` decoded bytes, padding included.
const fn base64_len(bytes: usize) -> usize {
    bytes.div_ceil(3) * 4
}

/// What the other fields of an answer add: sequence numbers, counters, and the
/// response envelope.
const ENVELOPE_BYTES: usize = 1024;

/// Path and link target text may double when JSON escapes `"` and `\`. The
/// name budget is checked after a record is added, so the record that reaches
/// it may carry one more longest path and link target past it.
pub(crate) const WORST_CASE_REPLY_BYTES: usize = base64_len(DOWNLOAD_CHUNK_BYTES)
    + 2 * (MAX_READ_META_BYTES
        + crate::upload::path::MAX_PATH_BYTES
        + crate::upload::path::MAX_LINK_TARGET_BYTES)
    + MAX_READ_RECORDS * RECORD_OVERHEAD_BYTES
    + ENVELOPE_BYTES;

const _: () = assert!(WORST_CASE_REPLY_BYTES <= MAX_DOWNLOAD_REPLY_JSON);
// Why 6: a control character becomes `\u00XX` in JSON.
const _: () = assert!(6 * MAX_WARNING_BYTES + ENVELOPE_BYTES <= MAX_DOWNLOAD_REPLY_JSON);
const _: () = assert!(
    MAX_DOWNLOAD_REPLY_JSON + 256 <= crate::protocol::MAX_FRAME_SIZE,
    "an answer plus the control message around it must fit one frame"
);
const _: () = assert!(MAX_WARNING_LINE_BYTES < MAX_WARNING_BYTES);

/// `text` with control characters shown as `\u{1b}`, safe to put in a message
/// a terminal will print. Why not `escape_debug`: it also escapes quotes and
/// backslashes, which would make ordinary paths unreadable. Names come from the
/// other side's filesystem, so an escape sequence in one must never reach a
/// terminal.
pub(crate) fn notice_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch.is_control() {
            out.extend(ch.escape_unicode());
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notice_text_shows_control_characters_and_keeps_everything_else() {
        assert_eq!(notice_text("a\u{1b}[31m\0b\n"), "a\\u{1b}[31m\\u{0}b\\u{a}");
        assert_eq!(notice_text(r#"日本 "q" \ ok"#), r#"日本 "q" \ ok"#);
    }
}
