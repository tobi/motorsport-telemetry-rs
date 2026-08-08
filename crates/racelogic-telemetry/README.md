# racelogic-telemetry

Standalone mmap-backed parser for Racelogic VBOX `.vbo` telemetry.

```rust
use motorsport_telemetry_core::TelemetrySource;
use racelogic_telemetry::RacelogicFile;

let file = RacelogicFile::open("run.vbo")?;
let velocity = file.channels().iter()
    .position(|channel| channel.name == "velocity kmh")
    .unwrap();
println!("first speed={}", file.decode(velocity, 0, 0));
# Ok::<(), Box<dyn std::error::Error>>(())
```

Public constructors:

- `RacelogicFile::open` — native mmap path
- `RacelogicFile::from_bytes` / `from_slice` — embedded input
- `read_metadata` / `read_metadata_from_bytes` — fast summary path

The reader handles section-based files, UTC time-of-day, midnight rollover,
irregular timestamps, custom channels, and native `avifileindex` /
`avisynctime` video linkage.

```sh
cargo run -p racelogic-telemetry --example inspect_vbo -- run.vbo
```
