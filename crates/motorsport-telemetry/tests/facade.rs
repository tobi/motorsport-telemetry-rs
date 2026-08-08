use motorsport_telemetry::{motorsport_telemetry_core::TelemetrySource, open, open_sessions};
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
    let track = file.match_track().unwrap();
    assert_eq!(track.matched.track.slug, "road-america");
    let roles = file.signal_roles();
    let sample = file.normalized_sample(0, &roles, Some(&track));
    assert!(sample.latitude_deg.is_some());
    assert!(sample.longitude_deg.is_some());
    assert!(sample.lap_progress.is_some());
}
