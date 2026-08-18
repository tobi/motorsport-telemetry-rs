use motorsport_telemetry::motorsport_telemetry_core::{
    Channel, Chunk, SampleType, SourceIdentity, TelemetrySource, UnitSource,
};
use std::path::PathBuf;
use std::process::Command;
use telemetry_format::write_from_source;

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
fn inspect_pds_reports_flying_laps() {
    let output = cli()
        .args([
            "inspect",
            "--json",
            fixture("synthetic_cosworth.pds").to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["format"], "pds");
    assert_eq!(report["driver_id"], 7);
    assert_eq!(report["laps"], 5);
    assert_eq!(report["complete_laps"], 3);
    assert_eq!(report["fastest_lap_number"], 2);
    let fastest = report["fastest_lap"].as_str().unwrap();
    assert!(fastest.contains(':'), "{fastest}");
    assert_ne!(fastest, "unknown");
    assert_eq!(report["track_name"], "Road America");
}

#[test]
fn command_help_is_specific() {
    let root = String::from_utf8(cli().arg("--help").output().unwrap().stdout).unwrap();
    assert!(root.contains("inspect"));
    assert!(root.contains("convert"));
    assert!(root.contains("verify"));

    let inspect =
        String::from_utf8(cli().args(["help", "inspect"]).output().unwrap().stdout).unwrap();
    assert!(inspect.contains("--mask"));
    assert!(inspect.contains("folder"));

    let convert =
        String::from_utf8(cli().args(["convert", "--help"]).output().unwrap().stdout).unwrap();
    assert!(convert.contains(".telemetry.jsonl"));
    assert!(convert.contains("Default"));

    let verify =
        String::from_utf8(cli().args(["verify", "--help"]).output().unwrap().stdout).unwrap();
    assert!(verify.contains("zstd"));
    assert!(verify.contains("without rewriting"));
}

#[test]
fn convert_without_output_writes_next_to_the_input() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("run.pds");
    std::fs::copy(fixture("synthetic_cosworth.pds"), &input).unwrap();
    let output = cli()
        .args(["convert", input.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    let dest = String::from_utf8(output.stdout).unwrap();
    assert!(dest.contains("run.pds.telemetry"), "{dest}");
    let dest = dest.trim();
    assert!(std::path::Path::new(dest).is_file());

    let verified = cli().args(["verify", dest]).output().unwrap();
    assert!(verified.status.success(), "{:?}", verified);
    let report = String::from_utf8(verified.stdout).unwrap();
    assert!(report.contains("native v"), "{report}");
    assert!(report.contains("ok"), "{report}");
}

#[test]
fn inspect_reports_when_a_folder_mask_matches_nothing() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("notes.txt"), "ignore").unwrap();
    let output = cli()
        .args([
            "inspect",
            "--mask",
            "**/*.pds",
            dir.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("no telemetry files"), "{stderr}");
}

#[test]
fn verify_rejects_garbage() {
    let dir = tempfile::tempdir().unwrap();
    let junk = dir.path().join("fake.telemetry");
    std::fs::write(&junk, b"not a zip").unwrap();
    let output = cli()
        .args(["verify", junk.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
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

#[test]
fn inspect_prints_diagnostics_none_for_a_clean_fixture() {
    let output = cli()
        .args(["inspect", fixture("synthetic_aimd.mp4").to_str().unwrap()])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("diagnostics: none\n"),
        "expected a clean diagnostics section, got:\n{stdout}"
    );
}

#[test]
fn json_inspect_carries_a_diagnostics_array() {
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
    assert!(
        report["diagnostics"].is_array(),
        "expected a diagnostics array, got: {}",
        report["diagnostics"]
    );
    // A clean fixture has an empty array, with the documented field shape.
    assert_eq!(report["diagnostics"].as_array().unwrap().len(), 0);
}

/// A source with widespread absurd values, mimicking a source decoded with the
/// wrong sample layout while remaining structurally safe to inspect.
struct ImplausibleSource {
    channels: Vec<Channel>,
    packed: Vec<Vec<u8>>,
}

impl TelemetrySource for ImplausibleSource {
    fn path(&self) -> &str {
        "implausible"
    }
    fn format(&self) -> &'static str {
        "pds"
    }
    fn channels(&self) -> &[Channel] {
        &self.channels
    }
    fn decode(&self, _: usize, _: usize, _: u64) -> f64 {
        f64::MAX
    }
    fn chunk_bytes(&self, channel_index: usize, _chunk_index: usize) -> Option<&[u8]> {
        self.packed.get(channel_index).map(Vec::as_slice)
    }
    fn identity(&self) -> SourceIdentity {
        SourceIdentity {
            driver: "Stub".into(),
            venue: "Stub Track".into(),
            ..SourceIdentity::default()
        }
    }
}

fn claimed_float64_channel(id: u32, name: &str, count: u64) -> Channel {
    Channel {
        id,
        name: name.into(),
        unit: "m/s".into(),
        unit_source: UnitSource::Declared,
        sample_type: SampleType::F64,
        chunks: vec![Chunk {
            sample_period_ns: 1_000_000,
            sample_count: count,
            data_ptr: 0,
            sample_base: 0,
            time_base_ns: 0,
        }],
        sample_count: count,
        duration_ns: count.saturating_mul(1_000_000),
    }
}

#[test]
fn verify_fails_on_widespread_absurd_values() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("bad.telemetry");
    let count = 32u64;
    let packed_channel = || {
        (0..count)
            .flat_map(|_| f64::MAX.to_le_bytes())
            .collect::<Vec<_>>()
    };
    let source = ImplausibleSource {
        channels: (0..5)
            .map(|index| claimed_float64_channel(index, &format!("Implausible {index}"), count))
            .collect(),
        packed: (0..5).map(|_| packed_channel()).collect(),
    };
    write_from_source(&source, &dest).unwrap();

    let output = cli()
        .args(["verify", dest.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!output.status.success(), "verify must fail: {:?}", output);
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("FAIL"), "{stderr}");
    assert!(stderr.contains("decode fault"), "{stderr}");
    assert!(
        stderr.contains("value.widespread_absurd_magnitude"),
        "{stderr}"
    );
}

#[test]
fn verify_help_describes_decode_fault_exit_behavior() {
    let text =
        String::from_utf8(cli().args(["verify", "--help"]).output().unwrap().stdout).unwrap();
    assert!(text.contains("decode fault"), "{text}");
    assert!(text.contains("Plain warnings do not fail"), "{text}");
    assert!(text.contains("Exit status is 1"), "{text}");

    let inspect =
        String::from_utf8(cli().args(["inspect", "--help"]).output().unwrap().stdout).unwrap();
    assert!(inspect.contains("diagnostics"), "{inspect}");
}
