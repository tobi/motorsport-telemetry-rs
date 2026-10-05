//! Private real-file acceptance. No race telemetry or media is bundled.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "private acceptance fixtures fail loudly on missing evidence or changed source clocks"
)]

use motorsport_telemetry::{open_metadata_with_options, open_with_options, OpenOptions};
use motorsport_telemetry_core::{TelemetrySource, VideoMappingError};
use std::path::{Path, PathBuf};

fn fixture_root() -> PathBuf {
    std::env::var_os("VBO_VIDEO_2025_FIXTURES")
        .map(PathBuf::from)
        .expect("set VBO_VIDEO_2025_FIXTURES to the privately staged original fixtures")
}

fn assert_round_trip(path: &Path, first_pts: u64) {
    let options = OpenOptions {
        ignore_track_yml: true,
        ..OpenOptions::default()
    };
    let source = open_with_options(path, &options).unwrap();
    let indexed = open_metadata_with_options(path, &options).unwrap();
    let timeline = source.video_timeline().expect("native VBO video mapping");
    assert_eq!(
        timeline.presentation_at(0).unwrap().presentation_time_ns,
        first_pts
    );
    assert_eq!(indexed.video_timeline(), Some(timeline));
    assert_eq!(source.metadata().video_timeline.as_ref(), Some(timeline));
    let destination = tempfile::NamedTempFile::new().unwrap();
    telemetry_format::write_telemetry(&source, destination.path()).unwrap();
    let cached = telemetry_format::JsonlRecording::open(destination.path()).unwrap();
    let header = telemetry_format::JsonlRecording::read_header_metadata(destination.path())
        .unwrap()
        .unwrap();
    assert_eq!(cached.video_timeline(), Some(timeline));
    assert_eq!(header.video_timeline.as_ref(), Some(timeline));
    assert!(cached
        .video_files()
        .iter()
        .all(|video| video.blake3.is_none()));
    for segment in timeline.segments() {
        for point in &segment.points {
            let expected = timeline.presentation_at(point.telemetry_time_ns).unwrap();
            assert_eq!(
                cached
                    .video_timeline()
                    .unwrap()
                    .presentation_at(point.telemetry_time_ns)
                    .unwrap(),
                expected
            );
        }
    }
}

#[test]
#[ignore = "requires privately staged 2025 Road Atlanta originals"]
fn original_vbo_clocks_survive_metadata_and_native_conversion() {
    let root = fixture_root();
    assert_round_trip(
        &root.join("VBOX202510110940090001-vbox.vbo"),
        30_933_000_000,
    );
    assert_round_trip(
        &root.join("25IR07_PLM_R_Run07_DHH-vbox.vbo"),
        16_233_000_000,
    );
    assert_round_trip(
        &root.join("VBOX202510111105560001-vbox.vbo"),
        31_099_000_000,
    );
}

#[test]
#[ignore = "requires privately staged 2025 Road Atlanta split excerpt"]
fn exact_roll_and_omitted_interval_remain_explicit() {
    let source = open_with_options(
        fixture_root().join("VBOX202510111105560001-vbox-excerpt.vbo"),
        &OpenOptions {
            ignore_track_yml: true,
            ..OpenOptions::default()
        },
    )
    .unwrap();
    let clock = source.video_timeline().unwrap();
    assert_eq!(
        clock.presentation_at(1_000_000_000),
        Err(VideoMappingError::Unmapped)
    );
    let position = clock.presentation_at(3_647_200_000_000).unwrap();
    assert_eq!(position.file_index, 2);
    assert_eq!(position.presentation_time_ns, 0);
    assert_eq!(
        clock
            .presentation_at(3_647_160_000_000)
            .unwrap()
            .presentation_time_ns,
        3_678_266_000_000
    );
    assert_eq!(clock.telemetry_at(2, 0).unwrap(), 3_647_200_000_000);
}
