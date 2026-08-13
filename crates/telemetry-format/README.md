# telemetry-format

Native `.telemetry` recordings: an aligned STORE zip whose first member is a
FlatBuffers catalog (`metadata.fb`) and whose remaining members are lossless
native channel columns.

`FORMAT_VERSION` is stored in the catalog (`schema_version`). Current version is
`3` (`video_frames.bin`, `presentation_offset_ns`, per-lap `first_video_frame`).
`NativeRecording::open` rewrites a writable older file in place. Header-only
reads do not. Clients can still call `file_needs_update` or `needs_update` for
a read-only file.

```sh
cargo run -p motorsport-telemetry --bin telemetry-convert -- recording.pds
```
