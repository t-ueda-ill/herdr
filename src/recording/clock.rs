//! Event times: when each recorded event happened, and the `[log]
//! time_format` that spells it as local date and time in the time markers
//! and in `herdr log show`.

use std::fmt::Write as _;
use std::ops::Add;
use std::time::{Duration, Instant, SystemTime};

use time::{OffsetDateTime, PrimitiveDateTime, UtcOffset};

pub(crate) const DEFAULT_TIME_FORMAT: &str = "[yyyy/mm/dd hh:mi:ss.sss]";
/// Why refresh at all: a server runs across daylight-saving changes. Why not
/// per event: the writer thread handles tens of MB/s, and a minute of lag
/// at a change of offset is acceptable for a log.
const OFFSET_REFRESH: Duration = Duration::from_secs(60);

/// When an event happened, taken where the bytes were seen rather than when
/// the writer thread got to them, so queueing never shifts the time.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Stamp {
    /// Orders events and measures `elapsed`; never steps back.
    pub mono: Instant,
    /// What the clock on the wall said.
    pub wall: SystemTime,
}

impl Stamp {
    pub(crate) fn now() -> Self {
        Self {
            mono: Instant::now(),
            wall: SystemTime::now(),
        }
    }
}

impl Add<Duration> for Stamp {
    type Output = Self;

    fn add(self, rhs: Duration) -> Self {
        Self {
            mono: self.mono + rhs,
            wall: self.wall + rhs,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Token {
    Year,
    Month,
    Day,
    Hour,
    Minute,
    Second,
    Millis,
    Literal(String),
}

/// A parsed `[log] time_format`: local date and time spelled with
/// `yyyy mm dd hh mi ss sss`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TimeFormat(Vec<Token>);

impl Default for TimeFormat {
    fn default() -> Self {
        Self::parse(DEFAULT_TIME_FORMAT).unwrap_or_else(|_| Self(Vec::new()))
    }
}

impl TimeFormat {
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        // Longest first, so `sss` is not read as `ss` followed by `s`.
        const NAMES: [(&str, Token); 7] = [
            ("yyyy", Token::Year),
            ("sss", Token::Millis),
            ("mm", Token::Month),
            ("dd", Token::Day),
            ("hh", Token::Hour),
            ("mi", Token::Minute),
            ("ss", Token::Second),
        ];
        let mut tokens = Vec::new();
        let mut literal = String::new();
        let mut rest = text;
        'outer: while !rest.is_empty() {
            for (name, token) in &NAMES {
                if rest
                    .get(..name.len())
                    .is_some_and(|head| head.eq_ignore_ascii_case(name))
                {
                    if !literal.is_empty() {
                        tokens.push(Token::Literal(std::mem::take(&mut literal)));
                    }
                    tokens.push(token.clone());
                    rest = &rest[name.len()..];
                    continue 'outer;
                }
            }
            let mut chars = rest.chars();
            if let Some(c) = chars.next() {
                // Why reject: a line break would split one event over lines.
                if c.is_control() {
                    return Err(format!(
                        "time_format may not contain control characters: {text:?}"
                    ));
                }
                literal.push(c);
            }
            rest = chars.as_str();
        }
        if !literal.is_empty() {
            tokens.push(Token::Literal(literal));
        }
        if !tokens.iter().any(|t| !matches!(t, Token::Literal(_))) {
            return Err(format!(
                "time_format {text:?} has none of yyyy, mm, dd, hh, mi, ss, sss"
            ));
        }
        Ok(Self(tokens))
    }

    pub(crate) fn spell(&self, at: OffsetDateTime) -> String {
        let mut out = String::new();
        self.write_clock(at, &mut out);
        out
    }

    pub(crate) fn write_clock(&self, at: OffsetDateTime, out: &mut String) {
        for token in &self.0 {
            // Writing to a String cannot fail.
            let _ = match token {
                Token::Year => write!(out, "{:04}", at.year()),
                Token::Month => write!(out, "{:02}", u8::from(at.month())),
                Token::Day => write!(out, "{:02}", at.day()),
                Token::Hour => write!(out, "{:02}", at.hour()),
                Token::Minute => write!(out, "{:02}", at.minute()),
                Token::Second => write!(out, "{:02}", at.second()),
                Token::Millis => write!(out, "{:03}", at.millisecond()),
                Token::Literal(text) => {
                    out.push_str(text);
                    Ok(())
                }
            };
        }
    }
}

/// Turns stamps into local time, asking the OS for the UTC offset at most
/// once per [`OFFSET_REFRESH`].
pub(crate) struct LocalClock {
    offset: UtcOffset,
    checked_at: Option<Instant>,
    source: fn() -> Option<i32>,
}

impl Default for LocalClock {
    fn default() -> Self {
        Self::with_source(platform_offset_seconds)
    }
}

/// The local offset from UTC right now, from the platform's local time.
///
/// Why not `time::UtcOffset::current_local_offset`: it refuses to answer in
/// a multi-threaded Unix process, and the server always is one.
fn platform_offset_seconds() -> Option<i32> {
    let utc = OffsetDateTime::now_utc();
    let local = crate::platform::local_datetime()?;
    let diff = (local - PrimitiveDateTime::new(utc.date(), utc.time())).whole_seconds();
    // The two clocks are read a moment apart and the local one has whole
    // seconds only; offsets are whole minutes, so round to the minute.
    i32::try_from((diff + 30).div_euclid(60) * 60).ok()
}

impl LocalClock {
    /// `source` gives the local offset from UTC in seconds, `None` for UTC.
    pub(crate) fn with_source(source: fn() -> Option<i32>) -> Self {
        Self {
            offset: UtcOffset::UTC,
            checked_at: None,
            source,
        }
    }

