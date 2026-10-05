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
