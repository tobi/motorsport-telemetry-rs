# telemetry-format

Read and write MTJ recordings and MTX sidecars as time-aligned JSONL.
`.telemetry` is an MTJ document compressed with zstd at level 11.

- `JsonlRecording::open` / `from_bytes` detect compression by content.
- `write_telemetry` writes compressed recordings; `write_telemetry_stripped`
  removes derived pass outputs.
- `write_jsonl_from_source_with(..., false)` writes plain UTF-8 JSONL.
- `read_metadata`, `read_laps`, `read_valid_laps`, and `read_channels` use header
  summaries without parsing channel values. Documents without summaries and
  sidecar metadata require a full parse.
- MTX sidecars contain channels and/or spans, with UTC placement for joining.

Conversion aligns samples, rounds values, and omits irregular channels.
Video linkage and processing provenance live in the recording header.
`JSONL_VERSION` and `JSONL_EXT_VERSION` version recordings and sidecars.

See the [format guide](../../TELEMETRY.md), [JSONL specification](JSONL.md),
and [writer schema](../../telemetry.schema.json).
