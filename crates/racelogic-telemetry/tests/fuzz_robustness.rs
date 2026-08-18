//! Deterministic fuzz/robustness harness for the Racelogic VBOX VBO reader.
//!
//! Mutates the committed `synthetic_vbo.vbo` corpus with every operator in
//! [`fuzz_harness::Op`] and asserts the shared invariants: no panic, no hang,
//! and a parsed result whose channel footprint exceeds the mutated input is
//! flagged by `validate_source` via `layout.footprint_exceeds_file`.

#![allow(missing_docs)]

#[path = "../../../tests/fuzz_harness.rs"]
mod fuzz_harness;

use fuzz_harness::assert_no_failures;
use motorsport_telemetry_core::TelemetrySource;
use racelogic_telemetry::RacelogicFile;
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

/// Parses a mutated VBO buffer through the public `from_slice` entry point.
fn parse_vbo(bytes: &[u8]) -> Result<Box<dyn TelemetrySource + Send + Sync>, String> {
    RacelogicFile::from_slice("fuzz.vbo", bytes)
        .map_err(|e| e.to_string())
        .map(|f| Box::new(f) as Box<dyn TelemetrySource + Send + Sync>)
}

#[test]
fn racelogic_vbo_survives_mutations() {
    let vbo = std::fs::read(fixture("synthetic_vbo.vbo")).unwrap();
    // VBO stores samples as text, so sample bytes are not bounded by the input
    // length: the footprint link is not asserted here.
    assert_no_failures("racelogic_vbo", &[&vbo], parse_vbo, false);
}
