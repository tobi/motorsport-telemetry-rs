# Motorsport Telemetry for Rust

A format-neutral Rust workspace for reading, normalizing, and joining motorsport telemetry.
DuckDB is one consumer of these crates, not part of this repository.

## Crates

| Crate | Purpose |
|---|---|
| `motorsport-telemetry` | Unified format detection, normalized signal roles, sessions, laps, video references, GPS, and track matching |
| `motorsport-telemetry-core` | Generic channel/chunk/sample model, units, metadata, stints, laps, and session grouping |
| `motorsport-track-atlas` | Offline track/layout metadata and GPS-to-track matching |
| `aim-telemetry` | Native mmap reader for AiM `aimd` telemetry embedded in MP4 |
| `cosworth-telemetry` | Native mmap reader for Pi/Cosworth PDS |
| `motec-telemetry` | Native mmap reader and writer for MoTeC LD/LDX |
| `racelogic-telemetry` | Native mmap reader for Racelogic VBOX VBO |

## Common interface

Every parser implements `TelemetrySource` and provides:

```rust
Type::open(path)?;                  // mmap on native platforms
Type::from_bytes(name, bytes)?;    // owned bytes for embedded callers
read_metadata(path)?;              // fast file/session/lap summary
read_metadata_from_bytes(name, bytes)?;
```

The facade detects formats:

```rust
use motorsport_telemetry::{open, TelemetrySource};

let recording = open("run.mp4")?;
let metadata = recording.metadata();
let roles = recording.signal_roles();
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Normalized model

The facade exposes source-exact values and explicit normalized roles:

- speed in m/s
- throttle and brake as fractions when the source unit supports truthful normalization
- WGS84 longitude/latitude in degrees
- lap number and lap progress
- internal driver identity and stints
- session-relative and file-relative clocks
- source file, video file index, sync time, and MP4 presentation frame index when available

Unknown or incompatible inputs remain `None`; the library never guesses a unit or fabricates a frame.

## Track atlas

Track data is generated from a pinned revision of [`tobi/track-atlas`](https://github.com/tobi/track-atlas) and committed as an offline build input. Cargo builds never require network access. Run `python scripts/update_track_atlas.py /path/to/track-atlas` to refresh the pinned dataset deliberately.

Track matching returns facility/layout name, official length, direction, centerline, corner layers, and range layers. OpenStreetMap-derived geometry retains ODbL attribution; see `ATTRIBUTION.md`.

## Development

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
