//! Behavior and losslessness tests on a synthetic source.

use motorsport_telemetry_core::{Channel, Chunk, SampleType, TelemetrySource, UnitSource};
use telemetry_passes::{apply_registry, PassOutcome};

const PERIOD_NS: u64 = 100_000_000; // 10 Hz
const SAMPLES: u64 = 100;

struct Synthetic {
    channels: Vec<Channel>,
    data: Vec<Vec<u8>>,
}

fn f64_channel(id: u32, name: &str, unit: &str, values: &[f64]) -> (Channel, Vec<u8>) {
    let mut data = Vec::with_capacity(values.len() * 8);
    for value in values {
        data.extend_from_slice(&value.to_le_bytes());
    }
    (
        Channel {
            id,
            name: name.to_owned(),
            unit: unit.to_owned(),
            unit_source: if unit.is_empty() {
                UnitSource::Unknown
            } else {
                UnitSource::Declared
            },
            sample_type: SampleType::F64,
            chunks: vec![Chunk {
                sample_period_ns: PERIOD_NS,
                sample_count: values.len() as u64,
                data_ptr: 0,
                sample_base: 0,
                time_base_ns: 0,
            }],
            sample_count: values.len() as u64,
            duration_ns: values.len() as u64 * PERIOD_NS,
        },
        data,
    )
}

impl TelemetrySource for Synthetic {
    fn path(&self) -> &str {
        "session.test"
    }
    fn format(&self) -> &'static str {
        "test"
    }
    fn channels(&self) -> &[Channel] {
        &self.channels
    }
    fn decode(&self, channel_index: usize, chunk_index: usize, local_index: u64) -> f64 {
        assert_eq!(chunk_index, 0);
        let at = local_index as usize * 8;
        f64::from_le_bytes(self.data[channel_index][at..at + 8].try_into().unwrap())
    }
    fn chunk_bytes(&self, channel_index: usize, _chunk_index: usize) -> Option<&[u8]> {
        Some(&self.data[channel_index])
    }
}

/// 100 samples at 10 Hz. Samples 10..=14 have no solution (2 satellites,
/// null-island coordinates). Sample 50 is a teleport (+0.1 deg in 100 ms).
fn synthetic() -> Synthetic {
    let mut latitude = Vec::new();
    let mut longitude = Vec::new();
    let mut satellites = Vec::new();
    let mut speed = Vec::new();
    for index in 0..SAMPLES {
        let ramp = index as f64 * 1e-5;
        let no_solution = (10..=14).contains(&index);
        let teleport = index == 50;
        latitude.push(if no_solution {
            0.0
        } else if teleport {
            43.797 + ramp + 0.1
        } else {
            43.797 + ramp
        });
        longitude.push(if no_solution { 0.0 } else { -87.990 + ramp });
        satellites.push(if no_solution { 2.0 } else { 9.0 });
        speed.push(40.0);
    }
    let specs = [
        f64_channel(1, "GPS Latitude", "deg", &latitude),
        f64_channel(2, "GPS Longitude", "deg", &longitude),
        f64_channel(3, "GPS Satellites", "", &satellites),
        f64_channel(4, "GPS Speed", "m/s", &speed),
    ];
    let mut channels = Vec::new();
    let mut data = Vec::new();
    for (channel, bytes) in specs {
        channels.push(channel);
        data.push(bytes);
    }
    Synthetic { channels, data }
}

fn channel_index(source: &dyn TelemetrySource, name: &str) -> usize {
    source
        .channels()
        .iter()
        .position(|channel| channel.name == name)
        .unwrap_or_else(|| panic!("channel {name:?} missing"))
}

