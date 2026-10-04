# TRACK.yml: metadata beside recordings

The facade loads `TRACK.yml` beside a recording by default. It supplies names
and context the logger did not record, or overrides incorrect descriptive
metadata. It never changes channels, samples, driver IDs, lap boundaries,
session keys, UTC clocks, or video timing. Opening never writes anything.

## One file for a collection

```yaml
car:
  class: LMP2
driver:
  mappings:
    '1': Tobi
    '2': Another Driver
    '*': Guest Driver

overrides:
  - match: '2026/**'
    metadata:
      series: IMSA WeatherTech SportsCar Championship
      car:
        number: '11'

  - match: '2026/2026-07-10_IMSA_CTMP-2H40/**'
    metadata:
      event: CTMP 2H40
      track:
        name: Canadian Tire Motorsport Park
        slug: mosport
      archive:
        event_date: '2026-07-10'

  - match: '**/FP1/**'
    metadata:
      session: Free Practice 1
```

There is no required `schema` field. Legacy Omatrack `schema: '2'` entries
are accepted and ignored. Existing Omatrack folder metadata works unchanged;
its current parser needs rule-evaluation support to use the new `overrides`
list. It otherwise reads the defaults and preserves the unknown list.

## Discovery and precedence

| Options | Files read |
|---|---|
| Default | Only the recording directory's `TRACK.yml`, if present |
| `root_path` supplied | Every `TRACK.yml` on the ancestor chain, root through recording directory, inclusive |
| `ignore_track_yml: true` | No external metadata; metadata already embedded in the recording remains |

The supplied root must exist, be a directory, and contain the recording.
Canonical paths are used: `..` and symlinks cannot bypass the ancestry check.
The recording's canonical location determines its directory and match paths.
A metadata symlink must also resolve within the boundary (the supplied root,
or the recording directory without one). An explicit root is validated even
when `ignore_track_yml` is true.

Start from embedded metadata, then apply files **root to leaf**. Within each
file, apply the top-level defaults followed by matching `overrides` in list
order. A nearer file wins even over a more specific ancestor rule. Rules are
never inherited as rules or evaluated again in a child directory.

- Nested maps merge recursively; arrays and scalar values replace.
- Absent fields inherit. Blank strings do not erase inherited values.
- `null` explicitly masks a value. For example, `car: {number: null}` hides
  an inherited car number when a folder contains mixed cars.
- Unknown metadata fields are retained, including objects, arrays, booleans,
  numbers, and nulls. JSON-compatible YAML values are supported; custom YAML
  tags and non-finite numbers are rejected.
- Malformed files, invalid rules, unreadable files, and boundary violations
  return an error naming the offending path. Missing files are normal.
- Files are limited to 1 MiB and metadata nesting to 64 levels. Alias
  expansion is bounded during parsing: at most 65,536 values/keys and 4 MiB
  of text. A file must contain a single YAML document.

## Matchers

`match` is a string or a nonempty list of strings (any match qualifies).
Patterns are **case-sensitive**, anchored to the complete recording path
relative to the directory containing that `TRACK.yml`. Use `/` separators:

- `*.mp4`: MP4 files directly in this directory, not descendants.
- `**/*.mp4`: MP4 files here or in any descendant directory.
- `2026/**`: any recording below `2026`.
- `**/FP1/**`: any recording inside an `FP1` directory.
- `match: ['**/*.mp4', '**/*.vbo']`: either pattern.

`*` does not cross a separator; `**` does. `?`, character classes, and brace
alternatives are also supported by `globset`. Absolute paths, `..`, and
backslash separators are rejected. Matching tests the one recording path;
it does not enumerate subdirectories. Rules may contain only `match` and a
`metadata` mapping; nested `overrides` are not supported.

## Rust

```rust,no_run
use motorsport_telemetry::{open_with_options, OpenOptions};
use motorsport_telemetry::motorsport_telemetry_core::TelemetrySource;

let options = OpenOptions {
    root_path: Some("/recordings/collection".into()),
    ..OpenOptions::default()
};
let recording = open_with_options(
    "/recordings/collection/2026/event/FP1/run.mp4", &options,
)?;
let metadata = recording.metadata();
println!("{}", metadata.identity.session);
println!("{:?}", metadata.extra.get("car"));
println!("{:?}", metadata.driver_name_for_id(1.0));
# Ok::<(), Box<dyn std::error::Error>>(())
```

Use `open_metadata_with_options` for the vendor metadata-oriented loading
path, or `read_metadata_with_options` for summaries. MTJ summary reads still
stop after the header and laps, plus the bounded YAML files; channel data is
not parsed. Direct vendor/format readers do not discover `TRACK.yml`.

`FileMetadata.extra` contains the resolved structured metadata, without
`schema`, `overrides`, or matcher control fields. Recognized descriptive
fields also populate the existing identity:

| Metadata key | `FileMetadata.identity` field |
|---|---|
| `driver.name` | `driver` |
| `car.name` | `vehicle` |
| `track.name` | `venue` |
| `event` | `event` |
| `session` | `session` |
| `date` | `date` |
| `time` | `time` |

`metadata.source_identity` and `recording.identity()` still return native
identity. Effective identity is rebuilt from that base after each merge,
so removed embedded overrides cannot leave stale names behind. Source channel mapping
hints under `channels` are retained as metadata, not executed as transforms.
Track slugs are retained, not treated as GPS or evidence of a layout match.

`driver_name_for_id` consults `driver.mappings`: exact numeric keys win over
`'*'`; positive fractional codes such as `'2.5'` are supported. Invalid or
missing IDs do not use the wildcard, and a null exact name masks it. Names
are not injected into sample channels or used to rewrite numeric IDs.

`archive.event_date` describes the event; it is not the recording's start
clock. Neither it nor a `date`, `time`, `session`, or track-name override
changes time alignment or clock-based session grouping.

## CLI and persistence

```sh
motorsport-telemetry inspect run.mp4
motorsport-telemetry inspect --root-path /recordings/collection --json /recordings/collection/2026/event/FP1/run.mp4
motorsport-telemetry convert --root-path /recordings/collection /recordings/collection/2026/event/FP1/run.mp4 run.telemetry
motorsport-telemetry inspect --ignore-track-yml run.mp4
```

Inspection JSON includes `extra` and applies descriptive overrides to its
usual display fields. `verify` checks the stored document, not nearby YAML.

Converting to an MTJ recording persists resolved metadata in the optional
`meta` header object, alongside native identity and clocks. It remains
available after moving the converted file away from its original directory.
New adjacent YAML can overlay that embedded metadata. `--ignore-track-yml`
does not strip already embedded metadata, and `--strip-passes` removes only
derived channels, not file metadata. MTX sidecars currently do not persist
file-level `meta`; use MTJ recordings for an enriched archive.
