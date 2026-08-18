//! Version-to-version catalog migrations.
//!
//! Bump [`FORMAT_VERSION`] when the on-disk layout changes, then add a step
//! here. [`crate::NativeRecording::open`] rewrites writable older files.
//!
//! Migration only advances the catalog version and applies the structural
//! changes each version requires. Placement recovery — deriving
//! `utc_start_ns` and `timezone` from stored clocks and the track atlas — is
//! deferred to the rewrite path ([`crate::NativeRecording::rewrite_migrated`]
//! and the writers), which resolves them through
//! `motorsport_telemetry_core::placement`. A catalog never invents an absolute
//! instant or venue timezone on its own.

use crate::catalog::{Catalog, FORMAT_VERSION};
use motorsport_telemetry_core::ChannelDisplay;

/// Errors from advancing a catalog to the current format version.
#[derive(Debug, thiserror::Error)]
pub enum MigrateError {
    /// The catalog reports a version this build cannot migrate: either a
    /// future version newer than [`FORMAT_VERSION`] or an unknown gap in the
    /// step chain. Either way the file must not be silently rewritten as if
    /// it were current.
    #[error("unsupported catalog format version {0}")]
    UnsupportedVersion(u16),
}

/// Applies every step from `catalog.format_version` up to [`FORMAT_VERSION`].
///
/// Returns [`MigrateError::UnsupportedVersion`] for a future/unknown version
/// instead of silently leaving the catalog in place.
pub fn apply(catalog: &mut Catalog) -> Result<(), MigrateError> {
    while catalog.format_version < FORMAT_VERSION {
        let before = catalog.format_version;
        match catalog.format_version {
            0 | 1 => v1_to_v2(catalog),
            2 => v2_to_v3(catalog),
            3 => v3_to_v4(catalog),
            4 => v4_to_v5(catalog),
            5 => v5_to_v6(catalog),
            6 => v6_to_v7(catalog),
            7 => v7_to_v8(catalog),
            8 => v8_to_v9(catalog),
            9 => v9_to_v10(catalog),
            other => return Err(MigrateError::UnsupportedVersion(other)),
        }
        if catalog.format_version <= before {
            return Err(MigrateError::UnsupportedVersion(before));
        }
    }
    if catalog.format_version > FORMAT_VERSION {
        return Err(MigrateError::UnsupportedVersion(catalog.format_version));
    }
    Ok(())
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
    // v4 stamps UTC start-of-file and IANA timezone. The catalog itself does
    // not recover them: the rewrite path fills `utc_start_ns` and `timezone`
    // from the recording's clocks and venue via core placement, never
    // inventing a value. Advancing the version here marks the file as v4 so
    // the writer persists the resolved fields.
    catalog.format_version = 4;
}

fn v4_to_v5(catalog: &mut Catalog) {
    // v5 adds spans and per-channel visibility. Older files have neither;
    // do not invent annotations.
    for channel in &mut catalog.channels {
        channel.visible = true;
    }
    catalog.spans.clear();
    catalog.format_version = 5;
}

fn v5_to_v6(catalog: &mut Catalog) {
    // v6 adds sparse per-channel comment labels. Older files have none.
    for channel in &mut catalog.channels {
        channel.labels.clear();
    }
    catalog.format_version = 6;
}

fn v6_to_v7(catalog: &mut Catalog) {
    // v7 adds plot class / scale / rounding. Older channels stay traces.
    for channel in &mut catalog.channels {
        channel.display = ChannelDisplay::trace();
        if !channel.display.plot.is_trace() {
            channel.labels.clear();
        }
    }
    catalog.format_version = 7;
}

fn v7_to_v8(catalog: &mut Catalog) {
    // v8 types span meta: racing-time strings become timespan_ms (u32).
    // unpack_spans already reinterprets v5–v7 strings; this step only
    // advances the version so the rewrite packs the u32 form.
    catalog.format_version = 8;
}

fn v8_to_v9(catalog: &mut Catalog) {
    // v9 records applied processing passes and keeps `source_format` /
    // `source_path` stable across rewrites. Older files never ran a pass;
    // their channels are raw conversions. Do not invent provenance. A file
    // whose source_path was lost to a pre-v9 rewrite stays as it is.
    catalog.passes.clear();
    catalog.format_version = 9;
}

fn v9_to_v10(catalog: &mut Catalog) {
    // v10 adds signed int8 (SampleType::I8, sample-type code 0). No schema
    // fields or zip members change. v1–v9 writers never emitted code 0, so
    // there is nothing to reinterpret — the step only advances the version
    // so the rewrite packs code 0 for any I8 channel the source exposes.
    catalog.format_version = 10;
}

#[cfg(test)]
mod tests {
    use super::*;
    use motorsport_telemetry_core::AbsoluteTimeRange;

    fn empty_catalog(version: u16) -> Catalog {
        Catalog {
            format_version: version,
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
            spans: Vec::new(),
            passes: Vec::new(),
        }
    }

    #[test]
    fn walks_from_missing_version_to_current() {
        let mut catalog = empty_catalog(0);
        apply(&mut catalog).unwrap();
        assert_eq!(catalog.format_version, FORMAT_VERSION);
        // apply advances the version only; placement recovery is deferred to
        // the rewrite path, so a bare catalog stays without utc/timezone.
        assert!(catalog.utc_start_ns.is_none());
        assert!(catalog.timezone.is_empty());
        assert!(catalog.spans.is_empty());
    }

    #[test]
    fn apply_advances_v3_without_recovering_placement() {
        // A v3 catalog with a gps clock and a known venue used to recover
        // utc_start_ns/timezone inside the migration step. Recovery now lives
        // in the rewrite path (core placement), so apply must only advance the
        // version and leave placement fields empty.
        let mut catalog = empty_catalog(3);
        catalog.identity.venue = "Sebring".into();
        catalog.clock = Some(AbsoluteTimeRange {
            clock: "gps".into(),
            start_ns: 1_700_000_000_000_000_000,
            end_ns: 1_700_000_000_100_000_000,
            session_hint: String::new(),
        });
        apply(&mut catalog).unwrap();
        assert_eq!(catalog.format_version, FORMAT_VERSION);
        assert!(catalog.utc_start_ns.is_none());
        assert!(catalog.timezone.is_empty());
        assert!(catalog.spans.is_empty());
    }

    #[test]
    fn rejects_future_version() {
        let mut catalog = empty_catalog(FORMAT_VERSION + 1);
        let err = apply(&mut catalog).unwrap_err();
        assert!(matches!(err, MigrateError::UnsupportedVersion(v) if v == FORMAT_VERSION + 1));
    }
}
