//! Shared deterministic fuzz/robustness harness for every format reader.
//!
//! This is not a standalone test target. It is `#[path]`-included by the
//! per-crate integration tests so the same primitives compile into each test
//! crate without a new shared dependency. It uses only `std` and
//! `motorsport-telemetry-core`, both of which every parser crate already
//! depends on.
//!
//! Design goals, mirroring the assignment:
//!   * Every case is derived from an explicit `u64` seed, so a failure
//!     reproduces exactly. The seed is printed in every assertion message.
//!   * Mutation operators include bit flips, truncation, region fills, and
//!     implausible value injection into u16/u32/u64/f32/f64 fields.
//!   * For every mutated input we assert that parsing, decoding, sampling,
//!     and validation never panic and never hang.
//!   * For binary-packed formats (PDS, MoTeC LD, native `.telemetry`) we also
//!     assert the footprint link: a parsed result whose channels claim more
//!     sample bytes than the mutated input holds MUST be flagged by
//!     `validate_source` via `layout.footprint_exceeds_file`. That is the
//!     invariant which would have caught the original 1.5e308 m/s misread.
//!     Text formats (VBO, JSONL) and packet-expanding formats (AiM) do not
//!     bound sample bytes by input length, so the link is not asserted there.

#![allow(missing_docs)]
#![allow(dead_code)]
#![allow(unused_variables)]

use motorsport_telemetry_core::{
    read_source_metadata, validate_source_with, TelemetrySource, ValidateOptions,
};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// Per-case wall-clock budget. Generous for the tiny synthetic corpora used
/// here, but bounded so a pathological infinite loop is reported as a hang
/// rather than freezing the suite.
const PER_CASE_TIMEOUT: Duration = Duration::from_millis(1500);

/// Default number of mutated cases run per format when `FUZZ_CASES` is unset.
/// Low enough for CI; raise `FUZZ_CASES` for longer local runs.
const DEFAULT_CASES: u32 = 128;

/// Maximum samples decoded per channel per case. Bounds per-case work while
/// still exercising every chunk and the public decode/sample_at paths.
const MAX_SAMPLES_PER_CHANNEL: u64 = 32;

/// Reads the per-run case count override, defaulting to [`DEFAULT_CASES`].
pub fn case_count() -> u32 {
    std::env::var("FUZZ_CASES")
        .ok()
        .and_then(|raw| raw.parse::<u32>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_CASES)
}

/// Deterministic SplitMix64 PRNG.
///
/// Every case starts from an explicit `u64` seed, so a reported failure
/// reproduces bit-for-bit. SplitMix64 is small, std-only, and good enough for
/// byte mutation.
pub struct Rng {
    state: u64,
}

impl Rng {
    /// Seeds the generator. The addend keeps `seed = 0` from being a fixed
    /// point while remaining a pure function of the seed.
    pub fn from_seed(seed: u64) -> Self {
        Self {
            state: seed.wrapping_add(0x9E3779B97F4A7C15),
        }
    }

    /// Returns the next 64-bit value.
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }

    /// Returns the next 32-bit value.
    pub fn next_u32(&mut self) -> u32 {
        (self.next_u64() >> 32) as u32
    }

    /// Returns a value in `0..n`, or `0` when `n == 0`.
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next_u64() as usize) % n
        }
    }
}

/// One mutation operator. The label is printed in assertion messages.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// Flip a single bit.
    BitFlip,
    /// Flip up to eight bits.
    BitFlips,
    /// Truncate to a shorter length.
    Truncate,
    /// Zero a contiguous region.
    ZeroRegion,
    /// Splat `0xff` over a contiguous region.
    SplatFF,
    /// Overwrite a little-endian u16 field with an implausible value.
    U16Field,
    /// Overwrite a little-endian u32 field with an implausible value.
    U32Field,
    /// Overwrite a little-endian u64 field with an implausible value.
    U64Field,
    /// Overwrite an f32 region with an implausible value.
    F32Field,
    /// Overwrite an f64 region with an implausible value.
    F64Field,
}

impl Op {
    /// All operators, in a stable order.
    pub const ALL: &'static [Op] = &[
        Op::BitFlip,
        Op::BitFlips,
        Op::Truncate,
        Op::ZeroRegion,
        Op::SplatFF,
        Op::U16Field,
        Op::U32Field,
        Op::U64Field,
        Op::F32Field,
        Op::F64Field,
    ];

    /// Stable label for assertion messages.
    pub fn label(self) -> &'static str {
        match self {
            Op::BitFlip => "bit_flip",
            Op::BitFlips => "bit_flips",
            Op::Truncate => "truncate",
            Op::ZeroRegion => "zero_region",
            Op::SplatFF => "splat_ff",
            Op::U16Field => "u16_field",
            Op::U32Field => "u32_field",
            Op::U64Field => "u64_field",
            Op::F32Field => "f32_field",
            Op::F64Field => "f64_field",
        }
    }

    fn width(self) -> usize {
        match self {
            Op::U16Field => 2,
            Op::U32Field | Op::F32Field => 4,
            Op::U64Field | Op::F64Field => 8,
            _ => 0,
        }
    }

    fn is_field(self) -> bool {
        matches!(
            self,
            Op::U16Field | Op::U32Field | Op::U64Field | Op::F32Field | Op::F64Field
        )
    }
}

