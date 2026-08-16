# telemetry-passes

Named, versioned, lossless processing passes over any
`TelemetrySource`.

A pass reads existing channels and appends derived channels under new
names. It never mutates or resamples a source channel. Which passes ran —
name, version, parameters, inputs, outputs — is recorded as `AppliedPass`
provenance and persisted by the `.telemetry` writer, so a viewer can always
show how a file was processed and what had to be true for each pass to be
employed. Dropping the channels named in the recorded outputs recovers the
raw conversion byte for byte (`telemetry-format::write_from_source_stripped`).

Every pass declares its preconditions twice: as prose (`requirements()`)
for documentation and UI, and as code (`check()`) that inspects the actual
source and either reports `Ready` or `Skipped { reason }`. A skip reason
names the missing requirement — "no GPS coordinate channels present" — so
the resulting telemetry software can explain why a strategy was not
employed for a given source file.

Implemented registry: `gps.quality`, `gps.clean`, `speed.distance`.
Planned (documented, not yet implemented): `progress.project`,
`progress.fuse`, `landmark.damper`, `progress.time` — see `PLANNED`.
