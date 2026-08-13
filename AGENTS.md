# Agent notes

## `.telemetry` format version

`FORMAT_VERSION` in `crates/telemetry-format/src/catalog.rs` is the on-disk
catalog version (`3`: lap `first_video_frame` + video-handle presentation
offset). Clients compare `FileMetadata::format_version` (or
`read_format_version`) against it.

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
from the original vendor recording.