/// Returns a mutated copy of `base` for `op`, derived from `seed`.
///
/// Length-preserving operators leave the byte count unchanged; [`Op::Truncate`]
/// shrinks it. Field injection overwrites a little-endian u16/u32/u64 or
/// f32/f64 region with an implausible value drawn from a fixed table.
pub fn mutate(base: &[u8], seed: u64, op: Op) -> Vec<u8> {
    let mut rng = Rng::from_seed(seed ^ (op as u64).wrapping_mul(0x100000001b3));
    let mut out = base.to_vec();
    if out.is_empty() {
        return out;
    }
    match op {
        Op::BitFlip => {
            let bit = rng.below(out.len() * 8);
            out[bit / 8] ^= 1u8 << (bit % 8);
        }
        Op::BitFlips => {
            let n = 1 + rng.below(8);
            for _ in 0..n {
                let bit = rng.below(out.len() * 8);
                out[bit / 8] ^= 1u8 << (bit % 8);
            }
        }
        Op::Truncate => {
            // Many lengths, including empty: a parser must reject or recover,
            // never panic, on a short buffer.
            let keep = rng.below(out.len() + 1);
            out.truncate(keep);
        }
        Op::ZeroRegion | Op::SplatFF => {
            let start = rng.below(out.len());
            let max_len = out.len() - start;
            let len = 1 + rng.below(max_len);
            let fill = if op == Op::ZeroRegion { 0x00 } else { 0xff };
            for byte in &mut out[start..start + len] {
                *byte = fill;
            }
        }
        Op::U16Field | Op::U32Field | Op::U64Field | Op::F32Field | Op::F64Field => {
            let width = op.width();
            if out.len() < width {
                return out;
            }
            let start = rng.below(out.len() - width + 1);
            let bytes = implausible_bytes(op, &mut rng);
            out[start..start + width].copy_from_slice(&bytes[..width]);
        }
    }
    out
}

/// Returns little-endian bytes for an implausible value of the operator's type.
///
/// Integer tables include `0`, `1`, the type's maximum, and `i32::MIN`
/// (zero-extended for the unsigned widths) to inject huge counts/offsets and
/// negative-looking magnitudes. Float tables include NaN, both infinities,
/// `MAX`, `MIN_POSITIVE`, and a denormal.
fn implausible_bytes(op: Op, rng: &mut Rng) -> [u8; 8] {
    let mut buf = [0u8; 8];
    match op {
        Op::U16Field => {
            const V: [u16; 3] = [0, 1, u16::MAX];
            let pick = V[rng.below(V.len())];
            buf[..2].copy_from_slice(&pick.to_le_bytes());
        }
        Op::U32Field => {
            const V: [u32; 4] = [0, 1, u32::MAX, i32::MIN as u32];
            let pick = V[rng.below(V.len())];
            buf[..4].copy_from_slice(&pick.to_le_bytes());
        }
        Op::U64Field => {
            const V: [u64; 4] = [0, 1, u64::MAX, i32::MIN as u64];
            let pick = V[rng.below(V.len())];
            buf[..8].copy_from_slice(&pick.to_le_bytes());
        }
        Op::F32Field => {
            const V: [f32; 6] = [
                f32::NAN,
                f32::INFINITY,
                f32::NEG_INFINITY,
                f32::MAX,
                f32::MIN_POSITIVE,
                f32::from_bits(1), // smallest denormal
            ];
            let pick = V[rng.below(V.len())];
            buf[..4].copy_from_slice(&pick.to_le_bytes());
        }
        Op::F64Field => {
            const V: [f64; 6] = [
                f64::NAN,
                f64::INFINITY,
                f64::NEG_INFINITY,
                f64::MAX,
                f64::MIN_POSITIVE,
                f64::from_bits(1), // smallest denormal
            ];
            let pick = V[rng.below(V.len())];
            buf[..8].copy_from_slice(&pick.to_le_bytes());
        }
        _ => {}
    }
    buf
}

/// A parse function boxed as a trait object so the harness is format-agnostic.
///
/// Returns the parsed source on success, or an error string on a clean
/// rejection. Panics are caught separately by the harness.
pub type Parse = fn(&[u8]) -> Result<Box<dyn TelemetrySource + Send + Sync>, String>;

