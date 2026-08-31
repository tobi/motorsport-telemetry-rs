# racelogic-telemetry

Standalone mmap-backed parser for Racelogic VBOX `.vbo` telemetry.

```rust,no_run
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
- `RacelogicFile::open_metadata` — mmap path that skips unrelated signal values
- `RacelogicFile::from_bytes` / `from_slice` — embedded input
- `read_metadata` / `read_metadata_from_bytes` — fast summary path

The reader handles section-based files, UTC time-of-day, midnight rollover,
irregular timestamps, custom channels, and native `avifileindex` /
`avisynctime` video linkage. Custom unit lists with an extra leading
`avisynctime` unit are right-aligned so speed, throttle and brake units do
not shift onto neighbouring channels.

`utc_start_ns()` is the header date (`File created on dd/mm/yyyy at|@ …`)
combined with the first UTC time-of-day sample, so a VBO with a header date
places on the absolute axis without a timezone. The header's clock time is
the logger's local time and is only reported as identity.

### Inferred GPS laps

When `[laptiming]` declares start/finish marks, the reader can infer lap
boundaries from GPS crossings. This avoids swallowing laps when the CAN
`Lap_Number` resets at a driver change. It reports `vbo.laps_from_gate` and
returns sequential file-local lap numbers; these are not the CAN counter's
original numbers or vendor-reported beacon times.

The current inference interprets the two marks as position plus travel-line
direction, using a perpendicular 50 m gate. This is an empirical policy
checked against the audited collection, **not a verified vendor spec**.
Crossings require adjacent fixes no more than 2 seconds apart and at least
four satellites when a satellite channel exists. Impossible position jumps
are excluded; frequent jumps disable gate inference and retain counter/timer
recovery. GPS dropouts can still hide crossings, and a gate that misses the
pit lane can span multiple physical laps. Treat diagnostics and implausible
intervals as reasons to review the source, not as exact race timing.

`open_metadata` retains the same GPS quality inputs as `open`, so both paths
make the same lap decision.

```sh
cargo run -p racelogic-telemetry --example inspect_vbo -- run.vbo
```
