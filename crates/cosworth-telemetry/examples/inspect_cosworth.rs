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
    let path = std::env::args().nth(1).expect("usage: inspect FILE.pds");
    let file = CosworthFile::open(path)?;
    println!("{}: {} channels", file.path(), file.channels().len());
    for channel in file
        .channels()
        .iter()
        .filter(|channel| channel.sample_count > 0)
    {
        println!(
            "{:5.1} Hz {:10} {:8} {}",
            channel.frequency_hz().unwrap_or(0.0),
            channel.sample_count,
            channel.unit,
            channel.name
        );
    }
    Ok(())
}
