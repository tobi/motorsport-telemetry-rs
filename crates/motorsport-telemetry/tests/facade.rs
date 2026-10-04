#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::unreadable_literal,
    clippy::float_cmp,
    clippy::format_push_string,
    reason = "test and example code: fail loudly, print freely, exact fixture values"
)]

use motorsport_telemetry::{
    motorsport_telemetry_core::TelemetrySource, open, open_metadata, open_sessions,
    read_lap_metadata, SourceExt, TelemetryNormalizer,
};
use std::path::PathBuf;
use telemetry_format::{write_jsonl_from_source, write_jsonl_from_source_with, write_telemetry};

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
fn delayed_aim_gps_recovers_existing_coordinates_through_jsonl_conversion() {
    const SECOND_NS: u64 = 1_000_000_000;
    let source = open(fixture("synthetic_aimd_delayed_gps.mp4")).unwrap();
    let anchor = source.normalizer().sample(183 * SECOND_NS);
    assert!((anchor.latitude_deg.unwrap() - 43.8).abs() < 1e-6);
    assert_eq!(
        source.normalizer().sample(0).latitude_deg,
        anchor.latitude_deg
    );
    let (passed, _) = telemetry_passes::apply_registry(&source).unwrap();
    assert_eq!(passed.channels().len(), source.channels().len() + 6);
    assert_eq!(passed.applied_passes().len(), 3);
    assert_eq!(passed.applied_passes()[1].version, 2);
    let normalizer = passed.normalizer();
    for second in [0, 60, 179, 180, 182, 183, 209] {
        let sample = normalizer.sample(second * SECOND_NS);
        assert_eq!(sample.latitude_deg, anchor.latitude_deg);
        assert_eq!(sample.longitude_deg, anchor.longitude_deg);
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("delayed.telemetry");
    write_telemetry(&passed, &path).unwrap();
    let recording = open(&path).unwrap();
    let normalizer = recording.normalizer();
    for second in [0, 60, 179, 180, 182, 183, 209] {
        assert_eq!(
            normalizer.sample(second * SECOND_NS).latitude_deg,
            anchor.latitude_deg
        );
    }
    assert!(
        normalizer.sample(220 * SECOND_NS).latitude_deg.unwrap() > anchor.latitude_deg.unwrap()
    );
    for second in 230..235 {
        assert_eq!(normalizer.sample(second * SECOND_NS).latitude_deg, None);
    }
    assert!(normalizer.sample(235 * SECOND_NS).latitude_deg.is_some());
    let fix = recording
        .channels()
        .iter()
        .position(|channel| channel.name == "GPS Fix Type")
        .unwrap();
    assert_eq!(recording.sample_at(fix, 0, false), None);
    assert_eq!(recording.sample_at(fix, 180 * SECOND_NS, false), Some(0.0));
    assert_eq!(recording.sample_at(fix, 183 * SECOND_NS, false), Some(3.0));
    // The inferred lead-in must not change the GPS channel's rate in MTJ.
    let latitude = recording.signal_roles().latitude.unwrap();
    assert_eq!(recording.channels()[latitude].sample_count, 300);
    let stripped = dir.path().join("stripped.telemetry");
    telemetry_format::write_telemetry_stripped(&recording, &stripped).unwrap();
    let raw = open(stripped).unwrap();
    assert_eq!(raw.channels().len(), source.channels().len());
    assert_eq!(raw.normalizer().sample(0).latitude_deg, anchor.latitude_deg);
    assert!(raw.applied_passes().is_empty());
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
fn matches_track_without_inventing_gps_progress() {
    let file = open(fixture("synthetic_aimd.mp4")).unwrap();
    assert_eq!(
        file.match_track().unwrap().matched.track.slug,
        "road-america"
    );
    for sample in [
        file.normalizer().sample(0),
        telemetry_passes::apply_registry(&file)
            .unwrap()
            .0
            .normalizer()
            .sample(0),
    ] {
        assert!(sample.latitude_deg.is_some());
        assert!(sample.longitude_deg.is_some());
        assert_eq!(sample.lap_progress, None);
    }
}

#[test]
fn vbo_sample_exposes_time_of_day() {
    let file = open(fixture("synthetic_vbo.vbo")).unwrap();
    let sample = file.normalizer().sample(0);
    assert!(sample.time_of_day_ns.is_some());
    assert!(sample.absolute_time_ns.is_some());
}

#[test]
fn lap_distance_and_elapsed_time_do_not_invent_progress() {
    let file = open(fixture("synthetic_cosworth.pds")).unwrap();
    let mut roles = file.signal_roles();
    let distance = roles.lap_distance.unwrap();
    assert_eq!(file.channels()[distance].unit, "m");
    // Force lap-time fallback to metadata, not the source timer.
    roles.lap_time = None;
    let normalizer = TelemetryNormalizer::new(&file, roles);
    let flying = file
        .metadata()
        .laps
        .into_iter()
        .find(|lap| lap.kind.is_flying())
        .expect("flying lap");
    for elapsed in [flying.duration_ns / 4, flying.duration_ns / 2] {
        let time_ns = flying.start_ns + elapsed;
        assert!(file.sample_at(distance, time_ns, true).is_some());
        let sample = normalizer.sample(time_ns);
        assert_eq!(sample.lap_number, Some(flying.number));
        assert_eq!(sample.lap_time_s, Some(elapsed as f64 / 1e9));
        assert_eq!(sample.lap_progress, None);
    }
}

#[test]
fn source_reported_progress_survives_conversion() {
    let source = open(fixture("synthetic_motec_multilap.ld")).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("progress.telemetry");
    write_telemetry(&source, &dest).unwrap();
    let recording = open(&dest).unwrap();
    for file in [&source, &recording] {
        let normalizer = file.normalizer();
        let channel = normalizer.roles().lap_distance.unwrap();
        assert_eq!(file.channels()[channel].name, "Lap Progression");
        assert_eq!(file.channels()[channel].unit, "%");
        for time_ns in [0, 2_000_000_000, 12_000_000_000] {
            let raw = file.sample_at(channel, time_ns, true).unwrap();
            let progress = normalizer.sample(time_ns).lap_progress.unwrap();
            assert!((progress - raw / 100.0).abs() < 1e-12);
        }
    }
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
    write_telemetry(&source, &dest).unwrap();
    let opened = open(&dest).unwrap();

    assert_eq!(opened.format(), "aimd");
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
        let left = opened.decode(index, 0, 0);
        let right = source.decode(original, 0, 0);
        assert!(
            (left - right).abs() <= 1e-9 * right.abs().max(1.0),
            "{} {left} != {right}",
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
fn jsonl_uncompressed_round_trip_keeps_cosworth_lap_summary() {
    let source = open(fixture("synthetic_cosworth.pds")).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("synthetic_cosworth.telemetry.jsonl");
    write_jsonl_from_source_with(&source, &dest, false).unwrap();
    let bytes = std::fs::read(&dest).unwrap();
    assert_eq!(bytes[0], b'{');
    assert_ne!(&bytes[..4], &[0x28, 0xB5, 0x2F, 0xFD]);
    let opened = open(&dest).unwrap();

    let source_meta = source.metadata();
    let opened_meta = opened.metadata();
    assert_eq!(opened_meta.laps.len(), source_meta.laps.len());
    assert_eq!(
        opened_meta.laps.iter().filter(|lap| lap.complete).count(),
        source_meta.laps.iter().filter(|lap| lap.complete).count()
    );
    assert_eq!(opened_meta.valid_laps, source_meta.valid_laps);
    assert_eq!(
        opened_meta.fastest_lap.as_ref().map(|lap| lap.number),
        source_meta.fastest_lap.as_ref().map(|lap| lap.number)
    );
    assert_eq!(source_meta.laps.len(), 5);
    assert_eq!(source_meta.valid_laps, 3);
    assert_eq!(
        source_meta.fastest_lap.as_ref().map(|lap| lap.number),
        Some(2)
    );
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

/// Compressed and plain JSONL use the same reader regardless of the suffix.
#[test]
fn telemetry_opens_plain_and_compressed_jsonl_by_content() {
    let source = open(fixture("synthetic_cosworth.pds")).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let plain = dir.path().join("plain.telemetry");
    let compressed = dir.path().join("compressed.telemetry.jsonl");
    write_jsonl_from_source_with(&source, &plain, false).unwrap();
    write_telemetry(&source, &compressed).unwrap();
    let plain_file = open(&plain).unwrap();
    let compressed_file = open(&compressed).unwrap();
    assert_eq!(plain_file.metadata().laps, compressed_file.metadata().laps);
    let directory = |file: &motorsport_telemetry::TelemetryFile| {
        file.channels()
            .iter()
            .map(|channel| {
                (
                    channel.name.clone(),
                    channel.unit.clone(),
                    channel.sample_count,
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(directory(&plain_file), directory(&compressed_file));
    assert_eq!(
        read_lap_metadata(&plain).unwrap(),
        plain_file.metadata().laps
    );
    assert_eq!(
        motorsport_telemetry::read_metadata(&compressed)
            .unwrap()
            .laps,
        compressed_file.metadata().laps
    );
    let plain_report = motorsport_telemetry::verify(&plain).unwrap();
    let compressed_report = motorsport_telemetry::verify(&compressed).unwrap();
    assert_eq!(plain_report.kind, motorsport_telemetry::VerifyKind::Mtj);
    assert!(!plain_report.compressed);
    assert!(compressed_report.compressed);
    for time_ns in [5_000_000_000, 60_000_000_000, 200_000_000_000] {
        let a = plain_file.normalizer().sample(time_ns);
        let b = compressed_file.normalizer().sample(time_ns);
        assert_eq!(a.speed_mps, b.speed_mps);
        assert_eq!(a.lap_label, b.lap_label);
    }
}

#[test]
fn legacy_zip_is_rejected_without_modifying_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.telemetry");
    let bytes = b"PK\x03\x04obsolete archive";
    std::fs::write(&path, bytes).unwrap();
    assert!(open(&path).is_err());
    assert!(motorsport_telemetry::verify(&path).is_err());
    assert!(motorsport_telemetry::read_metadata(&path).is_err());
    assert!(telemetry_format::read_channels(&path).is_err());
    assert_eq!(std::fs::read(path).unwrap(), bytes);
}

/// A source whose channels declare no unit (`AiM` CAN echoes, VBOX CAN columns)
/// plus a Cosworth-style pedal-as-angle channel. The normalizer must read the
/// pedal, steering, rpm and lap time by their proven ranges, keep the pressure
/// unresolved, and fall back to the classified laps for the running lap time
/// and the stint lap counter.
#[test]
fn unitless_channels_are_read_by_range_and_laps_fill_the_gaps() {
    use motorsport_telemetry::motorsport_telemetry_core::{Channel, Chunk, SampleType, UnitSource};
    struct Synthetic {
        channels: Vec<Channel>,
        values: Vec<Vec<f64>>,
    }
    impl TelemetrySource for Synthetic {
        fn path(&self) -> &'static str {
            "unitless"
        }
        fn format(&self) -> &'static str {
            "synthetic"
        }
        fn channels(&self) -> &[Channel] {
            &self.channels
        }
        fn decode(&self, c: usize, _: usize, i: u64) -> f64 {
            self.values[c][i as usize]
        }
    }
    let n = 120u64; // 1 s samples, 120 s
    let mk = |id: u32, name: &str, unit: &str| Channel {
        id,
        name: name.into(),
        unit: unit.into(),
        unit_source: if unit.is_empty() {
            UnitSource::Unknown
        } else {
            UnitSource::Declared
        },
        sample_type: SampleType::F64,
        chunks: vec![Chunk {
            sample_period_ns: 1_000_000_000,
            sample_count: n,
            data_ptr: 0,
            sample_base: 0,
            time_base_ns: 0,
        }],
        sample_count: n,
        duration_ns: n * 1_000_000_000,
    };
    let t = |f: &dyn Fn(u64) -> f64| (0..n).map(f).collect::<Vec<_>>();
    let build = |specs: Vec<(&str, &str, Vec<f64>)>| Synthetic {
        channels: specs
            .iter()
            .enumerate()
            .map(|(i, (name, unit, _))| mk(i as u32, name, unit))
            .collect(),
        values: specs.into_iter().map(|(_, _, v)| v).collect(),
    };
    let source = build(vec![
        // Lap counter 1,2,3 every 40 s; unitless lap timer in milliseconds.
        ("Lap_Number", "", t(&|i| (1 + i / 40) as f64)),
        (
            "Current_Lap_Time",
            "",
            t(&|i| ((i % 40) * 1000 + 50) as f64),
        ),
        ("Speed_Wspd_App", "", t(&|_| 180.0)), // km/h by range
        ("STEER_001", "", t(&|i| (i as f64 - 60.0) * 3.0)), // -180..177 -> degrees
        ("RPM", "", t(&|_| 6500.0)),           // rpm by range
        ("P_Brake_Front", "", t(&|_| 45.0)),   // pressure: never inferred
        ("PPS", "rad", t(&|_| 50.0_f64.to_radians())), // Cosworth angle-pedal: 50 deg = 50 %
    ]);
    let normalizer = source.normalizer();
    let units = normalizer.units();
    assert_eq!(units.speed.as_deref(), Some("km/h"));
    assert_eq!(
        units.throttle.as_deref(),
        Some("rad"),
        "declared unit is kept"
    );
    assert_eq!(units.steering.as_deref(), Some("deg"));
    assert_eq!(units.rpm.as_deref(), Some("rpm"));
    assert_eq!(units.lap_time.as_deref(), Some("ms"));
    assert_eq!(
        units.brake_pressure, None,
        "bar vs psi is ambiguous without a unit"
    );

    let sample = normalizer.sample(50_000_000_000); // 50 s: lap 2, 10 s in
    assert!((sample.speed_mps.unwrap() - 50.0).abs() < 1e-9);
    assert!(
        (sample.throttle_fraction.unwrap() - 0.5).abs() < 1e-9,
        "50 deg of pedal angle is 50 %"
    );
    assert!((sample.steering_deg.unwrap() - (-30.0)).abs() < 1e-9);
    assert_eq!(sample.rpm, Some(6500.0));
    assert_eq!(sample.brake_pressure_bar, None);
    assert!(
        (sample.lap_time_s.unwrap() - 10.05).abs() < 1e-9,
        "unitless ms timer"
    );
    assert_eq!(sample.stint_lap_number, Some(2));
    assert_eq!(sample.lap_number, Some(2));
    assert_eq!(sample.lap_label.as_deref(), Some("S1 L2"));

    // A unitless pedal that reaches 99 is percent; one that never exceeds 1 is a ratio.
    let percent = build(vec![("Pedal_Pos", "", t(&|i| (i % 100) as f64))]);
    assert_eq!(percent.normalizer().units().throttle.as_deref(), Some("%"));
    assert!(
        (percent
            .normalizer()
            .sample(50_000_000_000)
            .throttle_fraction
            .unwrap()
            - 0.5)
            .abs()
            < 1e-9
    );
    let ratio = build(vec![("Pedal_Pos", "", t(&|i| (i % 100) as f64 / 100.0))]);
    assert_eq!(
        ratio.normalizer().units().throttle.as_deref(),
        Some("ratio")
    );
    // A unitless speed that never exceeds 130 is ambiguous and stays None.
    let slow = build(vec![("Speed", "", t(&|_| 90.0))]);
    assert_eq!(slow.normalizer().units().speed, None);
    assert_eq!(slow.normalizer().sample(1_000_000_000).speed_mps, None);
    // ...and then a lower-priority GPS speed with a declared unit is used.
    let gps = build(vec![
        ("Speed_Wspd_App", "", t(&|_| 90.0)),
        ("GPS Speed", "m/s", t(&|_| 25.0)),
    ]);
    let n = gps.normalizer();
    assert_eq!(n.roles().speed, Some(1));
    assert_eq!(n.sample(1_000_000_000).speed_mps, Some(25.0));
    // But a provably-km/h dash speed outranks GPS by priority.
    let dash = build(vec![
        ("Speed_Wspd_App", "", t(&|_| 180.0)),
        ("GPS Speed", "m/s", t(&|_| 25.0)),
    ]);
    let n = dash.normalizer();
    assert_eq!(n.roles().speed, Some(0));
    assert_eq!(n.units().speed.as_deref(), Some("km/h"));
    assert!((n.sample(1_000_000_000).speed_mps.unwrap() - 50.0).abs() < 1e-9);

    // Without any timer or counter channel the same values come from the laps.
    let bare = Synthetic {
        channels: source.channels()[2..].to_vec(),
        values: source.values[2..].to_vec(),
    };
    let bare_n = bare.normalizer();
    let s = bare_n.sample(50_000_000_000);
    assert_eq!(
        s.lap_number, None,
        "no laps at all without a counter or timer"
    );
    assert_eq!(s.lap_time_s, None);
    assert!((s.throttle_fraction.unwrap() - 0.5).abs() < 1e-9);
}
