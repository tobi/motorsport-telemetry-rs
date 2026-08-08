use std::path::PathBuf;
use std::process::Command;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

#[test]
fn reports_requested_metadata() {
    let output = Command::new(env!("CARGO_BIN_EXE_motorsport-telemetry"))
        .arg(fixture("synthetic_aimd.mp4"))
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("driver_id: 3\n"));
    assert!(stdout.contains("event_date: 2026-08-01\n"));
    assert!(stdout.contains("event_date_source: gps_clock\n"));
    assert!(stdout.contains("laps: 1\n"));
    assert!(stdout.contains("video_included: true\n"));
    assert!(stdout.contains("video_filenames: synthetic_aimd.mp4\n"));
    assert!(stdout.contains("part_of_larger_session: unknown (single-file inspection)\n"));
    assert!(stdout.contains("track_name: Road America\n"));
    assert!(stdout.contains("layout: Full Course\n"));
    assert!(stdout.contains("track_length: 6514 m\n"));
}

#[test]
fn emits_machine_readable_json() {
    let output = Command::new(env!("CARGO_BIN_EXE_motorsport-telemetry"))
        .args(["--json", fixture("synthetic_aimd.mp4").to_str().unwrap()])
        .output()
        .unwrap();
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["driver_id"], 3);
    assert_eq!(report["event_date"], "2026-08-01");
    assert_eq!(report["event_date_source"], "gps_clock");
    assert_eq!(report["laps"], 1);
    assert_eq!(report["video_included"], true);
    assert_eq!(report["track_name"], "Road America");
    assert_eq!(report["layout"], "Full Course");
    assert_eq!(report["track_length_m"], 6514.0);
    assert!(report["part_of_larger_session"].is_null());
}

#[test]
fn recognizes_decimal_degree_vbox_exports() {
    let output = Command::new(env!("CARGO_BIN_EXE_motorsport-telemetry"))
        .arg(fixture("synthetic_vbo.vbo"))
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("track_name: Road America\n"));
    assert!(stdout.contains("layout: Full Course\n"));
    assert!(stdout.contains("track_length: 6514 m\n"));
}
