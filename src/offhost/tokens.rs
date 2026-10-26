//! Splits the stdout of an off-host helper script into NUL-terminated tokens,
//! and into raw bodies where the grammar says one follows. Everything a helper
//! prints is untrusted: field lengths, keywords and body sizes are all checked
//! here before any caller interprets them.

use super::scripts::OUTPUT_MARKER;

/// A login banner longer than this means the script never started.
const MAX_PREAMBLE_BYTES: usize = 64 * 1024;
const MAX_FIELD_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Malformed(pub(crate) String);

/// Computes a body's length from the token's fields, keyword first.
pub(crate) type BodyLength = fn(&[Vec<u8>]) -> Result<u64, Malformed>;

/// What follows a keyword: how many more fields, and optionally a raw body.
pub(crate) struct Shape {
    /// Fields after the keyword.
    pub(crate) arity: usize,
    /// Length of the raw bytes that follow the last field, computed from the
    /// fields (keyword first). `None`: the token has no body.
    pub(crate) body: Option<BodyLength>,
}

impl Shape {
    pub(crate) const fn fields(arity: usize) -> Self {
        Self { arity, body: None }
    }
}

pub(crate) trait Grammar {
    /// What the script is called in error messages, e.g. "receiver".
    const SCRIPT: &'static str;

    /// `None` for a keyword the helper must never print.
    fn shape(keyword: &[u8]) -> Option<Shape>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Item {
    /// The keyword and its fields.
    Token(Vec<Vec<u8>>),
    /// The next part of the preceding token's body, in order. Never empty.
    Body(Vec<u8>),
}

/// Splits a helper's stdout into [`Item`]s as bytes arrive.
///
/// Why bodies are streamed out as they arrive instead of collected: a body is a
/// file's content and may be far larger than anything worth holding here.
/// Why the grammar gives the body length: a body may contain NUL and
/// token-looking bytes, so only an exact count can find its end.
pub(crate) struct TokenReader<G> {
    pending: Vec<u8>,
    fields: Vec<Vec<u8>>,
    marker_seen: bool,
    /// Bytes buffered before the marker, bounded: a login banner is dropped.
    preamble: usize,
    /// Body bytes still to come for the token just completed.
    body_left: u64,
    grammar: std::marker::PhantomData<fn() -> G>,
}

impl<G> std::fmt::Debug for TokenReader<G> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenReader")
            .field("marker_seen", &self.marker_seen)
            .field("body_left", &self.body_left)
            .finish_non_exhaustive()
    }
}

impl<G> Default for TokenReader<G> {
    fn default() -> Self {
        Self {
            pending: Vec::new(),
            fields: Vec::new(),
            marker_seen: false,
            preamble: 0,
            body_left: 0,
            grammar: std::marker::PhantomData,
        }
    }
}

