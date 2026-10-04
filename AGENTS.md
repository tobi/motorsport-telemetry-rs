# Agent notes

## Shared primitives (`crates/telemetry-core`)

Readers implement `TelemetrySource` and nothing beside it. Timing is one of
two models declared by `sample_times()`: `SampleTimes::Grid` (chunk
`time_base_ns + local * sample_period_ns`) or `SampleTimes::Explicit(&[u64])`
(one stamp per sample, channel-global index). `sample_time_ns` and
`sample_at` are core defaults dispatching on that; never override them. Other things that live in core and must not be re-implemented per crate:
`storage::Storage` (mmap/owned bytes), `SampleType::decode_le/encode_le` +
`sample_bytes`/`chunk_bytes` (checked packed-sample access; malformed
offsets decode to NaN, never panic), `names::{normalize,eq,contains,find}`
(channel-name matching), `ViewSource` (subset/reorder/append over another
source; passes and `write_telemetry_stripped` are built on it),
`placement` (UTC start + venue timezone), and blanket
`TelemetrySource` impls for `&T`/`Box<T>`/`Arc<T>` (the facade's
`TelemetryFile` is `Box<dyn TelemetrySource>`; cross-format helpers are the
`SourceExt` trait in `motorsport-telemetry`). Adding a trait method means
adding it to the blanket macro and `ViewSource` once.

## `.telemetry` documents

A `.telemetry` file is a zstd-compressed MTJ JSONL document. `write_telemetry`
and CLI `convert` write that by default. `JsonlRecording` is the sole recording
reader and detects compression from zstd magic, independent of the name.
Plain UTF-8 JSONL is also accepted. There is no ZIP/FlatBuffers container or
migration path; reconvert older archives from their original vendor sources.
Opening a recording never rewrites it.

## Metadata is O(header)

`telemetry_format::read_metadata` / `read_laps` / `read_valid_laps` /
`read_channels` and the facade `read_*` must never parse channel data. For MTJ
that means `JsonlRecording::read_header_metadata` / `read_header_channels`:
stream the zstd frame, stop after the laps line, answer from the header keys
`nc`/`nsc`/`ns`/`dids`/`fl`/`ch` (fall back to a full open only for documents
that predate them). Anything added to `FileMetadata` or needed by a metadata
table function must be written into the header too, or it silently turns the
metadata scan back into a full decode.

## Directory metadata (`TRACK.yml`)

The facade loads adjacent `TRACK.yml` by default; `OpenOptions::root_path`
enables ancestor traversal within a canonical directory boundary, and
`ignore_track_yml` disables external metadata. Root-to-leaf defaults and
ordered relative-path glob overrides merge into `FileMetadata::extra`. The
legacy YAML `schema` key is ignored. This is descriptive metadata, not
channels: project onto identity only AFTER native clock/placement derivation.
Never change UTC, session keys, samples, IDs, laps, or video offsets. Core
`extra_metadata` and `ViewSource` carry it through passes; MTJ stores it in
header `meta` for full and O(header) reads. See `docs/TRACK_YML.md`.

## Laps are stints

