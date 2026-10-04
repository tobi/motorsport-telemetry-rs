--------------------------- MODULE LapState ---------------------------
EXTENDS Naturals, Sequences, TLC

\* Events are confirmed/deglitched or explicitly rejected candidates.
\* CounterCrossing excludes
\* active-counter 0->1 arming, and pit-close increments followed by reset.
\* TimerCrossing is eligible only in the absence of usable counter/reset
\* evidence. This model does not assert that a sensor implies a physical
\* start/finish crossing or that a stop alone identifies a pit lane.
CONSTANTS MaxSteps, ArmAsCrossing, MovementExitsPit, RejectedAsCrossing, PitCounterAsCrossing, TrackStopAsPit, PitEntryAsCrossing
VARIABLES phase, anchor, epoch, anchorEpoch, now, from, intervals,
          ended, lastEvent, previousPhase, emitted, timerEligible, previousTimerEligible
vars == <<phase, anchor, epoch, anchorEpoch, now, from, intervals,
          ended, lastEvent, previousPhase, emitted, timerEligible, previousTimerEligible>>
Phases == {"initial", "out", "track", "pit", "uncertain", "stopped"}
Events == {"counter", "timer", "pit-reset", "reset", "arm", "move",
           "stop", "glitch", "tick", "rejected", "exit-confirmed", "track-stop", "initial-parked", "pit-entry-confirmed", "end"}
Kinds == {"out", "flying", "in", "out-in", "pit", "uncertain", "stopped"}

Init == /\ phase = "initial" /\ anchor = FALSE
        /\ epoch = 0 /\ anchorEpoch = 0 /\ now = 0 /\ from = 0
        /\ intervals = <<>> /\ ended = FALSE
        /\ timerEligible = TRUE /\ previousTimerEligible = TRUE
        /\ lastEvent = "init" /\ previousPhase = "initial" /\ emitted = FALSE

Crossing(e) == (e = "pit-entry-confirmed" /\ PitEntryAsCrossing)
              \/ (e = "counter" /\ (phase \notin {"pit", "stopped"} \/ (phase = "pit" /\ PitCounterAsCrossing)))
              \/ (e = "timer" /\ (phase \in {"initial", "out", "track"}
                  \/ (phase = "uncertain" /\ timerEligible)))
              \/ (e = "rejected" /\ RejectedAsCrossing)
              \/ (e = "arm" /\ ArmAsCrossing /\ phase \in {"pit", "uncertain"})
Reset(e) == e \in {"pit-reset", "reset"}
Boundary(e) == Crossing(e) \/ e \in {"end", "rejected", "track-stop"}
               \/ (e = "exit-confirmed" /\ phase \in {"pit", "uncertain", "stopped"})
               \/ (e = "pit-entry-confirmed" /\ phase \notin {"pit", "stopped"})
               \/ (Reset(e) /\ phase \in {"initial", "out", "track"})
ClosingKind(e) == CASE e = "track-stop" -> "uncertain"
                   [] phase = "stopped" -> "stopped"
                   [] phase = "pit" -> "pit"
                   [] phase = "uncertain" \/ e = "rejected" -> "uncertain"
                   [] Crossing(e) /\ anchor -> "flying"
                   [] Crossing(e) -> "out"
                   [] anchor -> "in"
                   [] OTHER -> "out-in"
NextPhase(e) == CASE Crossing(e) -> "track"
                 [] e = "track-stop" -> IF TrackStopAsPit THEN "pit" ELSE "stopped"
                 [] e = "initial-parked" /\ phase = "initial" -> "pit"
                 [] e = "exit-confirmed" /\ phase \in {"pit", "uncertain", "stopped"} -> "out"
                 [] phase = "stopped" -> "stopped"
                 [] e = "pit-entry-confirmed" -> "pit"
                 [] e = "rejected" -> IF phase = "pit" THEN "pit" ELSE "uncertain"
                 [] Reset(e) -> IF e = "pit-reset" \/ phase = "pit"
                                THEN "pit" ELSE "uncertain"
                 [] e = "move" /\ MovementExitsPit /\ phase = "pit" -> "track"
                 [] OTHER -> phase