/// Outcome of one fuzz case.
#[derive(Debug)]
pub struct CaseOutcome {
    /// The seed that produced this case.
    pub seed: u64,
    /// The operator label.
    pub op: &'static str,
    /// The observed outcome.
    pub outcome: Outcome,
}

/// What happened for one mutated input.
#[derive(Debug)]
pub enum Outcome {
    /// Parsed and all invariants held.
    Accepted,
    /// Parser returned `Err`; acceptable.
    Rejected,
    /// A genuine failure: panic, hang, or broken invariant.
    Failure(String),
}

/// Runs one mutated case under a watchdog thread and returns its outcome.
///
/// `validate_file_len` should be `true` only for binary-packed formats whose
/// channel footprint is bounded by the input length (PDS, MoTeC LD, native
/// `.telemetry`). It MUST be `false` for text formats (VBO, JSONL) and
/// packet-expanding formats (AiM), where sample bytes are not bounded by the
/// input length. When true, a parsed result whose channels claim more sample
/// bytes than the mutated input holds MUST be flagged by `validate_source`
/// via `layout.footprint_exceeds_file`, or the case is recorded as a failure.
pub fn run_case(
    seed: u64,
    op: Op,
    base: &[u8],
    parse: Parse,
    validate_file_len: bool,
) -> CaseOutcome {
    let mutated = mutate(base, seed, op);
    let file_len = if validate_file_len {
        Some(mutated.len() as u64)
    } else {
        None
    };
    let (tx, rx) = mpsc::channel::<WorkerMsg>();
    let worker_seed = seed;
    let worker_op = op.label();
    let parse_fn = parse;
    let _ = thread::Builder::new()
        .name(format!("fuzz-{worker_op}-{seed:#x}"))
        .spawn(move || {
            let msg = run_worker(&mutated, parse_fn, file_len, worker_seed, worker_op);
            let _ = tx.send(msg);
        });
    match rx.recv_timeout(PER_CASE_TIMEOUT) {
        Ok(WorkerMsg::Accepted) => CaseOutcome {
            seed,
            op: op.label(),
            outcome: Outcome::Accepted,
        },
        Ok(WorkerMsg::Rejected) => CaseOutcome {
            seed,
            op: op.label(),
            outcome: Outcome::Rejected,
        },
        Ok(WorkerMsg::Failure(msg)) => CaseOutcome {
            seed,
            op: op.label(),
            outcome: Outcome::Failure(msg),
        },
        Err(mpsc::RecvTimeoutError::Timeout) => CaseOutcome {
            seed,
            op: op.label(),
            outcome: Outcome::Failure(format!(
                "hang: case did not finish within {}ms",
                PER_CASE_TIMEOUT.as_millis()
            )),
        },
        Err(_) => CaseOutcome {
            seed,
            op: op.label(),
            outcome: Outcome::Failure("worker thread died without reporting".into()),
        },
    }
}

enum WorkerMsg {
    Accepted,
    Rejected,
    Failure(String),
}

fn run_worker(
    bytes: &[u8],
    parse: Parse,
    file_len: Option<u64>,
    seed: u64,
    op: &'static str,
) -> WorkerMsg {
    // Parsing may panic on hostile bytes; catch it so the case is reported as
    // a failure with the panic payload instead of aborting the whole suite.
    let parsed = catch_unwind(AssertUnwindSafe(|| parse(bytes)));
    match parsed {
        Err(payload) => WorkerMsg::Failure(format!(
            "seed={seed:#x} op={op}: parse panicked: {}",
            panic_message(&payload)
        )),
        Ok(Err(err)) => WorkerMsg::Rejected,
        Ok(Ok(source)) => {
            let result = catch_unwind(AssertUnwindSafe(|| {
                check_invariants(&*source, file_len, seed, op)
            }));
            match result {
                Err(payload) => WorkerMsg::Failure(format!(
                    "seed={seed:#x} op={op}: invariant check panicked: {}",
                    panic_message(&payload)
                )),
                Ok(Err(msg)) => WorkerMsg::Failure(format!("seed={seed:#x} op={op}: {msg}")),
                Ok(Ok(())) => WorkerMsg::Accepted,
            }
        }
    }
}

/// Extracts a readable string from a panic payload.
fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

