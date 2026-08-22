//! Lap-recovery strategies composed by [`crate::metadata::read_source_metadata`].
//!
//! Vendor files almost never agree on lap identity, so the pipeline follows a
//! fixed precedence (matching the README "How laps are recovered" section):
//!
//! 1. [`authoritative_laps`] — laps a source format supplies directly (MoTeC
//!    LDX, a `.telemetry` catalog).
//! 2. [`counter_laps`] — an incrementing counter. `Lap Number` wins only when
//!    it actually counts (high-water >= 2); a 0/1 flag loses to
//!    `beaconEventCount` / `lap_beacon` counts. Shutdown resets are ignored.
//! 3. [`timer_reset_laps`] — a running timer or progress channel that resets.
//! 4. Otherwise no laps.
//!
//! [`pick_laps`] applies that precedence; [`fastest_lap`] derives the fastest
//! complete lap from the chosen laps and any reported previous-lap channel.

use crate::metadata::{finite_i64, finite_u64, samples, LapMetadata, SourceLapMetadata};
use crate::{names, TelemetrySource};

const LAP_COUNTER_NAMES: &[&str] = &[
    "lapnumber",
    "lapnum",
    "lapcount",
    "lapcounter",
    "currentlap",
    "lap",
    "beaconeventcount",
    "beaconcount",
    "lapbeaconcount",
];

fn lap_counter_rank(name: &str) -> Option<usize> {
    LAP_COUNTER_NAMES
        .iter()
        .position(|wanted| names::eq(name, wanted))
}

fn is_completed_lap_counter(channel: &crate::Channel) -> bool {
    ["beaconeventcount", "beaconcount", "lapbeaconcount"]
        .iter()
        .any(|wanted| names::eq(&channel.name, wanted))
}

/// Returns the active lap number at `time_ns`, offsetting beacon counts.
///
/// `checked_add`/`checked_sub` drop the sample on `i64` overflow instead of
/// saturating: a counter that has run past `i64::MAX` is corrupt, not a lap.
pub(crate) fn counter_lap_number_at(
    source: &dyn TelemetrySource,
    channel_index: usize,
    time_ns: u64,
    previous: bool,
) -> Option<i64> {
    let value = source.sample_at(channel_index, time_ns, false)?;
    let value = finite_i64(value)?;
    let completed_count = is_completed_lap_counter(&source.channels()[channel_index]);
    Some(match (completed_count, previous) {
        // counter overflow: no lap number for this sample
        (true, false) => value.checked_add(1)?,
        (true, true) | (false, false) => value,
        (false, true) => value.checked_sub(1)?,
    })
}

/// Picks a lap-counter channel that actually increments.
///
/// File order must not win: Cosworth logs often have a `Lap Number` that only
/// toggles 0/1 while `beaconEventCount` counts crossings. A counter is *strong*
/// when it increments at least twice (high-water >= 2). Strong counters beat
/// weak ones; more crossings beat fewer; name-list order is the tie-break.
fn select_lap_counter(
    source: &dyn TelemetrySource,
    duration_ns: u64,
) -> (Option<usize>, Vec<LapMetadata>, usize) {
    let mut best_strong: Option<(usize, usize, usize, Vec<LapMetadata>)> = None;
    let mut best_weak: Option<(usize, usize, usize, Vec<LapMetadata>)> = None;
    let mut best_constant: Option<(usize, usize, usize, Vec<LapMetadata>)> = None;
    for (index, channel) in source.channels().iter().enumerate() {
        let Some(rank) = (channel.sample_count > 0)
            .then(|| lap_counter_rank(&channel.name))
            .flatten()
        else {
            continue;
        };
        let (laps, crossings) = increasing_counter_laps(source, index, duration_ns);
        if laps.is_empty() {
            continue;
        }
        let candidate = (rank, crossings, index, laps);
        if crossings >= 2 {
            if best_strong
                .as_ref()
                .is_none_or(|(rank0, crossings0, _, _)| {
                    crossings > *crossings0 || (crossings == *crossings0 && rank < *rank0)
                })
            {
                best_strong = Some(candidate);
            }
        } else if crossings == 1 {
            if best_weak
                .as_ref()
                .is_none_or(|(rank0, _, _, _)| rank < *rank0)
            {
                best_weak = Some(candidate);
            }
        } else if best_constant
            .as_ref()
            .is_none_or(|(rank0, _, _, _)| rank < *rank0)
        {
            best_constant = Some(candidate);
        }
    }
    let selected = best_strong.or(best_weak).or(best_constant);
    match selected {
        Some((_, crossings, index, laps)) => (Some(index), laps, crossings),
        None => (None, Vec::new(), 0),
    }
}

