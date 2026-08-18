//! UTC start-of-file and venue timezone for absolute placement.
//!
//! Sample times stay file-relative. The header stamps Unix-epoch nanoseconds
//! at `t = 0` plus an IANA zone so a recording and its sidecars share one
//! absolute axis: `utc_epoch_ns = file_relative_ns + utc_start_ns`.
//! Timezone is for civil display only. It is never a join key.

use crate::{FileMetadata, TelemetrySource};
use motorsport_track_atlas::timezone_for_venue;

/// Resolves the IANA venue timezone without inventing one.
///
/// Order: the source's own stamp, then the track atlas for `identity.venue`.
pub fn resolve_timezone(source: &dyn TelemetrySource) -> String {
    let stamped = source.timezone();
    if !stamped.is_empty() {
        return stamped;
    }
    timezone_for_venue(&source.identity().venue)
        .unwrap_or("")
        .to_owned()
}

/// Resolves Unix-epoch nanoseconds at file `t = 0`.
///
/// Uses a value the source already stamped. Otherwise a `gps` clock is
/// already Unix epoch. A Motec-style `utc` clock is civil time pretending
/// to be UTC and is converted only when `timezone` is known. `time_of_day`
/// and missing clocks stay `None`.
pub fn resolve_utc_start_ns(source: &dyn TelemetrySource, timezone: &str) -> Option<u64> {
    if let Some(utc) = source.utc_start_ns() {
        return Some(utc);
    }
    let metadata = crate::read_source_metadata(source);
    utc_from_metadata(&metadata, timezone)
}

/// UTC start from a derived metadata clock. See [`resolve_utc_start_ns`].
pub fn utc_from_metadata(metadata: &FileMetadata, timezone: &str) -> Option<u64> {
    utc_from_clock(
        metadata.absolute_clock.as_deref(),
        metadata
            .clock_offset_ns
            .and_then(|offset| u64::try_from(offset).ok())
            .or(metadata.absolute_start_ns),
        timezone,
    )
}

/// Interprets one absolute clock's start nanoseconds as true UTC epoch ns.
///
/// `"gps"` is already Unix epoch. `"utc"` is civil time pretending to be UTC
/// and is converted to true UTC only when `timezone` is known. All other
/// (or missing) clocks return `None`.
pub fn utc_from_clock(clock: Option<&str>, start_ns: Option<u64>, timezone: &str) -> Option<u64> {
    let start_ns = start_ns?;
    match clock? {
        "gps" => Some(start_ns),
        "utc" if !timezone.is_empty() => civil_ns_to_utc_ns(start_ns, timezone),
        _ => None,
    }
}

/// Interprets `civil_ns` (Unix epoch if that civil clock were UTC) as local
/// time in `timezone` and returns true UTC epoch nanoseconds.
pub fn civil_ns_to_utc_ns(civil_ns: u64, timezone: &str) -> Option<u64> {
    let seconds = i64::try_from(civil_ns / 1_000_000_000).ok()?;
    let nanos = i32::try_from(civil_ns % 1_000_000_000).ok()?;
    let as_utc = jiff::Timestamp::new(seconds, nanos).ok()?;
    let civil = as_utc.to_zoned(jiff::tz::TimeZone::UTC).datetime();
    let tz = jiff::tz::TimeZone::get(timezone).ok()?;
    let zoned = civil.to_zoned(tz).ok()?;
    let out = zoned.timestamp();
    let secs = u64::try_from(out.as_second()).ok()?;
    let sub = u64::from(u32::try_from(out.subsec_nanosecond()).ok()?);
    Some(secs.saturating_mul(1_000_000_000).saturating_add(sub))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sebring_civil_converts_to_utc() {
        // 2025-03-15 10:00:00 as if it were UTC, then as America/New_York (EDT).
        let civil = jiff::civil::date(2025, 3, 15)
            .at(10, 0, 0, 0)
            .to_zoned(jiff::tz::TimeZone::UTC)
            .unwrap()
            .timestamp();
        let civil_ns = u64::try_from(civil.as_second()).unwrap() * 1_000_000_000;
        let utc_ns = civil_ns_to_utc_ns(civil_ns, "America/New_York").unwrap();
        let expected = jiff::civil::date(2025, 3, 15)
            .at(10, 0, 0, 0)
            .to_zoned(jiff::tz::TimeZone::get("America/New_York").unwrap())
            .unwrap()
            .timestamp();
        assert_eq!(
            utc_ns,
            u64::try_from(expected.as_second()).unwrap() * 1_000_000_000
        );
        assert_eq!(utc_ns, civil_ns + 4 * 3_600 * 1_000_000_000);
    }

    #[test]
    fn gps_clock_is_already_utc() {
        assert_eq!(utc_from_clock(Some("gps"), Some(42), ""), Some(42));
        assert_eq!(utc_from_clock(Some("time_of_day"), Some(42), ""), None);
        assert_eq!(utc_from_clock(Some("utc"), Some(42), ""), None);
    }
}
