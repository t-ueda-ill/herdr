//! Modification times as `touch -t` reads them.

use time::OffsetDateTime;

/// `CCYYMMDDhhmm.SS` in UTC; the receiver runs `touch` with `TZ=UTC0`.
/// `None` outside 1970..=9999, which `touch -t` cannot express or which
/// usually means the client had no real time.
pub(crate) fn touch_stamp(unix_seconds: i64) -> Option<String> {
    let at = OffsetDateTime::from_unix_timestamp(unix_seconds).ok()?;
    if !(1970..=9999).contains(&at.year()) {
        return None;
    }
    Some(format!(
        "{:04}{:02}{:02}{:02}{:02}.{:02}",
        at.year(),
        u8::from(at.month()),
        at.day(),
        at.hour(),
        at.minute(),
        at.second()
    ))
}

/// The same bound as [`touch_stamp`], for sinks that set times directly.
pub(crate) fn in_range(unix_seconds: i64) -> bool {
    touch_stamp(unix_seconds).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_seconds_become_a_utc_touch_stamp() {
        assert_eq!(touch_stamp(0).as_deref(), Some("197001010000.00"));
        assert_eq!(
            touch_stamp(1_700_000_000).as_deref(),
            Some("202311142213.20")
        );
    }

    #[test]
    fn times_before_1970_or_after_9999_have_no_stamp() {
        assert_eq!(touch_stamp(-1), None);
        assert_eq!(touch_stamp(253_402_300_800), None);
        assert!(touch_stamp(253_402_300_799).is_some());
    }
}
