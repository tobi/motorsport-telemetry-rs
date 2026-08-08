# Telemetry fixtures

Small deterministic files are committed for every supported format.

`public-fixtures.json` contains only externally hosted fixtures whose format,
license, provenance, immutable URL, and SHA-256 have been verified. Run:

```sh
python tests/fixtures/download_public_fixtures.py
```

Downloaded files go to `tests/fixtures/public/`, which is ignored. Never add a
URL without explicit redistribution permission or an upstream license covering
the telemetry file itself.
