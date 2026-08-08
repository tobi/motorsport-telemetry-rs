# motorsport-track-atlas

Offline Rust access to the pinned `tobi/track-atlas` dataset.

```rust
use motorsport_track_atlas::match_track;

let matched = match_track(43.7978, -87.9899, 20_000.0).unwrap();
assert_eq!(matched.track.slug, "road-america");
assert_eq!(matched.layout.length_m, Some(6514.0));
```

Each layout exposes official length, direction, embedded centerline GeoJSON,
point layers such as corners/start-finish/pit markers, and range layers such as
timing sectors and complexes.

The dataset is committed and generated before publication, so crate builds are
offline and reproducible. Refresh it explicitly with:

```sh
python scripts/update_track_atlas.py /path/to/track-atlas
```

See the workspace `ATTRIBUTION.md` for ODbL and upstream data attribution.