fn increasing_counter_laps(
    source: &dyn TelemetrySource,
    channel_index: usize,
    duration_ns: u64,
) -> (Vec<LapMetadata>, usize) {
    let channel = &source.channels()[channel_index];
    let completed_count = is_completed_lap_counter(channel);
    let number_offset = i64::from(completed_count);
    let mut laps = Vec::new();
    let mut current: Option<(i64, u64, bool)> = None;
    let mut high_water: Option<i64> = None;
    let mut crossings = 0;

    for (chunk_index, chunk) in channel.chunks.iter().enumerate() {
        for local_index in 0..chunk.sample_count {
            let value = source.decode(channel_index, chunk_index, local_index);
            let Some(counter) = finite_i64(value) else {
                continue;
            };
            if counter < 0 {
                continue;
            }
            let time_ns = source.sample_time_ns(channel_index, chunk_index, local_index);
            let Some(before) = high_water else {
                high_water = Some(counter);
                // counter + beacon offset overflowed i64: drop this sample
                let Some(number) = counter.checked_add(number_offset) else {
                    continue;
                };
                current = Some((number, time_ns, false));
                continue;
            };
            if counter <= before {
                // Shutdown resets and transient backwards values are not lap
                // crossings. Keep the high-water mark so a later 0 -> 1 does
                // not create a second, overlapping lap sequence.
                continue;
            }
            // counter + beacon offset overflowed i64: drop this crossing
            let Some(number) = counter.checked_add(number_offset) else {
                continue;
            };
            if let Some((prev_number, start_ns, start_known)) =
                current.replace((number, time_ns, true))
            {
                if prev_number > 0 && time_ns > start_ns {
                    laps.push(LapMetadata {
                        number: prev_number,
                        start_ns,
                        end_ns: time_ns,
                        duration_ns: time_ns - start_ns,
                        complete: start_known,
                        first_video_frame: None,
                    });
                }
            }
            high_water = Some(counter);
            crossings += 1;
        }
    }
    if let Some((number, start_ns, _)) = current {
        if number > 0 && duration_ns > start_ns {
            laps.push(LapMetadata {
                number,
                start_ns,
                end_ns: duration_ns,
                duration_ns: duration_ns - start_ns,
                complete: false,
                first_video_frame: None,
            });
        }
    }
    (laps, crossings)
}

/// Authoritative lap information supplied directly by a source format.
pub(crate) fn authoritative_laps(source: &dyn TelemetrySource) -> Option<SourceLapMetadata> {
    source.source_lap_metadata()
}

/// Lap boundaries from the best incrementing counter channel.
///
/// Returns the chosen channel index, the lap intervals, and the crossing count
/// (zero crossings still yields a single incomplete lap for a constant
/// counter, which [`pick_laps`] uses only as a last resort).
pub(crate) fn counter_laps(
    source: &dyn TelemetrySource,
    duration_ns: u64,
) -> (Option<usize>, Vec<LapMetadata>, usize) {
    select_lap_counter(source, duration_ns)
}

/// Lap boundaries inferred from a running timer or progress channel resetting.
///
/// `lap_channel_index` numbers the inferred laps from the selected counter when
/// available; otherwise laps are numbered by position. Reset detection treats
/// percentage progression and absolute timers separately. An inverted boundary
/// (`end < start`) is dropped instead of producing a zero-duration lap.
pub(crate) fn timer_reset_laps(
    source: &dyn TelemetrySource,
    duration_ns: u64,
    lap_channel_index: Option<usize>,
) -> Vec<LapMetadata> {
    let timer_resets = names::find(
        source.channels(),
        &[
            "currentlaptime",
            "lapcurrentlaptime",
            "laptime",
            "laptimerunning",
            "lapprogression",
            "lapprogress",
            "lapprogresspct",
        ],
    )
    .map(|index| {
        let values = samples(source, index);
        let is_progress = ["lapprogression", "lapprogress", "lapprogresspct"]
            .iter()
            .any(|wanted| names::eq(&source.channels()[index].name, wanted));
        let max_value = values
            .iter()
            .map(|(_, value)| *value)
            .filter(|value| value.is_finite())
            .fold(0.0_f64, f64::max);
        // A timer above 1000 at its peak is counting milliseconds; anything
        // smaller is seconds.
        let milliseconds = max_value > 1_000.0;
        let reset_threshold = if milliseconds { 5_000.0 } else { 5.0 };
        values
            .windows(2)
            .filter_map(|pair| {
                let before = pair[0].1;
                let after = pair[1].1;
                if !before.is_finite() || !after.is_finite() {
                    return None;
                }
                if is_progress {
                    let full_lap = if max_value > 2.0 { 100.0 } else { 1.0 };
                    return (before >= full_lap * 0.75 && after <= full_lap * 0.25)
                        .then_some(pair[1].0);
                }
                if before - after <= reset_threshold {
                    return None;
                }
                // The first sample after a reset already reads the time
                // elapsed since the beacon; the crossing itself was that much
                // earlier. Subtracting it recovers the beacon instant to the
                // timer's own resolution instead of the channel's sample
                // spacing, which is what makes the lap durations agree with
                // the logger's reported lap times.
                let elapsed_ns = if milliseconds {
                    after.max(0.0) * 1e6
                } else {
                    after.max(0.0) * 1e9
                };
                let elapsed_ns = finite_u64(elapsed_ns).unwrap_or(0);
                Some(pair[1].0.saturating_sub(elapsed_ns))
            })
            .collect::<Vec<_>>()
    })
    .unwrap_or_default();
    if timer_resets.is_empty() {
        return Vec::new();
    }
    let mut boundaries = Vec::with_capacity(timer_resets.len() + 2);
    boundaries.push(0);
    boundaries.extend(timer_resets);
    boundaries.push(duration_ns);
    let count = boundaries.len() - 1;
    boundaries
        .windows(2)
        .enumerate()
        .filter_map(|(index, pair)| {
            let number = lap_channel_index
                .and_then(|channel| counter_lap_number_at(source, channel, pair[0], false))
                .unwrap_or(index as i64 + 1);
            // inverted boundary: drop rather than report a zero-duration lap
            let duration_ns = pair[1].checked_sub(pair[0])?;
            (number > 0).then_some(LapMetadata {
                number,
                start_ns: pair[0],
                end_ns: pair[1],
                duration_ns,
                complete: index > 0 && index + 1 < count,
                first_video_frame: None,
            })
        })
        .collect()
}

