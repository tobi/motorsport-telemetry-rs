# aim-telemetry

Native reader for AiM Sports telemetry stored as an `aimd` track inside an ISO Base Media/MP4 recording.

## Design

- Memory-maps the MP4. It never reads or decodes the video or audio tracks.
- Locates telemetry by the `aimd` sample-entry FourCC, not by track number or the localized handler name.
- Resolves samples from standard `stsd`, `stts`, `stsc`, `stsz`, and `stco`/`co64` tables.
- Reads channel names, record IDs, widths, and layout from AiM `CHS` schema blocks.
- Reads scalar `(S … )` records using their declared record IDs and timestamps.
- Uses the modal recorded timestamp delta as each channel's native frequency, while retaining acquisition gaps as separate chunks.
- Rejects an MP4 immediately with `NoAimdTrack` when no `aimd` sample entry exists.

See the [format notes](https://github.com/tobi/motorsport-telemetry-rs/blob/master/crates/aim-telemetry/FORMAT.md)
for the documented MP4 tables, packet framing, `CHS` schema fields, scalar
record layout, and compatibility policy.

`GPS0` is decoded into geodetic position, speed, heading, accuracy, satellite, timing and status channels at its native 25 Hz. `LapPk` remains unexpanded: it is defined but has no payload in any of the five available recordings, while lap number and lap timing are carried by ordinary scalar channels. Scalar channels without a safely decoded unit remain unitless with `unit_source = unknown`.

The standalone library exposes `AimFile::open(path)` for local native MP4s;
it memory-maps the file. `AimFile::from_bytes(path, bytes)` is the equivalent
owned-buffer constructor for callers that already have MP4 bytes.

`aim_telemetry::read_metadata(path)` returns a fast `FileMetadata` summary,
including internal driver IDs, lap information, GPS session clock, schema hash,
and video-frame count. `read_metadata_from_bytes` provides the same summary for
owned input.

`GPS0` expands into 16 derived channels: `GPS Latitude`, `GPS Longitude`,
`GPS Altitude`, `GPS Speed`, `GPS Heading`, `GPS Satellites`,
`GPS Position Accuracy`, `GPS Speed Accuracy`, `GPS ECEF Velocity X`,
`GPS ECEF Velocity Y`, `GPS ECEF Velocity Z`, `GPS iTOW`, `GPS Week`,
`GPS DOP`, `GPS Fix Type`, and `GPS Fix Flags`. ECEF-derived values are only
reported for a u-blox position-bearing fix. MP4 edit lists provide the offset from
file-relative telemetry timestamps to the video's presentation timeline.

The reader does not decode video or audio, does not reconstruct `LapPk` without
a payload, and leaves scalar units unknown when the `CHS` record does not
provide a validated quantity. See the [format notes](https://github.com/tobi/motorsport-telemetry-rs/blob/master/crates/aim-telemetry/FORMAT.md) for the exact
packet and field compatibility rules.

```rust,no_run
use aim_telemetry::AimFile;
use motorsport_telemetry_core::TelemetrySource;

let recording = AimFile::open("smartycam.mp4")?;
for channel in recording.channels() {
    println!("{}: {:?} Hz", channel.name, channel.frequency_hz());
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

```sh
cargo run -p aim-telemetry --example inspect -- smartycam.mp4
```
