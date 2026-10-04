//! Acceptance checks against the private recordings that exposed parser bugs.
use motorsport_telemetry::{verify_with, TrackAuditOptions};
use std::path::PathBuf;

#[test]
#[ignore = "requires ROAD_ATLANTA_CORPUS pointing at the private 2026 weekend"]
fn every_reported_parser_case_remains_visible_in_verify() {
    let root = PathBuf::from(std::env::var_os("ROAD_ATLANTA_CORPUS").expect("ROAD_ATLANTA_CORPUS"));
    let options = TrackAuditOptions {
        track: Some("road-atlanta".into()),
        min_lap_s: Some(72.0),
        max_lap_s: Some(120.0),
        ..Default::default()
    };
    for (path, code) in [
        (
            "FP1/26IMSAR07_PLM_FP1_Run01_MB.MP4",
            "recovered-first-crossing",
        ),
        (
            "FP1/26IMSAR07_PLM_FP1_Run04_DHH.MP4",
            "ignored-pit-lane-beacon",
        ),
        (
            "FP2/26IMSAR07_PLM_FP2_Run06_TL.MP4",
            "recovered-timer-dropout",
        ),
        (
            "FP3/26IMSAR07_PLM_FP3_Run01_TL.MP4",
            "recovered-moving-pit-pass",
        ),
        (
            "Qualifying/26IMSAR07_PLM_Q_Run01_TL.MP4",
            "ignored-counter-rearm",
        ),
        ("Race/SCHD0295.MP4", "recovered-motion-departure"),
        ("Race/SCHD0301.MP4", "rejected-short-crossing"),
        ("Race/SCHD0304.MP4", "recovered-motion-departure"),
        ("Race/SCHD0306.MP4", "short-in-lap"),
    ] {
        let report =
            verify_with(root.join(path), &options).unwrap_or_else(|e| panic!("{path}: {e}"));
        let audit = report.track_audit.expect("native track audit");
        assert!(!audit.has_errors(), "{path}: {audit:?}");
        assert!(
            audit.findings.iter().any(|f| f.code == code),
            "{path}: missing {code}: {audit:?}"
        );
    }
}
