#!/usr/bin/env python3
"""Copy the compact track-atlas dataset from an explicit local checkout."""
from __future__ import annotations
import json
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DATA = ROOT / "crates" / "motorsport-track-atlas" / "data"


def main() -> None:
    if len(sys.argv) != 2:
        raise SystemExit(f"usage: {sys.argv[0]} /path/to/track-atlas")
    source = Path(sys.argv[1]).resolve()
    tracks = source / "tracks.jsonl"
    attribution = source / "ATTRIBUTION.md"
    if not tracks.is_file() or not attribution.is_file():
        raise SystemExit(f"{source} is not a track-atlas checkout")
    revision = subprocess.check_output(
        ["git", "-C", str(source), "rev-parse", "HEAD"], text=True
    ).strip()
    records = []
    for line in tracks.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        track = json.loads(line)
        for layout in track.get("layouts", []):
            centerline = layout.get("geometry", {}).get("centerline")
            if centerline:
                geometry_path = source / "tracks" / track["slug"] / "raw" / centerline
                layout["centerline_geojson"] = json.loads(
                    geometry_path.read_text(encoding="utf-8")
                )
        records.append(json.dumps(track, separators=(",", ":")))
    DATA.mkdir(parents=True, exist_ok=True)
    (DATA / "tracks.jsonl").write_text("\n".join(records) + "\n", encoding="utf-8")
    shutil.copyfile(attribution, DATA / "ATTRIBUTION.md")
    (DATA / "track-atlas-revision.txt").write_text(revision + "\n", encoding="utf-8")
    print(f"updated track-atlas data to {revision}")


if __name__ == "__main__":
    main()
