use motorsport_telemetry::{
    motorsport_telemetry_core::TelemetrySource, open, open_metadata, open_sessions,
    read_lap_metadata, SourceExt, TelemetryNormalizer,
};
use std::path::PathBuf;
use telemetry_format::{write_from_source, write_jsonl_from_source, write_jsonl_from_source_with};

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
    let flying = file
        .metadata()
        .laps
        .into_iter()
        .find(|lap| lap.number == 2 && lap.complete)
        .expect("flying lap 2");
    let quarter = flying.start_ns + flying.duration_ns / 4;
    let half = flying.start_ns + flying.duration_ns / 2;
    let q = normalizer.sample(quarter).lap_progress.unwrap();
    let h = normalizer.sample(half).lap_progress.unwrap();
    assert!((q - 0.25).abs() < 0.03, "quarter={q}");
    assert!((h - 0.5).abs() < 0.03, "half={h}");
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

/// Both `.telemetry` containers, side by side in one process: the committed
/// legacy zip fixture (`PK\x03\x04`, FlatBuffers catalog, written by
/// `convert --native-zip`) and the committed zstd-MTJ fixture
/// (`28 B5 2F FD`) of the same synthetic Cosworth recording. Same name
/// extension, different first bytes, identical content once opened.
#[test]
fn legacy_zip_and_zstd_mtj_telemetry_files_open_side_by_side() {
    use telemetry_format::{sniff_container, Container, TelemetryRecording};

    let legacy_path = fixture("synthetic_cosworth.legacy.telemetry");
    let modern_path = fixture("synthetic_cosworth.telemetry");
    assert_eq!(&std::fs::read(&legacy_path).unwrap()[..4], b"PK\x03\x04");
    assert_eq!(
        &std::fs::read(&modern_path).unwrap()[..4],
        &[0x28, 0xB5, 0x2F, 0xFD]
    );
    assert_eq!(sniff_container(&legacy_path).unwrap(), Container::NativeZip);
    assert_eq!(sniff_container(&modern_path).unwrap(), Container::JsonlZstd);

    // Same generic entry point for both; the container is decided by content.
    let legacy = open(&legacy_path).unwrap();
    let modern = open(&modern_path).unwrap();
    let vendor = open(fixture("synthetic_cosworth.pds")).unwrap();
    for (name, file) in [("legacy", &legacy), ("modern", &modern)] {
        assert_eq!(file.format(), "pds", "{name} keeps the vendor format");
        assert_eq!(
            file.channels().len(),
            vendor.channels().len(),
            "{name} channel count"
        );
    }

    // Identical laps, including the stint model, for both and the source.
    let strip = |laps: Vec<motorsport_telemetry::motorsport_telemetry_core::LapMetadata>| {
        laps.into_iter()
            .map(|lap| {
                (
                    lap.number,
                    lap.stint,
                    lap.stint_lap,
                    lap.kind,
                    lap.label(),
                    lap.complete,
                    lap.start_ns / 1_000_000,
                    lap.end_ns / 1_000_000,
                )
            })
            .collect::<Vec<_>>()
    };
    let legacy_laps = strip(read_lap_metadata(&legacy_path).unwrap());
    let modern_laps = strip(read_lap_metadata(&modern_path).unwrap());
    let vendor_laps = strip(vendor.metadata().laps);
    assert_eq!(legacy_laps, vendor_laps);
    assert_eq!(modern_laps, vendor_laps);
    assert_eq!(legacy_laps.len(), 5);
    assert_eq!(
        legacy_laps
            .iter()
            .map(|lap| lap.4.as_str())
            .collect::<Vec<_>>(),
        ["S1 out", "S1 L2", "S1 L3", "S1 L4", "S1 in"]
    );

    // Identical normalized samples at the same instants.
    let legacy_n = legacy.normalizer();
    let modern_n = modern.normalizer();
    let vendor_n = vendor.normalizer();
    for time_ns in [5_000_000_000u64, 60_000_000_000, 200_000_000_000] {
        let (l, m, v) = (
            legacy_n.sample(time_ns),
            modern_n.sample(time_ns),
            vendor_n.sample(time_ns),
        );
        assert_eq!(l.speed_mps, v.speed_mps, "legacy speed at {time_ns}");
        assert!(
            (m.speed_mps.unwrap() - v.speed_mps.unwrap()).abs() < 1e-9,
            "modern speed at {time_ns}"
        );
        assert_eq!(l.lap_number, v.lap_number);
        assert_eq!(m.lap_number, v.lap_number);
        assert_eq!(l.lap_label, m.lap_label);
    }

    // The typed entry point reports which container it found, and header-only
    // helpers work for both.
    assert!(matches!(
        TelemetryRecording::open_unchanged(&legacy_path).unwrap(),
        TelemetryRecording::Native(_)
    ));
    assert!(matches!(
        TelemetryRecording::open_unchanged(&modern_path).unwrap(),
        TelemetryRecording::Jsonl(_)
    ));
    assert_eq!(
        motorsport_telemetry::read_valid_laps(&legacy_path).unwrap(),
        motorsport_telemetry::read_valid_laps(&modern_path).unwrap()
    );
    assert!(!motorsport_telemetry::telemetry_needs_update(&legacy_path).unwrap());
    assert!(!motorsport_telemetry::telemetry_needs_update(&modern_path).unwrap());
    assert!(motorsport_telemetry::read_format_version(&legacy_path).is_ok());
    assert!(motorsport_telemetry::read_format_version(&modern_path).is_err());

    // verify() accepts both and names the container it saw.
    let legacy_report = motorsport_telemetry::verify(&legacy_path).unwrap();
    let modern_report = motorsport_telemetry::verify(&modern_path).unwrap();
    assert_eq!(legacy_report.kind, motorsport_telemetry::VerifyKind::Native);
    assert_eq!(modern_report.kind, motorsport_telemetry::VerifyKind::Mtj);
    assert!(modern_report.compressed);
    assert_eq!(legacy_report.laps, modern_report.laps);
}

/// A source whose channels declare no unit (AiM CAN echoes, VBOX CAN columns)
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
        fn path(&self) -> &str {
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
