#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::unreadable_literal,
    clippy::float_cmp,
    clippy::format_push_string,
    reason = "test and example code: fail loudly, print freely, exact fixture values"
)]

use motorsport_telemetry::motorsport_telemetry_core::{
    Channel, Chunk, SampleType, SourceIdentity, TelemetrySource, UnitSource,
};
use std::path::{Path, PathBuf};
use std::process::Command;
use telemetry_format::write_telemetry;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_motorsport-telemetry"))
}

fn inspect_json(path: &Path, options: &[&str]) -> serde_json::Value {
    let output = cli()
        .args(["inspect", "--json"])
        .args(options)
        .arg(path)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn adjacent_track_yml_overrides_inferred_inspection_fields() {
    let dir = tempfile::tempdir().unwrap();
    let metadata = "\
track:
  name: Club Circuit
  layout: Short Loop
  length_m: 1234
car:
  name: Club Car
  number: 27
  class: Touring
driver:
  name: Test Driver
event: Club Weekend
session: Qualifying
date: '2001-02-03'
time: '09:30:00'
notes:
  conditions: dry
  tags: [practice, test]
";
    std::fs::write(dir.path().join("TRACK.yml"), metadata).unwrap();
    // VBOX inspection uses the metadata-oriented loader; MP4 uses the full loader.
    for name in ["synthetic_aimd.mp4", "synthetic_vbo.vbo"] {
        let input = dir.path().join(name);
        std::fs::copy(fixture(name), &input).unwrap();
        let report = inspect_json(&input, &[]);
        assert_eq!(report["track_name"], "Club Circuit");
        assert_eq!(report["layout"], "Short Loop");
        assert_eq!(report["track_length_m"], 1234.0);
        assert_eq!(report["car_type"], "Club Car");
        assert_eq!(report["car_number"], "27");
        assert_eq!(report["car_class"], "Touring");
        assert_eq!(report["driver"], "Test Driver");
        assert_eq!(report["event"], "Club Weekend");
        assert_eq!(report["session"], "Qualifying");
        assert_eq!(report["date"], "2001-02-03");
        assert_eq!(report["time"], "09:30:00");
        assert_eq!(report["event_date"], "2001-02-03");
        assert_eq!(report["event_date_source"], "date");
        assert!(report["event_date_warning"].is_null());
        assert_eq!(
            report["extra"]["notes"]["tags"],
            serde_json::json!(["practice", "test"])
        );
    }

    std::fs::write(
        dir.path().join("TRACK.yml"),
        format!("{metadata}archive:\n  event_date: '1999-04-05'\n"),
    )
    .unwrap();
    let input = dir.path().join("synthetic_aimd.mp4");
    let report = inspect_json(&input, &[]);
    let ignored = inspect_json(&input, &["--ignore-track-yml"]);
    assert_eq!(report["event_date"], "1999-04-05");
    assert_eq!(report["event_date_source"], "archive.event_date");
    assert_eq!(report["date"], "2001-02-03");
    assert_eq!(report["session_key"], ignored["session_key"]);
    assert_eq!(report["lap_table"], ignored["lap_table"]);
    assert_eq!(ignored["track_name"], "Road America");
    assert_eq!(ignored["event_date"], "2026-08-01");
    assert_eq!(ignored["extra"], serde_json::json!({}));

    let human = cli().arg("inspect").arg(&input).output().unwrap();
    assert!(human.status.success(), "{human:?}");
    let text = String::from_utf8(human.stdout).unwrap();
    assert!(text.contains("track_name: Club Circuit\n"), "{text}");
    assert!(text.contains("event: Club Weekend\n"), "{text}");
    assert!(text.contains("session: Qualifying\n"), "{text}");
    assert!(text.contains("event_date: 1999-04-05\n"), "{text}");
}

#[test]
fn explicit_null_metadata_masks_inferred_inspection_fields() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("run.mp4");
    std::fs::copy(fixture("synthetic_aimd.mp4"), &input).unwrap();
    std::fs::write(
        dir.path().join("TRACK.yml"),
        "track: null\ncar: null\ndriver: null\nevent: null\nsession: null\ndate: null\ntime: null\n",
    )
    .unwrap();
    let report = inspect_json(&input, &[]);
    for key in [
        "track_name",
        "layout",
        "track_length_m",
        "car_type",
        "car_number",
        "car_class",
        "driver",
        "event",
        "session",
        "date",
        "time",
        "event_date",
    ] {
        assert!(report[key].is_null(), "{key}: {}", report[key]);
    }
    assert_eq!(report["event_date_source"], "date");
}

