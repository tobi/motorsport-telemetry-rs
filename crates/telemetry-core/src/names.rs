//! Punctuation- and case-insensitive channel-name matching.
//!
//! Telemetry channel names arrive with mixed case, spaces, underscores, and
//! vendor punctuation (`Lap Number`, `lap_number`, `LapNumber`). The helpers
//! here normalize to ASCII-alphanumeric lowercase so callers match one stable
//! form without allocating where possible.

use crate::Channel;

/// Returns `value` with every non-ASCII-alphanumeric byte dropped and the
/// rest lowercased.
///
/// This is the canonical form used by [`eq`] and [`contains`].
pub fn normalize(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes().filter(u8::is_ascii_alphanumeric) {
        out.push(byte.to_ascii_lowercase() as char);
    }
    out
}

/// Allocation-free normalized equality: `value` matches `wanted` when the two
/// are equal after dropping non-alphanumeric bytes and lowercasing.
///
/// `wanted` is compared verbatim byte-for-byte against the filtered stream, so
/// callers should pass an already-normalized lowercase alphanumeric needle.
pub fn eq(value: &str, wanted: &str) -> bool {
    value
        .bytes()
        .filter(u8::is_ascii_alphanumeric)
        .map(|byte| byte.to_ascii_lowercase())
        .eq(wanted.bytes())
}

/// Allocation-free normalized substring: `needle` occurs within `value` when it
/// appears as a contiguous subsequence of the filtered, lowercased stream.
///
/// `needle` is compared verbatim byte-for-byte, so callers should pass an
/// already-normalized lowercase alphanumeric needle (or rely on the on-the-fly
/// filtering of `value` only).
pub fn contains(value: &str, needle: &str) -> bool {
    let needle = needle.as_bytes();
    if needle.is_empty() {
        return true;
    }
    value
        .char_indices()
        .filter(|(_, character)| character.is_ascii_alphanumeric())
        .any(|(start, _)| {
            let mut matched = 0;
            for byte in value[start..].bytes().filter(u8::is_ascii_alphanumeric) {
                if byte.to_ascii_lowercase() != needle[matched] {
                    return false;
                }
                matched += 1;
                if matched == needle.len() {
                    return true;
                }
            }
            false
        })
}

/// Finds the first channel whose name [`eq`]uals any of `names`, in `names`
/// priority order.
///
/// For each name in `names` (highest priority first) the first matching channel
/// index is returned; a higher-priority name always wins over a lower-priority
/// one regardless of channel order. Returns `None` when no channel matches.
pub fn find(channels: &[Channel], names: &[&str]) -> Option<usize> {
    for wanted in names {
        if let Some(index) = channels
            .iter()
            .position(|channel| eq(&channel.name, wanted))
        {
            return Some(index);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_punctuation_and_lowercases() {
        assert_eq!(normalize("Lap Number"), "lapnumber");
        assert_eq!(normalize("lap_number"), "lapnumber");
        assert_eq!(normalize("LapNumber"), "lapnumber");
        assert_eq!(normalize("GPS/Speed (km/h)"), "gpsspeedkmh");
        assert_eq!(normalize(""), "");
    }

    #[test]
    fn eq_ignores_punctuation_and_case() {
        assert!(eq("Lap Number", "lapnumber"));
        assert!(eq("lap_number", "lapnumber"));
        assert!(eq("LapNumber", "lapnumber"));
        assert!(!eq("Lap Time", "lapnumber"));
        assert!(eq("", ""));
    }

    #[test]
    fn contains_finds_normalized_substring() {
        assert!(contains("Gear Position", "gear"));
        assert!(contains("Lap Beacon Count", "beacon"));
        assert!(contains("GPS/Speed", "gpsspeed"));
        assert!(!contains("Speed", "rpm"));
        assert!(contains("Speed", ""));
    }

    #[test]
    fn find_respects_name_priority_over_channel_order() {
        let mk = |name: &str| crate::Channel {
            id: 0,
            name: name.into(),
            unit: String::new(),
            unit_source: crate::UnitSource::Unknown,
            sample_type: crate::SampleType::F32,
            chunks: Vec::new(),
            sample_count: 1,
            duration_ns: 0,
        };
        let channels = vec![mk("lap"), mk("lapnumber")];
        // "lapnumber" is higher priority even though "lap" is channel 0.
        assert_eq!(find(&channels, &["lapnumber", "lap"]), Some(1));
        // Falls back to the lower-priority name when the first misses.
        assert_eq!(find(&channels, &["missing", "lap"]), Some(0));
        assert_eq!(find(&channels, &["missing"]), None);
    }
}
