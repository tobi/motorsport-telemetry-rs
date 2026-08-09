# motec-telemetry

Standalone memory-mapped MoTeC `.ld` parser. It validates the LD header, walks channel metadata, decodes float and integer channels, and applies MoTeC scale/shift/multiplier conversion.

## Library contract

`MotecFile::open(path)` memory-maps a local LD and parses metadata and samples
without a read-then-copy staging buffer. When `run.ldx` exists beside `run.ld`,
the reader also uses its beacon markers and reported lap timing.
`MotecFile::from_bytes(path, bytes)` owns an in-memory LD buffer for embedded
callers; `MotecFile::from_bytes_with_ldx(path, bytes, ldx_bytes)` supplies the
same sidecar information without filesystem access.

`motec_telemetry::read_metadata(path)` returns the shared `FileMetadata`
summary, including embedded driver/vehicle/venue/date/time, lap information,
and an internal session candidate key. `read_metadata_from_bytes` provides the
same summary for owned LD input, while `read_metadata_from_bytes_with_ldx`
also accepts sidecar bytes.

```rust,no_run
use motec_telemetry::MotecFile;
use motorsport_telemetry_core::TelemetrySource;

let file = MotecFile::open("run.ld")?;
println!("{} / {} / {}", file.driver, file.vehicle, file.venue);
for channel in file.channels() {
    println!("{}: {:?} Hz", channel.name, channel.frequency_hz());
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

Sample parsing does not require the companion `.ldx`. Without one, shared
metadata derives lap boundaries from conventional lap counters, timers, or lap
progression resets. Increasing lap and beacon-count channels take precedence
over timer or progression heuristics. The writer creates an LDX alongside
every exported LD, recovering beacon markers from a dedicated lap trigger (or
an increasing lap counter as fallback) and carrying supplied session metadata:

```rust,no_run
use motec_telemetry::{write_motec, MotecMetadata};
# use motorsport_telemetry_core::TelemetrySource;
# fn export(source: &dyn TelemetrySource) -> Result<(), Box<dyn std::error::Error>> {
write_motec(source, &MotecMetadata::default(), "run.ld")?;
// Writes run.ld and run.ldx.
# Ok(())
# }
```

```sh
cargo run -p motec-telemetry --example inspect_motec -- run.ld
```