#[test]
fn folder_mask_loads_metadata_from_explicit_root_through_each_leaf() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("archive");
    let leaf = root.join("weekend/car-1");
    std::fs::create_dir_all(&leaf).unwrap();
    std::fs::write(dir.path().join("TRACK.yml"), "outside_root: true\n").unwrap();
    std::fs::write(
        root.join("TRACK.yml"),
        "\
track:
  name: Root Circuit
archive:
  owner: Team
  event_date: '2001-01-01'
overrides:
  - match: 'weekend/**/*.pds'
    metadata:
      session: PDS Session
  - match: 'weekend/**/*.vbo'
    metadata:
      session: VBOX Session
",
    )
    .unwrap();
    std::fs::write(
        root.join("weekend/TRACK.yml"),
        "event: Weekend\narchive:\n  event_date: '2002-02-02'\n",
    )
    .unwrap();
    std::fs::write(leaf.join("TRACK.yml"), "car:\n  name: Leaf Car\n").unwrap();
    for name in ["synthetic_cosworth.pds", "synthetic_vbo.vbo"] {
        std::fs::copy(fixture(name), leaf.join(name)).unwrap();
    }

    let adjacent_only = inspect_json(&root, &["--mask", "**/*.pds"]);
    let adjacent = &adjacent_only["files"][0];
    assert_eq!(adjacent["car_type"], "Leaf Car");
    assert!(adjacent["extra"]["archive"].is_null());
    assert_eq!(adjacent["track_name"], "Road America");

    let inherited = inspect_json(
        &root,
        &[
            "--mask",
            "**/*.{pds,vbo}",
            "--root-path",
            root.to_str().unwrap(),
        ],
    );
    assert_eq!(inherited["ok"], 2);
    assert_eq!(inherited["failed"], 0);
    for report in inherited["files"].as_array().unwrap() {
        assert_eq!(report["track_name"], "Root Circuit");
        assert_eq!(report["car_type"], "Leaf Car");
        assert_eq!(report["event"], "Weekend");
        assert_eq!(report["event_date"], "2002-02-02");
        assert_eq!(report["extra"]["archive"]["owner"], "Team");
        assert!(report["extra"]["outside_root"].is_null());
        let expected_session = if report["format"] == "pds" {
            "PDS Session"
        } else {
            "VBOX Session"
        };
        assert_eq!(report["session"], expected_session);
        assert!(report["extra"]["overrides"].is_null());
    }
}

#[test]
fn ignore_track_yml_skips_malformed_documents_for_inspect_and_convert() {
    let dir = tempfile::tempdir().unwrap();
    let leaf = dir.path().join("logs");
    std::fs::create_dir(&leaf).unwrap();
    let input = leaf.join("run.mp4");
    std::fs::copy(fixture("synthetic_aimd.mp4"), &input).unwrap();
    for folder in [dir.path(), leaf.as_path()] {
        std::fs::write(folder.join("TRACK.yml"), "track: [broken\n").unwrap();
    }
    let report = inspect_json(
        &input,
        &[
            "--root-path",
            dir.path().to_str().unwrap(),
            "--ignore-track-yml",
        ],
    );
    assert_eq!(report["track_name"], "Road America");
    assert_eq!(report["extra"], serde_json::json!({}));
    let dest = dir.path().join("run.telemetry");
    let converted = cli()
        .args([
            "convert",
            "--no-passes",
            "--ignore-track-yml",
            "--root-path",
        ])
        .arg(dir.path())
        .arg(&input)
        .arg(&dest)
        .output()
        .unwrap();
    assert!(converted.status.success(), "{converted:?}");
    let stored = telemetry_format::read_metadata(&dest).unwrap();
    assert!(stored.extra.is_empty());

    // Verification concerns stored content, even beside an invalid TRACK.yml.
    let verified = cli().arg("verify").arg(&dest).output().unwrap();
    assert!(verified.status.success(), "{verified:?}");
}

#[test]
fn explicit_root_is_validated_even_when_track_yml_is_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("run.mp4");
    std::fs::copy(fixture("synthetic_aimd.mp4"), &input).unwrap();
    let unrelated = dir.path().join("unrelated");
    std::fs::create_dir(&unrelated).unwrap();
    let missing = dir.path().join("missing");
    let dest = dir.path().join("output.telemetry");
    for root in [&unrelated, &missing, &input] {
        for ignore in [false, true] {
            for command in ["inspect", "convert"] {
                let mut invocation = cli();
                invocation.arg(command).arg("--root-path").arg(root);
                if ignore {
                    invocation.arg("--ignore-track-yml");
                }
                invocation.arg(&input);
                if command == "convert" {
                    invocation.arg(&dest);
                }
                let output = invocation.output().unwrap();
                assert_eq!(
                    output.status.code(),
                    Some(1),
                    "{command}: {root:?}: {output:?}"
                );
                assert_ne!(output.stderr, [] as [u8; 0]);
                assert!(!dest.exists());
            }
        }
    }
}

