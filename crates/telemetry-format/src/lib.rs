//! Read and write time-aligned telemetry as JSONL, with optional zstd compression.
//!
//! `.telemetry` is an MTJ document compressed at zstd level 11.
//! [`JsonlRecording`] detects compression by content, independent of the file name.
//! The document contract is specified in `JSONL.md`.

#![deny(missing_docs)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::print_stdout,
        clippy::print_stderr,
        clippy::unreadable_literal,
        clippy::float_cmp,
        reason = "unit tests: fail loudly, print freely, exact fixture values"
    )
)]

mod jsonl;
mod native_content;
mod write;

pub use native_content::{
    native_recording_content_fingerprint, NativeContentFingerprint, NativeContentFingerprintError,
};

pub use jsonl::{
    is_jsonl_ext_path, is_jsonl_path, is_jsonl_zstd_path, period_ns_from_hz,
    write_jsonl_extension_from_source, write_jsonl_extension_from_source_with,
    write_jsonl_from_source, write_jsonl_from_source_with, write_jsonl_timeline,
    write_jsonl_timeline_with, write_jsonl_to, HeaderChrome, JsonlRecording, SidecarGroup,
    SidecarHeader, Span, SpanPrimary, JSONL_EXT_VERSION, JSONL_VERSION, JSONL_ZSTD_LEVEL,
};
pub use write::{stripped_view, write_telemetry, write_telemetry_stripped, TelemetryFormatError};

/// Reads file metadata and classified laps from the document header.
///
/// Reads only the header and laps lines for current recordings. Documents
/// without header summaries and MTX sidecars require a full parse.
pub fn read_metadata(
    path: impl AsRef<std::path::Path>,
) -> Result<motorsport_telemetry_core::FileMetadata, TelemetryFormatError> {
    let path = path.as_ref();
    match JsonlRecording::read_header_metadata(path)? {
        Some(metadata) => Ok(metadata),
        None => Ok(JsonlRecording::open(path)?.metadata()),
    }
}

/// Reads the classified laps. Same cost model as [`read_metadata`].
pub fn read_laps(
    path: impl AsRef<std::path::Path>,
) -> Result<Vec<motorsport_telemetry_core::LapMetadata>, TelemetryFormatError> {
    Ok(read_metadata(path)?.laps)
}

/// Reads the flying-lap count. Same cost model as [`read_metadata`].
pub fn read_valid_laps(path: impl AsRef<std::path::Path>) -> Result<u32, TelemetryFormatError> {
    Ok(read_metadata(path)?.valid_laps)
}

