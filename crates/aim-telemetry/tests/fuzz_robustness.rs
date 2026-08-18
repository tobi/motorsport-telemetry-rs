//! Deterministic fuzz/robustness harness for the AiM `aimd` MP4 reader.
//!
//! Mutates the committed `synthetic_aimd.mp4` corpus with every operator in
//! [`fuzz_harness::Op`] and asserts the shared invariants: no panic, no hang,
//! and a parsed result whose channel footprint exceeds the mutated input is
//! flagged by `validate_source` via `layout.footprint_exceeds_file`.

#![allow(missing_docs)]

#[path = "../../../tests/fuzz_harness.rs"]
mod fuzz_harness;

use aim_telemetry::AimFile;
use fuzz_harness::{assert_no_failures, run_case, Op, Outcome};
use motorsport_telemetry_core::TelemetrySource;
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

/// Parses a mutated AiM MP4 buffer through the public `from_bytes` entry point.
fn parse_aim(bytes: &[u8]) -> Result<Box<dyn TelemetrySource + Send + Sync>, String> {
    AimFile::from_bytes("fuzz.mp4", bytes.to_vec())
        .map_err(|e| e.to_string())
        .map(|f| Box::new(f) as Box<dyn TelemetrySource + Send + Sync>)
}
#[test]
fn aim_mp4_survives_mutations() {
    let mp4 = std::fs::read(fixture("synthetic_aimd.mp4")).unwrap();
    // AiM expands one aimd packet into many channels, so sample bytes are not
    // bounded by the input length: the footprint link is not asserted here.
    assert_no_failures("aim_mp4", &[&mp4], parse_aim, false);
}

/// Regression: `bit_flips` on `synthetic_aimd.mp4` at seed `0xfa8cfc37711c2dbe`
/// once panicked in `tagged_blocks` because `Option::then_some` indexed
/// `&sample[payload..end]` before the `end <= sample.len()` guard ran, so a
/// mutated `<h...>` block advertising a size past the sample end sliced out
/// of range. The guard now gates the slice in a branch.
#[test]
fn regression_tagged_blocks_then_some_panic() {
    let mp4 = std::fs::read(fixture("synthetic_aimd.mp4")).unwrap();
    let outcome = run_case(0xfa8cfc37711c2dbe, Op::BitFlips, &mp4, parse_aim, false);
    if let Outcome::Failure(msg) = outcome.outcome {
        panic!("regression returned: {msg}");
    }
}

/// Regression: `bit_flips` at seed `0xc40ed661dc35f6f4` and `splat_ff` at
/// seed `0xaf8c18bce18bd123` once hung `AimFile::from_bytes` because
/// `video_frame_times_ns` reserved and filled `decode_times` from a mutated
/// stts `samples` count with no upper bound, and `parse_track` reserved an
/// stsc vector from an unvalidated count. Both counts are now bounded against
/// the file/box size, so the cases reject instead of looping or OOMing.
#[test]
fn regression_video_stts_unbounded_frame_count_hang() {
    let mp4 = std::fs::read(fixture("synthetic_aimd.mp4")).unwrap();
    for (seed, op) in [
        (0xc40ed661dc35f6f4, Op::BitFlips),
        (0xaf8c18bce18bd123, Op::SplatFF),
    ] {
        let outcome = run_case(seed, op, &mp4, parse_aim, false);
        if let Outcome::Failure(msg) = outcome.outcome {
            panic!("regression {seed:#x} returned: {msg}");
        }
    }
}
