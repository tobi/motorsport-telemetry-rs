# telemetry-format

Native `.telemetry` recordings: an aligned STORE zip whose first member is a
FlatBuffers catalog (`metadata.fb`) and whose remaining members are lossless
native channel columns.

[JSONL.md](JSONL.md) is the Motorsport Telemetry JSONL (MTJ) standard: a
compact, time-aligned interchange with a header, then laps, then one channel
per line. `telemetry-convert` writes it when the destination ends in
`.telemetry.jsonl`, `.jsonl`, `.mtj`, or those names plus `.zstd` / `.zst`.
The writer compresses with zstd level 11 by default. A destination ending in
`.telemetry.ext.jsonl` writes an MTX sidecar (header + channels, no laps).
Sidecars join on integer nanoseconds; header `utc` is required.

`FORMAT_VERSION` is stored in the catalog (`schema_version`). Current version is
`5` (spans + channel visibility; v4 added `utc_start_ns` + IANA `timezone`).
`NativeRecording::open` rewrites a writable older file in place. Header-only
reads do not. Clients can still call `file_needs_update` or `needs_update` for
a read-only file.

```sh
cargo run -p motorsport-telemetry --bin telemetry-convert -- recording.pds
```
