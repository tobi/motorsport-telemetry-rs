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
Counter resets enter pit or uncertain activity until a supported crossing
restores track state; delayed re-arming and movement cannot create laps.
Boundary evidence is available in metadata and MTJ headers. See the
[TLA+ recovery model and reproducible TLC checks](specs/laps/README.md).
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

## Corpus verification

`verify` audits native sources and converted recordings without writing them.
Directories are scanned recursively, with one JSON result per file:

```sh
motorsport-telemetry verify --track road-atlanta --layout gp --min-lap 72 --max-lap 120 --json weekend/ > road-atlanta-audit.jsonl
motorsport-telemetry verify --json /mnt/nas-home/Racing/collection/ > corpus-audit.jsonl
```

Omit the track for mixed venues; native GPS/venue supplies atlas matching.
Without an explicit minimum the atlas length divided by `--max-speed` is a
loose physical lower bound. `--max-lap` produces review findings, allowing FCY.
`--corridor` controls centerline tolerance. Missing GPS, uncertain activity,
missing counters/timers/motion, possible missed crossings, multiple circuit
tours merged into an out fragment (including unrecorded pit passes), and GPS drift are
reported explicitly. Impossible interval/state/physical-limit findings fail
with exit 1; review findings do not. Parsed recordings are not certifications
of physical accuracy. The stopped-on-track kind covers observed standstill
on the circuit without inferring crash cause. Moving pit passes can be separated by a corroborated native GPS lane even when the dash counter and timer never reset; atlas-marker boundary times remain estimates. See [the model](specs/laps/README.md).

The audit also reports informational recovery observations: transient timer or
counter dropouts, ignored counter rearming and pit-lane beacons, motion departures,
and moving GPS pit passes. These preserve evidence after successful normalization.
Stored annotations are compared against fresh native recovery when lap signals
exist; disagreement requests review rather than overwriting authoritative laps.
A parsed file with missing GPS or an unidentified track is not physically certified.
