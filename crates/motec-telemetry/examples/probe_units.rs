//! Probe channel value ranges to sanity-check declared units.

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

use cosworth_telemetry::CosworthFile;
use motorsport_telemetry_core::TelemetrySource;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .expect("usage: probe_units FILE.pds");
    let file = CosworthFile::open(&path)?;
    println!(
        "{:<26} {:>8} {:>14} {:>14} {:>14}",
        "channel", "unit", "min", "max", "mean"
    );
    for (index, channel) in file.channels().iter().enumerate() {
        if channel.sample_count == 0 {
            continue;
        }
        let mut min = f64::INFINITY;
        let mut max = f64::NEG_INFINITY;
        let mut sum = 0.0f64;
        let mut n = 0u64;
        for (chunk_index, chunk) in channel.chunks.iter().enumerate() {
            for sample in 0..chunk.sample_count {
                let value = file.decode(index, chunk_index, sample);
                if value.is_finite() {
                    min = min.min(value);
                    max = max.max(value);
                    sum += value;
                    n += 1;
                }
            }
        }
        println!(
            "{:<26} {:>8} {:>14.4} {:>14.4} {:>14.4}",
            channel.name,
            channel.unit,
            min,
            max,
            sum / n as f64
        );
    }
    Ok(())
}