`FileMetadata::laps` is the normalised lap model (`telemetry-core/src/laps.rs`,
`classify_laps`): `number` is the virtual session lap (1-based, monotonic across
the recording), `stint` / `stint_lap` carry the stint index and the dash's own
counter value, `kind` is `LapKind` (`Flying`, `Out`, `In`, `OutIn`, `Pit`, `Uncertain`, `Stopped`),
`label()` renders `S2 L3` / `S1 in`. Vendor counters are *stint* counters: a
drop that does not recover within `RESET_CONFIRM_NS` is a stint boundary; the
AiM pit sequence (+1 to close the lap, → 0 parked, 0 → 1 armed) closes the
in-lap and enters pit/uncertain activity. Re-arming and delay alone never
supply a crossing. Native GPS at the calibrated atlas-near beacon gate or
independent circuit motion/timer activation may corroborate an actual initial
0 → 1 crossing. Pit requires confirmed departure then an out fragment before
flying laps resume. Departure is backdated from later circuit motion and a
supported crossing or independently GPS-confirmed circuit outing ending at
pit entry to the observed end of standstill; it is an estimate, not
a GPS pit-exit timestamp. Pit activity otherwise continues through movement.
A moving pit pass requires a separate slow GPS lane, a displaced entry
approach and a return beyond the atlas exit, calibrated against two fast
native dash crossings. GPS pit entry/exit cannot anchor flying laps.
A complete candidate below half the reference lap is `Uncertain`; rejected
crossings clear the anchor. Never apply an upper classification bound to FCY
laps. A long stop alone remains uncertain. Native GPS-located standstill on
the circuit is `Stopped` (no crash cause is inferred); it cannot become pit
state or skip departure. GPS drift, missing fixes and carried-back positions
cannot establish track gates. `valid_laps` and `fastest_lap` consider flying
laps only. Never select a fastest lap by duration alone, never treat a confirmed counter
reset as a glitch to skip, and never emit an unclassified lap from
`read_source_metadata`. MTJ stores stint/stint_lap/kind at tuple positions 5–7;
all readers expose the same classified lap model. Boundary evidence persists
at tuple positions 8–9. The reducer contract and reproducible TLC checks live
in `specs/laps/`; changes must preserve its invariants and regression tests.

## Processing passes

`crates/telemetry-passes` holds the named, versioned, lossless pass registry
(`gps.quality`, `gps.clean`, `speed.distance`). Passes only append
derived channels; `write_telemetry_stripped` recovers the raw conversion
byte-for-byte. Provenance (`AppliedPass`: name, version, params, inputs,
outputs) is stored in the MTJ `passes` header key. Rules
when touching a pass: any change to its output values bumps its `version`;
new behavior with the same outputs is a new pass name; `check()` must give a
user-facing reason for every skip; keep `derive()` deterministic (no clocks,
no randomness). Keep this library focused on source signals and simple,
local derivations. Track-progress estimation, sensor-fusion smoothing,
landmark matching, and cross-lap alignment belong in consumers such as
Omatrack, not this registry. The normalizer exposes source-reported progress
only; never substitute GPS projection or elapsed-time fraction for it.

## JSONL (MTJ)

`JSONL_VERSION` in `crates/telemetry-format/src/jsonl/mod.rs` versions recording
documents; `JSONL_EXT_VERSION` independently versions MTX sidecars. The user guide (layout + examples) is `TELEMETRY.md`. The writer-strict
schema is `telemetry.schema.json`. The normative JSONL rules are
`crates/telemetry-format/JSONL.md`.
A valid file is time-aligned: no per-sample timestamps, every `t0` / sample
instant / lap boundary / `dur` on the header lattice `q`. Irregular channels
are omitted, not given `[t, v]` pairs. Preferred names are `.telemetry.jsonl`
and `.telemetry.jsonl.zstd`. Writers compress with zstd level 11 by default
(`write_jsonl_from_source`); pass `compress: false` to
`write_jsonl_from_source_with` for raw UTF-8. Readers sniff the zstd magic
so a compressed frame still opens under a `.telemetry.jsonl` name.
Recording documents carry video linkage in the header (`vo` / `vf` /
`vpts`, `JSONL.md` §4.2): presentation offset, file references, and the
frame timestamp table, preserved bit-exactly when rewriting JSONL. Sidecars MUST NOT carry
those keys.

An MTX sidecar (`.telemetry.ext.jsonl`) is header + records. The sidecar is
the group (header `n` + `vis`). Records are sample channels and/or spans.
There is no folder record. The primary key is integer nanoseconds: sample
times are file-relative; header `utc` (required) is Unix-epoch ns at that
file's `t = 0`. `tz` is display only. Join is
`host_file = ext_file + ext.utc − host.utc`. See `JSONL.md` §3 and §11.3.
