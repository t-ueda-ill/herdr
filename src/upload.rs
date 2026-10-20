//! Copies files a client holds into the environment a pane's shell sees.
//!
//! A transfer is staged beside its destination and published at the end, so a
//! failed, cancelled, or abandoned upload leaves nothing behind (spec Q12).
//! Everything that touches a filesystem or a process runs on a session worker,
//! never on the app thread or in render/layout loops.

pub(crate) mod path;
pub(crate) mod receiver_script;
pub(crate) mod registry;
pub(crate) mod reply;
pub(crate) mod session;
pub(crate) mod sink;
pub(crate) mod sink_host;
pub(crate) mod sink_remote;
#[cfg(all(test, unix))]
pub(crate) mod sink_tests;
pub(crate) mod source;
pub(crate) mod stamp;
pub(crate) mod worker;

// Every test that needs scratch trees or shells is Unix-only.
#[cfg(all(test, unix))]
pub(crate) mod test_support;

/// Largest decoded chunk. Why 512 KiB: base64 makes it 699,052 bytes, which
/// keeps one write envelope under the 1 MiB endpoint command and API request
/// limits with room for a 4 KiB path and the entry header.
pub(crate) const UPLOAD_CHUNK_BYTES: usize = 512 * 1024;

/// Writes in flight on one ordered stream. Why not more: each write is already
/// half a megabyte, and two hide one round trip without queueing behind input.
pub(crate) const UPLOAD_WINDOW: u32 = 2;

/// Server caps, independent of the client's `ui.upload_*` thresholds. Why not
/// the client defaults: those are user-configurable (spec Q16), and a server
/// stopping silently at the default would make raising them ineffective.
pub(crate) const MAX_UPLOAD_BYTES: u64 = 16 * 1024 * 1024 * 1024;
pub(crate) const MAX_UPLOAD_ENTRIES: u64 = 100_000;

/// Prefix of every stage directory. Uploaded top-level names may not use it,
/// so a stage can never be mistaken for, or published over, user content.
pub(crate) const STAGE_PREFIX: &str = ".herdr-upload-";

/// 32 lowercase hex digits naming one stage directory.
///
/// Why `RandomState`: std seeds it from the OS once per process and then
/// varies it per instance, which is unpredictable enough here. Exclusivity
/// does not depend on it: the stage is created with `mkdir`, which fails when
/// the name exists.
pub(crate) fn stage_token() -> String {
    use std::hash::{BuildHasher, Hasher};
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut halves = [0u64; 2];
    for (index, half) in halves.iter_mut().enumerate() {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u64(sequence);
        hasher.write_usize(index);
        hasher.write_u32(std::process::id());
        if let Ok(elapsed) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            hasher.write_u128(elapsed.as_nanos());
        }
        *half = hasher.finish();
    }
    format!("{:016x}{:016x}", halves[0], halves[1])
}

/// Whether `token` is what [`stage_token`] makes; the receiver puts it in a path.
pub(crate) fn is_stage_token(token: &str) -> bool {
    token.len() == 32
        && token
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_tokens_are_32_hex_digits_and_differ_each_time() {
        let first = stage_token();
        let second = stage_token();

        assert!(is_stage_token(&first), "{first}");
        assert!(is_stage_token(&second), "{second}");
        assert_ne!(first, second);
        assert!(!is_stage_token("../../etc"));
        assert!(!is_stage_token(&"A".repeat(32)));
    }
}
