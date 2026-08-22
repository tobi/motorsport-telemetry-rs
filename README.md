# Motorsport Telemetry for Rust

A format-neutral Rust workspace for reading, normalizing, and joining motorsport telemetry.

The format, memory layout, and examples are in [TELEMETRY.md](TELEMETRY.md).
The writer-strict schema is [telemetry.schema.json](telemetry.schema.json).

## Supported formats

| Format | Extension | Crate | Support |
|---|---:|---|---|
| AiM `aimd` in MP4 | `.mp4` | [`aim-telemetry`](crates/aim-telemetry) | Read; video and audio payloads are not decoded |
| Pi/Cosworth PDS | `.pds` | [`cosworth-telemetry`](crates/cosworth-telemetry) | Read |
| MoTeC LD/LDX | `.ld` | [`motec-telemetry`](crates/motec-telemetry) | Read and write |
| Racelogic VBOX | `.vbo` | [`racelogic-telemetry`](crates/racelogic-telemetry) | Read |
| Native `.telemetry` | `.telemetry` | [`telemetry-format`](crates/telemetry-format) | Read and write; aligned STORE zip, FlatBuffers catalog first |
| MTJ JSONL | `.telemetry.jsonl` | [`telemetry-format`](crates/telemetry-format/JSONL.md) | Read and write; time-aligned header / laps / channels; video linkage in the header |
| MTJ JSONL + zstd | `.telemetry.jsonl.zstd` | same | Same document, one zstd frame |

[`motorsport-telemetry`](crates/motorsport-telemetry) is the unified facade.
[`motorsport-telemetry-core`](crates/telemetry-core) defines the shared source,
channel, unit, metadata, lap, and session model. [`motorsport-track-atlas`](crates/motorsport-track-atlas)
provides offline circuit metadata and GPS-to-track matching.
[`telemetry-passes`](crates/telemetry-passes) is the registry of named,
versioned, lossless processing passes applied at conversion time.

## CLI

Install the release CLI for the current user:

```sh
make install
```

This writes `motorsport-telemetry` to `~/.local/bin`. Override `PREFIX` for a
different installation root, for example
`make install PREFIX=/usr/local` (normally with `sudo`) or use `DESTDIR` when
staging a package.

The facade crate includes a CLI. It memory-maps native recordings and
does not decode video payloads:

```sh
cargo run -p motorsport-telemetry -- inspect recording.mp4
cargo run -p motorsport-telemetry -- inspect --json recording.mp4
cargo run -p motorsport-telemetry -- inspect ~/Documents/Telemetry --mask '**/*.pds'
cargo run -p motorsport-telemetry -- help inspect
cargo run -p motorsport-telemetry -- convert recording.pds
cargo run -p motorsport-telemetry -- convert recording.pds recording.telemetry.jsonl
cargo run -p motorsport-telemetry -- convert recording.pds recording.telemetry.jsonl.zstd
cargo run -p motorsport-telemetry -- convert --no-passes recording.pds
cargo run -p motorsport-telemetry -- convert --strip-passes recording.pds.telemetry
cargo run -p motorsport-telemetry -- verify recording.telemetry recording.telemetry.jsonl recording.telemetry.jsonl.zstd
```

## Processing passes

`telemetry-convert` runs the [`telemetry-passes`](crates/telemetry-passes)
registry by default. Each pass is named and versioned (`gps.quality@1`,
`gps.clean@1`, `speed.distance@1`), documents what must be true of the source
to employ it, and is **lossless**: passes only append derived channels
(cleaned GPS, an integrated distance odometer, and per-estimate sigma
channels), never touch source data, and `--strip-passes` recovers the raw
conversion byte-for-byte. Applied passes are reported on stderr
(`gps.clean@1 skipped — no GPS coordinate channels present`) and recorded in
the file — the `.telemetry` catalog and the MTJ header both carry the pass
list plus the original source format and path, so any converted file can
explain which processing it received and where it came from. Rationale and
the planned lap-progress passes: [`docs/WHY_POSITIONING_IS_HARD.md`](docs/WHY_POSITIONING_IS_HARD.md).

## Quick start

`open` selects the parser from the case-insensitive file extension. Import
`TelemetrySource` to access source-exact channels and samples:

