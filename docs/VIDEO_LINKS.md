# Companion video discovery

The facade exports `find_video_recording(video_path, root_path)` and
`resolve_linked_videos(recording_path, videos, root_path)`. Both accept existing
regular files, including zero-byte remote discovery stubs. Neither reads video
payloads or changes any source recording.

`find_video_recording` searches native VBO headers in the video's directory and
its immediate subdirectories. The `[AVI]` prefix and extension must declare the
selected basename, with a positive source roll index formatted to at least four
digits. Recording stems do not participate. Sample rows are never decoded.
The result contains a canonical recording path and the source roll index.
This establishes a catalog link, not proof that the recording has synchronized
samples for that roll; the native video timeline supplies that evidence.

Identical VBO copies are compared using bounded telemetry-only BLAKE3 reads and
choose the lexically first canonical path. Nonidentical matching recordings
produce `AmbiguousRecording`. A unique recording needs only its header read.

`resolve_linked_videos` is format independent. It searches the recording folder
and its parent for declared basenames. Results retain catalog order and source
indices. Duplicate catalog indices, unsafe names, missing files and multiple
matching paths are explicit errors; no partial result is returned. Stems are
exact and extensions are ASCII case insensitive. Two distinct files differing
only in extension case remain ambiguous, even on Linux. Symlink aliases of the
same canonical file are deduplicated.

With `root_path`, the existing canonical directory bounds both target and
candidate files. Without it, discovery is bounded to the video's parent;
resolution is bounded to the recording folder and its parent. Escaping file
symlinks are errors; discovery skips escaping directory symlinks. A video
symlink is resolved before discovery, so its canonical basename and directory
are used.

Limits are 4096 neighboring directory entries per call, 1 MiB per VBO header,
16 AVI header entries, and 128 MiB per duplicate telemetry file / 512 MiB total
duplicate comparison. These are discovery APIs, not content authentication:
matching a basename does not establish playable extent or media identity.
Consumers must independently validate the chosen media and native timeline.

## Bounded MP4 media inspection

`inspect_video_media(path, root_path)` uses the same canonical target/root
checks and returns `VideoMediaMetadata`. `byte_size` is the actual source size;
`video_streams` holds native track IDs, encoded frame counts and half-open
presentation extents in signed nanoseconds. Encoded frame counts include frames
excluded by edit lists. Extents come from native `stts`/`ctts` sample tables and
rate-one `elst` edits, including lead-in empty edits, clipping and multiple
media edits. Rational times are converted to integer nanoseconds. These values
are independent of VBO sync timestamps; the last reported `avitime` does not
define video duration and can exceed the measured media end.

The inspector seeks over `mdat`, reading only top-level box headers, `ftyp`
(64 KiB limit) and `moov` (32 MiB limit). Each container level has a 4096-box
limit, timing tables at most one million runs and edit lists at most 64 entries.
Run tables are combined without expanding arrays per frame. Fragmented MP4,
unsupported edit rates, malformed/count-inconsistent tables, absent video and
zero-byte stubs return explicit errors. Remote stub extent remains unknown
until real media metadata or the player supplies it. The facade exposes path
failures separately from container failures through `VideoMediaError`.

`header_fingerprint` is BLAKE3 over a versioned domain separator, source size,
and length-framed `ftyp`/`moov` bytes. It supports header/cache identity checks.
It deliberately ignores video payloads: equal-size files with identical
headers and different video content have the same fingerprint. It is not
content authentication or a substitute for a supplied whole-file digest.
Existing AiM clocks and frame-table readers are unchanged.