impl<G: Grammar> TokenReader<G> {
    pub(crate) fn feed(&mut self, bytes: &[u8]) -> Result<Vec<Item>, Malformed> {
        let mut items = Vec::new();
        let mut at = 0;
        while at < bytes.len() {
            if self.body_left > 0 {
                let available = bytes.len() - at;
                let take =
                    usize::try_from(self.body_left).map_or(available, |left| left.min(available));
                items.push(Item::Body(bytes[at..at + take].to_vec()));
                self.body_left -= take as u64;
                at += take;
                continue;
            }
            let byte = bytes[at];
            at += 1;
            if byte != 0 {
                self.pending.push(byte);
                let limit = if self.marker_seen {
                    MAX_FIELD_BYTES
                } else {
                    MAX_PREAMBLE_BYTES
                };
                if self.pending.len() > limit {
                    return Err(Malformed(format!("{} output field is too long", G::SCRIPT)));
                }
                continue;
            }
            let field = std::mem::take(&mut self.pending);
            if !self.marker_seen {
                // Whatever a login shell printed first ends with the marker.
                if field.ends_with(OUTPUT_MARKER.as_bytes()) {
                    self.marker_seen = true;
                } else {
                    self.preamble += field.len() + 1;
                    if self.preamble > MAX_PREAMBLE_BYTES {
                        return Err(Malformed(format!("{} did not start", G::SCRIPT)));
                    }
                }
                continue;
            }
            self.fields.push(field);
            let Some(shape) = G::shape(&self.fields[0]) else {
                return Err(Malformed(format!(
                    "unknown {} token {:?}",
                    G::SCRIPT,
                    String::from_utf8_lossy(&self.fields[0])
                )));
            };
            if self.fields.len() == shape.arity + 1 {
                let fields = std::mem::take(&mut self.fields);
                if let Some(body) = shape.body {
                    self.body_left = body(&fields)?;
                }
                items.push(Item::Token(fields));
            }
        }
        Ok(items)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Probe;

    fn body_of_first_field(fields: &[Vec<u8>]) -> Result<u64, Malformed> {
        // Why digits only: `u64::from_str` accepts a leading `+`.
        Some(&fields[1])
            .filter(|digits| !digits.is_empty() && digits.iter().all(u8::is_ascii_digit))
            .and_then(|digits| std::str::from_utf8(digits).ok()?.parse().ok())
            .ok_or_else(|| Malformed("size is not a number".into()))
    }

    impl Grammar for Probe {
        const SCRIPT: &'static str = "probe";

        fn shape(keyword: &[u8]) -> Option<Shape> {
            Some(match keyword {
                b"z" => Shape::fields(0),
                b"one" => Shape::fields(1),
                b"f" => Shape {
                    arity: 1,
                    body: Some(body_of_first_field),
                },
                _ => return None,
            })
        }
    }

    fn start() -> Vec<u8> {
        b"HERDR-OFFHOST-1\0".to_vec()
    }

    fn token(parts: &[&str]) -> Item {
        Item::Token(parts.iter().map(|part| part.as_bytes().to_vec()).collect())
    }

    /// Bodies split by where the input was cut; joined so cuts do not matter.
    fn joined(items: Vec<Item>) -> Vec<Item> {
        let mut out: Vec<Item> = Vec::new();
        for item in items {
            match (out.last_mut(), item) {
                (Some(Item::Body(last)), Item::Body(more)) => last.extend(more),
                (_, item) => out.push(item),
            }
        }
        out
    }

    #[test]
    fn a_login_banner_before_the_marker_is_dropped() {
        let mut reader = TokenReader::<Probe>::default();

        let items = reader
            .feed(b"Welcome!\nHERDR-OFFHOST-1\0one\0x\0z\0")
            .unwrap();

        assert_eq!(items, [token(&["one", "x"]), token(&["z"])]);
    }

    #[test]
    fn an_unknown_keyword_is_malformed() {
        let mut reader = TokenReader::<Probe>::default();

        assert!(reader
            .feed(&[start(), b"bogus\0".to_vec()].concat())
            .is_err());
    }

    #[test]
    fn a_field_over_the_limit_is_malformed_before_and_after_the_marker() {
        let mut after = TokenReader::<Probe>::default();
        let long = vec![b'x'; MAX_FIELD_BYTES + 1];
        let input = [start(), b"one\0".to_vec(), long.clone()].concat();
        assert!(after.feed(&input).is_err());

        let mut before = TokenReader::<Probe>::default();
        assert!(before.feed(&long).is_err());
    }

    #[test]
    fn a_banner_of_many_short_fields_never_reaching_the_marker_is_malformed() {
        let mut reader = TokenReader::<Probe>::default();
        let banner = b"noise\0".repeat(MAX_PREAMBLE_BYTES / 6 + 2);

        assert!(reader.feed(&banner).is_err());
    }

    #[test]
    fn a_body_that_looks_like_tokens_is_still_a_body() {
        let body = b"f\x005\0ab\0\0z\0\0";
        let mut input = start();
        input.extend(b"f\0");
        input.extend(body.len().to_string().as_bytes());
        input.push(0);
        input.extend(body);
        input.extend(b"z\0");
        let mut reader = TokenReader::<Probe>::default();

        let items = joined(reader.feed(&input).unwrap());

        assert_eq!(
            items,
            [
                token(&["f", &body.len().to_string()]),
                Item::Body(body.to_vec()),
                token(&["z"]),
            ]
        );
    }

    #[test]
    fn a_zero_length_body_yields_no_body_item() {
        let mut reader = TokenReader::<Probe>::default();

        let items = reader
            .feed(&[start(), b"f\x000\0z\0".to_vec()].concat())
            .unwrap();

        assert_eq!(items, [token(&["f", "0"]), token(&["z"])]);
    }

    #[test]
    fn the_same_items_come_out_however_the_input_is_cut() {
        let body: Vec<u8> = (0..=255u8).chain(0..=255u8).collect();
        let mut input = b"junk\n".to_vec();
        input.extend(start());
        input.extend(b"one\0a\0f\0");
        input.extend(body.len().to_string().as_bytes());
        input.push(0);
        input.extend(&body);
        input.extend(b"z\0f\x003\0abc");
        input.extend(b"one\0b\0");
        let whole = joined(TokenReader::<Probe>::default().feed(&input).unwrap());

        for cut in 0..input.len() {
            let mut reader = TokenReader::<Probe>::default();
            let mut items = reader.feed(&input[..cut]).unwrap();
            items.extend(reader.feed(&input[cut..]).unwrap());
            assert_eq!(joined(items), whole, "cut at {cut}");
        }
        let mut reader = TokenReader::<Probe>::default();
        let mut items = Vec::new();
        for byte in &input {
            items.extend(reader.feed(std::slice::from_ref(byte)).unwrap());
        }
        assert_eq!(joined(items), whole, "one byte at a time");
        assert_eq!(
            whole,
            [
                token(&["one", "a"]),
                token(&["f", "512"]),
                Item::Body(body),
                token(&["z"]),
                token(&["f", "3"]),
                Item::Body(b"abc".to_vec()),
                token(&["one", "b"]),
            ]
        );
    }

    #[test]
    fn a_body_size_the_grammar_rejects_is_malformed() {
        let mut reader = TokenReader::<Probe>::default();

        assert!(reader
            .feed(&[start(), b"f\0+1\0".to_vec()].concat())
            .is_err());
    }
}
