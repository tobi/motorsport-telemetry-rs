# Video Sync: the Consumer Contract

How to put a telemetry sample on the right video frame — and nothing else.
One page, three rules. This preserves source video timing; aligning different
laps or recordings by track position is the consuming application's job.

## The two timelines

| Timeline | Zero | Who uses it |
|---|---|---|
| **Telemetry time** — file-relative nanoseconds | first sample of the recording | every channel sample, lap boundary, and span in the file |
| **Player time** — the video's presentation timeline | what the player shows at `0:00` | seek bars, frame extraction, overlay rendering |

They are *not* the same axis. MP4 edit lists (`elst`) shift the presentation
timeline by an amount the camera chooses per file — we have measured
101.333 ms and 104 ms on back-to-back recordings from the same camera family.
Video players apply that shift invisibly, which is why telemetry "looks
synced" in a player and drifts in any tool that reconstructs time by hand.

`.telemetry` stores the bridge, per recording and per video file:

- header `vpts` — presentation timestamp of **every frame**, in
  presentation order (frame rate is *not* assumed constant),
- header `vo` (`video_presentation_offset_ns`) — the telemetry→player shift
  (`player_ns = telemetry_ns + offset`),
- per-lap `first_video_frame` in `FileMetadata::laps`,
- per-file `VideoFileRef { filename, index, blake3, frame_count,
  presentation_offset_ns }` for multi-file recordings,
- optional BLAKE3 identities explicitly supplied by a source. Conversion
  preserves them and never reads whole video payloads merely to hash them,
- header `vmap` / `VideoTimeline`: native telemetry→file→presentation points
  for clocks that change at split-file rolls or are not a constant offset.

## The three rules

1. **Never do frame math.** No `time / frame_rate`, no "offset is probably
   zero", no "offset is the same as the last file". Variable frame timing and
   per-file edit lists break all three, each by more than a frame.
2. **Always go through the stored mapping.** All of it is one call away on
   any opened source (`TelemetrySource`):
   - `video_timeline()` → immutable normalized native clock, when supplied;
     `presentation_at(t)` returns `VideoPosition { file_index,
     presentation_time_ns }`; `telemetry_at(file_index, pts)` is its inverse;
   - `video_frame_at(telemetry_ns)` → frame index — filmstrips, thumbnails;
   - `video_presentation_time_ns(telemetry_ns)` → player seek position;
   - `video_reference_at(telemetry_ns)` → `VideoReference { file_index,
     presentation_time_ns, frame_index, .. }` — the multi-file-safe form;
   - when no native timeline exists, inverse (frame → telemetry):
     `video_presentation_times_ns()[frame] − video_presentation_offset_ns()`.
3. **Verify identity before trusting the pairing.** Match the video by the
   stored BLAKE3 (or at minimum basename + frame count). A `.telemetry` next
   to a re-encoded or trimmed MP4 is a different presentation timeline.

## Simple tasks, spelled out

- **Seek the player to a lap start**: `laps[n].start_ns` →
  `video_presentation_time_ns(start_ns)` → hand that to the player. Done.
  (For thumbnails, `laps[n].first_video_frame` is already precomputed.)
- **Overlay telemetry on frame `f`**: `t = video_presentation_times_ns()[f]
  − offset`, then `sample_at(channel, t, …)`. Render. The overlay is now on
  the frame the player would show at that instant.
- **VBOX two-file rolls**: call `video_reference_at(t)` and switch files on
  `file_index`, or use `video_timeline().presentation_at(t)` directly. VBOX
  uses its native integer milliseconds since each video file started, including
  nonzero initial PTS. The raw `avitime`/`avisynctime` channel is unchanged;
  the surplus builtin unit label `s` does not redefine the native clock.

## Native clock support and ambiguity

`VideoTimeline::from_segments` validates nonempty segments with positive file
indices, strictly increasing telemetry timestamps and nondecreasing PTS values.
Segments follow telemetry order without overlapping. Clock resets, file rolls,
invalid/no-video rows and missing-sample gaps split the VBOX timeline. Exact
duplicate observations are deduplicated in this cached mapping while all raw
samples remain unchanged. Conflicting duplicates or reversed telemetry time
disable the native mapping with a diagnostic.

Interpolation uses integer arithmetic inside one supported segment only. No
query extrapolates before/after observations or through a gap. Repeated PTS
values and overlapping PTS ranges after a same-file reset have an ambiguous
inverse: `VideoMappingError::Ambiguous`. Uncovered instants return `Unmapped`;
malformed supplied segments return `InvalidTimeline`. Do not replace a refused
inverse with an estimated constant offset or nominal frame rate.

`FileMetadata::video_timeline` and MTJ header-only reads retain exactly this
clock, independently of decoded channel arrays. The segment bounds describe
**recorded synchronization support**, not playable media duration. A real VBOX
roll can report a final old-file PTS about 66 ms beyond the MP4's stream end.
Check independently measured media extents before seeking and report unsupported
positions; never silently clamp or claim max-avitime is the media duration.

The native millisecond convention is documented in Racelogic's
[VBOX Video HD2 CAN output](https://en.racelogic.support/automotive/data-loggers/vbvdhd2/technical/can-output/).

## Which files can do this

| Container | Video sync? |
|---|---|
| Original vendor MP4 (AiM) | Yes — same API, computed from the container |
| MTJ (`.telemetry`, `.telemetry.jsonl`) | **Yes** — header keys `vo`/`vf`/`vpts` preserve offsets, video references, hashes, and frame timestamps exactly |
| MTX sidecars (`.telemetry.ext.jsonl`) | **No** — sidecars never carry video; the linkage belongs to the host recording |

## What sync does *not* depend on

Processing passes (`gps.quality`, `gps.clean`, `speed.distance`, …) clean
sensor data; they neither move samples in time nor touch the video clock
chain. The mapping above is written by every recording conversion,
with or without passes. If sync looks wrong, the suspect list is: raw frame
math somewhere downstream (rule 1), a mismatched video file (rule 3), or a
stale sidecar `utc` — not the passes.
