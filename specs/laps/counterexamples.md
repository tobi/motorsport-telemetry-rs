# Defects detected by the checked model

The runner requires all three mutations to produce TLC invariant violations.
These are abbreviated counterexample traces; TLC prints the complete variable
valuations when running either configuration directly.

## Re-arming counted as a crossing

Configuration: `BadArming.cfg` (`ArmAsCrossing = TRUE`).

| Event | Defective state | Defective output |
|---|---|---|
| Initial | Initial, no anchor | None |
| Counter reset | Pit/uncertain, no anchor | Close initial fragment |
| Re-arm | On track, anchor established | Close pit/uncertain interval |

`ArmingNeverClosesInterval` fails. A following crossing would fabricate a
flying lap beginning at re-arming. Inserting waits before re-arming makes no
difference to the correct transition: there is no elapsed-time exception.
The Road Atlanta qualifying recording re-arms 26 seconds after reset.

## Movement interpreted as pit exit

Configuration: `BadMovement.cfg` (`MovementExitsPit = TRUE`).

| Event | Defective state | Defective output |
|---|---|---|
| Initial | Initial, no anchor | None |
| Counter reset + independent zero speed | Pit, no anchor | Close initial fragment |
| Movement | On track, no anchor | None |

`MovementDoesNotExitPit` fails. The track state has no supported start
crossing; `TrackHasAnchor` would also fail. Moving through a pit lane or other
unidentified off-track activity cannot establish a new flying-lap anchor.

## Rejected candidate supplying a new anchor

Configuration: `BadRejected.cfg` (`RejectedAsCrossing = TRUE`).
A candidate contradicted by the reference-lap lower bound is counted as a
crossing and establishes a track anchor. `RejectedClearsAnchor` fails; the
next interval could incorrectly become a flying lap. The corrected reducer
keeps the candidate as uncertain activity and clears the anchor.

In `Race/SCHD0301.MP4` the dash reports a 75.356 s reference lap. Candidate
intervals of 22.798 s and 12.599 s are below half that reference, despite
counter increments and timer resets agreeing at their boundaries. They are
preserved as uncertain activity, excluded from valid laps, and followed by
an uncertain recording tail rather than a fabricated in-lap anchor.
