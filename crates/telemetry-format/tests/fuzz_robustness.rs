//! Deterministic mutations of compressed MTJ, plain MTJ, and MTX documents.
//! Checks that corrupted documents cannot panic or hang the reader.

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

#[path = "../../../tests/fuzz_harness.rs"]
mod fuzz_harness;

use fuzz_harness::assert_no_failures;
use motec_telemetry::MotecFile;
use motorsport_telemetry_core::{Span, SpanPrimary, TelemetrySource};
use std::path::PathBuf;
use telemetry_format::{
    write_jsonl_extension_from_source_with, write_jsonl_from_source_with, write_telemetry,
    JsonlRecording, SidecarHeader,
};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

/// A small `MoTeC` LD source used as the basis for the MTJ corpora.
fn motec_source() -> MotecFile {
    let ld = std::fs::read(fixture("synthetic_motec_multilap.ld")).unwrap();
    MotecFile::from_bytes("synthetic_motec_multilap.ld", ld).unwrap()
}

fn write_temp(suffix: &str, write: impl FnOnce(&std::path::Path)) -> Vec<u8> {
    let dir = std::env::temp_dir();
    let path = dir.join(format!(
        "fuzz_robustness_{}_{pid}.{suffix}",
        suffix.replace('.', "_"),
        pid = std::process::id()
    ));
    write(&path);
    std::fs::read(&path).unwrap()
}

fn compressed_corpus() -> Vec<u8> {
    let source = motec_source();
    write_temp("telemetry", |path| {
        write_telemetry(&source, path).unwrap();
    })
}

fn mtj_corpus() -> Vec<u8> {
    let source = motec_source();
    write_temp("telemetry.jsonl", |path| {
        // Uncompressed so mutations edit the JSON text, not zstd framing.
        write_jsonl_from_source_with(&source, path, false).unwrap();
    })
}

fn mtx_corpus() -> Vec<u8> {
    let header = SidecarHeader {
        name: "fuzz-sidecar".into(),
        visible: true,
        right: Vec::new(),
        utc_start_ns: 1_700_000_000_000_000_000,
        timezone: "UTC".into(),
    };
    let spans = vec![Span {
        name: "stint-1".into(),
        start_ns: 0,
        end_ns: 10_000_000_000,
        visible: true,
        color: "#443".into(),
        primary: SpanPrimary {
            title: "#443".into(),
            subtitle: "EL".into(),
        },
        meta: Vec::new(),
    }];
    write_temp("telemetry.ext.jsonl", |path| {
        // Uncompressed so mutations edit the JSON text.
        write_jsonl_extension_from_source_with(&motec_source(), path, false).unwrap_or_else(|_| {
            // The extension writer requires a UTC start the MoTeC fixture may
            // not provide; fall back to a minimal span-only sidecar so the
            // corpus is still a valid MTX document.
            telemetry_format::write_jsonl_timeline_with(
                path,
                &header,
                1_000_000,
                10_000_000_000,
                &spans,
                false,
            )
            .unwrap();
        });
    })
}

/// Parses a mutated MTJ or MTX JSONL document through `JsonlRecording::from_bytes`.
fn parse_jsonl(bytes: &[u8]) -> Result<Box<dyn TelemetrySource + Send + Sync>, String> {
    JsonlRecording::from_bytes("fuzz.telemetry.jsonl", bytes)
        .map_err(|e| e.to_string())
        .map(|f| Box::new(f) as Box<dyn TelemetrySource + Send + Sync>)
}

/// Parses a mutated MTX sidecar through `JsonlRecording::from_bytes`.
fn parse_mtx(bytes: &[u8]) -> Result<Box<dyn TelemetrySource + Send + Sync>, String> {
    JsonlRecording::from_bytes("fuzz.telemetry.ext.jsonl", bytes)
        .map_err(|e| e.to_string())
        .map(|f| Box::new(f) as Box<dyn TelemetrySource + Send + Sync>)
}

#[test]
fn compressed_telemetry_survives_mutations() {
    let corpus = compressed_corpus();
    assert_no_failures("compressed_telemetry", &[&corpus], parse_jsonl, false);
}

#[test]
fn mtj_jsonl_survives_mutations() {
    let corpus = mtj_corpus();
    // JSONL stores samples as text, so sample bytes are not bounded by the
    // input length: the footprint link is not asserted here.
    assert_no_failures("mtj_jsonl", &[&corpus], parse_jsonl, false);
}

#[test]
fn mtx_sidecar_survives_mutations() {
    let corpus = mtx_corpus();
    // An MTX sidecar carries spans and/or text sample channels; sample bytes
    // are not bounded by the input length: footprint link not asserted.
    assert_no_failures("mtx_sidecar", &[&corpus], parse_mtx, false);
}