```rust,no_run
use motorsport_telemetry::{open, motorsport_telemetry_core::TelemetrySource, SourceExt};

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

Hard parse failures remain typed `Result` errors. Recoverable damage and every
assumption/clamp/drop made by a reader are available through
`recording.diagnostics()`. `recording.validate()` combines those findings with
format-neutral checks for non-finite values, physically implausible values, and
impossible packed sample footprints:

```rust,no_run
# use motorsport_telemetry::{open, SourceExt};
# let recording = open("run.pds")?;
for diagnostic in recording.validate() {
    eprintln!("{diagnostic}");
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

The CLI prints the same report under `diagnostics:` in `inspect` (and as a JSON
array under `--json`). `verify` returns non-zero for a proven decode-layout
fault; ordinary warnings keep the file usable.

`SourceExt::normalizer()` is intended for sampling loops. It resolves
signal roles and track context once, and lazily computes lap metadata at most
once when lap progress needs that fallback.

## Core channels

`SourceExt::normalizer().sample(time_ns)` is the stable way to read the
driver-facing signals. Names are matched after stripping punctuation and case;
units are converted only when the registry can do so honestly. Missing or
incompatible inputs stay `None`.

| Role | `NormalizedSample` field | Unit | Typical source names |
|---|---|---|---|
| Speed | `speed_mps` | m/s | `Speed`, `Ground Speed`, `GPS Speed` |
| Throttle | `throttle_fraction` | 0–1 | `Throttle Pos`, `Throttle Pedal` |
| Brake | `brake_fraction` | 0–1 | `Brake Pedal Pos`, `Brake` |
| Clutch | `clutch_fraction` | 0–1 | `Clutch Pos`, `Clutch Pedal` |
| Steering | `steering_deg` | deg | `Steering Angle`, `SW Angle` |
| Gear | `gear` | count | `Gear`, `Gear Pos` |
| RPM | `rpm` | rpm | `RPM`, `Engine RPM` |
| Lap number | `lap_number` | count | `Lap Number`, `Current Lap` |
| Lap progress | `lap_progress` | 0–1 | see below |
| Current lap time | `lap_time_s` | s | `Lap Time`, `Current Lap Time` |
| Latitude | `latitude_deg` | deg | `GPS Latitude` |
| Longitude | `longitude_deg` | deg | `GPS Longitude` |
| Time of day | `time_of_day_ns` | ns since midnight | GPS/UTC clock, or VBOX time-of-day |
| Absolute time | `absolute_time_ns` | ns on the source clock | same clock + file-relative time |

Lap progress is the trickiest role. The normalizer tries, in order:

1. A lap-distance or lap-progress channel (`Lap Distance Corrected`, `%` progress, …).
2. GPS projected onto a matched track centerline.
3. Time through the current derived lap (`(t - lap.start) / lap.duration`). That last step is what you get from speed and time when there is no GPS: first recover lap bounds, then treat the lap as a time interval.

### How laps are recovered

Vendor files almost never agree on lap identity. Readers feed the same
heuristics in `read_source_metadata`:

1. Authoritative source laps (MoTeC LDX, a `.telemetry` catalog).
2. An incrementing counter. `Lap Number` is preferred when it actually counts
   (high-water ≥ 2). A 0/1 flag loses to `beaconEventCount` / `lap_beacon`
   counts. Shutdown resets are ignored.
3. A running timer or progress channel that resets (`Current Lap Time`,
   `Lap Progression`). When a counter *and* a lap timer both exist, the
   counter supplies the lap numbers and the timer supplies the boundaries:
   a 10 Hz counter changes a sample after the beacon, while the timer resets
   at the beacon and its first post-reset value says how long ago — so the
   crossing is recovered to the timer's resolution and lap durations agree
   with the logger's own reported lap times.
4. Otherwise no laps. We do not invent in/out from “first/last incomplete”.

The fastest lap is always one of those laps (the shortest plausible complete
one). It is never an interval rebuilt from a `Previous Lap Time` report, so a
source recording and its `.telemetry` conversion can never disagree about
which lap was fastest.

`.telemetry` stores the result in the header (`laps` plus the `valid_laps`
scalar) so later `read_laps` / `read_valid_laps` do not scan samples.

VBOX recordings that roll to a second video (`avifileindex` 1 then 2, files
`stem_0001.mp4` / `stem_0002.mp4`) keep both files in the catalog. Mapping at a
timestamp still uses the `avifileindex` / `avisynctime` channels; those stay
ordinary lossless columns. `video_reference_at` reports the active file index
and sync time. Video payloads stay in the MP4s; the catalog stores basename
plus BLAKE3 when the files were present at convert time. The full
telemetry-to-video-frame recipe for consumers is
[`docs/VIDEO_SYNC.md`](docs/VIDEO_SYNC.md).

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
    open, SourceExt,
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

The absolute clock comes, in order, from a range the format declares (MoTeC
date/time, VBOX), from GPS week + iTOW channels, or from a channel that logs
Unix-epoch seconds (Cosworth `Global Time`). A seconds channel is trusted
only when every value is a plausible date, it never runs backwards, and it
advances at the rate of the sample timeline.

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
