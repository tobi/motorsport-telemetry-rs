#!/usr/bin/env bash
set -euo pipefail

export OMATRACK_CARGO_TARGET_DIR="${OMATRACK_CARGO_TARGET_DIR:-/tmp/motorsport-telemetry-rs-omatrack-target}"
export OMATRACK_REPETITIONS="${OMATRACK_REPETITIONS:-3}"
export OMATRACK_KEEP_RESULTS="${OMATRACK_KEEP_RESULTS:-0}"
exec scripts/bench-omatrack-folder-scan.sh
