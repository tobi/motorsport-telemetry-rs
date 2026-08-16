# motorsport-telemetry

Unified facade over AiM MP4, Cosworth PDS, MoTeC LD, Racelogic VBO, and native `.telemetry`.

It exposes format detection, source-exact channels, normalized signal roles,
laps, driver stints, multi-file sessions, video references, WGS84 GPS, track
matching, and invariant lap progress.

| Extension | Parser |
|---:|---|
| `.mp4` | AiM `aimd` telemetry track |
| `.pds` | Pi/Cosworth PDS |
| `.ld` | MoTeC LD |
| `.vbo` | Racelogic VBOX |
| `.telemetry` | Native aligned STORE zip |
| `.telemetry.jsonl` / `.jsonl` / `.mtj` | Time-aligned MTJ interchange ([JSONL.md](../telemetry-format/JSONL.md)) |
| `.telemetry.jsonl.zstd` / `.zst` | Same document, one zstd frame |

Selection is case-insensitive and based on the extension. A recognized
extension with invalid contents returns the underlying parser error.

The crate also installs a fast, mmap-backed metadata command. It reads lap and
driver metadata, event date, video linkage, vehicle identity, GPS, and the
offline track match without decoding video payloads:

```sh
motorsport-telemetry inspect recording.mp4
motorsport-telemetry inspect --json recording.mp4
motorsport-telemetry inspect ~/Documents/Telemetry --mask '**/*.pds'
motorsport-telemetry help inspect
motorsport-telemetry convert recording.pds
motorsport-telemetry convert recording.pds recording.telemetry.jsonl
motorsport-telemetry verify recording.telemetry recording.telemetry.jsonl recording.telemetry.jsonl.zstd
```

For application indexes and lap filmstrips, use `open_metadata(path)` or the
smaller `read_lap_metadata(path)` convenience API. These return every derived
lap interval without requiring callers to know whether a format supplies
native lap packets, an LDX sidecar, counters, or timer resets. `open(path)`
remains the full signal-analysis path.

Unknown fields are printed as `unknown` (or JSON `null`) instead of being
guessed. In particular, one file can expose a session key but cannot prove that
other parts of the same session exist, and VBOX video links may provide only a
file index when the external filename is not stored in the telemetry. Event
dates prefer a real source clock, but fall back to file creation when that clock
is at least two years older or more than seven days newer.

```rust,no_run
use motorsport_telemetry::{open, motorsport_telemetry_core::TelemetrySource};

let file = open("run.mp4")?;
let metadata = file.metadata();
println!("{} channels", metadata.channel_count);
let normalizer = file.normalizer();
for time_ns in [0, 100_000_000, 200_000_000] {
    let sample = normalizer.sample(time_ns);
    println!("{:?}", sample.speed_mps);
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

Normalization is explicit and conservative: unsupported units or unavailable
signals remain `None`; they are never guessed. The reusable normalizer resolves
roles and track context once and caches lazily derived lap boundaries.

## Multi-file sessions

`open_sessions` opens each input, derives internal metadata, and joins adjacent
recordings only when their session keys and clocks are compatible within the
requested gap. It does not infer identity from filenames.

```rust,no_run
use motorsport_telemetry::open_sessions;

let sessions = open_sessions(["part-1.vbo", "part-2.vbo"], 5_000_000_000)?;
for session in sessions {
    println!("{} files, {} ns", session.files.len(), session.metadata.duration_ns);
}
# Ok::<(), Box<dyn std::error::Error>>(())
```
