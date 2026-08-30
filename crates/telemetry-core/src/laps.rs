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

    // Only finite, non-negative samples take part; the look-ahead below needs
    // them as a flat list.
    let values: Vec<(u64, i64)> = samples(source, channel_index)
        .into_iter()
        .filter_map(|(time_ns, value)| {
            finite_i64(value)
                .filter(|counter| *counter >= 0)
                .map(|counter| (time_ns, counter))
        })
        .collect();

    for (position, &(time_ns, counter)) in values.iter().enumerate() {
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
        // A counter that jumps by more than one lap and is not held by
        // the very next sample is a corrupt sample (radio bit errors in
        // telemetry-received logs put a 1e9 into `Lap Number`). Taking
        // it would raise the high-water mark above every real lap that
        // follows and silence lap detection for the rest of the file.
        if counter - before > 1
            && values
                .get(position + 1)
                .is_some_and(|&(_, next)| next < counter)
        {
            continue;
        }
        // counter + beacon offset overflowed i64: drop this crossing
        let Some(number) = counter.checked_add(number_offset) else {
            continue;
        };
        if let Some((prev_number, start_ns, start_known)) = current.replace((number, time_ns, true))
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
    let timer_resets = timer_channel(source)
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
            // Stored units win: a 20-minute timer in seconds is not a
            // millisecond timer. Keep the legacy magnitude fallback only for
            // unitless dash/CAN exports (e.g. AiM Current_Lap_Time).
            let seconds_per_unit =
                timer_seconds_per_unit(&source.channels()[index].unit, max_value);
            // The first sample after a genuine reset reads at most one sample
            // period plus logger latency. A drop to a value of many seconds is
            // a *resync* — the dash adopting a lap time from elsewhere (AiM
            // `Current_Lap_Time` falling from 8.7 h of power-on count to
            // 126.65 s) — not a beacon, and "correcting" by that much would
            // place a boundary minutes off or at `t = 0`.
            let period_ns = source.channels()[index]
                .chunks
                .first()
                .map_or(0, |chunk| chunk.sample_period_ns);
            let max_elapsed_ns = 2_000_000_000u64.saturating_add(period_ns.saturating_mul(2));
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
                    let seconds_per_unit = seconds_per_unit?;
                    if after < 0.0 || (before - after) * seconds_per_unit <= 5.0 {
                        return None;
                    }
                    // The first sample after a reset already reads the time
                    // elapsed since the beacon; the crossing itself was that much
                    // earlier. Subtracting it recovers the beacon instant to the
                    // timer's own resolution instead of the channel's sample
                    // spacing, which is what makes the lap durations agree with
                    // the logger's reported lap times.
                    let elapsed_ns = finite_u64(after * seconds_per_unit * 1e9)?;
                    if elapsed_ns > max_elapsed_ns {
                        return None;
                    }
                    Some(pair[1].0.saturating_sub(elapsed_ns))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut timer_resets = timer_resets;
    timer_resets.retain(|&at| at > 0 && at < duration_ns);
    timer_resets.sort_unstable();
    timer_resets.dedup();
    if timer_resets.is_empty() {
        return Vec::new();
    }
    let mut boundaries = Vec::with_capacity(timer_resets.len() + 2);
    boundaries.push(0);
    boundaries.extend(timer_resets);
    boundaries.push(duration_ns);
    let count = boundaries.len() - 1;
    // The counter is sampled slower than the timer and changes a sample or
    // two *after* the beacon. A fragment that begins at a reset the counter
    // has not caught up with yet (typically the tail of a recording that
    // stopped right after a crossing) would repeat the previous number; a
    // timer reset is a crossing, so the number must advance.
    let mut last_number: Option<i64> = None;
    boundaries
        .windows(2)
        .enumerate()
        .filter_map(|(index, pair)| {
            let counted = lap_channel_index
                .and_then(|channel| counter_lap_number_at(source, channel, pair[0], false))
                .unwrap_or(index as i64 + 1);
            let number = match last_number {
                Some(previous) if counted <= previous => previous.checked_add(1)?,
                _ => counted,
            };
            last_number = Some(number);
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

/// Convert declared timer units, retaining magnitude inference only when no
/// unit was stored. Unknown non-empty units must not be interpreted as time.
fn timer_seconds_per_unit(unit: &str, max_value: f64) -> Option<f64> {
    if unit.trim().is_empty() {
        Some(if max_value > 1_000.0 { 0.001 } else { 1.0 })
    } else {
        crate::convert(1.0, unit, "s").ok()
    }
}

/// The running lap-timer / lap-progress channel [`timer_reset_laps`] reads.
pub(crate) fn timer_channel(source: &dyn TelemetrySource) -> Option<usize> {
    names::find(
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
}

/// Sample period of a channel's first run, or zero when it has none.
pub(crate) fn channel_period_ns(source: &dyn TelemetrySource, index: Option<usize>) -> u64 {
    index
        .and_then(|index| source.channels().get(index))
        .and_then(|channel| channel.chunks.first())
        .map_or(0, |chunk| chunk.sample_period_ns)
}

/// How far a counter crossing may sit from a timer reset and still be the
/// same beacon, given the two channels' sample periods.
///
/// The counter changes up to one of its own samples after the beacon; the
/// timer's first post-reset sample (whose elapsed value is subtracted to
/// recover the beacon instant) can itself be a full timer period late. Both
/// lags add, on top of a fixed allowance for a logger that stamps the
/// counter late. With 1 Hz `Lap Number` and `Lap Time` (Cosworth dash
/// channels) that is 3.5 s; with 10 Hz / 100 Hz it stays near 1.6 s.
pub(crate) fn snap_window_ns(counter_period_ns: u64, timer_period_ns: u64) -> u64 {
    TIMER_SNAP_WINDOW_NS
        .saturating_add(counter_period_ns)
        .saturating_add(timer_period_ns)
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
    snap_window_ns: u64,
) -> Vec<LapMetadata> {
    if let Some(source_laps) = authoritative {
        source_laps.laps.clone()
    } else if counter_crossings > 0 {
        refine_with_timer(counter_laps, &timer_laps, snap_window_ns)
    } else if !timer_laps.is_empty() {
        timer_laps
    } else {
        counter_laps
    }
}

/// Fixed part of the snap window (see [`snap_window_ns`]): a logger that
/// stamps the counter late, independent of sample rates.
const TIMER_SNAP_WINDOW_NS: u64 = 1_500_000_000;

/// Moves every counter-lap boundary onto the nearest timer reset within
/// `snap_window_ns`, keeping the counter's lap numbers.
///
/// A lap counter only says which lap the car is on; it changes one sample
/// after the beacon at its own (often 10 Hz) rate. The lap timer resets *at*
/// the beacon and runs at 100 Hz, so where both describe the same crossing
/// the timer's instant is the boundary. Boundaries with no reset nearby (the
/// very first crossing of a recording that started mid-lap, a counter bump
/// the timer never saw) stay where the counter put them.
fn refine_with_timer(
    mut laps: Vec<LapMetadata>,
    timer_laps: &[LapMetadata],
    snap_window_ns: u64,
) -> Vec<LapMetadata> {
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
            .filter(|reset| reset.abs_diff(boundary) <= snap_window_ns)
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
                let scale =
                    timer_seconds_per_unit(&source.channels()[index].unit, max_value)? * 1e9;
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
        let refined = pick_laps(None, counter, 3, timer, TIMER_SNAP_WINDOW_NS);
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
    fn snap_window_grows_with_one_hertz_dash_channels() {
        // Cosworth dash `Lap Number` and `Lap Time` both run at 1 Hz. The
        // counter flipped at 304.9 s while the timer's first post-reset
        // sample read 1.78 s, putting the beacon at 303.12 s: 1.78 s apart,
        // outside the fixed window but inside the rate-aware one. The
        // logger's own `Previous Lap Time` (1:47.434 / 1:42.592) agrees
        // with the snapped boundaries, not the counter's.
        let counter = vec![lap(2, 195.69, 304.9, true), lap(3, 304.9, 405.7, true)];
        let timer = vec![
            lap(0, 0.0, 195.69, false),
            lap(0, 195.69, 303.12, true),
            lap(0, 303.12, 405.7, true),
            lap(0, 405.7, 500.0, false),
        ];
        let fixed = pick_laps(
            None,
            counter.clone(),
            2,
            timer.clone(),
            TIMER_SNAP_WINDOW_NS,
        );
        assert_eq!(
            fixed[0].end_ns, 304_900_000_000,
            "fixed window must not snap"
        );
        let window = snap_window_ns(1_000_000_000, 1_000_000_000);
        assert_eq!(window, 3_500_000_000);
        let refined = pick_laps(None, counter, 2, timer, window);
        assert_eq!(refined[0].end_ns, 303_120_000_000);
        assert_eq!(refined[1].start_ns, 303_120_000_000);
        assert_eq!(refined[0].duration_ns, 107_430_000_000);
        assert_eq!(refined[1].duration_ns, 102_580_000_000);
    }

    #[test]
    fn counter_boundaries_without_a_nearby_reset_are_kept() {
        let counter = vec![lap(1, 10.0, 130.0, true), lap(2, 130.0, 250.0, true)];
        // A lone reset far from every crossing is not the same beacon.
        let timer = vec![lap(0, 0.0, 60.0, false), lap(0, 60.0, 250.0, false)];
        let refined = pick_laps(None, counter.clone(), 2, timer, TIMER_SNAP_WINDOW_NS);
        assert_eq!(refined, counter);
    }
}