Step(e) == /\ ~ended /\ now < MaxSteps
           /\ (now = MaxSteps - 1 => e = "end")
           /\ now' = now + 1
           /\ previousPhase' = phase /\ lastEvent' = e
           /\ previousTimerEligible' = timerEligible
           /\ timerEligible' = IF Reset(e) \/ e \in {"track-stop", "pit-entry-confirmed"} THEN FALSE ELSE IF e = "counter" THEN TRUE ELSE timerEligible
           /\ emitted' = Boundary(e)
           /\ intervals' = IF Boundary(e)
                 THEN Append(intervals,
                      [kind |-> ClosingKind(e), start |-> from, end |-> now + 1,
                       startCrossing |-> anchor, endCrossing |-> Crossing(e),
                       startEpoch |-> anchorEpoch, endEpoch |-> epoch])
                 ELSE intervals
           /\ from' = IF Boundary(e) THEN now + 1 ELSE from
           /\ phase' = NextPhase(e)
           /\ epoch' = IF (Reset(e) /\ phase \in {"initial", "out", "track"}) \/ e = "rejected" \/ e = "track-stop"
                        \/ (e = "pit-entry-confirmed" /\ phase \notin {"pit", "stopped"})
                        THEN epoch + 1 ELSE epoch
           /\ anchor' = IF Crossing(e) THEN TRUE
                         ELSE IF Reset(e) \/ e \in {"rejected", "track-stop", "pit-entry-confirmed"}
                              \/ (e = "initial-parked" /\ phase = "initial")
                              \/ (e = "exit-confirmed" /\ phase \in {"pit", "uncertain", "stopped"}) THEN FALSE ELSE anchor
           /\ anchorEpoch' = IF Crossing(e) THEN epoch ELSE anchorEpoch
           /\ ended' = (e = "end")

Next == \E e \in Events : Step(e)
Spec == Init /\ [][Next]_vars

TypeOK == /\ phase \in Phases /\ anchor \in BOOLEAN
          /\ now \in 0..MaxSteps /\ from \in 0..now
          /\ epoch \in 0..MaxSteps /\ anchorEpoch \in 0..epoch
          /\ ended \in BOOLEAN /\ emitted \in BOOLEAN
          /\ timerEligible \in BOOLEAN /\ previousTimerEligible \in BOOLEAN
          /\ \A i \in 1..Len(intervals): intervals[i].kind \in Kinds
FlyingNeedsTwoCrossings == \A i \in 1..Len(intervals):
    intervals[i].kind = "flying" =>
      (intervals[i].startCrossing /\ intervals[i].endCrossing
       /\ intervals[i].startEpoch = intervals[i].endEpoch)
NoFlyingAcrossReset == phase \in {"pit", "uncertain", "stopped", "out"} => ~anchor
ArmingNeverClosesInterval == lastEvent = "arm" => ~emitted
MovementDoesNotExitPit == lastEvent = "move" /\ previousPhase = "pit"
                          => phase = "pit"
TimerCannotExitReset == lastEvent = "timer"
                       /\ previousPhase \in {"pit", "uncertain"} /\ ~previousTimerEligible
                       => phase = previousPhase /\ ~emitted
RejectedClearsAnchor == lastEvent = "rejected" => ~anchor /\ phase \in {"pit", "uncertain", "stopped"}
PitExitRequiresEvidence == previousPhase = "pit" /\ phase \notin {"pit", "stopped"}
                          => lastEvent = "exit-confirmed"
TrackStopNeverPit == lastEvent = "track-stop" => phase = "stopped" /\ ~anchor
StoppedCannotFly == previousPhase = "stopped" /\ lastEvent # "exit-confirmed"
                    => phase = "stopped" /\ ~anchor
ConfirmedPitEntryClearsAnchor == lastEvent = "pit-entry-confirmed"
    => phase \in {"pit", "stopped"} /\ ~anchor
PositiveOrderedIntervals == \A i \in 1..Len(intervals):
    /\ intervals[i].end > intervals[i].start
    /\ intervals[i].end <= now
    /\ (i = 1 => intervals[i].start = 0)
    /\ (i > 1 => intervals[i].start = intervals[i-1].end)
EndCoversRecording == ended => Len(intervals) > 0
                              /\ intervals[Len(intervals)].end = now
TrackHasAnchor == phase = "track" => anchor
=======================================================================
