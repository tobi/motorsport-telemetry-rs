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
            driver_stints: Vec::new(),
            videos: Vec::new(),
            presentation_offset_ns: None,
        };
        apply(&mut catalog);
        assert_eq!(catalog.format_version, FORMAT_VERSION);
    }
}
