//! asciicast v2 lines: a JSON header, then one `[time, code, data]` array per
//! event. <https://docs.asciinema.org/manual/asciicast/v2/>

use std::time::Duration;

/// Event codes asciicast v2 defines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EventCode {
    Output,
    Input,
    Marker,
    Resize,
}

impl EventCode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Output => "o",
            Self::Input => "i",
            Self::Marker => "m",
            Self::Resize => "r",
        }
    }
}

pub(crate) fn header(width: u16, height: u16, timestamp_secs: u64, title: &str) -> String {
    let header = serde_json::json!({
        "version": 2,
        "width": width,
        "height": height,
        "timestamp": timestamp_secs,
        "title": title,
    });
    format!("{header}\n")
}

pub(crate) fn event(at: Duration, code: EventCode, data: &str) -> String {
    // Why not serde_json for the whole array: it would print the time with
    // full f64 precision, and players read microseconds at most.
    let data = serde_json::Value::String(data.to_owned());
    format!("[{:.6}, \"{}\", {data}]\n", at.as_secs_f64(), code.as_str())
}

pub(crate) fn resize_data(cols: u16, rows: u16) -> String {
    format!("{cols}x{rows}")
}

/// Turns a byte stream into text when a read can end inside a character.
#[derive(Debug, Default)]
pub(crate) struct Utf8Stream {
    pending: Vec<u8>,
    /// Continuation bytes still to skip after a gap in the stream.
    skip_after_gap: u8,
}

/// The most continuation bytes one UTF-8 character carries.
const MAX_CHARACTER_TAIL: u8 = 3;

fn is_continuation(byte: u8) -> bool {
    byte & 0xC0 == 0x80
}

impl Utf8Stream {
    /// Decodes `bytes` after what the last call held back. An incomplete
    /// character at the end waits for the next call; invalid bytes become
    /// U+FFFD, since asciicast data must be a JSON string.
    pub(crate) fn decode(&mut self, mut bytes: &[u8]) -> String {
        if self.skip_after_gap > 0 {
            bytes = self.skip_character_tail(bytes);
        }
        let mut input = std::mem::take(&mut self.pending);
        input.extend_from_slice(bytes);
        let mut text = String::with_capacity(input.len());
        let mut rest = input.as_slice();
        loop {
            match std::str::from_utf8(rest) {
                Ok(valid) => {
                    text.push_str(valid);
                    break;
                }
                Err(err) => {
                    let (valid, after) = rest.split_at(err.valid_up_to());
                    // `valid_up_to` marks a valid prefix, so this never falls back.
                    text.push_str(std::str::from_utf8(valid).unwrap_or_default());
                    match err.error_len() {
                        Some(len) => {
                            text.push(char::REPLACEMENT_CHARACTER);
                            rest = &after[len..];
                        }
                        None => {
                            self.pending = after.to_vec();
                            break;
                        }
                    }
                }
            }
        }
        text
    }

    /// Cuts the stream where bytes were left out: a held-back partial
    /// character comes back as U+FFFD, and the tail of a character whose
    /// start was left out is skipped from the next reads. Why not join them:
    /// the bytes on both sides of the gap belong to different characters, so
    /// joining them prints U+FFFD or a character the program never wrote.
    pub(crate) fn break_at_gap(&mut self) -> Option<String> {
        self.skip_after_gap = MAX_CHARACTER_TAIL;
        self.finish()
    }

