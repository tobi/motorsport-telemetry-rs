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

## Errors, recovery diagnostics, and plausibility

Constructors return format-specific `Result` errors when a source cannot be
opened safely. If a parser can recover but has to assume, clamp, substitute, or
drop data, the opened source reports that decision through
`TelemetrySource::diagnostics()`. Each `Diagnostic` has a stable dotted `code`,
`Info` / `Warning` / `Error` severity, a concrete message, and optional channel
attribution. Callers match `code`; they do not parse the message.

Parsing only validates structure. `validate_source` separately stride-samples
decoded values and reports non-finite, physically implausible, and widespread
absurd values without refusing the recording. `validate_source_with` also
accepts a source-file length for packed binary formats, where it can prove that
the claimed sample footprint is impossible. Do not supply that length for text
or packet formats that expand one stored value into several decoded channels.

```rust,no_run
use motorsport_telemetry_core::{validate_source, Severity, TelemetrySource};

# fn check(source: &dyn TelemetrySource) {
for diagnostic in source.diagnostics() {
    eprintln!("{diagnostic}");
}
let findings = validate_source(source);
if findings.max_severity() >= Some(Severity::Warning) {
    eprintln!("{findings}");
}
# }
```

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
with string labels. `channel_labels(i)` returns sparse comment dots on
**trace** channels only. `channel_display(i)` carries plot class (`trace`,
`gauge`, `compass`), optional scale, and rounding. Native `.telemetry` v8
and MTJ/MTX store the same model. Span hover times use `timespan_ms`.

`group_sessions(&files, max_gap_ns)` orders files by internal clocks and groups
only adjacent compatible candidates. It merges driver and lap fragments across
file boundaries while preserving real gaps. Filenames are never part of the
identity.

## Units

The shared registry normalizes aliases and rejects conversions across physical
dimensions instead of guessing. Every convertible unit is listed in
`telemetry.schema.json` (`$defs.unitCatalog`). `km/h` and `mph` (`mp/h`)
convert; `bar` and `psi` convert.

```rust
use motorsport_telemetry_core::{convert, normalize_unit};

assert_eq!(normalize_unit("kph"), Some("km/h"));
assert_eq!(normalize_unit("mp/h"), Some("mph"));
assert!((convert(36.0, "km/h", "m/s")? - 10.0).abs() < 1e-12);
assert!((convert(1.0, "bar", "psi")? - 14.503_773_773_020_923).abs() < 1e-9);
# Ok::<(), motorsport_telemetry_core::ConvertError>(())
```

`timespan_ms` is integer milliseconds (`u32`, max 100 hours = 360_000_000).
It converts with `s` / `ms` and renders as `M:SS.FFF`:

```rust
use motorsport_telemetry_core::{average_timespan_ms, format_timespan_ms};

assert_eq!(format_timespan_ms(110_332), "1:50.332");
assert_eq!(average_timespan_ms(&[110_332, 112_104]), Some(111_218));
```

## Names, storage, views, and placement

Channel-name matching, byte storage, derived-channel views, and absolute
placement are shared here so every parser and the facade reuse one
implementation:

- `names::{normalize, eq, contains, find}` match channel names after dropping
  punctuation/case. `find` returns the first channel matching any name in
  priority order.
- `storage::Storage` is a memory-mapped or owned `[u8]` backing buffer.
- `SampleType::decode_le`/`encode_le` and the free `sample_bytes`/`chunk_bytes`
  helpers decode and slice packed native samples with checked arithmetic.
- `SampleTimes` distinguishes grid-derived instants from explicit per-sample
  stamps; `TelemetrySource::sample_times` defaults to grid and the
  `sample_time_ns`/`sample_at` defaults dispatch on it.
- `view::ViewSource` exposes a subset of an inner source's channels plus
  in-memory appended channels that mirror an existing channel's layout and
  sample times.
- `placement::{resolve_timezone, resolve_utc_start_ns, utc_from_metadata,
  civil_ns_to_utc_ns}` resolve venue timezone and UTC start-of-file;
  `read_source_metadata` fills `utc_start_ns`/`timezone` from them.
