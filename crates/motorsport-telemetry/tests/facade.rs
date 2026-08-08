use motorsport_telemetry::{
    motorsport_telemetry_core::TelemetrySource, open, open_sessions, TelemetryNormalizer,
};
use std::path::PathBuf;

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
    assert_eq!(position.video.frame_index, Some(0));
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
fn reusable_normalizer_uses_lap_metadata_fallback() {
    let file = open(fixture("synthetic_cosworth.pds")).unwrap();
    let normalizer = TelemetryNormalizer::new(&file, file.signal_roles(), None);

    assert_eq!(normalizer.sample(1_000_000_000).lap_progress, Some(0.25));
    assert_eq!(normalizer.sample(2_000_000_000).lap_progress, Some(0.5));
}
