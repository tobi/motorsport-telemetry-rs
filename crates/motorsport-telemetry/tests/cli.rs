use std::path::PathBuf;
use std::process::Command;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_motorsport-telemetry"))
}

#[test]
fn reports_requested_metadata() {
    let output = cli()
        .args(["inspect", fixture("synthetic_aimd.mp4").to_str().unwrap()])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("driver_id: 3\n"));
    assert!(stdout.contains("event_date: 2026-08-01\n"));
    assert!(stdout.contains("event_date_source: gps_clock\n"));
    assert!(stdout.contains("laps: 1\n"));
    assert!(stdout.contains("video_included: true\n"));
    assert!(stdout.contains("video_filenames: synthetic_aimd.mp4\n"));
    assert!(stdout.contains("video_presentation_offset_ns: 104000000\n"));
    assert!(stdout.contains("part_of_larger_session: unknown (single-file inspection)\n"));
    assert!(stdout.contains("track_name: Road America\n"));
    assert!(stdout.contains("layout: Full Course\n"));
    assert!(stdout.contains("track_length: 6514 m\n"));
}

#[test]
fn emits_machine_readable_json() {
    let output = cli()
        .args([
            "inspect",
            "--json",
            fixture("synthetic_aimd.mp4").to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["driver_id"], 3);
    assert_eq!(report["event_date"], "2026-08-01");
    assert_eq!(report["event_date_source"], "gps_clock");
    assert_eq!(report["laps"], 1);
    assert_eq!(report["video_included"], true);
    assert_eq!(report["video_presentation_offset_ns"], 104_000_000);
    assert_eq!(report["track_name"], "Road America");
    assert_eq!(report["layout"], "Full Course");
    assert_eq!(report["track_length_m"], 6514.0);
    assert!(report["part_of_larger_session"].is_null());
}

#[test]
fn recognizes_decimal_degree_vbox_exports() {
    let output = cli()
        .args(["inspect", fixture("synthetic_vbo.vbo").to_str().unwrap()])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("track_name: Road America\n"));
    assert!(stdout.contains("layout: Full Course\n"));
    assert!(stdout.contains("track_length: 6514 m\n"));
}

#[test]
fn convert_defaults_to_native_and_verify_accepts_all_encodings() {
    let dir = tempfile::tempdir().unwrap();
    let input = fixture("synthetic_cosworth.pds");
    let native = dir.path().join("run.telemetry");
    let jsonl = dir.path().join("run.telemetry.jsonl");
    let zstd = dir.path().join("run.telemetry.jsonl.zstd");

    for dest in [native.as_path(), jsonl.as_path(), zstd.as_path()] {
        let out = cli()
            .args(["convert", input.to_str().unwrap(), dest.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(out.status.success(), "{dest:?} {:?}", out);
    }

    let verified = cli()
        .args([
            "verify",
            native.to_str().unwrap(),
            jsonl.to_str().unwrap(),
            zstd.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(verified.status.success(), "{:?}", verified);
    let stdout = String::from_utf8(verified.stdout).unwrap();
    assert!(stdout.contains("native v"), "{stdout}");
    assert!(stdout.contains("mtj:1"), "{stdout}");
    assert!(stdout.contains("zstd"), "{stdout}");
    assert!(!stdout.contains("FAIL"), "{stdout}");

    let rejected = cli()
        .args(["verify", input.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    let stderr = String::from_utf8(rejected.stderr).unwrap();
    assert!(stderr.contains("FAIL"), "{stderr}");
}

#[test]
fn inspect_folder_honors_mask() {
    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("weekend").join("car-1");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::copy(fixture("synthetic_cosworth.pds"), nested.join("run.pds")).unwrap();
    std::fs::copy(fixture("synthetic_vbo.vbo"), nested.join("run.vbo")).unwrap();
    std::fs::write(nested.join("notes.txt"), "ignore").unwrap();

    let masked = cli()
        .args([
            "inspect",
            "--json",
            "--mask",
            "**/*.pds",
            dir.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(masked.status.success(), "{:?}", masked);
    let report: serde_json::Value = serde_json::from_slice(&masked.stdout).unwrap();
    assert_eq!(report["ok"], 1);
    assert_eq!(report["failed"], 0);
    assert_eq!(report["files"].as_array().unwrap().len(), 1);
    let file = report["files"][0]["file"].as_str().unwrap();
    assert!(file.ends_with("run.pds"), "{file}");

    let help = cli().args(["inspect", "--help"]).output().unwrap();
    assert!(help.status.success());
    let text = String::from_utf8(help.stdout).unwrap();
    assert!(text.contains("--mask"));
    assert!(text.contains("folder"));
}