/// Asserts the per-case invariants over a successfully parsed source.
///
/// Returns `Err(message)` when an invariant is violated. Decoding every chunk
/// through the public API is what enforces "every index used must be in range":
/// an out-of-range access inside a parser panics, which the worker catches and
/// reports as a failure.
fn check_invariants(
    source: &dyn TelemetrySource,
    file_len: Option<u64>,
    seed: u64,
    op: &str,
) -> Result<(), String> {
    let channels = source.channels();

    // Exercise the cheap accessors; any of them may panic on a broken layout.
    let _ = read_source_metadata(source);
    let _ = source.absolute_time_range();
    let _ = source.identity();
    let _ = source.timezone();
    let _ = source.spans();
    let _ = source.applied_passes();
    let _ = source.source_origin();
    let _ = source.video_files();
    let _ = source.video_presentation_times_ns();
    let _ = source.video_frame_count();
    let _ = source.diagnostics();
    let _ = source.channel_visible();

    for (ci, channel) in channels.iter().enumerate() {
        let _ = source.channel_display(ci);
        let _ = source.channel_labels(ci);
        let _ = source.sample_affine(ci);

        let mut budget = MAX_SAMPLES_PER_CHANNEL;
        for (cki, chunk) in channel.chunks.iter().enumerate() {
            // `chunk_bytes` must return an in-range slice when present; a
            // broken layout would panic on the slicing inside the reader and
            // be caught by the worker's panic guard.
            let _ = source.chunk_bytes(ci, cki);

            if budget == 0 || chunk.sample_count == 0 || chunk.sample_period_ns == 0 {
                continue;
            }
            // Stride so we visit up to `budget` samples across this chunk,
            // always staying within [0, sample_count). `div_ceil` avoids the
            // `(n + budget - 1) / budget` overflow on hostile u64::MAX counts.
            let stride = chunk.sample_count.div_ceil(budget).max(1);
            let mut local = 0u64;
            while local < chunk.sample_count && budget > 0 {
                // The core invariant: every index used must be in range.
                let _ = source.decode(ci, cki, local);
                // Exercise the reader's `sample_time_ns` override with a
                // valid local index. This is where the native overflow
                // regression was found; direct saturating math here would
                // hide such regressions. The override is saturating in every
                // reader, so a huge mutated period must not panic.
                let time_ns = source.sample_time_ns(ci, cki, local);
                let _ = source.sample_at(ci, time_ns, true);
                let _ = source.sample_at(ci, time_ns, false);
                budget -= 1;
                local = match local.checked_add(stride) {
                    Some(next) => next,
                    None => break,
                };
            }
        }
    }

    // The footprint link: this is what would have caught the 1.5e308 m/s bug.
    // A source whose channels claim more sample bytes than the mutated input
    // physically holds MUST be reported by the validator.
    let diags = validate_source_with(
        source,
        ValidateOptions {
            samples_per_channel: MAX_SAMPLES_PER_CHANNEL,
            file_len,
        },
    );
    if let Some(len) = file_len.filter(|len| *len > 0) {
        let footprint: u128 = channels
            .iter()
            .map(|channel| {
                (channel.sample_count as u128) * (channel.sample_type.byte_width() as u128)
            })
            .sum();
        if footprint > len as u128 && diags.find("layout.footprint_exceeds_file").is_none() {
            return Err(format!(
                "footprint {footprint} sample-bytes > file_len {len}, but validate_source \
                 did not emit layout.footprint_exceeds_file; diagnostics: {diags}"
            ));
        }
    }

    Ok(())
}

/// Runs `case_count()` cases against `corpus`, cycling through [`Op::ALL`],
/// and asserts that none of them is a [`Outcome::Failure`].
///
/// `format_name` is included in the final assertion so a CI failure names the
/// format, every failing seed, and the operator. Acceptable outcomes
/// (Accepted/Rejected) are counted and reported via `eprintln` for visibility.
pub fn assert_no_failures(
    format_name: &str,
    corpus: &[&[u8]],
    parse: Parse,
    validate_file_len: bool,
) {
    let n = case_count() as u64;
    let mut failures = Vec::new();
    let mut accepted = 0u32;
    let mut rejected = 0u32;
    for index in 0..n {
        let seed = index.wrapping_mul(0x9E3779B97F4A7C15) ^ (format_name.len() as u64);
        let op = Op::ALL[(index as usize) % Op::ALL.len()];
        let base = corpus[(index as usize) % corpus.len()];
        let outcome = run_case(seed, op, base, parse, validate_file_len);
        match outcome.outcome {
            Outcome::Accepted => accepted += 1,
            Outcome::Rejected => rejected += 1,
            Outcome::Failure(msg) => {
                failures.push(format!("seed={seed:#x} op={}: {msg}", op.label()))
            }
        }
    }
    eprintln!(
        "[fuzz/{format_name}] {n} cases: {accepted} accepted, {rejected} rejected, {} failures",
        failures.len()
    );
    assert!(
        failures.is_empty(),
        "[fuzz/{format_name}] {} case(s) failed the robustness invariants:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