#[test]
fn conversion_persists_effective_metadata_without_changing_clocks() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("archive");
    let leaf = root.join("logs");
    std::fs::create_dir_all(&leaf).unwrap();
    let input = leaf.join("run.mp4");
    std::fs::copy(fixture("synthetic_aimd.mp4"), &input).unwrap();
    let original = motorsport_telemetry::read_metadata(&input).unwrap();
    std::fs::write(
        root.join("TRACK.yml"),
        "track:\n  name: Archive Circuit\narchive:\n  event_date: '1999-01-01'\n  owner: Team\n",
    )
    .unwrap();
    std::fs::write(
        leaf.join("TRACK.yml"),
        "car:\n  name: Archive Car\ndate: '2001-01-01'\ntime: '01:02:03'\narchive:\n  owner: Driver\n",
    )
    .unwrap();
    for suffix in ["telemetry", "telemetry.jsonl", "telemetry.jsonl.zstd"] {
        // Output lives outside the metadata root; inspect must use stored extras.
        let dest = dir.path().join(format!("converted.{suffix}"));
        let converted = cli()
            .arg("convert")
            .arg(format!("--root-path={}", root.display()))
            .arg(&input)
            .arg(&dest)
            .output()
            .unwrap();
        assert!(converted.status.success(), "{suffix}: {converted:?}");
        let stored = telemetry_format::read_metadata(&dest).unwrap();
        assert_eq!(stored.extra["archive"]["owner"], "Driver");
        assert_eq!(stored.identity.venue, "Archive Circuit");
        assert_eq!(stored.identity.vehicle, "Archive Car");
        assert_eq!(stored.identity.date, "2001-01-01");
        assert_eq!(stored.identity.time, "01:02:03");
        assert_eq!(stored.utc_start_ns, original.utc_start_ns);
        assert_eq!(stored.absolute_start_ns, original.absolute_start_ns);
        assert_eq!(stored.clock_offset_ns, original.clock_offset_ns);
        let report = inspect_json(&dest, &["--ignore-track-yml"]);
        assert_eq!(report["track_name"], "Archive Circuit");
        assert_eq!(report["car_type"], "Archive Car");
        assert_eq!(report["event_date"], "1999-01-01");
        assert_eq!(report["date"], "2001-01-01");
        assert_eq!(report["extra"]["archive"]["owner"], "Driver");
    }
}

#[test]
fn reports_requested_metadata() {
    let output = cli()
        .args(["inspect", fixture("synthetic_aimd.mp4").to_str().unwrap()])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
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
    assert!(output.status.success(), "{output:?}");
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
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("track_name: Road America\n"));
    assert!(stdout.contains("layout: Full Course\n"));
    assert!(stdout.contains("track_length: 6514 m\n"));
}

