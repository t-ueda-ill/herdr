//! The client's upload thresholds (spec Q16): ask past the confirm values,
//! refuse past the maximums. The server caps are applied when the limits are
//! read from the config.

use crate::config::UploadLimits;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Assessment {
    Proceed,
    Confirm,
    Refuse(String),
}

pub(crate) fn assess(entries: u64, bytes: u64, limits: &UploadLimits) -> Assessment {
    if entries > limits.max_entries {
        return Assessment::Refuse(format!(
            "{entries} items is more than ui.upload_max_entries ({})",
            limits.max_entries
        ));
    }
    if bytes > limits.max_bytes {
        return Assessment::Refuse(format!(
            "{} is more than ui.upload_max_bytes ({})",
            format_bytes(bytes),
            format_bytes(limits.max_bytes)
        ));
    }
    if entries > limits.confirm_entries || bytes > limits.confirm_bytes {
        return Assessment::Confirm;
    }
    Assessment::Proceed
}

/// Decimal megabytes with one digit, as the progress card shows them.
pub(crate) fn format_bytes(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1_000_000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: UploadLimits = UploadLimits {
        confirm_bytes: 100,
        confirm_entries: 10,
        max_bytes: 1000,
        max_entries: 100,
    };

    #[test]
    fn small_uploads_proceed_and_ones_past_a_confirm_value_ask() {
        assert_eq!(assess(10, 100, &LIMITS), Assessment::Proceed);
        assert_eq!(assess(11, 0, &LIMITS), Assessment::Confirm);
        assert_eq!(assess(1, 101, &LIMITS), Assessment::Confirm);
    }

    #[test]
    fn uploads_past_a_maximum_are_refused_naming_the_setting() {
        let Assessment::Refuse(entries) = assess(101, 0, &LIMITS) else {
            panic!("expected a refusal");
        };
        assert!(entries.contains("ui.upload_max_entries"), "{entries}");
        let Assessment::Refuse(bytes) = assess(1, 1001, &LIMITS) else {
            panic!("expected a refusal");
        };
        assert!(bytes.contains("ui.upload_max_bytes"), "{bytes}");
    }

    #[test]
    fn byte_counts_are_shown_in_decimal_megabytes() {
        assert_eq!(format_bytes(12_345_678), "12.3 MB");
        assert_eq!(format_bytes(0), "0.0 MB");
    }
}
