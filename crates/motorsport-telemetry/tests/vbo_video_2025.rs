//! Public sampling regression and private real-file acceptance.
//! No race telemetry or media is bundled.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::float_cmp,
    reason = "acceptance requires exact numeric equality and fails loudly on missing evidence or changed source clocks"
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
    assert_eq!(source.metadata().laps, indexed.metadata().laps);
    for (index, channel) in indexed.channels().iter().enumerate() {
        if channel.sample_count == 0 {
            continue;
        }
        assert_eq!(channel.sample_count, source.channels()[index].sample_count);
        assert_eq!(channel.chunks.len(), source.channels()[index].chunks.len());
        for (chunk_index, chunk) in channel.chunks.iter().enumerate() {
            assert_eq!(
                chunk.sample_base,
                source.channels()[index].chunks[chunk_index].sample_base
            );
            for local in 0..chunk.sample_count {
                assert_eq!(
                    indexed.sample_time_ns(index, chunk_index, local),
                    source.sample_time_ns(index, chunk_index, local)
                );
                let expected = source.decode(index, chunk_index, local);
                let actual = indexed.decode(index, chunk_index, local);
                assert!(
                    expected.to_bits() == actual.to_bits()
                        || (expected.is_nan() && actual.is_nan())
                );
            }
        }
    }
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
    // Compare every lattice-retained native number, including promoted f32
    // values. MTJ quantizes placement; it must not additionally round values.
    for (index, channel) in source.channels().iter().enumerate() {
        let cached_index = cached
            .channels()
            .iter()
            .position(|candidate| candidate.name == channel.name)
            .expect("native channel retained");
        let cached_channel = &cached.channels()[cached_index];
        let period = cached_channel.chunks[0].sample_period_ns;
        let origin = cached_channel.chunks[0].time_base_ns;
        let mut previous_slot = None;
        for (chunk_index, chunk) in channel.chunks.iter().enumerate() {
            for local in 0..chunk.sample_count {
                let time = source.sample_time_ns(index, chunk_index, local);
                let slot = (time - origin + period / 2) / period;
                if previous_slot == Some(slot) {
                    continue; // MTJ keeps the earlier observation in a slot.
                }
                previous_slot = Some(slot);
                let value = source.decode(index, chunk_index, local);
                let actual = cached.decode(cached_index, 0, slot);
                if value.is_nan() {
                    assert!(actual.is_nan());
                } else {
                    assert_eq!(
                        value,
                        actual,
                        "{} row {}",
                        channel.name,
                        chunk.sample_base + local
                    );
                }
                if value.is_finite() {
                    assert_eq!(
                        cached.sample_at(cached_index, origin + slot * period, true),
                        Some(value),
                        "exact native-lattice sample {} row {}",
                        channel.name,
                        chunk.sample_base + local
                    );
                }
            }
        }
    }
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
fn declared_vbo_gap_survives_native_conversion_and_exact_sample_lookup() {
    let fixture = tempfile::Builder::new().suffix(".vbo").tempfile().unwrap();
    std::fs::write(fixture.path(), "[header]\ntime\nsampleperiod\nvelocity kmh\ngear\n[column names]\ntime Tsample velocity Gear\n[data]\n120000.000 0.040 10 3\n120000.040 0.040 20 4\n120010.000 0.040 30 5\n120010.040 0.040 40 6\n").unwrap();
    let source = open_with_options(
        fixture.path(),
        &OpenOptions {
            ignore_track_yml: true,
            ..OpenOptions::default()
        },
    )
    .unwrap();
    let destination = tempfile::NamedTempFile::new().unwrap();
    telemetry_format::write_telemetry(&source, destination.path()).unwrap();
    let cached = telemetry_format::JsonlRecording::open(destination.path()).unwrap();
    for index in [2, 3] {
        assert_eq!(source.channels()[index].sample_count, 4);
        for linear in [false, true] {
            for time in [80_000_000, 1_000_000_000, 9_960_000_000] {
                assert_eq!(source.sample_at(index, time, linear), None);
                assert!(cached
                    .sample_at(index, time, linear)
                    .is_none_or(f64::is_nan));
            }
            for (time, expected) in [
                (0, 10.0),
                (40_000_000, 20.0),
                (10_000_000_000, 30.0),
                (10_040_000_000, 40.0),
            ] {
                let expected = if index == 2 {
                    expected
                } else {
                    expected / 10.0 + 2.0
                };
                assert_eq!(source.sample_at(index, time, linear), Some(expected));
                assert_eq!(cached.sample_at(index, time, linear), Some(expected));
            }
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
    let speed = source
        .channels()
        .iter()
        .position(|channel| channel.name == "velocity kmh")
        .expect("native speed channel");
    let destination = tempfile::NamedTempFile::new().unwrap();
    telemetry_format::write_telemetry(&source, destination.path()).unwrap();
    let cached = telemetry_format::JsonlRecording::open(destination.path()).unwrap();
    let cached_speed = cached
        .channels()
        .iter()
        .position(|channel| channel.name == "velocity kmh")
        .unwrap();
    for linear in [false, true] {
        for time in [1_000_000_000, 1_800_000_000_000] {
            assert_eq!(source.sample_at(speed, time, linear), None);
            assert!(cached
                .sample_at(cached_speed, time, linear)
                .is_none_or(f64::is_nan));
        }
        assert_eq!(
            source.sample_at(speed, 3_647_200_000_000, linear),
            cached.sample_at(cached_speed, 3_647_200_000_000, linear)
        );
    }
    assert_eq!(cached.video_timeline(), Some(clock));
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
