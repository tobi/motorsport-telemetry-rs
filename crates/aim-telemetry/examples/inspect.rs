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

use aim_telemetry::AimFile;
use motorsport_telemetry_core::TelemetrySource;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("usage: inspect FILE.MP4")?;
    let file = AimFile::open(path)?;
    println!("{} channels", file.channels().len());
    for (index, channel) in file.channels().iter().enumerate() {
        let first = (!channel.chunks.is_empty()).then(|| file.decode(index, 0, 0));
        println!(
            "{}\t{}\t{}\t{:?}",
            channel.name,
            channel.sample_count,
            channel.frequency_hz().unwrap_or(0.0),
            first
        );
    }
    Ok(())
}
