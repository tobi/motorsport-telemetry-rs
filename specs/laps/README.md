# Lap/pit recovery contract

`LapState.tla` describes the event interpretation in
[`lap_state.rs`](../../crates/telemetry-core/src/lap_state.rs), used by the
shared counter walk. TLC explores arbitrary event orderings, including
repeated resets, delayed arming, missing motion, timer glitches, FCY waits,
and recording termination. There is no circuit-specific minimum/maximum
lap time and no destination-specific state.

| Current state | Supported track crossing | Reset | Confirmed departure / track stop |
|---|---|---|---|
| Initial fragment | Close out fragment; establish anchor | Close out-in; clear anchor | Track stop enters stopped state |
| Out fragment | Close out fragment; establish anchor | Close out-in; clear anchor | Ordinary movement preserves state |
| On track, anchored | Close flying lap; establish next anchor | Close in fragment; clear anchor | Track stop clears anchor |
| Pit | Cannot establish an anchor without departure | Preserve interval | Confirmed departure closes pit activity and enters out |
| Stopped on track | Cannot establish an anchor without departure | Preserve stopped state | Confirmed departure enters out |
| GPS-confirmed pit entry | Closes in/out-in activity and clears the anchor; enters pit (stopped remains stopped) | No counter reset required | Confirmed GPS return enters out, never flying |
| Uncertain | Close uncertain activity; establish anchor | Preserve interval or upgrade with independent evidence | Confirmed departure enters out |

A counter reset plus independently observed zero wheel/car speed corroborates
pit activity unless native GPS locates the car on the circuit. An initially
parked recording may also have a pit head. A long standstill alone remains
uncertain: it cannot distinguish pits, an FCY stop, or a crash. GPS-located
standstill on the circuit is `stopped`, which describes observation rather than
cause. Locations alongside the pit sector remain ambiguous without pit geometry.

Departure requires an observed standstill followed by sustained motion at
least half this recording's native speed 80th percentile, and a subsequent
supported crossing or independently GPS-confirmed circuit outing ending at pit entry. The boundary is backdated to the last end of standstill.
It is a motion estimate, not a surveyed pit-exit gate. Slow movement or arming
without the crossing keeps pit activity continuous to recording end. The
first crossing closes an out fragment; only the following interval can fly.

Moving pit passes are confirmed separately from counter resets. The detector requires a slow native GPS route separated from the track lane observed at two fast dash crossings, with receiver-error margins, a displaced entry approach and a return to the circuit beyond the atlas exit. A slow circuit lap alone cannot identify pit time. Entry/exit timestamps are estimated at atlas markers, not surveyed lane gates. Missing fixes, acquisition gaps or absent track references prevent this inference. Currently this applies to pit sectors straddling atlas start/finish; other geometry remains an evidence limit.

A rejected candidate closes uncertain activity and clears the anchor; it
cannot start the following lap. Rejection keeps an existing pit state.

A flying interval needs two supported crossings with no confirmed reset
between them. Recording edges, arming and movement cannot supply an anchor.
Timer resets refine only crossing boundaries, never reset boundaries. The
current model is conservative when there is no independent pit-exit signal.

## Signal confirmation and scope

The input alphabet describes confirmed events, not raw samples. The Rust
adapter in `laps.rs` is responsible for:

- Ignoring backwards counter values that promptly recover and single-sample
  counter spikes. A sustained parked `1 -> 0 -> 1` sequence is reset/arming,
  even when it fits inside the ordinary glitch window.
- Treating active-counter `0 -> 1` as arming unless independent GPS at the
  calibrated track gate, or sufficient prior circuit motion/timer activation,
  corroborates the first physical crossing. Delay alone is insufficient.
  Completed-beacon counters have different semantics.
- Folding a pit-closing increment immediately followed by reset into the
  in fragment. This is vendor evidence, not a physical beacon assertion.
- Rejecting a timer dropout that resumes the old elapsed-time trajectory.
- Preferring counter reset evidence over timer fallback, including when the
  counter has no crossings. Once in pit/uncertain state, a timer alone does
  not restore track state.

- Rejecting a complete candidate shorter than half the recording's reference
  lap. This lower bound already protected fastest-lap selection; it now
  protects classification and valid-lap counts too. A rejected crossing
  clears the anchor. The next otherwise eligible crossing closes uncertain
  activity and reestablishes a start; only the following interval can fly.
  Timer-only recovery after rejection is allowed when no counter reset has
  made those timer events ineligible. No upper bound classifies FCY laps.

The model verifies confirmed-event ordering, not sensor interpretation or the
Rust implementation. Native GPS fix status, reported error, freshness and
atlas proximity are checked separately. The dash gate is calibrated from
several fast crossings near atlas start/finish: beacon coordinates need not
match the landmark exactly. A nearby parallel GPS crossing plus low native
wheel speed rejects a pit-lane beacon. GPS drift cannot veto a fast lap alone.
No GPS-derived progress channels or smoothing passes are introduced.

## Reproduce the check

Install Java 11+; use `JAVA=/path/to/java` when it is not on PATH. Then run:

```sh
scripts/check-lap-model.sh
cargo +1.97.1 test -p motorsport-telemetry-core -p aim-telemetry -p telemetry-format --lib
```

The script downloads the pinned TLA+ tools v1.7.4 jar (TLC 2.19) into the user cache and
checks its SHA-256. `TLA_JAR` can point to an existing copy. See the
[official tools documentation](https://github.com/tlaplus/tlaplus/blob/master/USE.md).
It first requires the correct model to pass, then requires each deliberately
broken configuration to fail with its intended invariant, not an arbitrary
checker/setup error:

- `BadArming.cfg`: treating re-arming after reset as a crossing violates
  `ArmingNeverClosesInterval`.
- `BadMovement.cfg`: letting movement leave pit state violates
  `MovementDoesNotExitPit`.
- `BadPitCrossing.cfg`: a pit counter event cannot skip departure (`PitExitRequiresEvidence`).
- `BadTrackStop.cfg`: a circuit stop cannot become pit state (`TrackStopNeverPit`).
- `BadPitEntry.cfg`: a GPS pit-entry event cannot become a track crossing (`ConfirmedPitEntryClearsAnchor`).
- `BadRejected.cfg`: treating a rejected candidate as a crossing violates
  `RejectedClearsAnchor`.

The checked configuration bounds executions to ten events, with the final
event forced to recording end. TLC checked **13,223,347 distinct states**
(**38,043,056 generated**) with all fourteen invariants passing. This is bounded
exhaustive model checking, not an unbounded proof or formal verification of
the Rust implementation. The Rust reducer has its own exhaustive
seven-event safety test; signal/metadata/reader regressions exercise the
adapter and persistence separately.

## Evidence on disk

`LapMetadata` exposes `start_boundary` and `end_boundary`:
`recording-edge`, `counter-crossing`, `timer-crossing`, `counter-reset`,
`rejected-crossing`, `stationary`, `motion-departure`, `gps-pit-entry`, `gps-pit-exit`, or `unspecified` for older/vendor metadata. Modern MTJ lap
headers persist them at tuple positions 8 and 9, preserving header-only
metadata reads. `uncertain` is a classified activity kind, excluded from
flying-lap statistics. Older files keep unspecified evidence; opening them
does not rewrite them.
