//! Bounded, unit-aware motion estimates for plausibility checks and audits.
//!
//! These are estimates, not distance integrals. Missing data and invalid speed
//! values are not stationary observations. Long recordings are subsampled so
//! a corrupt duration cannot turn a validation check into an unbounded scan.

use crate::{convert, SampleTimes, TelemetrySource};

/// Maximum speed lookups per interval, independent of its reported duration.
const MAX_PROBES: u64 = 4096;
const SECOND_NS: u64 = 1_000_000_000;

/// Motion estimated from at most 4096 evenly spaced time bins.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct MotionSummary {
    /// Estimated time with a finite, plausible, unit-convertible speed.
    pub observed_ns: u64,
    /// Estimated time above 30 km/h, excluding missing/corrupt samples.
    pub moving_ns: u64,
    /// Highest inspected plausible speed in m/s; `None` means no evidence.
    pub top_speed_mps: Option<f64>,
}

/// Estimate motion in `[start_ns, end_ns)` using a speed channel.
///
/// Declared units must convert to m/s. Only finite speeds in `0..=150` m/s
/// count as evidence; larger values remain available to the value validator
/// but must not make an invalid sensor look like a moving car. Grid gaps and
/// stale explicit samples are excluded. Work is bounded even for `u64::MAX`
/// durations, and the last bin is weighted only by its actual length.
pub fn summarize_motion(
    source: &dyn TelemetrySource,
    speed: usize,
    start_ns: u64,
    end_ns: u64,
) -> MotionSummary {
    let mut result = MotionSummary::default();
    let Some(channel) = source.channels().get(speed) else {
        return result;
    };
    if convert(1.0, &channel.unit, "m/s").is_err() {
        return result;
    }
    let end_ns = end_ns.min(channel.duration_ns);
    let Some(duration) = end_ns.checked_sub(start_ns).filter(|&d| d > 0) else {
        return result;
    };
    let step = duration.div_ceil(MAX_PROBES).max(SECOND_NS);
    let max_age = channel.chunks.first().map_or(2 * SECOND_NS, |c| {
        c.sample_period_ns.saturating_mul(2).max(2 * SECOND_NS)
    });
    for bin in 0..duration.div_ceil(step) {
        // u128 avoids overflow for malformed/extreme durations.
        let start = (u128::from(start_ns) + u128::from(bin) * u128::from(step)) as u64;
        let width = step.min(end_ns - start);
        let at = start + width / 2;
        if let SampleTimes::Explicit(times) = source.sample_times(speed) {
            let Some(previous) = times.partition_point(|&t| t <= at).checked_sub(1) else {
                continue;
            };
            if at.saturating_sub(times[previous]) > max_age {
                continue;
            }
        }
        let Some(raw) = source.sample_at(speed, at, false).filter(|v| v.is_finite()) else {
            continue;
        };
        let Ok(mps) = convert(raw, &channel.unit, "m/s") else {
            continue;
        };
        if !mps.is_finite() || !(0.0..=150.0).contains(&mps) {
            continue;
        }
        result.observed_ns += width;
        result.top_speed_mps = Some(result.top_speed_mps.map_or(mps, |top| top.max(mps)));
        if mps > 30.0 / 3.6 {
            result.moving_ns += width;
        }
    }
    result
}

/// Speed below which the car counts as standing still, in m/s (1.8 km/h):
/// wheel-speed and GPS noise on a parked car stays under it, a pit-lane
/// crawl does not.
pub const STATIONARY_MPS: f64 = 0.5;

/// Longest contiguous stretch of `[start_ns, end_ns)` with the car standing
/// still, in nanoseconds, from a speed channel.
///
/// Bins are at most one second and at most [`MAX_PROBES`] per interval, so
/// the estimate resolves a stop to within a second on a normal lap and stays
/// bounded on a corrupt duration. A bin with no plausible speed sample
/// (grid gap, stale explicit sample, non-finite value) breaks a run rather
/// than extending it: missing data is not evidence of standing still.
pub fn longest_stop_ns(
    source: &dyn TelemetrySource,
    speed: usize,
    start_ns: u64,
    end_ns: u64,
) -> u64 {
    longest_stop_interval(source, speed, start_ns, end_ns).map_or(0, |(start, end)| end - start)
}