#[test]
fn registry_applies_all_three_passes() {
    let source = synthetic();
    let (passed, reports) = apply_registry(&source).unwrap();
    assert_eq!(reports.len(), 3);
    for report in &reports {
        assert!(
            matches!(report.outcome, PassOutcome::Applied { .. }),
            "{} unexpectedly skipped: {:?}",
            report.label(),
            report.outcome
        );
    }
    assert_eq!(passed.channels().len(), 4 + 6);
    assert_eq!(passed.applied_passes().len(), 3);

    // gps.quality: solution loss is flagged invalid, sigma NaN there.
    let valid = channel_index(&passed, "GPS Fix Valid");
    let sigma = channel_index(&passed, "GPS Position Sigma");
    assert_eq!(passed.decode(valid, 0, 12), 0.0);
    assert_eq!(passed.decode(valid, 0, 30), 1.0);
    assert!(passed.decode(sigma, 0, 12).is_nan());
    // No accuracy/DOP channel: default sigma.
    assert_eq!(passed.decode(sigma, 0, 30), 15.0);

    // gps.clean: invalid fixes and the teleport are masked, the rest passes
    // through exactly.
    let clean_lat = channel_index(&passed, "GPS Latitude Clean");
    let clean_lon = channel_index(&passed, "GPS Longitude Clean");
    assert!(passed.decode(clean_lat, 0, 12).is_nan());
    assert!(passed.decode(clean_lat, 0, 50).is_nan());
    assert!(passed.decode(clean_lon, 0, 50).is_nan());
    assert_eq!(passed.decode(clean_lat, 0, 51), 43.797 + 51.0 * 1e-5);
    let raw_lat = channel_index(&passed, "GPS Latitude");
    assert_eq!(
        passed.decode(clean_lat, 0, 30),
        passed.decode(raw_lat, 0, 30)
    );

    // speed.distance: monotone odometer, 40 m/s for 9.9 s.
    let odometer = channel_index(&passed, "Distance Odometer");
    let odometer_sigma = channel_index(&passed, "Distance Odometer Sigma");
    assert_eq!(passed.decode(odometer, 0, 0), 0.0);
    let total = passed.decode(odometer, 0, SAMPLES - 1);
    assert!((total - 396.0).abs() < 1e-6, "total {total}");
    for index in 1..SAMPLES {
        assert!(passed.decode(odometer, 0, index) >= passed.decode(odometer, 0, index - 1));
    }
    let sigma_end = passed.decode(odometer_sigma, 0, SAMPLES - 1);
    assert!(
        (sigma_end - 0.005 * total).abs() < 1e-3,
        "sigma {sigma_end}"
    );

    // Provenance records inputs/outputs/params.
    let quality = &passed.applied_passes()[0];
    assert_eq!(quality.name, "gps.quality");
    assert_eq!(quality.version, 1);
    assert_eq!(
        quality.inputs,
        vec!["GPS Latitude", "GPS Longitude", "GPS Satellites"]
    );
    assert_eq!(quality.outputs, vec!["GPS Fix Valid", "GPS Position Sigma"]);
    assert!(quality
        .params
        .iter()
        .any(|(key, value)| key == "min_satellites" && value == "4"));
}

#[test]
fn arc_minute_coordinates_skip_gps_passes() {
    let mut source = synthetic();
    // Same fixes expressed in arc-minutes (degrees * 60), unit lost.
    for channel_index in [0usize, 1] {
        let mut values = Vec::new();
        for local in 0..SAMPLES {
            values.push(source.decode(channel_index, 0, local) * 60.0);
        }
        let (channel, data) = f64_channel(
            source.channels[channel_index].id,
            &source.channels[channel_index].name.clone(),
            "",
            &values,
        );
        source.channels[channel_index] = channel;
        source.data[channel_index] = data;
    }
    let (_, reports) = apply_registry(&source).unwrap();
    for name in ["gps.quality", "gps.clean"] {
        let report = reports.iter().find(|report| report.name == name).unwrap();
        match &report.outcome {
            PassOutcome::Skipped { reason } => {
                assert!(reason.contains("decode-level normalization"), "{reason}")
            }
            other => panic!("{name} should skip on arc-minutes: {other:?}"),
        }
    }
    // The odometer does not care about coordinates.
    let distance = reports
        .iter()
        .find(|report| report.name == "speed.distance")
        .unwrap();
    assert!(matches!(distance.outcome, PassOutcome::Applied { .. }));
}

#[test]
fn missing_gps_skips_with_named_reasons() {
    let full = synthetic();
    let source = Synthetic {
        channels: vec![full.channels[3].clone()],
        data: vec![full.data[3].clone()],
    };
    let (passed, reports) = apply_registry(&source).unwrap();
    assert_eq!(passed.appended_len(), 2); // odometer + sigma only
    for name in ["gps.quality", "gps.clean"] {
        let report = reports.iter().find(|report| report.name == name).unwrap();
        assert_eq!(
            report.outcome,
            PassOutcome::Skipped {
                reason: "no GPS coordinate channels present".to_owned()
            }
        );
    }
}

