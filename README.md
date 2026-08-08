# Motorsport Telemetry for Rust

A format-neutral Rust workspace for reading, normalizing, and joining motorsport telemetry.

## Supported formats

| Format | Extension | Crate | Support |
|---|---:|---|---|
| AiM `aimd` in MP4 | `.mp4` | [`aim-telemetry`](crates/aim-telemetry) | Read; video and audio payloads are not decoded |
| Pi/Cosworth PDS | `.pds` | [`cosworth-telemetry`](crates/cosworth-telemetry) | Read |
| MoTeC LD/LDX | `.ld` | [`motec-telemetry`](crates/motec-telemetry) | Read and write |
| Racelogic VBOX | `.vbo` | [`racelogic-telemetry`](crates/racelogic-telemetry) | Read |

[`motorsport-telemetry`](crates/motorsport-telemetry) is the unified facade.
[`motorsport-telemetry-core`](crates/telemetry-core) defines the shared source,
channel, unit, metadata, lap, and session model. [`motorsport-track-atlas`](crates/motorsport-track-atlas)
provides offline circuit metadata and GPS-to-track matching.

The facade crate includes a metadata CLI. It memory-maps native recordings and
does not decode video payloads:

```sh
cargo run -p motorsport-telemetry -- recording.mp4
cargo run -p motorsport-telemetry -- --json recording.mp4
```

## Quick start

`open` selects the parser from the case-insensitive file extension. Import
`TelemetrySource` to access source-exact channels and samples:

```rust,no_run
use motorsport_telemetry::{open, motorsport_telemetry_core::TelemetrySource};

let recording = open("run.mp4")?;
println!("{} channels", recording.channels().len());

let normalizer = recording.normalizer();
let sample = normalizer.sample(0);
println!("speed={:?} m/s", sample.speed_mps);
# Ok::<(), Box<dyn std::error::Error>>(())
```

Every format crate also exposes `Type::open(path)` for memory-mapped native
input, `Type::from_bytes(name, bytes)` for owned input, and `read_metadata` /
`read_metadata_from_bytes` when only the session summary is needed.

`TelemetryFile::normalizer()` is intended for sampling loops. It resolves
signal roles and track context once, and lazily computes lap metadata at most
once when lap progress needs that fallback.

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

## Lap times, top speed, and brake pressure

`FileMetadata::laps` contains lap boundaries in file-relative nanoseconds. The
example below keeps complete laps, scans native samples inside each boundary,
and converts units through the shared unit registry. It considers every
pressure-valued channel with `brake` in its name, so separate front/rear or
master-cylinder channels are handled together.

```rust
use motorsport_telemetry::{
    motorsport_telemetry_core::{can_convert, convert, TelemetrySource},
    open,
};

fn maximum_between(
    source: &dyn TelemetrySource,
    channel_index: usize,
    start_ns: u64,
    end_ns: u64,
) -> Option<f64> {
    let channel = source.channels().get(channel_index)?;
    let mut maximum: Option<f64> = None;

    for (chunk_index, chunk) in channel.chunks.iter().enumerate() {
        for local_index in 0..chunk.sample_count {
            let time_ns = source.sample_time_ns(channel_index, chunk_index, local_index);
            if time_ns < start_ns || time_ns >= end_ns {
                continue;
            }

            let value = source.decode(channel_index, chunk_index, local_index);
            if value.is_finite() {
                maximum = Some(maximum.map_or(value, |before| before.max(value)));
            }
        }
    }

    maximum
}

fn format_lap_time(duration_ns: u64) -> String {
    let total_ms = duration_ns / 1_000_000;
    format!(
        "{}:{:02}.{:03}",
        total_ms / 60_000,
        total_ms % 60_000 / 1_000,
        total_ms % 1_000
    )
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or("usage: lap_stats TELEMETRY_FILE")?;
    let file = open(path)?;
    let metadata = file.metadata();

    let speed_index = file
        .signal_roles()
        .speed
        .ok_or("no recognized speed channel")?;
    let speed_unit = &file.channels()[speed_index].unit;
    if !can_convert(speed_unit, "km/h") {
        return Err(format!("speed unit {speed_unit:?} cannot be converted to km/h").into());
    }

    let brake_pressure_indices = file
        .channels()
        .iter()
        .enumerate()
        .filter(|(_, channel)| {
            channel.sample_count > 0
                && channel.name.to_ascii_lowercase().contains("brake")
                && can_convert(&channel.unit, "bar")
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();

    for lap in metadata.laps.iter().filter(|lap| lap.complete) {
        let top_speed_kmh = maximum_between(&file, speed_index, lap.start_ns, lap.end_ns)
            .and_then(|value| convert(value, speed_unit, "km/h").ok());

        let max_brake_bar = brake_pressure_indices
            .iter()
            .filter_map(|&index| {
                maximum_between(&file, index, lap.start_ns, lap.end_ns)
                    .and_then(|value| convert(value, &file.channels()[index].unit, "bar").ok())
            })
            .reduce(f64::max);

        println!(
            "lap {:>3}: {}  top speed {:>7} km/h  max brake {:>7} bar",
            lap.number,
            format_lap_time(lap.duration_ns),
            top_speed_kmh.map_or_else(|| "n/a".into(), |value| format!("{value:.1}")),
            max_brake_bar.map_or_else(|| "n/a".into(), |value| format!("{value:.1}")),
        );
    }

    Ok(())
}
```

A runnable version with explicit missing-channel checks is included as
[`lap_stats.rs`](crates/motorsport-telemetry/examples/lap_stats.rs):

```sh
cargo run -p motorsport-telemetry --example lap_stats -- recording.ld
```

Example output:

```text
lap   2: 1:32.481  top speed   274.6 km/h  max brake    78.2 bar
lap   3: 1:31.907  top speed   277.1 km/h  max brake    81.5 bar
```

## Multi-file sessions

`open_sessions(paths, max_gap_ns)` groups adjacent files only when they have a
compatible internal session key and absolute clock. Files without reliable
internal identity remain separate; filenames are never used as evidence that
two recordings belong together.

```rust,no_run
use motorsport_telemetry::open_sessions;

let sessions = open_sessions(["part-1.vbo", "part-2.vbo"], 5_000_000_000)?;
if let Some(position) = sessions
    .first()
    .and_then(|session| session.position(10_000_000_000))
{
    println!("{} at {} ns", position.source_path.display(), position.file_time_ns);
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Track atlas

Track data is generated from a pinned revision of [`tobi/track-atlas`](https://github.com/tobi/track-atlas) and committed as an offline build input. Cargo builds never require network access. Run `python scripts/update_track_atlas.py /path/to/track-atlas` to refresh the pinned dataset deliberately.

Track matching returns facility/layout name, official length, direction, centerline, corner layers, and range layers. OpenStreetMap-derived geometry retains ODbL attribution; see `ATTRIBUTION.md`.

## Development

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Licensed under the [MIT License](LICENSE). Track data has additional attribution
described in [ATTRIBUTION.md](ATTRIBUTION.md).
