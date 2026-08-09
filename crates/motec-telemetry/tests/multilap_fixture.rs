use motec_telemetry::{infer_lap_markers, MotecFile};
use motorsport_telemetry_core::TelemetrySource;
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

#[test]
fn realistic_multilap_fixture_uses_authoritative_ldx_timing() {
    let file = MotecFile::open(fixture("synthetic_motec_multilap.ld")).unwrap();
    let metadata = file.metadata();

    assert_eq!(file.channels().len(), 7);
    assert!(metadata.sample_count > 30_000);
    assert_eq!(metadata.laps.len(), 12);
    assert_eq!(metadata.laps.iter().filter(|lap| lap.complete).count(), 10);
    assert!(!metadata.laps.first().unwrap().complete);
    assert!(!metadata.laps.last().unwrap().complete);
    assert_eq!(metadata.fastest_lap.as_ref().unwrap().number, 7);
    assert_eq!(
        metadata.fastest_lap.as_ref().unwrap().duration_ns,
        11_000_000_000
    );
}

#[test]
fn same_fixture_without_ldx_prefers_the_upward_counter_and_ignores_shutdown_reset() {
    let bytes = std::fs::read(fixture("synthetic_motec_multilap.ld")).unwrap();
    let file = MotecFile::from_bytes("synthetic-without-sidecar.ld", bytes).unwrap();
    let metadata = file.metadata();

    assert!(file.ldx.is_none());
    assert_eq!(metadata.laps.len(), 12);
    assert_eq!(metadata.laps.iter().filter(|lap| lap.complete).count(), 10);
    assert_eq!(metadata.laps.first().unwrap().number, 4);
    assert_eq!(metadata.laps.last().unwrap().number, 15);
    assert_eq!(metadata.laps.first().unwrap().end_ns, 12_500_000_000);
    assert_eq!(metadata.fastest_lap.as_ref().unwrap().number, 10);
    assert_eq!(
        metadata.fastest_lap.as_ref().unwrap().duration_ns,
        11_000_000_000
    );

    let inferred = infer_lap_markers(&file).unwrap();
    assert_eq!(inferred.source_channel, "Lap Count");
    assert_eq!(inferred.times_ns.len(), 11);
    assert_eq!(inferred.times_ns.last(), Some(&142_000_000_000));
}
