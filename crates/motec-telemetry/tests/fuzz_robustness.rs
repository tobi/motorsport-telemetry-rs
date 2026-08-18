//! Deterministic fuzz/robustness harness for the MoTeC LD + LDX readers.
//!
//! Mutates the committed `synthetic_motec_multilap.ld` corpus and the
//! `.ldx` sidecar corpus with every operator in [`fuzz_harness::Op`] and
//! asserts the shared invariants: no panic, no hang, and a parsed result
//! whose channel footprint exceeds the mutated input is flagged by
//! `validate_source` via `layout.footprint_exceeds_file`.

#![allow(missing_docs)]

#[path = "../../../tests/fuzz_harness.rs"]
mod fuzz_harness;

use fuzz_harness::assert_no_failures;
use motec_telemetry::MotecFile;
use motorsport_telemetry_core::TelemetrySource;
use std::path::PathBuf;
use std::sync::LazyLock;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

/// Parses a mutated LD buffer through the public entry point.
fn parse_ld(bytes: &[u8]) -> Result<Box<dyn TelemetrySource + Send + Sync>, String> {
    MotecFile::from_bytes("fuzz.ld", bytes.to_vec())
        .map_err(|e| e.to_string())
        .map(|f| Box::new(f) as Box<dyn TelemetrySource + Send + Sync>)
}

static LD_BYTES: LazyLock<Vec<u8>> =
    LazyLock::new(|| std::fs::read(fixture("synthetic_motec_multilap.ld")).unwrap());
fn ld_bytes() -> &'static [u8] {
    &LD_BYTES
}

/// Parses a mutated LDX sidecar together with a fixed valid LD, exercising the
/// `from_bytes_with_ldx` entry point and the LDX parser underneath.
fn parse_with_ldx(ldx: &[u8]) -> Result<Box<dyn TelemetrySource + Send + Sync>, String> {
    MotecFile::from_bytes_with_ldx("fuzz.ld", ld_bytes().to_vec(), ldx)
        .map_err(|e| e.to_string())
        .map(|f| Box::new(f) as Box<dyn TelemetrySource + Send + Sync>)
}

#[test]
fn motec_ld_survives_mutations() {
    let ld = std::fs::read(fixture("synthetic_motec_multilap.ld")).unwrap();
    assert_no_failures("motec_ld", &[&ld], parse_ld, true);
}

#[test]
fn motec_ldx_sidecar_survives_mutations() {
    let ldx = std::fs::read(fixture("synthetic_motec_multilap.ldx")).unwrap();
    // The mutated bytes are the sidecar, which holds lap timing rather than
    // sample bytes, so the footprint link is checked against the LD, not the
    // sidecar. Disable file_len-based footprint assertions for this corpus.
    assert_no_failures("motec_ldx", &[&ldx], parse_with_ldx, false);
}
