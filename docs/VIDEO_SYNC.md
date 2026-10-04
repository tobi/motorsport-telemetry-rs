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
- BLAKE3 of each video so a consumer can verify it is pairing the
  telemetry with the exact file that was present at convert time.

## The three rules

1. **Never do frame math.** No `time / frame_rate`, no "offset is probably
   zero", no "offset is the same as the last file". Variable frame timing and
   per-file edit lists break all three, each by more than a frame.
2. **Always go through the stored mapping.** All of it is one call away on
   any opened source (`TelemetrySource`):
   - `video_frame_at(telemetry_ns)` → frame index — filmstrips, thumbnails;
   - `video_presentation_time_ns(telemetry_ns)` → player seek position;
   - `video_reference_at(telemetry_ns)` → `VideoReference { file_index,
     presentation_time_ns, frame_index, .. }` — the multi-file-safe form;
   - inverse (frame → telemetry): `video_presentation_times_ns()[frame] −
     video_presentation_offset_ns()`.
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
  `file_index`; each `VideoFileRef` carries its own offset.

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
