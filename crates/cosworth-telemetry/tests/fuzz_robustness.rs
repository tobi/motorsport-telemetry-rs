//! Deterministic fuzz/robustness harness for the Pi/Cosworth PDS reader.
//!
//! This file exercises the PDS reader against mutated inputs but does NOT
//! edit the parser (`crates/cosworth-telemetry/src/lib.rs` is owned by the
//! human doing the PDS type-detection fix). Any panic found here is recorded
//! for the final report with the exact seed and operator; the test still
//! fails on a panic so the regression is visible, but the fix is out of scope
//! for this agent.

#![allow(missing_docs)]

#[path = "../../../tests/fuzz_harness.rs"]
mod fuzz_harness;

use cosworth_telemetry::CosworthFile;
use fuzz_harness::assert_no_failures;
use motorsport_telemetry_core::TelemetrySource;
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

/// Parses a mutated PDS buffer through the public `from_bytes` entry point.
fn parse_pds(bytes: &[u8]) -> Result<Box<dyn TelemetrySource + Send + Sync>, String> {
    CosworthFile::from_bytes("fuzz.pds", bytes.to_vec())
        .map_err(|e| e.to_string())
        .map(|f| Box::new(f) as Box<dyn TelemetrySource + Send + Sync>)
}

#[test]
fn cosworth_pds_survives_mutations() {
    let pds = std::fs::read(fixture("synthetic_cosworth.pds")).unwrap();
    // Footprint link is the invariant that would have caught the 1.5e308 m/s
    // misread, so it is asserted here too.
    assert_no_failures("cosworth_pds", &[&pds], parse_pds, true);
}
