# telemetry-format

Native `.telemetry` recordings: an aligned STORE zip whose first member is a
FlatBuffers catalog (`metadata.fb`) and whose remaining members are lossless
native channel columns.

```sh
cargo run -p motorsport-telemetry --bin telemetry-convert -- recording.pds
```
