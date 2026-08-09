# Omatrack full-folder scan Autoresearch

## Objective

Minimize the median end-to-end cold metadata scan metric `full_scan_ms` for the actual roots in `${XDG_CONFIG_HOME:-$HOME/.config}/omatrack/omatrack.yml` while preserving Omatrack's discovery, `.ldx` resolution, overlapping-root file trees, bounded prefix fingerprints, cache, metadata parsing, negative-video caching, and `TRACK.yml` hierarchy behavior. The retained implementation uses BLAKE3, as requested after the measured Autoresearch loop.

## Measurement

- Direction: lower is better.
- Limit: 12 measured attempts total, including baseline.
- Command: `.auto/measure.sh`
- Primary metric: `METRIC full_scan_ms=<milliseconds>` (median of three cold empty-metadata-cache scans).
- Secondary metrics: warm total and discovery, folder metadata, fingerprint, Rust summary, and cache serialization phases.
- Correctness: `.auto/checks.sh`, benchmark inventory assertions, cold/warm cache accounting, and all workspace tests.

## Constraints

- Treat all configured telemetry, video, and `TRACK.yml` inputs as immutable/read-only.
- Keep Cargo target output and all runtime manifests, caches, reports, and logs under `/tmp`.
- Do not drop kernel page cache.
- Keep parser changes generic and format-neutral; do not change public parser semantics or remove required metadata.
- Keep only measured improvements that pass checks; revert failed or slower experiments.
- Do not commit or push.

## Allowed files

The benchmark harness, workspace manifests/lockfile, and generic Rust parser/library implementation files when supported by a focused performance hypothesis.

## What's Been Tried

- Attempt 1 (checks failed): three unprimed repetitions drifted 36.1 s -> 22.8 s -> 9.2 s as filesystem-backed parser pages warmed. Add one untimed full cold-metadata-cache priming pass, as allowed by the benchmark handoff, before establishing the valid baseline.
- Attempt 2: valid full-open baseline 18,032.086 ms; summary parsing is 96.1%.
- Attempt 3 (diagnostic, reverted): MP4 owns 15,306.717 ms of 15,401.957 ms summary time (99.4%).
- Attempt 4: pre-reserving full-open AiM sample references improved the median to 17,019.856 ms.
- User clarified that index metadata must be quick and explicitly permits dropping an expensive summary field. Experiment with a bounded `AimFile::open_index` path that jumps through MP4 sample-table offsets, retains schema/channel previews/GPS, and deliberately omits full video lap and frame construction; preserve full `open()` behavior.
- Attempt 5: bounded `open_index` cut full scan to 256.281 ms; full workspace tests passed.
- Attempt 6: reusable 1 MiB fingerprint reads improved to 238.992 ms.
- Attempt 7: 19 index packets (matching GPS summary coverage) improved to 237.317 ms.
- Attempt 8: read-only mmap fingerprint prefixes improved to 218.040 ms with identical 476,820,927 bytes hashed.
- Attempt 9: retain only the 20 selected MP4 sample offsets in index mode improved to 213.785 ms.
- Attempt 10 (discarded): cursor lookup for selected sample indexes regressed to 215.723 ms; reverted.
- Attempt 11 (final verification): full workspace tests passed; repeat median was 222.206 ms and the retained best remains 213.785 ms. Results are retained at `/tmp/omatrack-folder-scan.A0yKcQ`.
- Post-session user override: replace SHA-256 with BLAKE3 for the local cache fingerprint and bump the namespace. The same path, size, primary prefix, sidecar prefix, and missing marker remain covered.
- Post-session lap-filmstrip requirement: metadata mode now walks all AiM telemetry packets only for recognized lap counters/timers, while unrelated previews remain bounded to 19 packets. On the changed live inventory (457 sources), the median is 1,022.910 ms; Rust summary parsing is the new 893.064 ms bottleneck. This is still 94.327% below the original 18,032.086 ms baseline and makes complete lap intervals available without a full multi-GB parse.

## Final Result

- Valid untouched-library baseline: 18,032.086 ms.
- Autoresearch best median: 213.785 ms (98.814% lower).
- Post-session BLAKE3 override: 195.055 ms (98.918% below baseline), warm 122.484 ms.
- BLAKE3 phases: fingerprint 110.011 ms, summary 71.550 ms, discovery 12.438 ms, folder metadata 0.193 ms, cache serialization 0.546 ms.
- Remaining bottleneck: required 476,820,927-byte BLAKE3 fingerprint pass.
- With complete AiM filmstrip laps enabled, lap-record scanning is the new dominant phase (893.064 ms of a 1,022.910 ms median on the current 457-source inventory).
