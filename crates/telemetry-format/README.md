# telemetry-format

Native `.telemetry` recordings: an aligned STORE zip whose first member is a
FlatBuffers catalog (`metadata.fb`) and whose remaining members are lossless
native channel columns.

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

`FORMAT_VERSION` is stored in the catalog (`schema_version`). Current version is
`10` (signed `int8` sample encoding, code 0; v9 pass provenance; v8 typed span
meta `timespan_ms` as u32le; v7 plot class / scale / rounding).
`NativeRecording::open` rewrites a writable older file in place. Header-only
reads do not. Clients can still call `file_needs_update` or `needs_update` for
a read-only file.

```sh
cargo run -p motorsport-telemetry -- convert recording.pds
```
