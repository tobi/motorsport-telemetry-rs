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

## Decoder-independent resolution and editors

Applications indexing standalone videos or discovery stubs can resolve the
same external metadata without opening a vendor decoder or reading recording
contents. These additive APIs are exported by `motorsport_telemetry`:

```rust,ignore
pub struct TrackMetadataLayers {
    pub layers: Vec<MetadataMap>,
    pub paths: Vec<PathBuf>,
}
impl TrackMetadataLayers {
    pub fn apply_to(&self, metadata: &mut MetadataMap);
}
pub fn load_track_metadata(path: impl AsRef<Path>, options: &OpenOptions)
    -> Result<TrackMetadataLayers, TrackMetadataError>;
pub fn load_track_directory_metadata(
    path: impl AsRef<Path>, options: &OpenOptions, exclude_target: bool,
) -> Result<TrackMetadataLayers, TrackMetadataError>;
pub fn read_track_metadata_document(path: impl AsRef<Path>, root_path: Option<&Path>)
    -> Result<MetadataMap, TrackMetadataError>;
pub fn driver_name_for_id(metadata: &MetadataMap, driver_id: f64) -> Option<&str>;
```

`MetadataMap` and the pure driver-name helper are available from both the
facade and core. `FileMetadata::driver_name_for_id` delegates to that helper,
including exact/fractional aliases, sorted alias precedence, wildcard fallback,
invalid-ID rejection and explicit null/blank masks.

`load_track_metadata` accepts any existing regular recording file, including
MOV, MKV and zero-byte remote discovery stubs. It returns defaults and matching
rule layers in evaluation order; control keys are removed. `paths` contains
one discovered `TRACK.yml` path per document read, root to leaf, including
empty documents. Directories in these paths are canonical, while a metadata
symlink retains its discovered name. Missing documents do not contribute.
Keep layers separate until applying them to embedded metadata:

```rust,no_run
use motorsport_telemetry::{load_track_metadata, MetadataMap, OpenOptions};
let options = OpenOptions {
    root_path: Some("/recordings/collection".into()),
    ..OpenOptions::default()
};
let external = load_track_metadata("/recordings/collection/event/clip.MOV", &options)?;
let mut embedded = MetadataMap::new(); // Use the recording's real embedded map.
external.apply_to(&mut embedded);
# Ok::<(), Box<dyn std::error::Error>>(())
```

Flattening external layers into a single map before merging with embedded
metadata loses masking information: parent `custom: null` followed by child
`custom: {new: true}` must remove embedded `custom.old`. `apply_to` merges each
layer separately, preserving that effect. It updates only the supplied map;
identity projection remains a separate `FileMetadata` operation.

`load_track_directory_metadata` requires an existing directory and yields
only its bounded ancestor defaults. Every override in each file read is
validated, but rules are neither evaluated against a pretend file nor inherited
as metadata. Set `exclude_target: true` to obtain the inherited defaults for a
folder editor, skipping the directory's own document. With no explicit root,
that leaves no ancestors to read. If the target equals the explicit root,
exclusion also returns no layers. Included documents still obey the same
ordering, null masks and unknown-field preservation as recording resolution.

`read_track_metadata_document` reads only the specified existing document.
It returns the raw JSON-compatible map, retaining `schema`, the full validated
`overrides` list, and unrelated fields for an editor's atomic serialization.
It never writes or walks ancestors. Empty/null documents return an empty map.
An explicit root must be a directory containing the canonical document; without
one, the document's containing directory is the boundary. A metadata symlink
escaping that boundary is rejected. YAML comments and formatting are not
represented in the returned map.

All three APIs use the existing byte, depth, node and expanded-text limits,
field validation and matcher policy. New recording/directory resolvers validate
the target and any explicit root even when `ignore_track_yml` is true, then
return empty layers/paths without reading external documents. No source
identity, driver IDs, channels, laps, clocks or video offsets are inferred or
changed by these APIs.

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
