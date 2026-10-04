#!/usr/bin/env bash
set -euo pipefail
repo=$(cd "$(dirname "$0")/.." && pwd)
cache=${XDG_CACHE_HOME:-$HOME/.cache}/telemetry-tla
jar=${TLA_JAR:-$cache/tla2tools.jar}
java=${JAVA:-java}
if [[ ! -f "$jar" ]]; then
  mkdir -p "$cache"
  curl --fail --location --silent --show-error \
    https://github.com/tlaplus/tlaplus/releases/download/v1.7.4/tla2tools.jar \
    --output "$jar"
fi
printf '%s  %s\n' 936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88 "$jar" | sha256sum --check --status
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cd "$repo/specs/laps"
run() {
  "$java" -XX:+UseParallelGC -Xmx2g -jar "$jar" -workers 2 \
    -metadir "$work/$1" -config "$1.cfg" LapState.tla
}
run LapState
for model in BadArming BadMovement BadRejected BadPitCrossing BadTrackStop BadPitEntry; do
  case "$model" in
    BadArming) invariant=ArmingNeverClosesInterval ;;
    BadMovement) invariant=MovementDoesNotExitPit ;;
    BadRejected) invariant=RejectedClearsAnchor ;;
    BadPitCrossing) invariant=PitExitRequiresEvidence ;;
    BadPitEntry) invariant=ConfirmedPitEntryClearsAnchor ;;
    BadTrackStop) invariant=TrackStopNeverPit ;;
  esac
  set +e
  run "$model" > "$work/$model.log" 2>&1
  status=$?
  set -e
  if [[ "$status" != 12 ]] || ! rg --quiet "Invariant $invariant is violated" "$work/$model.log"; then
    cat "$work/$model.log"
    exit 1
  fi
  printf 'Mutation %s: expected counterexample for %s detected.\n' "$model" "$invariant"
done
