use motorsport_telemetry::{
    motorsport_telemetry_core::TelemetrySource, open, open_metadata, open_sessions,
    read_lap_metadata, TelemetryNormalizer,
};
use std::path::PathBuf;
use telemetry_format::{write_from_source, write_jsonl_from_source};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

#[test]
fn detects_every_supported_format_and_normalizes_roles() {
    for (name, format) in [
        ("synthetic_aimd.mp4", "aimd"),
        ("synthetic_cosworth.pds", "pds"),
        ("synthetic_motec.ld", "motec"),
        ("synthetic_vbo.vbo", "vbo"),
    ] {
        let file = open(fixture(name)).unwrap();
        assert_eq!(file.format(), format);
        assert!(file.metadata().sample_count > 0);
        let roles = file.signal_roles();
        assert!(roles.speed.is_some(), "{name} speed role");
    }
}

#[test]
fn joins_aim_files_and_resolves_video_frame() {
    let sessions = open_sessions(
        [
            fixture("synthetic_aimd.mp4"),
            fixture("synthetic_aimd_part2.mp4"),
        ],
        1_000_000_000,
    )
    .unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].files.len(), 2);
    let position = sessions[0].position(0).unwrap();
    assert_eq!(position.video.presentation_time_ns, Some(104_000_000));
    assert_eq!(position.video.frame_index, Some(2));
    assert_eq!(position.driver_id, Some(3));
}

#[test]
fn matches_track_and_computes_gps_progress() {
    let file = open(fixture("synthetic_aimd.mp4")).unwrap();
    let normalizer = file.normalizer();
    assert_eq!(
        normalizer.track().unwrap().matched.track.slug,
        "road-america"
    );
    let sample = normalizer.sample(0);
    assert!(sample.latitude_deg.is_some());
    assert!(sample.longitude_deg.is_some());
    assert!(sample.lap_progress.is_some());
}

#[test]
fn vbo_sample_exposes_time_of_day() {
    let file = open(fixture("synthetic_vbo.vbo")).unwrap();
    let sample = file.normalizer().sample(0);
    assert!(sample.time_of_day_ns.is_some());
    assert!(sample.absolute_time_ns.is_some());
}

#[test]
fn reusable_normalizer_uses_lap_metadata_fallback() {
    let file = open(fixture("synthetic_cosworth.pds")).unwrap();
    let normalizer = TelemetryNormalizer::new(&file, file.signal_roles(), None);

    assert_eq!(normalizer.sample(1_000_000_000).lap_progress, Some(0.25));
    assert_eq!(normalizer.sample(2_000_000_000).lap_progress, Some(0.5));
}

#[test]
fn metadata_open_and_lap_api_cover_every_format() {
    for name in [
        "synthetic_aimd.mp4",
        "synthetic_cosworth.pds",
        "synthetic_motec_multilap.ld",
        "synthetic_vbo.vbo",
    ] {
        let path = fixture(name);
        let file = open_metadata(&path).unwrap();
        assert_eq!(
            file.metadata().laps,
            read_lap_metadata(&path).unwrap(),
            "{name}"
        );
    }
}

#[test]
fn telemetry_round_trip_preserves_aimd_video_timeline() {
    let source = open(fixture("synthetic_aimd.mp4")).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("synthetic_aimd.telemetry");
    write_from_source(&source, &dest).unwrap();
    let opened = open(&dest).unwrap();

    assert_eq!(opened.format(), "aimd");
    assert_eq!(
        opened.metadata().format_version,
        Some(motorsport_telemetry::FORMAT_VERSION)
    );
    assert!(!motorsport_telemetry::telemetry_needs_update(&dest).unwrap());
    assert_eq!(opened.video_frame_count(), source.video_frame_count());
    assert_eq!(
        opened.video_presentation_offset_ns(),
        source.video_presentation_offset_ns()
    );
    assert_eq!(
        opened.video_presentation_times_ns(),
        source.video_presentation_times_ns()
    );
    assert_eq!(opened.video_frame_at(0), source.video_frame_at(0));
    assert_eq!(opened.video_frame_at(0), Some(2));
    assert_eq!(opened.video_presentation_time_ns(0), Some(104_000_000));
    assert_eq!(opened.metadata().laps, source.metadata().laps);
    assert_eq!(
        opened.metadata().laps[0].first_video_frame,
        source.video_frame_at(source.metadata().laps[0].start_ns)
    );
    assert_eq!(
        opened.metadata().videos[0].presentation_offset_ns,
        source.video_presentation_offset_ns()
    );
    for step in 0..=20 {
        let t = step * 1_000_000;
        assert_eq!(
            opened.video_frame_at(t),
            source.video_frame_at(t),
            "frame at {t} ns"
        );
        assert_eq!(
            opened.video_reference_at(t),
            source.video_reference_at(t),
            "video ref at {t} ns"
        );
    }
}