/// Longest contiguous standstill interval as `(start_ns, end_ns)`, so the
/// stop can be carved out of its lap instead of only measured. `None` means
/// the interval never stood still for a full sampled bin.
pub fn longest_stop_interval(
    source: &dyn TelemetrySource,
    speed: usize,
    start_ns: u64,
    end_ns: u64,
) -> Option<(u64, u64)> {
    let channel = source.channels().get(speed)?;
    if convert(1.0, &channel.unit, "m/s").is_err() {
        return None;
    }
    let end_ns = end_ns.min(channel.duration_ns);
    let duration = end_ns.checked_sub(start_ns).filter(|&d| d > 0)?;
    let step = duration.div_ceil(MAX_PROBES).max(SECOND_NS);
    let max_age = channel.chunks.first().map_or(2 * SECOND_NS, |c| {
        c.sample_period_ns.saturating_mul(2).max(2 * SECOND_NS)
    });
    let mut best: Option<(u64, u64)> = None;
    let mut run_start: Option<u64> = None;
    for bin in 0..duration.div_ceil(step) {
        let start = (u128::from(start_ns) + u128::from(bin) * u128::from(step)) as u64;
        let width = step.min(end_ns - start);
        let at = start + width / 2;
        let stale = match source.sample_times(speed) {
            SampleTimes::Explicit(times) => times
                .partition_point(|&t| t <= at)
                .checked_sub(1)
                .is_none_or(|previous| at.saturating_sub(times[previous]) > max_age),
            SampleTimes::Grid => false,
        };
        let stationary = !stale
            && source
                .sample_at(speed, at, false)
                .filter(|v| v.is_finite())
                .and_then(|raw| convert(raw, &channel.unit, "m/s").ok())
                .is_some_and(|mps| mps.is_finite() && (0.0..STATIONARY_MPS).contains(&mps));
        if stationary {
            let run_start = *run_start.get_or_insert(start);
            let run_end = start + width;
            if best.is_none_or(|(best_start, best_end)| run_end - run_start > best_end - best_start)
            {
                best = Some((run_start, run_end));
            }
        } else {
            run_start = None;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Channel, Chunk, SampleType, UnitSource};
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Source {
        channels: Vec<Channel>,
        value: f64,
        decodes: AtomicU64,
    }
    impl TelemetrySource for Source {
        fn path(&self) -> &str {
            "synthetic"
        }
        fn format(&self) -> &'static str {
            "synthetic"
        }
        fn channels(&self) -> &[Channel] {
            &self.channels
        }
        fn decode(&self, _: usize, _: usize, _: u64) -> f64 {
            self.decodes.fetch_add(1, Ordering::Relaxed);
            self.value
        }
    }
    fn source(value: f64, end: u64) -> Source {
        Source {
            value,
            decodes: AtomicU64::new(0),
            channels: vec![Channel {
                id: 0,
                name: "Speed".into(),
                unit: "km/h".into(),
                unit_source: UnitSource::Declared,
                sample_type: SampleType::F64,
                sample_count: end.div_ceil(SECOND_NS),
                duration_ns: end,
                chunks: vec![Chunk {
                    time_base_ns: 0,
                    sample_base: 0,
                    data_ptr: 0,
                    sample_count: end.div_ceil(SECOND_NS),
                    sample_period_ns: SECOND_NS,
                }],
            }],
        }
    }
    #[test]
    fn duration_is_bounded_and_fractional_tail_is_not_overcounted() {
        let s = source(180.0, u64::MAX);
        let summary = summarize_motion(&s, 0, 0, u64::MAX);
        assert_eq!(summary.moving_ns, u64::MAX);
        assert!(s.decodes.load(Ordering::Relaxed) <= MAX_PROBES);
        let s = source(180.0, 1_500_000_000);
        assert_eq!(
            summarize_motion(&s, 0, 0, u64::MAX).moving_ns,
            1_500_000_000
        );
    }
    #[test]
    fn invalid_speed_is_not_motion_or_stationary_evidence() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 3931.0] {
            assert_eq!(
                summarize_motion(&source(value, 20 * SECOND_NS), 0, 0, 20 * SECOND_NS),
                MotionSummary::default()
            );
        }
    }
    #[test]
    fn grid_gaps_are_not_filled_with_future_speed() {
        let mut s = source(180.0, 100 * SECOND_NS);
        s.channels[0].chunks[0].time_base_ns = 90 * SECOND_NS;
        s.channels[0].chunks[0].sample_count = 10;
        s.channels[0].sample_count = 10;
        let summary = summarize_motion(&s, 0, 0, 100 * SECOND_NS);
        assert_eq!(summary.observed_ns, 10 * SECOND_NS);
        assert_eq!(summary.moving_ns, 10 * SECOND_NS);
    }
}