/// Reads the channel directory (name, unit, rate, start, count; no values).
///
/// Reads only the header when it contains `ch`, otherwise parses the document.
pub fn read_channels(
    path: impl AsRef<std::path::Path>,
) -> Result<Vec<motorsport_telemetry_core::Channel>, TelemetryFormatError> {
    use motorsport_telemetry_core::TelemetrySource;
    let path = path.as_ref();
    match JsonlRecording::read_header_channels(path)? {
        Some(channels) => Ok(channels),
        None => Ok(JsonlRecording::open(path)?.channels().to_vec()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use motorsport_telemetry_core::TelemetrySource;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures")
            .join(name)
    }

    #[test]
    fn schema_documents_every_convertible_unit() {
        let schema = include_str!("../../../telemetry.schema.json");
        for def in motorsport_telemetry_core::UNITS {
            if !def.dimension.is_convertible() {
                continue;
            }
            assert!(
                schema.contains(&format!("\"{}\"", def.canonical)),
                "telemetry.schema.json is missing convertible unit {}",
                def.canonical
            );
        }
        assert!(schema.contains("\"mp/h\""));
        assert!(schema.contains("timespan_ms"));
        assert!(schema.contains("360000000"));
    }

    #[test]
    fn mtx_example_sidecars_validate() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let script = root.join("scripts/validate-mtx.py");
        // `uv run --with jsonschema` so the Draft 2020-12 checks always run
        // instead of being skipped when the package is missing. `--no-config`
        // ignores user/project uv config (private indexes); UV_* env vars
        // still apply.
        let validate = |arg: &std::path::Path| {
            let out = std::process::Command::new("uv")
                .args(["run", "--quiet", "--no-config", "--with", "jsonschema"])
                .arg("python")
                .arg(&script)
                .arg(arg)
                .output()
                .expect("uv (https://docs.astral.sh/uv/) is required to run this test");
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(
                out.status.success(),
                "validate-mtx.py {} failed:\n{stdout}{stderr}",
                arg.display()
            );
            assert!(
                !stdout.contains("install jsonschema"),
                "jsonschema checks were skipped:\n{stdout}"
            );
        };
        for name in [
            "sebring-lmp2.telemetry.ext.jsonl",
            "multi-folder.telemetry.ext.jsonl",
        ] {
            validate(&root.join("schema/examples").join(name));
        }
        validate(std::path::Path::new("--self-check"));
    }

    #[test]
    fn telemetry_preserves_jsonl_spans_and_visibility() {
        use motorsport_telemetry_core::TelemetrySource;
        let host = JsonlRecording::from_bytes(
            "host.jsonl",
            concat!(
                "{\"mtj\":1,\"q\":1000000000,\"dur\":2000000000,\"utc\":1000,\"tz\":\"UTC\"}\n",
                "[]\n",
                "{\"n\":\"Speed\",\"hz\":1,\"vis\":0,\"v\":[1,2],\"lbl\":[[0,\"brake lock\"]]}\n",
                "{\"k\":\"s\",\"n\":\"443-1\",\"s\":0,\"e\":1000000000,\"vis\":1,\"c\":\"#e11d48\",",
                "\"p\":{\"title\":\"#443\",\"sub\":\"EL\"},\"m\":[[\"Laps\",\"18\"],",
                "[\"Best\",{\"v\":110332,\"u\":\"timespan_ms\"}]]}\n",
            )
            .as_bytes(),
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("run.telemetry");
        write_telemetry(&host, &dest).unwrap();
        let opened = JsonlRecording::open(&dest).unwrap();
        assert_eq!(opened.channel_visible(), [false]);
        assert_eq!(opened.spans().len(), 1);
        assert_eq!(opened.spans()[0].name, "443-1");
        assert_eq!(opened.spans()[0].primary.title, "#443");
        assert_eq!(
            opened.spans()[0].meta,
            [
                (
                    "Laps".into(),
                    motorsport_telemetry_core::SpanMetaValue::Text("18".into())
                ),
                (
                    "Best".into(),
                    motorsport_telemetry_core::SpanMetaValue::TimeMs(110_332)
                ),
            ]
        );
        assert_eq!(opened.decode(0, 0, 0), 1.0);
        assert_eq!(opened.channel_labels(0).len(), 1);
        assert_eq!(opened.channel_labels(0)[0].text, "brake lock");

        let back = dir.path().join("back.telemetry.jsonl");
        write_jsonl_from_source_with(&opened, &back, false).unwrap();
        let again = JsonlRecording::open(&back).unwrap();
        assert_eq!(again.channel_visible(), [false]);
        assert_eq!(again.spans(), opened.spans());
        assert_eq!(again.channel_labels(0)[0].text, "brake lock");
    }

    #[test]
    fn round_trips_synthetic_motec_values_and_placement() {
        let source = motec_telemetry::MotecFile::open(fixture("synthetic_motec.ld")).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("run.telemetry");
        write_telemetry(&source, &dest).unwrap();
        let opened = JsonlRecording::open(&dest).unwrap();
        for (index, channel) in opened.channels().iter().enumerate() {
            if channel.sample_count == 0 {
                continue;
            }
            let original = source
                .channels()
                .iter()
                .position(|original| original.name == channel.name)
                .unwrap();
            let expected = source.decode(original, 0, 0);
            let actual = opened.decode(index, 0, 0);
            assert!(
                (actual - expected).abs() <= expected.abs().max(1.0) * 1e-7,
                "{}: {actual} != {expected}",
                channel.name
            );
        }
        let meta = opened.metadata();
        let venue_tz = motorsport_telemetry_core::placement::resolve_timezone(&source);
        assert_eq!(
            meta.timezone.as_str(),
            venue_tz.as_str(),
            "timezone should come from the venue atlas, never invented"
        );
        if source.absolute_time_range().is_some() {
            // Motec stamps a civil "utc" clock; GPS clocks copy through.
            // Either way, a known zone or a gps clock should produce utc.
            if !venue_tz.is_empty()
                || source
                    .absolute_time_range()
                    .is_some_and(|clock| clock.clock == "gps")
            {
                assert!(
                    meta.utc_start_ns.is_some()
                        || source
                            .absolute_time_range()
                            .is_some_and(|clock| clock.clock != "gps" && clock.clock != "utc")
                );
            }
        }
    }
}
