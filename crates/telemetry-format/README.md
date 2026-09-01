# telemetry-format

The `.telemetry` file format. A `.telemetry` is a **zstd-compressed MTJ JSONL
document** (`write_telemetry`); the earlier aligned STORE zip with a
FlatBuffers catalog (`metadata.fb`) is the *legacy native* container, still
opened and migrated by `NativeRecording`, written only by `write_from_source`
/ `convert --native-zip`. `TelemetryRecording::open` / `open_telemetry` sniff
the first bytes (`sniff_container`) and dispatch; nothing is decided by name.

The workspace guide (layout + examples) is [TELEMETRY.md](../../TELEMETRY.md).
The writer-strict schema is [telemetry.schema.json](../../telemetry.schema.json).
[JSONL.md](JSONL.md) is the Motorsport Telemetry JSONL (MTJ) standard: a
compact, time-aligned interchange with a header, then laps, then one channel
per line. `motorsport-telemetry convert` writes it when the destination ends in
`.telemetry.jsonl`, `.jsonl`, `.mtj`, or those names plus `.zstd` / `.zst`.
The writer compresses with zstd level 11 by default. A destination ending in
`.telemetry.ext.jsonl` writes an MTX sidecar (header + channels, no laps).
An MTX reader also accepts another complete header later in the file to start
another folder. Sidecar groups join on integer nanoseconds; every header
requires `utc`.

`FORMAT_VERSION` is the **legacy zip** catalog version (`schema_version`); the
MTJ container is versioned by `JSONL_VERSION`. Current catalog version is
`10` (signed `int8` sample encoding, code 0; v9 pass provenance; v8 typed span
meta `timespan_ms` as u32le; v7 plot class / scale / rounding).
`NativeRecording::open` rewrites a writable older file in place. Header-only
reads do not. Clients can still call `file_needs_update` or `needs_update` for
a read-only file.

```sh
cargo run -p motorsport-telemetry -- convert recording.pds
```
