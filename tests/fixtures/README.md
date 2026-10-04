# Telemetry fixtures

Small deterministic files are committed for every supported format.

`synthetic_aimd_delayed_gps.mp4` is a five-minute AiM recording with
unitless `Speed_Wspd_App` car speed at 10 Hz and GPS at 1 Hz. GPS packets start at 180 s;
the first valid fix arrives at 183 s. The car remains stationary until
210 s, then drives at 90 km/h. A second GPS outage at 230–235 s tests
that moving gaps remain unknown. The MP4 contains telemetry only.
The AiM reader recovers coordinates from the first valid fix back to the
start using the recorded zero car speed; fix-status channels still start
at 180 s. Regenerate with `make_aimd_delayed_gps` in
`generate_fixtures.py`. Convert it to `.telemetry` to inspect the recovered
coordinates and unchanged receiver quality together.

```sh
python3 tests/fixtures/generate_fixtures.py /tmp/telemetry-fixtures
cp /tmp/telemetry-fixtures/synthetic_aimd_delayed_gps.mp4 tests/fixtures/
motorsport-telemetry convert tests/fixtures/synthetic_aimd_delayed_gps.mp4 tests/fixtures/synthetic_aimd_delayed_gps.telemetry
```

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

`synthetic_cosworth.telemetry` is the zstd-compressed MTJ conversion of
`synthetic_cosworth.pds`, without processing passes. Regenerate with:

```sh
motorsport-telemetry convert --no-passes tests/fixtures/synthetic_cosworth.pds tests/fixtures/synthetic_cosworth.telemetry
```