#[test]
fn unitless_speed_skips_distance() {
    let full = synthetic();
    let mut speed = full.channels[3].clone();
    speed.unit = String::new();
    speed.unit_source = UnitSource::Unknown;
    let source = Synthetic {
        channels: vec![speed],
        data: vec![full.data[3].clone()],
    };
    let (_, reports) = apply_registry(&source).unwrap();
    let report = reports
        .iter()
        .find(|report| report.name == "speed.distance")
        .unwrap();
    match &report.outcome {
        PassOutcome::Skipped { reason } => {
            assert!(reason.contains("no declared unit"), "{reason}")
        }
        other => panic!("expected skip: {other:?}"),
    }
}

#[test]
fn passes_persist_and_strip_back_to_identical_bytes() {
    let source = synthetic();
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("raw.telemetry");
    let derived = dir.path().join("derived.telemetry");
    let stripped = dir.path().join("stripped.telemetry");

    telemetry_format::write_from_source(&source, &raw).unwrap();

    let (passed, _) = apply_registry(&source).unwrap();
    telemetry_format::write_from_source(&passed, &derived).unwrap();

    let recording = telemetry_format::NativeRecording::open(&derived).unwrap();
    // Provenance and origin survive the write/open round trip.
    assert_eq!(recording.passes().len(), 3);
    assert_eq!(recording.passes()[1].name, "gps.clean");
    assert_eq!(recording.passes()[1].version, 1);
    assert_eq!(
        recording.passes()[1].params,
        vec![
            ("max_speed_mps".to_owned(), "150".to_owned()),
            ("reanchor_after".to_owned(), "8".to_owned()),
        ]
    );
    let metadata = recording.metadata();
    assert_eq!(metadata.source_format, "test");
    assert_eq!(metadata.source_path, "session.test");
    assert_eq!(metadata.passes.len(), 3);
    assert_eq!(recording.channels().len(), 10);

    // Derived values survive: odometer still monotone and ends at 396 m.
    let odometer = channel_index(&recording, "Distance Odometer");
    let total = recording.decode(odometer, 0, SAMPLES - 1);
    assert!((total - 396.0).abs() < 1e-6, "total {total}");

    // Re-running the registry on the recording skips everything.
    let (_, reports) = apply_registry(&recording).unwrap();
    for report in &reports {
        assert_eq!(
            report.outcome,
            PassOutcome::Skipped {
                reason: "already applied (recorded in source)".to_owned()
            },
            "{}",
            report.label()
        );
    }

    // Strip: byte-identical to the raw conversion.
    telemetry_format::write_from_source_stripped(&recording, &stripped).unwrap();
    let raw_bytes = std::fs::read(&raw).unwrap();
    let stripped_bytes = std::fs::read(&stripped).unwrap();
    assert_eq!(
        raw_bytes, stripped_bytes,
        "strip must recover the raw conversion"
    );
}

#[test]
fn jsonl_header_round_trips_provenance() {
    let source = synthetic();
    let (passed, _) = apply_registry(&source).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("derived.mtj");
    telemetry_format::write_jsonl_from_source(&passed, &path).unwrap();

    let recording = telemetry_format::JsonlRecording::open(&path).unwrap();
    let metadata = recording.metadata();
    assert_eq!(metadata.source_format, "test");
    assert_eq!(metadata.source_path, "session.test");
    assert_eq!(metadata.passes.len(), 3);
    assert_eq!(metadata.passes[1].name, "gps.clean");
    assert_eq!(metadata.passes[1].version, 1);
    assert_eq!(
        metadata.passes[1].params,
        vec![
            ("max_speed_mps".to_owned(), "150".to_owned()),
            ("reanchor_after".to_owned(), "8".to_owned()),
        ]
    );
    assert_eq!(
        metadata.passes[2].outputs,
        vec!["Distance Odometer", "Distance Odometer Sigma"]
    );

    // A second conversion hop keeps the original identity.
    let hop = dir.path().join("hop.telemetry");
    telemetry_format::write_from_source(&recording, &hop).unwrap();
    let reopened = telemetry_format::NativeRecording::open(&hop).unwrap();
    let hop_metadata = reopened.metadata();
    assert_eq!(hop_metadata.source_format, "test");
    assert_eq!(hop_metadata.source_path, "session.test");
    assert_eq!(hop_metadata.passes.len(), 3);
}
