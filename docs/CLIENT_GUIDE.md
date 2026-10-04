# Client guide

External directory metadata is described in [TRACK.yml](TRACK_YML.md):
adjacent files load by default, optional roots enable bounded parent
traversal, and `ignore_track_yml` disables discovery. These fields merge
into the file summary without changing samples or clocks.

## The client contract: blessed channels, laps, video, clocks

Everything a client application needs is on this page. Two rules hold across
every format:

1. **Source-exact first.** `TelemetrySource::channels()` / `sample_at()`
   return what the file stored, in its own units (`Channel::unit`, provenance
   in `unit_source`). Nothing is renamed or rescaled behind your back.
2. **Blessed channels on top.** `SourceExt::normalizer().sample(time_ns)`
   returns a [`NormalizedSample`](../crates/motorsport-telemetry/src/lib.rs) with
   a fixed set of fields in fixed units, resolved from the source's channel
   names and units. A field is `Some` only when the library can stand behind
   the number; otherwise it is `None` — never a guess.

Everything below is in **file-relative integer nanoseconds** (`time_ns`,
`start_ns`, `end_ns`, `duration_ns`): zero is the first sample of the
recording, and there is no other time axis inside a file.

```rust,no_run
use motorsport_telemetry::{open, motorsport_telemetry_core::TelemetrySource, SourceExt};

let recording = open("run.mp4")?;                 // .pds .ld .vbo .mp4 .telemetry
let normalizer = recording.normalizer();
for lap in recording.metadata().laps.iter().filter(|lap| lap.kind.is_flying()) {
    let mid = (lap.start_ns + lap.end_ns) / 2;
    let s = normalizer.sample(mid);
    println!(
        "{:<8} {:>6.2}s  {:>5.1} km/h  thr {:.0}%  brake {:?} bar  gear {:?}  frame {:?}",
        lap.label(),
        lap.duration_ns as f64 / 1e9,
        s.speed_mps.unwrap_or(f64::NAN) * 3.6,
        s.throttle_fraction.unwrap_or(f64::NAN) * 100.0,
        s.brake_pressure_bar,
        s.gear,
        recording.video_frame_at(mid),
    );
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

### Blessed channels (`NormalizedSample`)

| Field | Unit | What it is | Resolved from (first match wins) |
|---|---|---|---|
| `speed_mps` | m/s | vehicle speed | `Ground Speed`, `Speed_Ref`, `Corr Speed`, `Vehicle_Speed`, `Speed_Wspd_App`, `vehRefSpeed`, `vCar`, `GPS Speed`, `Speed`, `velocity kmh` — first candidate in this order whose unit is declared *or* provable from its range wins (a dash's unitless wheel speed reaching 270 outranks a gappy GPS speed in m/s) |
| `throttle_fraction` | 0–1 | driver throttle demand (pedal), not throttle-plate | `Driver Throttle Pos`, `Throttle Pedal`, `Pedal_Pos`, `PPS`, `Throttle Pos`, `Throttle`, `TPS` |
| `brake_fraction` | 0–1 | brake **pedal position** | `Brake Pedal Pos`, `Brake Pedal`, `Brake Pos`, `Brake` |
| `brake_pressure_bar` | bar | brake **line pressure** (front or total) | `Driver Brake Pressure`, `Brake_Pressure_Front`, `P_F_BRAKE`, `P_Brake_Front`, `Brake Pressure` |
| `clutch_fraction` | 0–1 | clutch pedal | `Clutch Pos`, `Clutch Pedal`, `Clutch` |
| `steering_deg` | deg, sign as logged | steering-wheel angle | `Steering Angle`, `Steer Angle`, `Handwheel Angle`, `SW Angle`, `STEER`, `STEER_001` |
| `gear` | count | selected gear as logged (0/neutral and reverse conventions are the logger's) | `Gear Pos`, `Selected Gear`, `nGear`, `Gear` |
| `rpm` | rpm | engine speed | `Engine RPM`, `Eng Speed`, `RPM`, `nmot` |
| `lap_number` | count, 1-based | **virtual session lap**: strictly increasing across the whole recording and every stint (§ Laps). Falls back to the raw counter only when the recording has no laps at all | `FileMetadata::laps` |
| `stint_lap_number` | count | the number the dash displayed: the vendor counter, which restarts per stint (0 on an AiM out-lap). Falls back to the classified lap's `stint_lap` when there is no counter channel | `Lap Number`, `Lap_Number`, `Lap Count`, `Current Lap`, `Lap` |
| `stint` | 1-based | stint index of the containing lap | `FileMetadata::laps` |
| `lap_kind` | `LapKind` | `Flying`, `Out`, `In`, `OutIn`, `Pit` (§ Laps) | `FileMetadata::laps` |
| `lap_label` | text | `S1 out`, `S2 L3`, `S1 pit L5` | `FileMetadata::laps` |
| `lap_progress` | 0–1 | source-reported lap progress | a recorded percentage or ratio only; otherwise `None` |
| `lap_time_s` | s | running time within the current lap | `Current Lap Time`, `Lap Time`; otherwise `time_ns − lap.start_ns` of the classified lap |
| `latitude_deg` / `longitude_deg` | deg, WGS84, east-positive | GPS fix; pass-cleaned copies (`GPS Latitude Clean`) preferred | `GPS Latitude`/`Longitude`, `latitude`/`longitude` (VBOX arc-minutes are converted and west-positive longitude flipped) |
| `time_of_day_ns` | ns since local midnight on the source clock | wall clock | GPS / UTC clock, VBOX time-of-day |
| `absolute_time_ns` | ns on the source clock | `file_relative + clock_offset` | see § Clocks |

Name matching ignores case, spaces and punctuation (`Brake_Pressure_Front` ≡
`brakepressurefront`). The per-role result is visible: `normalizer.roles()`
tells you which channel index was chosen for each field, `normalizer.units()`
which unit it is read in.

**Units are never guessed from names.** Declared units convert through the
registry (`rad/s` → rpm, `Pa` → bar, `kmh` → m/s, arc-minutes → degrees).
Two documented exceptions make real dash data usable:

- **Cosworth pedals are angles.** Pi/Cosworth PDS stores `PPS`/`TPS` with the
  quantity code *angle*: 0.035–1.763 rad = 2–101 **degrees**, and the degree
  value is the percent. A pedal role whose channel declares an angle unit is
  read as `degrees / 100`.
- **Unitless channels are read by their value range, only where one reading
  is physically possible.** AiM `aimd` CAN echoes and VBOX CAN columns carry
  no unit string. For those channels (and only those) the normalizer probes
  the channel's range once (≤ 4096 samples) and decides:

  | Role | Range proves | Otherwise |
  |---|---|---|
  | pedal (throttle/brake/clutch) | max ≤ 1.05 → ratio; 1.05 < max ≤ 105 → percent | `None` |
  | steering | max magnitude > 2π (6.28) → degrees (radians of steering never get there) | `None` |
  | rpm | max > 3000 → rpm (rad/s tops out near 1000 ≈ 9550 rpm) | `None` |
  | lap time | max > 1000 → milliseconds, else seconds | — |
  | speed | max > 130 → km/h (m/s would be 468 km/h) | `None` (m/s and km/h overlap) |
  | brake pressure | never (bar and psi overlap) | `None` |

  `normalizer.units()` reports the inferred unit so a client can show or
  override it. To force a unit, pass a `channel_map` rule (DuckDB) or rename
  the channel yourself; the normalizer honours declared units first.

What each format gives you (Oreca 07 LMP2 data as measured):

| Field | AC sim MoTeC `.ld` | real MoTeC `.ld` export | Cosworth `.pds` | AiM `.mp4` (aimd) | VBOX `.vbo` |
|---|---|---|---|---|---|
| speed / throttle / steering / gear | ✓ | ✓ (gear absent in reduced exports) | ✓ (pedal = angle rule) | ✓ (range rule) | ✓ (range rule for steering) |
| brake_fraction | ✓ | ✓ | – (pressure only) | – | – |
| brake_pressure_bar | ✓ per wheel available raw | – | ✓ `P_F_BRAKE` Pa | – (unitless) | ✓ `bar` |
| rpm | ✓ | – | ✓ (rad/s) | ✓ (range rule) | ✓ |
| laps / stints / labels | ✓ (`Lap Progression` resets) | ✓ (LDX beacons) | ✓ (`Lap Number` + `Lap Time`) | ✓ (counter + timer, pit resets folded) | ✓ (GPS gate or counter) |
| lap_progress | channel | – | – (`Lap Distance` in metres remains raw) | – unless recorded | – unless recorded |
| latitude / longitude | – (world X/Y/Z only) | – | – (`FIA_Gps*` are flat) | ✓ | ✓ |
| video | – | – | – | ✓ | ✓ when the VBOX recorded video |
| time_of_day / absolute | ✓ (MoTeC date/time) | ✓ | ✓ (`Global Time`) | ✓ (GPS) | ✓ (GPS) |

`lap_progress` is only a unit conversion, not a positioning estimate. It
accepts finite source percentages/ratios in 0–1 (inclusive after conversion).
Metre-valued lap distance stays available as a raw channel; the normalizer
does not divide it by an atlas length, project GPS, or substitute elapsed lap
time. Track-progress estimation, sensor fusion, and comparison alignment
belong in consuming applications such as Omatrack.

`.telemetry` files carry the vendor channels that survive time alignment;
normalization uses the same rules when reading them.

### Laps (`FileMetadata::laps`)

`recording.metadata().laps` (or `read_lap_metadata(path)` without opening the
samples) is a `Vec<LapMetadata>`, sorted by `start_ns`, gap-free within a
stint:

| Field | Meaning |
|---|---|
| `number` | virtual session lap, 1-based, strictly increasing over the whole recording. **Use this to identify a lap.** |
| `stint` | 1-based stint index |
| `stint_lap` | the dash's own counter value (0 for an AiM out-lap), or the position in the stint when no counter exists |
| `kind` | `Flying` (beacon → beacon, moving) · `Out` (stint start → first beacon) · `In` (last beacon → stint end) · `OutIn` (a stint with no beacon) · `Pit` (beacon → beacon but ≥ 15 s standing still inside) |
| `label()` | `S1 out`, `S1 L2`, `S2 L3`, `S1 pit L5` — stint-local numbers match the dash |
| `start_ns`, `end_ns`, `duration_ns` | file-relative bounds; `complete` = both bounds are inside the recording |
| `first_video_frame` | presentation-order frame at `start_ns`, when video is linked |

`valid_laps` counts flying laps. `fastest_lap` is the shortest plausible
**flying** lap — an in-lap fragment cut by a pit-box counter reset, or a lap
with a stop in it, is never a candidate. Why this model exists: § Stints and
lap kinds.

### Video

For AiM MP4 recordings, VBOX video, and `.telemetry` converted from them:

| Call on any opened source | Returns |
|---|---|
| `video_files()` | `VideoFileRef { filename, index, blake3, frame_count, presentation_offset_ns }` per linked file |
| `video_frame_at(time_ns)` | presentation-order frame index for a telemetry instant |
| `video_presentation_time_ns(time_ns)` | player seek position (`telemetry_ns + presentation_offset_ns`) |
| `video_reference_at(time_ns)` | all of the above plus the source file index in one struct |
| `video_frame_count()` / `video_presentation_times_ns()` | the stored per-frame timestamp table (frame rate is *not* assumed constant) |

Never compute `time / frame_rate` yourself; MP4 edit lists shift the
presentation timeline per file (101.3 ms and 104 ms measured on back-to-back
files from one camera). The three rules are in
[`docs/VIDEO_SYNC.md`](VIDEO_SYNC.md).

### Clocks and identity (`FileMetadata`)

| Field | Meaning |
|---|---|
| `absolute_clock` / `absolute_start_ns` / `absolute_end_ns` | the source clock (`gps`, `utc`, MoTeC date+time) and the recording's span on it |
| `clock_offset_ns` | `absolute_ns = file_relative_ns + clock_offset_ns` |
| `utc_start_ns`, `timezone` | Unix-epoch ns at `t = 0` and the venue's IANA zone, when known (never invented) |
| `session_key` | groups files of one outing across formats (`gps:<week>:<schema hash>`) — see § Multi-file sessions |
| `identity` | driver, vehicle, venue, event, session, date, time as the file states them |
| `driver_ids` / `driver_stints` | internal driver identifiers and their intervals |
| `source_format` / `source_path` | for a `.telemetry`, the vendor format and path it was converted from |

### Cost model

| Call | Vendor file | `.telemetry` (zstd MTJ) |
|---|---|---|
| `read_metadata` / `read_lap_metadata` / `read_valid_laps` / `telemetry_format::read_channels` | mmap + index parse; speed probed for lap classification | **first two lines only** (~0.3 ms) |
| `open()` + `channels()` | mmap, no decode | full decompress + parse (~0.4 s / 9 MB) |
| `normalizer().sample()` | decodes the chosen channels at that instant | array lookup |

Header-only reads are what the DuckDB extension's `telemetry_file_metadata`,
`telemetry_laps` and `telemetry_metadata` use; a metadata scan never touches
channel data.

### Same contract in SQL

The [DuckDB extension](https://github.com/tobi/duckdb_motorsport_telemetry)
exposes the same model: `telemetry_laps(path)` is `FileMetadata::laps` with
one row per lap (`lap_number`, `stint`, `stint_lap`, `kind`, `label`,
`flying`, bounds), `telemetry_file_metadata(path)` is `FileMetadata`,
`telemetry_metadata(path)` the channel directory, and `read_telemetry(path,
channels := …)` the source-exact samples on a shared timeline.

### How laps are recovered

Vendor files almost never agree on lap identity. Readers feed the same
heuristics in `read_source_metadata`:

1. Source-provided laps (MoTeC LDX, an MTJ header, or VBO
   GPS-gate inference when usable; see below).
2. An incrementing counter. `Lap Number` is preferred when it actually counts
   (high-water ≥ 2). A 0/1 flag loses to `beaconEventCount` / `lap_beacon`
   counts. A drop that does not recover within 5 s is a **stint boundary**
   (see "Stints and lap kinds"); a drop that recovers is a transient.
3. A running timer or progress channel that resets (`Current Lap Time`,
   `Lap Progression`). When a counter *and* a lap timer both exist, the
   counter supplies the lap numbers and the timer supplies the boundaries:
   a 10 Hz counter changes a sample after the beacon, while the timer resets
   at the beacon and its first post-reset value says how long ago — so the
   crossing is recovered to the timer's resolution and lap durations agree
   with the logger's own reported lap times.
4. Otherwise no laps. We do not invent in/out from “first/last incomplete”.

VBO `[laptiming]` marks can recover crossings despite a CAN lap counter
resetting at driver changes. Those times are **inferred**, using a 50 m gate
and GPS-quality checks, not vendor-reported lap times. The reader emits
`vbo.laps_from_gate`; unreliable GPS falls back to counter/timer recovery.
See [the VBO reader](../crates/racelogic-telemetry/README.md) for limits.

Declared timer units take precedence over value magnitude: a 20-minute
Cosworth timer in seconds must not be mistaken for milliseconds. Resyncs to
large nonzero values are not crossings. Sampling inside a grid acquisition
gap returns `None`, not the following chunk's value.

Validation also flags missing laps during sustained motion and unusually
long moving laps. These are review hints, not proof of a decoder defect or
permission to fabricate missing boundaries.

### Stints and lap kinds

A vendor lap counter is a *stint* lap counter. An AiM dash resets
`Lap_Number` to 0 when the car stops in the box (after bumping it once to
close the running lap) and arms it 0 → 1 while still parked; a Cosworth
logger keeps counting straight through a pit stop; a power-cycled logger
starts again at 1. Read naively, the Indianapolis CT3 Run2 recording had one
364 s "complete" lap swallowing four flying laps and a 1:13 "fastest lap"
that was the in-lap cut short by the pit-box reset.

`FileMetadata::laps` is therefore normalised (`classify_laps`):

| Field | Meaning |
|---|---|
| `number` | **Virtual session lap**: 1-based, monotonic across the whole recording and every stint. Two consumers of the same file mean the same interval by "lap 7". |
| `stint` | 1-based stint index. A new stint starts at a counter reset, at more than 10 s of unrecorded time between laps, after an in-lap followed by an out-lap, and after a pit lap. |
| `stint_lap` | The counter as the dash showed it (0 for an AiM out-lap), or the position in the stint when no counter exists. |
| `kind` | `flying` (beacon to beacon, moving), `out` (stint start → first beacon), `in` (last beacon → stint end), `out-in` (a stint with no beacon), `pit` (beacon to beacon but standing still ≥ 15 s inside — an in+out the counter never separated). |
| `label()` | `S1 out`, `S1 L2`, `S1 in`, `S2 out`, `S2 L1`, …, `S1 pit L5` |

`valid_laps` counts flying laps. The fastest lap is always the shortest
plausible **flying** lap of the list; an in-lap fragment or a pit lap is
never a candidate, however short a broken beacon made it. `.telemetry`
(MTJ) stores `stint`, `stint_lap` and `kind` on each lap tuple. It is never an interval rebuilt from a `Previous Lap Time` report, so a
source recording and its `.telemetry` conversion can never disagree about
which lap was fastest.

`.telemetry` stores the result in the header (the laps line, with stints and
kinds) so `read_metadata` / `read_laps` / `read_valid_laps` / `read_channels`
are O(header): a zstd-MTJ document is streamed only through its first two
lines — 0.3 ms on a 9 MB, 700-channel recording versus 400 ms for a full
open. The header carries channel and
sample counts, the driver ids, the fastest lap and a channel directory
(`ch`) for exactly this purpose.

VBOX recordings that roll to a second video (`avifileindex` 1 then 2, files
`stem_0001.mp4` / `stem_0002.mp4`) keep both files in the header. Mapping at a
timestamp still uses the `avifileindex` / `avisynctime` channels; those stay
ordinary lossless columns. `video_reference_at` reports the active file index
and sync time. Video payloads stay in the MP4s; the header stores basename
plus BLAKE3 when the files were present at convert time. The full
telemetry-to-video-frame recipe for consumers is
[`docs/VIDEO_SYNC.md`](VIDEO_SYNC.md).

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
[`lap_stats.rs`](../crates/motorsport-telemetry/examples/lap_stats.rs):

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

Licensed under the [MIT License](../LICENSE). Track data has additional attribution
described in [ATTRIBUTION.md](../ATTRIBUTION.md).