#[test]
fn jsonl_round_trip_is_time_aligned() {
    let source = open(fixture("synthetic_cosworth.pds")).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("synthetic_cosworth.telemetry.jsonl");
    write_jsonl_from_source(&source, &dest).unwrap();
    let opened = open(&dest).unwrap();

    assert_eq!(opened.format(), "pds");
    assert_eq!(
        motorsport_telemetry::JSONL_VERSION,
        telemetry_format::JSONL_VERSION
    );
    assert!(!opened.channels().is_empty());
    let metadata = opened.metadata();
    assert_eq!(metadata.laps, source.metadata().laps);
    for (index, channel) in opened.channels().iter().enumerate() {
        let period = channel.first_period_ns().unwrap();
        assert!(period > 0, "{}", channel.name);
        assert_eq!(
            opened.sample_time_ns(index, 0, 0),
            channel.chunks[0].time_base_ns
        );
        if channel.sample_count > 1 {
            assert_eq!(
                opened.sample_time_ns(index, 0, 1),
                channel.chunks[0].time_base_ns + period
            );
        }
        let original = source
            .channels()
            .iter()
            .position(|candidate| candidate.name == channel.name)
            .unwrap();
        assert_eq!(
            opened.decode(index, 0, 0),
            source.decode(original, 0, 0),
            "{}",
            channel.name
        );
    }

    let zstd = dir.path().join("synthetic_cosworth.telemetry.jsonl.zstd");
    write_jsonl_from_source(&source, &zstd).unwrap();
    let compressed = open(&zstd).unwrap();
    assert_eq!(
        &std::fs::read(&zstd).unwrap()[..4],
        &[0x28, 0xB5, 0x2F, 0xFD]
    );
    assert_eq!(compressed.channels().len(), opened.channels().len());
    for (index, channel) in opened.channels().iter().enumerate() {
        assert_eq!(channel.name, compressed.channels()[index].name);
        assert_eq!(
            channel.sample_count,
            compressed.channels()[index].sample_count
        );
        for local in 0..channel.sample_count {
            assert_eq!(
                opened.decode(index, 0, local).to_bits(),
                compressed.decode(index, 0, local).to_bits(),
                "{}[{local}]",
                channel.name
            );
        }
    }
}

#[test]
fn jsonl_is_not_a_bit_copy_of_native_float32() {
    let source = open(fixture("synthetic_motec.ld")).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("synthetic_motec.telemetry.jsonl");
    write_jsonl_from_source(&source, &dest).unwrap();
    let jsonl = open(&dest).unwrap();

    let index = source
        .channels()
        .iter()
        .position(|channel| channel.name == "G_FORCE_LAT")
        .unwrap();
    let native = source.decode(index, 0, 1);
    let stored = jsonl
        .channels()
        .iter()
        .position(|channel| channel.name == "G_FORCE_LAT")
        .map(|index| jsonl.decode(index, 0, 1))
        .unwrap();
    assert_eq!(native, f64::from(0.2f32));
    assert_eq!(stored, 0.2);
    assert_ne!(
        native.to_bits(),
        stored.to_bits(),
        "JSON 0.2 is not the promoted f32 bit pattern"
    );
}

#[test]
fn jsonl_and_zstd_match_on_real_motec_when_present() {
    let src = PathBuf::from(
        "/home/tobi/.local/share/wineprefixes/motec-i2/drive_c/MoTeC/Logged Data/Samples/Circuit/Sample.ld",
    );
    if !src.is_file() {
        return;
    }
    let source = open(&src).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let plain = dir.path().join("sample.telemetry.jsonl");
    let zstd = dir.path().join("sample.telemetry.jsonl.zstd");
    write_jsonl_from_source(&source, &plain).unwrap();
    write_jsonl_from_source(&source, &zstd).unwrap();
    let a = open(&plain).unwrap();
    let b = open(&zstd).unwrap();
    assert_eq!(a.channels().len(), b.channels().len());
    let mut bit_mismatches = 0u64;
    let mut compared = 0u64;
    for (index, channel) in a.channels().iter().enumerate() {
        for local in 0..channel.sample_count {
            compared += 1;
            if a.decode(index, 0, local).to_bits() != b.decode(index, 0, local).to_bits() {
                bit_mismatches += 1;
            }
        }
    }
    assert_eq!(
        bit_mismatches, 0,
        "{bit_mismatches} of {compared} samples differ"
    );

    let mut dropped = 0usize;
    let mut source_mismatches = 0u64;
    let mut source_compared = 0u64;
    for (source_index, channel) in source.channels().iter().enumerate() {
        let Some(jsonl_index) = a
            .channels()
            .iter()
            .position(|candidate| candidate.name == channel.name)
        else {
            dropped += 1;
            continue;
        };
        let count = channel
            .sample_count
            .min(a.channels()[jsonl_index].sample_count);
        for local in 0..count {
            source_compared += 1;
            if source.decode(source_index, 0, local).to_bits()
                != a.decode(jsonl_index, 0, local).to_bits()
            {
                source_mismatches += 1;
            }
        }
    }
    eprintln!(
        "motec sample: jsonl/zstd identical; vs source dropped={dropped} compared={source_compared} bit_mismatches={source_mismatches}"
    );
    assert!(
        dropped > 0 || source_mismatches > 0,
        "expected JSONL not to be a bit-copy of the Motec source"
    );
}
