#!/usr/bin/env bash
set -euo pipefail

config="${OMATRACK_CONFIG:-${XDG_CONFIG_HOME:-$HOME/.config}/omatrack/omatrack.yml}"
repetitions="${OMATRACK_REPETITIONS:-3}"
target_dir="${OMATRACK_CARGO_TARGET_DIR:-/tmp/motorsport-telemetry-rs-omatrack-target}"
run_dir="$(mktemp -d /tmp/omatrack-folder-scan.XXXXXX)"
keep_results="${OMATRACK_KEEP_RESULTS:-0}"
completed=0

cleanup() {
    if [[ "$completed" == 1 && "$keep_results" != 1 ]]; then
        rm -rf -- "$run_dir"
        printf 'RESULT_DIR cleaned=%s\n' "$run_dir"
    else
        printf 'RESULT_DIR retained=%s\n' "$run_dir" >&2
    fi
}
trap cleanup EXIT

mkdir -p -- "$target_dir"
CARGO_TARGET_DIR="$target_dir" cargo build --release -p motorsport-telemetry --bin omatrack-folder-scan

arguments=(
    --config "$config"
    --run-dir "$run_dir"
    --repetitions "$repetitions"
)
if [[ "${OMATRACK_SKIP_INVENTORY_CHECK:-0}" == 1 ]]; then
    arguments+=(--skip-inventory-check)
fi

"$target_dir/release/omatrack-folder-scan" "${arguments[@]}" | tee "$run_dir/benchmark.log"
completed=1