/// Applies the lap-recovery precedence: authoritative > counter > timer.
///
/// A counter with zero crossings falls through to timer laps when any exist,
/// otherwise the counter's single incomplete lap (or empty vec) is kept.
pub(crate) fn pick_laps(
    authoritative: Option<&SourceLapMetadata>,
    counter_laps: Vec<LapMetadata>,
    counter_crossings: usize,
    timer_laps: Vec<LapMetadata>,
) -> Vec<LapMetadata> {
    if let Some(source_laps) = authoritative {
        source_laps.laps.clone()
    } else if counter_crossings > 0 {
        refine_with_timer(counter_laps, &timer_laps)
    } else if !timer_laps.is_empty() {
        timer_laps
    } else {
        counter_laps
    }
}

/// How far a counter crossing may sit from a timer reset and still be the
/// same beacon. A 10 Hz counter lags a 100 Hz timer by a sample or two; a
/// second covers that with room for a logger that stamps the counter late.
const TIMER_SNAP_WINDOW_NS: u64 = 1_500_000_000;

/// Moves every counter-lap boundary onto the nearest timer reset within
/// [`TIMER_SNAP_WINDOW_NS`], keeping the counter's lap numbers.
///
/// A lap counter only says which lap the car is on; it changes one sample
/// after the beacon at its own (often 10 Hz) rate. The lap timer resets *at*
/// the beacon and runs at 100 Hz, so where both describe the same crossing
/// the timer's instant is the boundary. Boundaries with no reset nearby (the
/// very first crossing of a recording that started mid-lap, a counter bump
/// the timer never saw) stay where the counter put them.
fn refine_with_timer(mut laps: Vec<LapMetadata>, timer_laps: &[LapMetadata]) -> Vec<LapMetadata> {
    if timer_laps.is_empty() {
        return laps;
    }
    let mut resets: Vec<u64> = timer_laps
        .iter()
        .filter(|lap| lap.complete || lap.start_ns > 0)
        .map(|lap| lap.start_ns)
        .collect();
    resets.sort_unstable();
    resets.dedup();
    let snap = |boundary: u64| -> u64 {
        let at = resets.partition_point(|reset| *reset < boundary);
        let candidates = [at.checked_sub(1), (at < resets.len()).then_some(at)];
        candidates
            .into_iter()
            .flatten()
            .map(|index| resets[index])
            .filter(|reset| reset.abs_diff(boundary) <= TIMER_SNAP_WINDOW_NS)
            .min_by_key(|reset| reset.abs_diff(boundary))
            .unwrap_or(boundary)
    };
    let count = laps.len();
    for (index, lap) in laps.iter_mut().enumerate() {
        // The recording's own edges are not crossings: a head fragment
        // starts where logging started and a tail fragment ends where it
        // stopped. Every other boundary is a beacon and is snapped — the
        // head fragment's *end* included, or it would overlap the lap that
        // follows by the counter's lag.
        let head = index == 0 && !lap.complete;
        let tail = index + 1 == count && !lap.complete;
        if !head {
            lap.start_ns = snap(lap.start_ns);
        }
        if !tail {
            lap.end_ns = snap(lap.end_ns);
        }
    }
    laps.retain(|lap| lap.end_ns > lap.start_ns);
    for lap in &mut laps {
        lap.duration_ns = lap.end_ns - lap.start_ns;
    }
    laps
}

