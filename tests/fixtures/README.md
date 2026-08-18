# Telemetry fixtures

Small deterministic files are committed for every supported format.

`synthetic_cosworth.pds` is a 5 Hz Cosworth log driven along the Road
America Full Course centerline from the offline `motorsport-track-atlas`
dataset (`crates/motorsport-track-atlas/data/tracks.jsonl`). It has an
out-lap from pit exit, three flying laps (lap 2 fastest), and an in-lap
that peels off toward pit entry. Speed, throttle, brake, and g-force
follow the straights and apexes; GPS is on that centerline.

`synthetic_motec_multilap.ld` and its `.ldx` companion exercise realistic
multi-rate lap telemetry: 100 Hz lap progression, 2 Hz lap state, partial
opening/closing laps, an invalidated lap, sidecar beacons, and a shutdown
counter reset. The rates and channel roles were informed by aggregate local
recording structure; all durations, samples, identities, and timing values are
invented. No proprietary data is embedded in the fixtures.

Regenerate all synthetic fixtures with:

```sh
python tests/fixtures/generate_fixtures.py tests/fixtures
```

`public-fixtures.json` contains only externally hosted fixtures whose format,
license, provenance, immutable URL, and SHA-256 have been verified. Run:

```sh
python tests/fixtures/download_public_fixtures.py
```

Downloaded files go to `tests/fixtures/public/`, which is ignored. Never add a
URL without explicit redistribution permission or an upstream license covering
the telemetry file itself.
