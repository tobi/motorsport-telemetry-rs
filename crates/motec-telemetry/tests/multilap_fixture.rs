use motec_telemetry::{infer_lap_markers, MotecFile};
use motorsport_telemetry_core::{LapKind, TelemetrySource};
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
fn same_fixture_without_ldx_prefers_the_upward_counter_and_splits_the_shutdown_reset_into_a_stint()
{
    let bytes = std::fs::read(fixture("synthetic_motec_multilap.ld")).unwrap();
    let file = MotecFile::from_bytes("synthetic-without-sidecar.ld", bytes).unwrap();
    let metadata = file.metadata();

    assert!(file.ldx.is_none());
    // The counter runs 4..=15, then the modelled shutdown drops it to 0 for a
    // second and restarts at 1. Under the stint model that drop is not
    // ignored: it ends stint 1 (lap 15 becomes its in-lap fragment) and
    // opens a second stint. The 0 -> 1 a second after the reset is the
    // restart arming its counter, not a beacon, so stint 2 is one out-in
    // fragment carrying count 1. Nothing there is flying, so the flying
    // count and the fastest lap are unchanged.
    assert_eq!(metadata.laps.len(), 13);
    assert_eq!(
        metadata
            .laps
            .iter()
            .filter(|lap| lap.kind.is_flying())
            .count(),
        10
    );
    assert_eq!(metadata.valid_laps, 10);
    // `number` is the virtual session lap; the vendor counter is `stint_lap`.
    assert_eq!(metadata.laps.first().unwrap().number, 1);
    assert_eq!(metadata.laps.first().unwrap().stint_lap, 4);
    assert_eq!(metadata.laps.first().unwrap().kind, LapKind::Out);
    assert_eq!(metadata.laps.first().unwrap().label(), "S1 out");
    assert_eq!(metadata.laps.first().unwrap().end_ns, 12_500_000_000);
    let stint_one_in = &metadata.laps[11];
    assert_eq!(
        (
            stint_one_in.stint,
            stint_one_in.stint_lap,
            stint_one_in.kind
        ),
        (1, 15, LapKind::In)
    );
    assert_eq!(stint_one_in.end_ns, 149_000_000_000); // total 151 s minus the 2 s reset tail
    let restart: Vec<(u32, i64, LapKind)> = metadata.laps[12..]
        .iter()
        .map(|lap| (lap.stint, lap.stint_lap, lap.kind))
        .collect();
    assert_eq!(restart, vec![(2, 1, LapKind::OutIn)]);
    assert_eq!(metadata.laps.last().unwrap().number, 13);
    assert_eq!(metadata.laps.last().unwrap().label(), "S2 out-in");
    let fastest = metadata.fastest_lap.as_ref().unwrap();
    assert_eq!(
        (fastest.number, fastest.stint_lap, fastest.label()),
        (7, 10, "S1 L10".into())
    );
    assert_eq!(fastest.duration_ns, 11_000_000_000);

    let inferred = infer_lap_markers(&file).unwrap();
    assert_eq!(inferred.source_channel, "Lap Count");
    assert_eq!(inferred.times_ns.len(), 11);
    assert_eq!(inferred.times_ns.last(), Some(&142_000_000_000));
}