#[test]
fn convert_defaults_to_zstd_mtj_and_verify_accepts_all_encodings() {
    let dir = tempfile::tempdir().unwrap();
    let input = fixture("synthetic_cosworth.pds");
    let recording = dir.path().join("run.telemetry");
    let jsonl = dir.path().join("run.telemetry.jsonl");
    let zstd = dir.path().join("run.telemetry.jsonl.zstd");

    for dest in [recording.as_path(), jsonl.as_path(), zstd.as_path()] {
        let out = cli()
            .args(["convert", input.to_str().unwrap(), dest.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(out.status.success(), "{dest:?} {out:?}");
    }

    let verified = cli()
        .args([
            "verify",
            recording.to_str().unwrap(),
            jsonl.to_str().unwrap(),
            zstd.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(verified.status.success(), "{verified:?}");
    let stdout = String::from_utf8(verified.stdout).unwrap();
    // `.telemetry` is a zstd MTJ frame now; verify tells that from content.
    assert_eq!(stdout.matches("mtj:1").count(), 3, "{stdout}");
    assert_eq!(stdout.matches("mtj:1  zstd").count(), 2, "{stdout}");
    assert!(!stdout.contains("FAIL"), "{stdout}");
    let head = std::fs::read(&recording).unwrap();
    assert_eq!(&head[..4], &[0x28, 0xB5, 0x2F, 0xFD], "zstd magic");

    let native = cli()
        .args([
            "verify",
            "--json",
            "--track",
            "road-atlanta",
            input.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(native.status.success(), "{native:?}");
    let report: serde_json::Value = serde_json::from_slice(&native.stdout).unwrap();
    assert_eq!(report["track_audit"]["track"], "road-atlanta");
    assert!(report["track_audit"]["findings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f["code"] == "missing-trusted-gps"));
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
    assert!(output.status.success(), "{output:?}");
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
    assert!(inspect.contains("--root-path"));
    assert!(inspect.contains("--ignore-track-yml"));

    let convert =
        String::from_utf8(cli().args(["convert", "--help"]).output().unwrap().stdout).unwrap();
    assert!(convert.contains(".telemetry.jsonl"));
    assert!(convert.contains("Default"));
    assert!(convert.contains("--root-path"));
    assert!(convert.contains("--ignore-track-yml"));

    let verify =
        String::from_utf8(cli().args(["verify", "--help"]).output().unwrap().stdout).unwrap();
    assert!(verify.contains("zstd"));
    assert!(verify.contains("MTJ recording or MTX sidecar"));
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
    assert!(output.status.success(), "{output:?}");
    let dest = String::from_utf8(output.stdout).unwrap();
    assert!(dest.contains("run.pds.telemetry"), "{dest}");
    let dest = dest.trim();
    assert!(Path::new(dest).is_file());

    let verified = cli().args(["verify", dest]).output().unwrap();
    assert!(verified.status.success(), "{verified:?}");
    let report = String::from_utf8(verified.stdout).unwrap();
    assert!(report.contains("mtj:1  zstd"), "{report}");
    assert!(report.contains("ok"), "{report}");
}

#[test]
fn native_zip_option_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("run.telemetry");
    let out = cli()
        .args([
            "convert",
            "--native-zip",
            fixture("synthetic_cosworth.pds").to_str().unwrap(),
            dest.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8(out.stderr)
        .unwrap()
        .contains("unknown option --native-zip"));
    assert!(!dest.exists());
}

#[test]
fn strip_passes_recovers_raw_bytes_in_all_recording_encodings() {
    let dir = tempfile::tempdir().unwrap();
    let input = fixture("synthetic_cosworth.pds");
    for suffix in ["telemetry", "telemetry.jsonl", "telemetry.jsonl.zstd"] {
        let passed = dir.path().join(format!("passed.{suffix}"));
        let raw = dir.path().join(format!("raw.{suffix}"));
        let stripped = dir.path().join(format!("stripped.{suffix}"));
        for (args, dest) in [
            (vec!["convert"], &passed),
            (vec!["convert", "--no-passes"], &raw),
        ] {
            let out = cli().args(args).arg(&input).arg(dest).output().unwrap();
            assert!(out.status.success(), "{suffix}: {out:?}");
        }
        let raw_bytes = std::fs::read(&raw).unwrap();
        assert_ne!(
            motorsport_telemetry::open(&passed)
                .unwrap()
                .applied_passes(),
            []
        );
        // Both separate output and an in-place rewrite use the requested encoding.
        for dest in [&stripped, &passed] {
            let out = cli()
                .args(["convert", "--strip-passes"])
                .arg(&passed)
                .arg(dest)
                .output()
                .unwrap();
            assert!(out.status.success(), "{suffix}: {out:?}");
            assert_eq!(
                motorsport_telemetry::open(dest).unwrap().applied_passes(),
                []
            );
            assert_eq!(std::fs::read(dest).unwrap(), raw_bytes, "{suffix}");
        }
    }
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
fn verify_streams_native_directory_results_and_continues_after_errors() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(
        fixture("synthetic_cosworth.pds"),
        dir.path().join("good.pds"),
    )
    .unwrap();
    std::fs::write(dir.path().join("bad.mp4"), b"invalid").unwrap();
    std::fs::write(dir.path().join("notes.txt"), b"ignored").unwrap();
    let output = cli()
        .args(["verify", "--json"])
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    let lines = String::from_utf8(output.stdout).unwrap();
    let reports: Vec<serde_json::Value> = lines
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(reports.len(), 2);
    assert!(reports.iter().any(|r| r["error"].is_string()));
    assert!(reports.iter().any(|r| r["track_audit"].is_object()));
}

#[test]
fn verify_rejects_invalid_physical_limits_and_unknown_tracks() {
    for argument in ["NaN", "-1", "0"] {
        let output = cli()
            .args(["verify", "--max-speed", argument])
            .arg(fixture("synthetic_cosworth.pds"))
            .output()
            .unwrap();
        assert!(!output.status.success());
    }
    let output = cli()
        .args(["verify", "--track", "invented-track", "--json"])
        .arg(fixture("synthetic_cosworth.pds"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    let r: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(r["error"].as_str().unwrap().contains("unknown atlas track"));
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
    assert!(masked.status.success(), "{masked:?}");
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
    assert!(output.status.success(), "{output:?}");
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
    assert!(output.status.success(), "{output:?}");
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
    fn path(&self) -> &'static str {
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
    write_telemetry(&source, &dest).unwrap();

    let output = cli()
        .args(["verify", dest.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!output.status.success(), "verify must fail: {output:?}");
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
    assert!(text.contains("Review findings"), "{text}");
    assert!(text.contains("Exit 1"), "{text}");

    let inspect =
        String::from_utf8(cli().args(["inspect", "--help"]).output().unwrap().stdout).unwrap();
    assert!(inspect.contains("diagnostics"), "{inspect}");
}
