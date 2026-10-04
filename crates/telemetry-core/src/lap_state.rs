//! Event interpretation shared by counter recovery. The executable contract
//! is modelled in `specs/laps/LapState.tla`; signal confirmation happens before
//! this reducer. Neither elapsed time nor movement is a crossing.

use crate::LapKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Phase {
    #[default]
    Initial,
    OnTrack,
    Out,
    Stopped,
    Pit,
    Uncertain,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Event {
    Crossing,
    RejectedCrossing,
    Reset { stationary: bool },
    Rearm,
    Departure,
    TrackStop,
    PitEntry,
    End,
}

impl Phase {
    /// Returns the role of the interval being closed. `None` means that the
    /// event cannot establish a boundary (re-arming or another pit reset).
    pub(crate) fn apply(&mut self, event: Event) -> Option<LapKind> {
        let before = *self;
        match event {
            Event::PitEntry => {
                if matches!(before, Self::Pit | Self::Stopped) {
                    return None;
                }
                *self = Self::Pit;
                Some(match before {
                    Self::Initial | Self::Out => LapKind::OutIn,
                    Self::OnTrack => LapKind::In,
                    _ => LapKind::Uncertain,
                })
            }
            Event::TrackStop => {
                *self = Self::Stopped;
                Some(LapKind::Uncertain)
            }
            Event::Departure => {
                if matches!(before, Self::Pit | Self::Uncertain | Self::Stopped) {
                    *self = Self::Out;
                    Some(match before {
                        Self::Pit => LapKind::Pit,
                        Self::Stopped => LapKind::Stopped,
                        _ => LapKind::Uncertain,
                    })
                } else {
                    None
                }
            }
            Event::Rearm => None,
            Event::RejectedCrossing => {
                *self = match before {
                    Self::Pit => Self::Pit,
                    Self::Stopped => Self::Stopped,
                    _ => Self::Uncertain,
                };
                Some(match before {
                    Self::Pit => LapKind::Pit,
                    Self::Stopped => LapKind::Stopped,
                    _ => LapKind::Uncertain,
                })
            }
            Event::Crossing => {
                if !matches!(before, Self::Pit | Self::Stopped) {
                    *self = Self::OnTrack;
                }
                Some(match before {
                    Self::Initial | Self::Out => LapKind::Out,
                    Self::OnTrack => LapKind::Flying,
                    Self::Pit => LapKind::Pit,
                    Self::Stopped => LapKind::Stopped,
                    Self::Uncertain => LapKind::Uncertain,
                })
            }
            Event::Reset { stationary } => {
                if before == Self::Stopped {
                    return None;
                }
                *self = if stationary || before == Self::Pit {
                    Self::Pit
                } else {
                    Self::Uncertain
                };
                match before {
                    Self::Initial | Self::Out => Some(LapKind::OutIn),
                    Self::OnTrack => Some(LapKind::In),
                    Self::Pit | Self::Uncertain | Self::Stopped => None,
                }
            }
            Event::End => Some(match before {
                Self::Initial | Self::Out => LapKind::OutIn,
                Self::OnTrack => LapKind::In,
                Self::Pit => LapKind::Pit,
                Self::Stopped => LapKind::Stopped,
                Self::Uncertain => LapKind::Uncertain,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exhaustive_event_sequences_respect_the_model_contract() {
        fn walk(phase: Phase, depth: usize) {
            if depth == 0 {
                return;
            }
            for event in [
                Event::Crossing,
                Event::RejectedCrossing,
                Event::Reset { stationary: true },
                Event::Reset { stationary: false },
                Event::Rearm,
                Event::Departure,
                Event::TrackStop,
                Event::PitEntry,
                Event::End,
            ] {
                let mut next = phase;
                let output = next.apply(event);
                if output == Some(LapKind::Flying) {
                    assert_eq!(phase, Phase::OnTrack);
                    assert!(matches!(event, Event::Crossing));
                }
                if matches!(event, Event::Rearm) {
                    assert_eq!(next, phase);
                    assert_eq!(output, None);
                }
                if phase == Phase::Pit && !matches!(next, Phase::Pit | Phase::Stopped) {
                    assert!(matches!(event, Event::Departure));
                }
                if matches!(event, Event::TrackStop) {
                    assert_eq!(next, Phase::Stopped);
                    assert_ne!(output, Some(LapKind::Pit));
                }
                if matches!(event, Event::PitEntry) {
                    assert!(matches!(next, Phase::Pit | Phase::Stopped));
                    assert_ne!(output, Some(LapKind::Flying));
                }
                if phase == Phase::Stopped && !matches!(event, Event::Departure) {
                    assert_eq!(next, Phase::Stopped);
                    assert_ne!(output, Some(LapKind::Flying));
                }
                if matches!(event, Event::Reset { .. }) {
                    assert!(matches!(
                        next,
                        Phase::Pit | Phase::Uncertain | Phase::Stopped
                    ));
                }
                if matches!(event, Event::RejectedCrossing) {
                    assert!(matches!(
                        next,
                        Phase::Pit | Phase::Uncertain | Phase::Stopped
                    ));
                    assert_ne!(output, Some(LapKind::Flying));
                }
                if matches!(phase, Phase::Pit | Phase::Uncertain)
                    && !matches!(event, Event::Crossing)
                {
                    assert_ne!(next, Phase::OnTrack);
                }
                walk(next, depth - 1);
            }
        }
        walk(Phase::Initial, 7);
    }
}
