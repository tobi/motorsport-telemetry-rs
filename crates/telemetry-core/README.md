# motorsport-telemetry-core

Format-neutral channel, chunk, sample-type, and interpolation model shared by
the standalone parsers and the unified facade.

## Common parser contract

Every file parser exposes a concrete file type implementing
`motorsport_telemetry_core::TelemetrySource`:

- `path()` and `format()` identify the source.
- `channels()` returns exact metadata, native clocks, chunks, units, and sample counts.
- `decode()` reads one native sample.
- `sample_time_ns()` returns the native sample timestamp.
- `sample_at(channel, time_ns, linear)` samples by file-relative nanoseconds.
  Passing `false` uses step sampling; passing `true` interpolates continuous
  floating-point channels but still steps known state/counter channels.

Native `open(path)` methods memory-map local files where the format supports it.
`from_bytes(path, bytes)` is the in-memory entry point for embedded callers and
WASM-compatible parsers. The core crate itself performs no file I/O and owns no
format-specific parser.

## Fast metadata and sessions

`read_source_metadata(&source)` returns a format-neutral `FileMetadata`
summary: counts, schema hash, internal driver IDs/stints, lap fragments,
fastest complete lap, absolute clock range/session candidate key, UTC
start-of-file / IANA timezone when a later writer stamped them, and video
frame count when available.

`TelemetrySource::source_lap_metadata()` is only an optional native/sidecar
override. Consumers should read `FileMetadata::laps`, which also derives laps
from conventional counters and timers when that hook is absent.

`TelemetrySource::spans()` returns interval annotations (stints, yellows)
with string labels. Native `.telemetry` v5 and MTJ/MTX store the same model.

`group_sessions(&files, max_gap_ns)` orders files by internal clocks and groups
only adjacent compatible candidates. It merges driver and lap fragments across
file boundaries while preserving real gaps. Filenames are never part of the
identity.

## Units

The shared registry normalizes aliases and rejects conversions across physical
dimensions instead of guessing:

```rust
use motorsport_telemetry_core::{convert, normalize_unit};

assert_eq!(normalize_unit("kph"), Some("km/h"));
assert!((convert(36.0, "km/h", "m/s")? - 10.0).abs() < 1e-12);
# Ok::<(), motorsport_telemetry_core::ConvertError>(())
```
