# motorsport-telemetry

Unified facade over AiM MP4, Cosworth PDS, MoTeC LD, and Racelogic VBO.

It exposes format detection, source-exact channels, normalized signal roles,
laps, driver stints, multi-file sessions, video references, WGS84 GPS, track
matching, and invariant lap progress.

```rust
use motorsport_telemetry::{open, motorsport_telemetry_core::TelemetrySource};

let file = open("run.mp4")?;
let metadata = file.metadata();
let track = file.match_track();
let roles = file.signal_roles();
let sample = file.normalized_sample(0, &roles, track.as_ref());
# Ok::<(), Box<dyn std::error::Error>>(())
```

Normalization is explicit and conservative: unsupported units or unavailable
signals remain `None`; they are never guessed.