    fn skip_character_tail<'a>(&mut self, bytes: &'a [u8]) -> &'a [u8] {
        let tail = bytes
            .iter()
            .take(usize::from(self.skip_after_gap))
            .take_while(|byte| is_continuation(**byte))
            .count();
        // Why not stop at the end of the read: a read can end inside the
        // tail, and only a non-continuation byte proves the tail is over.
        self.skip_after_gap = if tail == bytes.len() {
            self.skip_after_gap - tail as u8
        } else {
            0
        };
        &bytes[tail..]
    }

    /// Releases a held-back partial character as U+FFFD, for the end of a file.
    pub(crate) fn finish(&mut self) -> Option<String> {
        if self.pending.is_empty() {
            return None;
        }
        self.pending.clear();
        Some(char::REPLACEMENT_CHARACTER.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_is_one_json_line_with_version_size_time_and_title() {
        let line = header(120, 40, 1_700_000_000, "herdr default pane 3");
        assert!(line.ends_with('\n'));
        let value: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(value["version"], 2);
        assert_eq!(value["width"], 120);
        assert_eq!(value["height"], 40);
        assert_eq!(value["timestamp"], 1_700_000_000u64);
        assert_eq!(value["title"], "herdr default pane 3");
    }

    #[test]
    fn event_is_a_json_array_of_seconds_code_and_escaped_data() {
        let line = event(
            Duration::from_micros(1_500_250),
            EventCode::Output,
            "a\"\x1b[0m\n",
        );
        assert!(line.starts_with("[1.500250, \"o\", "));
        let value: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(value[1], "o");
        assert_eq!(value[2], "a\"\x1b[0m\n");
    }

    #[test]
    fn each_code_uses_the_asciicast_letter() {
        let codes = [
            (EventCode::Output, "o"),
            (EventCode::Input, "i"),
            (EventCode::Marker, "m"),
            (EventCode::Resize, "r"),
        ];
        for (code, letter) in codes {
            let value: serde_json::Value =
                serde_json::from_str(event(Duration::ZERO, code, "").trim_end()).unwrap();
            assert_eq!(value[1], letter);
        }
    }

    #[test]
    fn resize_data_is_columns_x_rows() {
        assert_eq!(resize_data(80, 24), "80x24");
    }

    #[test]
    fn a_character_split_across_reads_is_emitted_whole_by_the_later_read() {
        let mut stream = Utf8Stream::default();
        let bytes = "あい".as_bytes();
        assert_eq!(stream.decode(&bytes[..2]), "");
        assert_eq!(stream.decode(&bytes[2..4]), "あ");
        assert_eq!(stream.decode(&bytes[4..]), "い");
    }

    #[test]
    fn invalid_bytes_become_replacement_characters_without_stalling_the_stream() {
        let mut stream = Utf8Stream::default();
        assert_eq!(stream.decode(b"a\xffb\xc3"), "a\u{fffd}b");
        assert_eq!(stream.decode(b"\xa9"), "é");
    }

    #[test]
    fn a_gap_releases_the_held_back_partial_character_as_one_replacement() {
        let mut stream = Utf8Stream::default();
        assert_eq!(stream.decode(&"あ".as_bytes()[..1]), "");
        assert_eq!(stream.break_at_gap().as_deref(), Some("\u{fffd}"));
        assert_eq!(stream.decode("い".as_bytes()), "い");
    }

    #[test]
    fn continuation_bytes_right_after_a_gap_are_skipped() {
        let mut stream = Utf8Stream::default();
        assert_eq!(stream.decode(&"あ".as_bytes()[..1]), "");
        assert_eq!(stream.break_at_gap().as_deref(), Some("\u{fffd}"));
        assert_eq!(stream.decode(b"\x81\x82b"), "b");
    }

    #[test]
    fn skipping_after_a_gap_carries_over_a_read_of_only_continuation_bytes() {
        let mut stream = Utf8Stream::default();
        assert_eq!(stream.break_at_gap(), None);
        assert_eq!(stream.decode(b"\x81"), "");
        assert_eq!(stream.decode(b"\x82b"), "b");
    }

    #[test]
    fn skipping_after_a_gap_stops_at_the_longest_character_tail() {
        let mut stream = Utf8Stream::default();
        stream.break_at_gap();
        assert_eq!(stream.decode(b"\x80\x80\x80\x80b"), "\u{fffd}b");
    }

    #[test]
    fn a_gap_leaves_a_stream_that_starts_with_a_whole_character_unchanged() {
        let mut stream = Utf8Stream::default();
        assert_eq!(stream.decode(b"a"), "a");
        assert_eq!(stream.break_at_gap(), None);
        assert_eq!(stream.decode("い".as_bytes()), "い");
        assert_eq!(stream.decode(b"\x81"), "\u{fffd}");
    }

    #[test]
    fn finish_flushes_a_dangling_partial_character_once() {
        let mut stream = Utf8Stream::default();
        assert_eq!(stream.decode(&"é".as_bytes()[..1]), "");
        assert_eq!(stream.finish().as_deref(), Some("\u{fffd}"));
        assert_eq!(stream.finish(), None);
    }
}
