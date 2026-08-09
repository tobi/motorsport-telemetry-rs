#!/usr/bin/env bash
set -euo pipefail

export CARGO_TARGET_DIR="${OMATRACK_CARGO_TARGET_DIR:-/tmp/motorsport-telemetry-rs-omatrack-target}"
cargo fmt --all -- --check
cargo test --workspace
