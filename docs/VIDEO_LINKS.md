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

## Discovery from cached native catalogs

`VideoRecordingCatalog { recording_path, metadata_path, videos }` lets remote
consumers supply already-loaded native metadata while original VBOs remain
zero-byte mirror stubs. `find_video_recording_in_catalogs(video_path, catalogs,
root_path)` performs the same exact-stem / case-insensitive-extension match and
returns the original canonical `recording_path` and source index. It does not
read original recording rows, infer from renamed stems or fetch anything.
The caller supplies the candidate set and must obtain each catalog's `videos`
from its associated converted object; this function validates paths and catalog
names/indices, without parsing those objects again.

The canonical collection root applies to videos and original recording paths.
Native objects must be existing nonempty regular files but may live outside
that root in a shared cache. Multiple matching candidates select deterministically
only if their source indices agree and they share one canonical native object,
have byte-identical native objects, or match the path-independent native-content
fingerprint described below. Different native contents or conflicting source
indices are explicit ambiguity. Original zero-byte stub equality never
establishes duplicate identity. These checks establish converted-native identity,
not original source bytes or media payloads. Catalog objects must
actually be converted recordings; never supply a video as `metadata_path`.

Each call accepts at most 4096 catalogs and 4096 total video references. Distinct
native objects use the same 128 MiB/object / 512 MiB total candidate-size budget.
The normalized fallback has an additional shared 512 MiB decoded-byte budget.
Sharing one canonical native object needs no hash. Zero/no-video source indices
and duplicate indices are rejected by both catalog selection and resolution.

### Origin-independent native content

When encoded bytes differ, catalog selection can compare MTJ content using
`telemetry_format::native_recording_content_fingerprint(path, decoded_byte_limit)`.
It returns `NativeContentFingerprint { fingerprint, decoded_bytes }` or a typed
`NativeContentFingerprintError` (`Io`, `Invalid`, `LimitExceeded`). Only
`fingerprint` is identity; `decoded_bytes` includes ignored origin provenance and
is used to account for a caller's shared budget.

Only the documented **root header** `srcp` origin path is excluded. The writer
preserves it in each original converted document. All other root values,
including native clocks, catalog, source format/identity, placement, passes,
unknown fields and nested `srcp` metadata, remain significant. Header root keys
are sorted and length-framed; values retain exact raw JSON, preserving numeric
lexemes without floating-point conversion. Post-header bytes, including laps
and every channel record, are streamed verbatim into the fingerprint.

Compression representation and root-key order may differ. Whitespace within
values/body, number representations and nested-object order must still agree;
the comparator deliberately does not normalize arbitrary data. It never parses
channel samples, rewrites provenance, or establishes original-source/video
identity. Invalid or unprovable normalized content stays ambiguous. Bounds and
I/O failures remain explicit errors.

The format API accepts plain MTJ or zstd by magic. Limits are 128 MiB input,
32 MiB header, a 32 MiB zstd window, and decoded bytes capped at the caller's
limit or 512 MiB. It rejects duplicate root keys, MTX/unsupported header versions,
non-string `srcp` and absent bodies. This comparison verifies an MTJ envelope,
not full channel validity; callers supply already-converted native documents.

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

Range-capable consumers can call
`aim_telemetry::inspect_mp4_media_reader(&mut reader, byte_size)` with a caller-
owned `Read + Seek` adapter. The source size must be independently known. The
same upstream inspector chooses all reads/seeks and applies the same bounds;
it does not implement a separate remote box walker. It propagates transport
failures and never falls back to downloading the whole source. Read-at/Range
adapters remain responsible for their transport and source consistency.
The facade also exports `inspect_video_media_reader` with the same parameters
and the facade's `VideoMediaError` return type, so callers need no reader-crate
dependency.