    pub(crate) fn local(&mut self, at: Stamp) -> OffsetDateTime {
        let stale = self
            .checked_at
            .is_none_or(|checked| at.mono.saturating_duration_since(checked) >= OFFSET_REFRESH);
        if stale {
            self.offset = (self.source)()
                .and_then(|secs| UtcOffset::from_whole_seconds(secs).ok())
                .unwrap_or(UtcOffset::UTC);
            self.checked_at = Some(at.mono);
        }
        OffsetDateTime::from(at.wall).to_offset(self.offset)
    }
}

/// The local start time in a form that sorts by name: `20261125-103012`.
pub(crate) fn file_stamp(at: OffsetDateTime) -> String {
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        at.year(),
        u8::from(at.month()),
        at.day(),
        at.hour(),
        at.minute(),
        at.second()
    )
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};
    use std::time::UNIX_EPOCH;

    use super::*;

    /// 2026-11-18 11:06:40.250 UTC.
    fn stamp() -> Stamp {
        Stamp {
            mono: Instant::now(),
            wall: UNIX_EPOCH + Duration::from_millis(1_795_000_000_250),
        }
    }

    fn tokyo() -> Option<i32> {
        Some(9 * 3600)
    }

    fn spelled(format: &str, at: Stamp) -> String {
        let mut clock = LocalClock::with_source(tokyo);
        let mut out = String::new();
        TimeFormat::parse(format)
            .unwrap()
            .write_clock(clock.local(at), &mut out);
        out
    }

    #[test]
    fn the_default_format_is_bracketed_local_date_and_time_to_the_millisecond() {
        assert_eq!(
            spelled(DEFAULT_TIME_FORMAT, stamp()),
            "[2026/11/18 20:06:40.250]"
        );
    }

    #[test]
    fn tokens_can_be_rearranged_and_written_in_upper_case() {
        assert_eq!(
            spelled("DD.MM.YYYY HH:MI:SS", stamp()),
            "18.11.2026 20:06:40"
        );
    }

    #[test]
    fn sss_is_milliseconds_not_seconds_followed_by_an_s() {
        assert_eq!(spelled("ss sss", stamp()), "40 250");
    }

    #[test]
    fn a_format_without_any_token_is_rejected() {
        let err = TimeFormat::parse("time:").unwrap_err();
        assert!(err.contains("yyyy"), "{err}");
    }

    #[test]
    fn a_line_break_in_the_format_is_rejected() {
        assert!(TimeFormat::parse("yyyy\n").is_err());
    }

    #[test]
    fn without_a_local_offset_the_time_is_utc() {
        let mut clock = LocalClock::with_source(|| None);
        assert_eq!(clock.local(stamp()).hour(), 11);
    }

    static CALLS: AtomicUsize = AtomicUsize::new(0);
    static OFFSET: AtomicI32 = AtomicI32::new(0);

    fn counted() -> Option<i32> {
        CALLS.fetch_add(1, Ordering::SeqCst);
        Some(OFFSET.load(Ordering::SeqCst))
    }

    #[test]
    fn the_offset_is_asked_again_only_after_a_minute() {
        let mut clock = LocalClock::with_source(counted);
        let start = stamp();
        clock.local(start);
        OFFSET.store(3600, Ordering::SeqCst);
        assert_eq!(clock.local(start + Duration::from_secs(59)).hour(), 11);
        assert_eq!(CALLS.load(Ordering::SeqCst), 1);
        assert_eq!(clock.local(start + Duration::from_secs(60)).hour(), 12);
        assert_eq!(CALLS.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn the_file_stamp_sorts_by_name() {
        let at = OffsetDateTime::from(stamp().wall);
        assert_eq!(file_stamp(at), "20261118-110640");
    }
}
