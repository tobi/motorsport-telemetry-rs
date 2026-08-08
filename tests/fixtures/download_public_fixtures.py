#!/usr/bin/env python3
"""Download pinned, licensed public telemetry fixtures after explicit opt-in."""
from __future__ import annotations

import argparse
import hashlib
import json
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent
MANIFEST = ROOT / "public-fixtures.json"


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, default=ROOT / "public")
    args = parser.parse_args()
    fixtures = json.loads(MANIFEST.read_text(encoding="utf-8"))
    args.output.mkdir(parents=True, exist_ok=True)
    for fixture in fixtures:
        target = args.output / fixture["name"]
        request = urllib.request.Request(
            fixture["url"], headers={"User-Agent": "motorsport-telemetry-rs-fixtures"}
        )
        with urllib.request.urlopen(request, timeout=60) as response:
            content = response.read()
        digest = hashlib.sha256(content).hexdigest()
        if digest != fixture["sha256"]:
            raise SystemExit(
                f"checksum mismatch for {fixture['name']}: {digest} != {fixture['sha256']}"
            )
        target.write_bytes(content)
        print(f"{target} ({len(content)} bytes, {fixture['license']})")


if __name__ == "__main__":
    main()
