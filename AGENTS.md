# Agent notes

## `.telemetry` format version

`FORMAT_VERSION` in `crates/telemetry-format/src/catalog.rs` is the on-disk
catalog version (`6`: pass provenance + preserved `source_format`/`source_path`
across rewrites; v5 added spans + per-channel visibility; v4 added
`utc_start_ns` + IANA `timezone`). Clients compare
`FileMetadata::format_version` (or `read_format_version`) against it.

When the catalog layout or required zip members change:

1. Bump `FORMAT_VERSION`.
2. Add a migration step in `crates/telemetry-format/src/migrate.rs` from the
   previous version. Keep older files readable until they are rewritten.
3. `NativeRecording::open` rewrites a writable older file in place after those
   steps. Header-only reads (`read_metadata`, `read_laps`, `read_valid_laps`,
   `read_format_version`) do not rewrite. A read-only file is left as-is and
   `needs_update` stays true.

Do not invent payload that was never stored. A v1 file without `video_frames.bin`
becomes a current-version file still without video; recover frames by converting
from the original vendor recording. Likewise the v5 -> v6 migration leaves
`passes` empty and `source_path` as found: provenance that predates v6 is
unknowable, not defaultable.

## Processing passes

`crates/telemetry-passes` holds the named, versioned, lossless pass registry
(`gps.quality`, `gps.clean`, `speed.distance`, ...). Passes only append
derived channels; `write_from_source_stripped` recovers the raw conversion
byte-for-byte. Provenance (`AppliedPass`: name, version, params, inputs,
outputs) is stored in the v6 catalog and the MTJ `passes` header key. Rules
when touching a pass: any change to its output values bumps its `version`;
new behavior with the same outputs is a new pass name; `check()` must give a
user-facing reason for every skip; keep `derive()` deterministic (no clocks,
no randomness). The design rationale lives in
`docs/WHY_POSITIONING_IS_HARD.md`.

## JSONL (MTJ)

`JSONL_VERSION` in `crates/telemetry-format/src/jsonl.rs` is independent of
`FORMAT_VERSION`. The document rules are `crates/telemetry-format/JSONL.md`.
A valid file is time-aligned: no per-sample timestamps, every `t0` / sample
instant / lap boundary / `dur` on the header lattice `q`. Irregular channels
are omitted, not given `[t, v]` pairs. Preferred names are `.telemetry.jsonl`
and `.telemetry.jsonl.zstd`. Writers compress with zstd level 11 by default
(`write_jsonl_from_source`); pass `compress: false` to
`write_jsonl_from_source_with` for raw UTF-8. Readers sniff the zstd magic
so a compressed frame still opens under a `.telemetry.jsonl` name.
Recording documents carry video linkage in the header (`vo` / `vf` /
`vpts`, `JSONL.md` §4.2): the same presentation offset, file references,
and frame timestamp table as the native catalog plus `video_frames.bin`,
so MTJ ↔ native round-trips sync bit-exactly. Sidecars MUST NOT carry
those keys.

An MTX sidecar (`.telemetry.ext.jsonl`) is header + records. The sidecar is
the group (header `n` + `vis`). Records are sample channels and/or spans.
There is no folder record. The primary key is integer nanoseconds: sample
times are file-relative; header `utc` (required) is Unix-epoch ns at that
file's `t = 0`. `tz` is display only. Join is
`host_file = ext_file + ext.utc − host.utc`. See `JSONL.md` §3 and §11.3.