/// Derives the fastest complete lap from the chosen laps.
///
/// Prefers an authoritative fastest lap. Otherwise the shortest plausible
/// complete lap *of the list itself*: a `Ref Lap Time` channel only bounds
/// what is plausible (half to one-and-a-half times the reference). It never
/// manufactures an interval from a `Previous Lap Time` report — that produced
/// a fastest lap that was in no lap list, so a recording and its own
/// `.telemetry` conversion disagreed about which lap was fastest.
pub(crate) fn fastest_lap(
    source: &dyn TelemetrySource,
    laps: &[LapMetadata],
    authoritative: Option<&SourceLapMetadata>,
) -> Option<LapMetadata> {
    let reference_lap_ns = authoritative
        .is_none()
        .then(|| {
            names::find(source.channels(), &["reflaptime", "referencelaptime"]).and_then(|index| {
                let values = samples(source, index);
                let max_value = values
                    .iter()
                    .map(|(_, value)| *value)
                    .filter(|value| value.is_finite())
                    .fold(0.0_f64, f64::max);
                let scale = if max_value > 1_000.0 {
                    1_000_000.0
                } else {
                    1_000_000_000.0
                };
                values
                    .into_iter()
                    .map(|(_, value)| value)
                    .find(|value| value.is_finite() && *value > 0.0)
                    .and_then(|value| finite_u64(value * scale))
            })
        })
        .flatten();
    let plausible_lap = |duration_ns: u64| {
        duration_ns >= 10_000_000_000
            && reference_lap_ns.is_none_or(|reference| {
                // checked_mul keeps the upper bound permissive on overflow
                // instead of falsely dropping a plausible lap.
                duration_ns >= reference / 2 && duration_ns <= reference.saturating_mul(3) / 2
            })
    };
    authoritative
        .and_then(|source| source.fastest_lap.clone())
        .or_else(|| {
            laps.iter()
                .filter(|lap| lap.complete && plausible_lap(lap.duration_ns))
                .min_by_key(|lap| lap.duration_ns)
                .cloned()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lap(number: i64, start_s: f64, end_s: f64, complete: bool) -> LapMetadata {
        let start_ns = (start_s * 1e9) as u64;
        let end_ns = (end_s * 1e9) as u64;
        LapMetadata {
            number,
            start_ns,
            end_ns,
            duration_ns: end_ns - start_ns,
            complete,
            first_video_frame: None,
        }
    }

    #[test]
    fn timer_resets_place_counter_boundaries() {
        // A 10 Hz counter changes 0.3 s after each beacon; the 100 Hz timer
        // resets at the beacon. Numbers come from the counter, instants from
        // the timer. The head fragment's start and the tail fragment's end
        // are recording edges and stay put; the head fragment's end is a
        // crossing and moves like any other.
        let counter = vec![
            lap(1, 0.0, 120.3, false),
            lap(2, 120.3, 240.3, true),
            lap(3, 240.3, 360.3, true),
            lap(4, 360.3, 400.0, false),
        ];
        let timer = vec![
            lap(0, 0.0, 120.0, false),
            lap(0, 120.0, 240.0, true),
            lap(0, 240.0, 360.0, true),
            lap(0, 360.0, 400.0, false),
        ];
        let refined = pick_laps(None, counter, 3, timer);
        let bounds: Vec<(i64, u64, u64, bool)> = refined
            .iter()
            .map(|lap| (lap.number, lap.start_ns, lap.end_ns, lap.complete))
            .collect();
        assert_eq!(
            bounds,
            vec![
                (1, 0, 120_000_000_000, false),
                (2, 120_000_000_000, 240_000_000_000, true),
                (3, 240_000_000_000, 360_000_000_000, true),
                (4, 360_000_000_000, 400_000_000_000, false),
            ]
        );
        assert!(refined
            .iter()
            .all(|lap| lap.duration_ns == lap.end_ns - lap.start_ns));
    }

    #[test]
    fn counter_boundaries_without_a_nearby_reset_are_kept() {
        let counter = vec![lap(1, 10.0, 130.0, true), lap(2, 130.0, 250.0, true)];
        // A lone reset far from every crossing is not the same beacon.
        let timer = vec![lap(0, 0.0, 60.0, false), lap(0, 60.0, 250.0, false)];
        let refined = pick_laps(None, counter.clone(), 2, timer);
        assert_eq!(refined, counter);
    }
}
