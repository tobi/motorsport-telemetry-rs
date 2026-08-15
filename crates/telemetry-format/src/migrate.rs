//! Version-to-version catalog migrations.
//!
//! Bump [`FORMAT_VERSION`] when the on-disk layout changes, then add a step
//! here. [`crate::NativeRecording::open`] rewrites writable older files.

use crate::catalog::{Catalog, FORMAT_VERSION};

/// Applies every step from `catalog.format_version` up to [`FORMAT_VERSION`].
pub fn apply(catalog: &mut Catalog) {
    while catalog.format_version < FORMAT_VERSION {
        let before = catalog.format_version;
        match catalog.format_version {
            0 | 1 => v1_to_v2(catalog),
            2 => v2_to_v3(catalog),
            3 => v3_to_v4(catalog),
            _ => break,
        }
        if catalog.format_version <= before {
            break;
        }
    }
}

fn v1_to_v2(catalog: &mut Catalog) {
    // v2 added optional `video_frames.bin` and `presentation_offset_ns`.
    // Existing members stay as they are; the writer copies them through.
    catalog.format_version = 2;
}

fn v2_to_v3(catalog: &mut Catalog) {
    // v3 stores first_video_frame on laps and the presentation offset on each
    // video handle. Values are filled from the opened recording on rewrite.
    let offset = catalog.presentation_offset_ns;
    for video in &mut catalog.videos {
        if video.presentation_offset_ns.is_none() {
            video.presentation_offset_ns = offset;
        }
    }
    catalog.format_version = 3;
}

fn v3_to_v4(catalog: &mut Catalog) {
    // v4 stamps UTC start-of-file and IANA timezone. Recover only from
    // stored clocks and the track atlas. Do not invent a UTC instant.
    if catalog.timezone.is_empty() {
        if let Some(timezone) = motorsport_track_atlas::timezone_for_venue(&catalog.identity.venue)
        {
            catalog.timezone = timezone.to_owned();
        }
    }
    if catalog.utc_start_ns.is_none() {
        catalog.utc_start_ns = crate::placement::utc_from_clock(
            catalog.clock.as_ref().map(|clock| clock.clock.as_str()),
            catalog.clock.as_ref().map(|clock| clock.start_ns),
            &catalog.timezone,
        );
    }
    catalog.format_version = 4;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walks_from_missing_version_to_current() {
        let mut catalog = Catalog {
            format_version: 0,
            identity: Default::default(),
            laps: Vec::new(),
            valid_laps: 0,
            channels: Vec::new(),
            source_format: String::new(),
            source_path: String::new(),
            schema_hash: 0,
            duration_ns: 0,
            sample_count: 0,
            channel_count: 0,
            sampled_channel_count: 0,
            session_hint: String::new(),
            comment: String::new(),
            clock: None,
            utc_start_ns: None,
            timezone: String::new(),
            driver_stints: Vec::new(),
            videos: Vec::new(),
            presentation_offset_ns: None,
        };
        apply(&mut catalog);
        assert_eq!(catalog.format_version, FORMAT_VERSION);
        assert!(catalog.utc_start_ns.is_none());
        assert!(catalog.timezone.is_empty());
    }

    #[test]
    fn v3_recovers_utc_from_gps_and_timezone_from_venue() {
        let mut catalog = Catalog {
            format_version: 3,
            identity: motorsport_telemetry_core::SourceIdentity {
                venue: "Sebring".into(),
                ..Default::default()
            },
            laps: Vec::new(),
            valid_laps: 0,
            channels: Vec::new(),
            source_format: String::new(),
            source_path: String::new(),
            schema_hash: 0,
            duration_ns: 0,
            sample_count: 0,
            channel_count: 0,
            sampled_channel_count: 0,
            session_hint: String::new(),
            comment: String::new(),
            clock: Some(motorsport_telemetry_core::AbsoluteTimeRange {
                clock: "gps".into(),
                start_ns: 1_700_000_000_000_000_000,
                end_ns: 1_700_000_000_100_000_000,
                session_hint: String::new(),
            }),
            utc_start_ns: None,
            timezone: String::new(),
            driver_stints: Vec::new(),
            videos: Vec::new(),
            presentation_offset_ns: None,
        };
        apply(&mut catalog);
        assert_eq!(catalog.format_version, 4);
        assert_eq!(catalog.utc_start_ns, Some(1_700_000_000_000_000_000));
        assert_eq!(catalog.timezone, "America/New_York");
    }
}
