# Motorsport Telemetry for Rust

Read motorsport recordings through one Rust API, inspect laps and video linkage,
and convert to compressed JSONL.

## Supported formats

| Format | Files | Crate |
|---|---|---|
| AiM telemetry in MP4 | `.mp4` | [`aim-telemetry`](crates/aim-telemetry) |
| Pi/Cosworth PDS | `.pds` | [`cosworth-telemetry`](crates/cosworth-telemetry) |
| MoTeC LD/LDX (read and write) | `.ld` with optional `.ldx` sidecar | [`motec-telemetry`](crates/motec-telemetry) |
| Racelogic VBOX | `.vbo` | [`racelogic-telemetry`](crates/racelogic-telemetry) |
| Telemetry JSONL (read and write) | `.telemetry`, `.telemetry.jsonl`, `.telemetry.jsonl.zstd` | [`telemetry-format`](crates/telemetry-format) |

`.telemetry` is an MTJ JSONL document in one zstd frame. Plain JSONL and MTX
sidecars are also supported; readers detect compression from the contents.
Conversion aligns samples to a time lattice, rounds values, and omits irregular
channels. Keep the vendor recording when exact original samples are needed.
Video files remain separate; their timestamps and references are preserved.

## CLI

Install to `~/.local/bin` with `make install` (override `PREFIX` as needed),
or run through Cargo:

```sh
cargo run -p motorsport-telemetry -- inspect recording.mp4
cargo run -p motorsport-telemetry -- inspect --json recording.pds
cargo run -p motorsport-telemetry -- convert recording.pds
cargo run -p motorsport-telemetry -- convert recording.pds recording.telemetry.jsonl
cargo run -p motorsport-telemetry -- verify recording.pds.telemetry
```

`convert` appends derived channels from the processing-pass registry by default.
Use `--no-passes` for a raw conversion or `--strip-passes` to remove derived
channels. The header records pass provenance and the original source identity.
Run `motorsport-telemetry help <command>` for options and examples.

## Rust API

[`motorsport-telemetry`](crates/motorsport-telemetry) is the unified facade:

```rust,no_run
use motorsport_telemetry::{open, motorsport_telemetry_core::TelemetrySource, SourceExt};

let recording = open("run.mp4")?;
println!("{} channels", recording.channels().len());

let normalizer = recording.normalizer();
let sample = normalizer.sample(0);
println!("speed={:?} m/s", sample.speed_mps);

for lap in recording.metadata().laps.iter().filter(|lap| lap.kind.is_flying()) {
    println!("{}: {:.3}s", lap.label(), lap.duration_ns as f64 / 1e9);
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

`TelemetrySource` exposes channels in their recorded units. The normalizer
resolves common signals in fixed units and returns `None` for unknown values.
Times are file-relative integer nanoseconds; laps are classified across stints.
Use `read_metadata` and `read_lap_metadata` for summaries, and `validate()` for
reader diagnostics and plausibility checks. Current JSONL summaries read only
the header and laps, without decoding channel values.

## Documentation

- [Client guide](docs/CLIENT_GUIDE.md): normalized signals, units, laps, clocks, video, and sessions.
- [TRACK.yml](docs/TRACK_YML.md): directory metadata, overrides, and collection-wide glob rules.
- [Format guide](TELEMETRY.md): JSONL layout, sidecars, and examples.
- [JSONL specification](crates/telemetry-format/JSONL.md) and [writer schema](telemetry.schema.json).
- [Processing passes](crates/telemetry-passes): GPS quality, cleanup, and speed-derived distance.
- [Shared primitives](crates/telemetry-core) and [offline track atlas](crates/motorsport-track-atlas).
