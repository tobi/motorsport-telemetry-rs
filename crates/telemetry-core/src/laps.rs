//! Lap-recovery strategies composed by [`crate::metadata::read_source_metadata`].
//!
//! Vendor files almost never agree on lap identity, so the pipeline follows a
//! fixed precedence (matching the README "How laps are recovered" section):
//!
//! 1. [`authoritative_laps`] — laps a source format supplies directly (`MoTeC`
//!    LDX, a `.telemetry` catalog).
//! 2. [`counter_laps`] — an incrementing counter. `Lap Number` wins only when
//!    it actually counts (high-water >= 2); a 0/1 flag loses to
//!    `beaconEventCount` / `lap_beacon` counts. A counter that drops and
//!    stays down is a **stint boundary** (pit stop, logger restart): the
//!    interval ending there is an in-lap, the one starting there an out-lap,
//!    and the climb that follows is a new stint's laps. A drop that recovers
//!    within [`RESET_CONFIRM_NS`] is a transient and ignored.
//! 3. [`timer_reset_laps`] — a running timer or progress channel that resets.
//! 4. Otherwise no laps.
//!
//! [`pick_laps`] applies that precedence. [`classify_laps`] then turns the
//! intervals into the normalised lap model: stints, [`LapKind`], the vendor
//! counter as `stint_lap`, and a virtual session lap `number` that is
//! monotonic across stints. [`fastest_lap`] derives the fastest *flying* lap
//! from the classified laps and any reported previous-lap channel.

use crate::metadata::{finite_i64, finite_u64, samples, LapKind, LapMetadata, SourceLapMetadata};
use crate::motion::{longest_stop_interval, longest_stop_ns};
use crate::{convert, names, TelemetrySource};

/// A counter drop that has not recovered this long after it happened is a
/// stint reset, not a transient glitch.
pub(crate) const RESET_CONFIRM_NS: u64 = 5_000_000_000;
/// Standing still at least this long inside an interval marks a pit stop.
/// A driver-change or refuel stop is 30 s and up; a splash-and-go or a
/// practice stop without killing the engine is around 15 s; nothing on a
/// flying lap approaches it.
pub(crate) const PIT_STOP_NS: u64 = 15_000_000_000;
/// Unrecorded time between consecutive laps that separates two stints.
pub(crate) const STINT_GAP_NS: u64 = 10_000_000_000;

/// Speed channels used for stop detection, by [`names::eq`] spelling, in
/// priority order. Only a channel with a unit convertible to m/s qualifies.
pub(crate) const SPEED_NAMES: &[&str] = &[
    "groundspeed",
    "speedref",
    "corrspeed",
    "vehiclespeed",
    "vehrefspeed",
    "speedwspdapp",
    "speed",
    "gpsspeed",
    "velocitykmh",
];

/// The best speed channel for motion checks, if any.
pub(crate) fn speed_channel(source: &dyn TelemetrySource) -> Option<usize> {
    let channels = source.channels();
    SPEED_NAMES.iter().find_map(|wanted| {
        channels.iter().position(|channel| {
            channel.sample_count > 0
                && names::eq(&channel.name, wanted)
                && convert(1.0, &channel.unit, "m/s").is_ok()
        })
    })
}

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

/// One lap in progress while walking the counter.
#[derive(Clone, Copy)]
struct OpenLap {
    /// Counter value (plus beacon-count offset) for this interval.
    stint_lap: i64,
    start_ns: u64,
    /// True when the interval began at a counter increment (a beacon), false
    /// when it began at the recording start or at a stint reset.
    starts_at_beacon: bool,
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
    let mut current: Option<OpenLap> = None;
    let mut high_water: Option<i64> = None;
    let mut crossings = 0;
    let mut stint = 1u32;
    let mut last_reset_ns: Option<u64> = None;

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

    let close =
        |laps: &mut Vec<LapMetadata>, open: OpenLap, end_ns: u64, at_beacon: bool, stint: u32| {
            if end_ns <= open.start_ns {
                return;
            }
            let kind = match (open.starts_at_beacon, at_beacon) {
                (true, true) => LapKind::Unknown, // flying or pit: needs the speed trace
                (false, true) => LapKind::Out,
                (true, false) => LapKind::In,
                (false, false) => LapKind::OutIn,
            };
            laps.push(LapMetadata {
                number: 0,
                start_ns: open.start_ns,
                end_ns,
                duration_ns: end_ns - open.start_ns,
                complete: open.starts_at_beacon && at_beacon,
                first_video_frame: None,
                stint,
                stint_lap: open.stint_lap,
                kind,
            });
        };

    for (position, &(time_ns, counter)) in values.iter().enumerate() {
        let Some(before) = high_water else {
            high_water = Some(counter);
            // counter + beacon offset overflowed i64: drop this sample
            let Some(stint_lap) = counter.checked_add(number_offset) else {
                continue;
            };
            current = Some(OpenLap {
                stint_lap,
                start_ns: time_ns,
                starts_at_beacon: false,
            });
            continue;
        };
        if counter == before {
            continue;
        }
        if counter < before {
            // A transient backwards value (radio bit error, a dash
            // re-sending a stale frame) recovers within seconds. A drop
            // that stays down is the counter starting over: the car
            // stopped in the pits (AiM resets `Lap_Number` to 0 there) or
            // the logger was power-cycled. Either way the lap in progress
            // ended without a beacon and a new stint begins here.
            let recovers = values[position + 1..]
                .iter()
                .take_while(|&&(later_ns, _)| later_ns.saturating_sub(time_ns) <= RESET_CONFIRM_NS)
                .any(|&(_, later)| later >= before);
            if recovers {
                continue;
            }
            if let Some(open) = current.take() {
                // An AiM dash closes the running lap when the car stops in
                // the box — counter +1, `Previous_LT` published — and resets
                // to 0 a second or two later. That increment is the pit
                // event, not a beacon: the interval it closed is the in-lap
                // (it ends here, at the reset) and the seconds-long
                // fragment it opened is nothing.
                let pit_event = open.starts_at_beacon
                    && time_ns.saturating_sub(open.start_ns) <= RESET_CONFIRM_NS
                    && laps
                        .last()
                        .is_some_and(|lap: &LapMetadata| lap.end_ns == open.start_ns);
                if let Some(in_lap) = laps.last_mut().filter(|_| pit_event) {
                    in_lap.end_ns = time_ns;
                    in_lap.duration_ns = time_ns - in_lap.start_ns;
                    in_lap.complete = false;
                    in_lap.kind = if in_lap.kind == LapKind::Out {
                        LapKind::OutIn
                    } else {
                        LapKind::In
                    };
                    crossings -= 1;
                } else {
                    close(&mut laps, open, time_ns, false, stint);
                }
            }
            stint += 1;
            high_water = Some(counter);
            last_reset_ns = Some(time_ns);
            current = counter.checked_add(number_offset).map(|stint_lap| OpenLap {
                stint_lap,
                start_ns: time_ns,
                starts_at_beacon: false,
            });
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
        let Some(stint_lap) = counter.checked_add(number_offset) else {
            continue;
        };
        // The same dash arms the next lap with 0 -> 1 a second or two after
        // the reset, still parked. Part of the reset sequence, not a beacon:
        // the out-lap fragment keeps its start and takes the new count.
        if last_reset_ns.is_some_and(|reset| time_ns.saturating_sub(reset) <= RESET_CONFIRM_NS) {
            if let Some(open) = &mut current {
                open.stint_lap = stint_lap;
            }
            high_water = Some(counter);
            continue;
        }
        if let Some(open) = current.replace(OpenLap {
            stint_lap,
            start_ns: time_ns,
            starts_at_beacon: true,
        }) {
            close(&mut laps, open, time_ns, true, stint);
        }
        high_water = Some(counter);
        crossings += 1;
    }
    if let Some(open) = current {
        close(&mut laps, open, duration_ns, false, stint);
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
            pair[1].checked_sub(pair[0])?;
            (number > 0).then_some(LapMetadata::interval(
                number,
                pair[0],
                pair[1],
                index > 0 && index + 1 < count,
            ))
        })
        .collect()
}

/// Convert declared timer units, retaining magnitude inference only when no
/// unit was stored. Unknown non-empty units must not be interpreted as time.
fn timer_seconds_per_unit(unit: &str, max_value: f64) -> Option<f64> {
    if unit.trim().is_empty() {
        Some(if max_value > 1_000.0 { 0.001 } else { 1.0 })
    } else {
        convert(1.0, unit, "s").ok()
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

/// Resolves stints, [`LapKind`]s, stint-local numbers and the virtual
/// session lap number for `laps`, in place.
///
/// Stint boundaries come from three signals, any of which is enough:
///
/// * the counter walk already split the recording (every lap has `stint > 0`);
/// * more than [`STINT_GAP_NS`] of unrecorded time between two laps, or an
///   incomplete lap followed by another incomplete one (an in-lap and the
///   next out-lap that no counter separated);
/// * a complete lap in which the car stood still for [`PIT_STOP_NS`] — a
///   logger that keeps counting through the pits produces one
///   beacon-to-beacon interval holding both the in- and the out-lap. That
///   lap is [`LapKind::Pit`] and closes its stint.
///
/// Kinds a reader already stored are kept; [`LapKind::Unknown`] is resolved
/// from position in the stint and the speed trace. Complete laps with a
/// pit-length stop become [`LapKind::Pit`] even when stored as flying, since
/// a stop is a fact of the trace, not a labelling choice.
///
/// A [`LapKind::Pit`] lap is then *carved*: the standing time is split out as
/// its own [`LapKind::Pit`] interval, bounded by an [`LapKind::In`] and an
/// [`LapKind::Out`] fragment. The stop is not a lap, so lap-time work must not
/// count it; keeping it as a separate interval means a consumer can include or
/// exclude it explicitly instead of subtracting an opaque standstill from a
/// lap. The in-lap keeps the closing stint, the out-lap opens the next.
pub fn classify_laps(source: &dyn TelemetrySource, laps: &mut Vec<LapMetadata>) {
    laps.sort_by_key(|lap| (lap.start_ns, lap.end_ns));
    let speed = speed_channel(source);
    let stops: Vec<u64> = laps
        .iter()
        .map(|lap| {
            speed.map_or(0, |speed| {
                longest_stop_ns(source, speed, lap.start_ns, lap.end_ns)
            })
        })
        .collect();
    let pit_stop = |index: usize| stops[index] >= PIT_STOP_NS;

    if laps.iter().any(|lap| lap.stint == 0) {
        let mut stint = 1u32;
        for index in 0..laps.len() {
            if index > 0 {
                let previous = &laps[index - 1];
                let current = &laps[index];
                let gap = current.start_ns.saturating_sub(previous.end_ns) > STINT_GAP_NS;
                let in_then_out = !previous.complete && !current.complete;
                let after_pit_lap = previous.complete && pit_stop(index - 1);
                if gap || in_then_out || after_pit_lap {
                    stint += 1;
                }
            }
            laps[index].stint = stint;
        }
    }

    let mut index = 0;
    while index < laps.len() {
        let stint = laps[index].stint;
        let end = laps[index..]
            .iter()
            .position(|lap| lap.stint != stint)
            .map_or(laps.len(), |offset| index + offset);
        let count = end - index;
        for (offset, lap) in laps[index..end].iter_mut().enumerate() {
            let first = offset == 0;
            let last = offset + 1 == count;
            let stopped = pit_stop(index + offset);
            lap.kind = match lap.kind {
                LapKind::Unknown if lap.complete => {
                    if stopped {
                        LapKind::Pit
                    } else {
                        LapKind::Flying
                    }
                }
                LapKind::Unknown if first && last => LapKind::OutIn,
                LapKind::Unknown if first => LapKind::Out,
                LapKind::Unknown if last => LapKind::In,
                LapKind::Unknown => LapKind::Out,
                LapKind::Flying if stopped => LapKind::Pit,
                kept => kept,
            };
        }
        index = end;
    }

    // A pit lap closes its stint whichever way the stints were assigned: a
    // counter that kept counting through the stop put the next lap in the
    // same stint, and that is the one place the counter is wrong about it.
    let mut bump = 0u32;
    for index in 0..laps.len() {
        if index > 0
            && laps[index - 1].kind == LapKind::Pit
            && laps[index].stint == laps[index - 1].stint - bump
        {
            bump += 1;
        }
        laps[index].stint += bump;
    }

    // Carve the pit stop out of the lap that holds it. A logger that keeps
    // counting through the pits produces one beacon-to-beacon interval with
    // the in-lap, the standing time and the out-lap glued together; the stop
    // is not part of any lap, so give it its own interval. The in-lap keeps
    // the closing stint, the stop is neutral, and the out-lap opens the next
    // (matching the bump above, which already moved the following laps).
    if let Some(speed) = speed {
        let mut carved = Vec::with_capacity(laps.len());
        for lap in laps.drain(..) {
            let stop = (lap.kind == LapKind::Pit)
                .then(|| longest_stop_interval(source, speed, lap.start_ns, lap.end_ns))
                .flatten();
            if let Some((stop_start, stop_end)) = stop {
                if stop_end - stop_start >= PIT_STOP_NS
                    && stop_start > lap.start_ns
                    && stop_end < lap.end_ns
                {
                    let mut in_lap = lap.clone();
                    in_lap.end_ns = stop_start;
                    in_lap.duration_ns = stop_start - lap.start_ns;
                    in_lap.kind = LapKind::In;
                    in_lap.complete = false;
                    let mut pit = lap.clone();
                    pit.start_ns = stop_start;
                    pit.end_ns = stop_end;
                    pit.duration_ns = stop_end - stop_start;
                    pit.complete = false;
                    let mut out_lap = lap.clone();
                    out_lap.start_ns = stop_end;
                    out_lap.duration_ns = lap.end_ns - stop_end;
                    out_lap.kind = LapKind::Out;
                    out_lap.complete = false;
                    out_lap.stint = lap.stint + 1;
                    out_lap.stint_lap = 0;
                    carved.push(in_lap);
                    carved.push(pit);
                    carved.push(out_lap);
                    continue;
                }
            }
            carved.push(lap);
        }
        *laps = carved;
    }

    for (position, lap) in laps.iter_mut().enumerate() {
        lap.number = position as i64 + 1;
    }
}

/// Derives the fastest flying lap from the classified laps.
///
/// Prefers an authoritative fastest lap. Otherwise the shortest plausible
/// flying lap *of the list itself* (an in-lap cut short by a pit-box counter
/// reset once read as a 1:13 at a 1:16 circuit; a lap with a stop in it is
/// never a candidate): a `Ref Lap Time` channel only bounds
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
        .and_then(|reported| {
            // The source's pick, but as the classified lap (stint, kind,
            // virtual number) so it is the same value a consumer finds in
            // `laps`. A reported fastest lap that is not in the list is
            // returned as-is; one that classifies as anything but flying (a
            // VBO gate crossing inside a pit stop, an in-lap fragment) is
            // rejected and the flying selection below applies.
            match laps
                .iter()
                .find(|lap| lap.start_ns == reported.start_ns && lap.end_ns == reported.end_ns)
            {
                Some(lap) if lap.kind.is_flying() => Some(lap.clone()),
                Some(_) => None,
                None => Some(reported),
            }
        })
        .or_else(|| {
            laps.iter()
                .filter(|lap| {
                    lap.complete && lap.kind.is_flying() && plausible_lap(lap.duration_ns)
                })
                .min_by_key(|lap| lap.duration_ns)
                .cloned()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lap(number: i64, start_s: f64, end_s: f64, complete: bool) -> LapMetadata {
        LapMetadata::interval(
            number,
            (start_s * 1e9) as u64,
            (end_s * 1e9) as u64,
            complete,
        )
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

    /// A one-channel source at 1 Hz whose speed is 0 while parked.
    struct StopSource {
        channels: Vec<crate::Channel>,
        values: Vec<f64>,
    }
    impl TelemetrySource for StopSource {
        fn path(&self) -> &'static str {
            "synthetic"
        }
        fn format(&self) -> &'static str {
            "synthetic"
        }
        fn channels(&self) -> &[crate::Channel] {
            &self.channels
        }
        fn decode(&self, _: usize, _: usize, local_index: u64) -> f64 {
            self.values[local_index as usize]
        }
    }
    fn stop_source(seconds: u64, parked: std::ops::Range<u64>) -> StopSource {
        let values = (0..seconds)
            .map(|i| if parked.contains(&i) { 0.0 } else { 60.0 })
            .collect();
        StopSource {
            values,
            channels: vec![crate::Channel {
                id: 0,
                name: "Speed_Ref".into(),
                unit: "m/s".into(),
                unit_source: crate::UnitSource::Declared,
                sample_type: crate::SampleType::F64,
                sample_count: seconds,
                duration_ns: seconds * 1_000_000_000,
                chunks: vec![crate::Chunk {
                    time_base_ns: 0,
                    sample_base: 0,
                    data_ptr: 0,
                    sample_count: seconds,
                    sample_period_ns: 1_000_000_000,
                }],
            }],
        }
    }

    #[test]
    fn a_pit_lap_is_carved_into_in_stop_out() {
        // A logger that kept counting through the pits: one beacon-to-beacon
        // interval [50,150] holds the in-lap, 30 s parked at the box, and the
        // out-lap. The stop must become its own interval, not stay folded into
        // a lap (the search for a giant lap that only existed as a pit stop).
        let source = stop_source(200, 100..130);
        let mut laps = vec![
            lap(1, 0.0, 50.0, true),
            lap(2, 50.0, 150.0, true),
            lap(3, 150.0, 200.0, false),
        ];
        classify_laps(&source, &mut laps);
        let shape: Vec<(LapKind, i64, u64, u64)> = laps
            .iter()
            .map(|l| (l.kind, i64::from(l.stint), l.start_ns, l.end_ns))
            .collect();
        assert_eq!(
            shape,
            vec![
                (LapKind::Flying, 1, 0, 50_000_000_000),
                (LapKind::In, 1, 50_000_000_000, 100_000_000_000),
                (LapKind::Pit, 1, 100_000_000_000, 130_000_000_000),
                (LapKind::Out, 2, 130_000_000_000, 150_000_000_000),
                (LapKind::OutIn, 2, 150_000_000_000, 200_000_000_000),
            ]
        );
        // Numbering stays monotonic across the carve.
        assert_eq!(
            laps.iter().map(|l| l.number).collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5]
        );
        // The stop is never a flying lap.
        assert!(!laps[2].kind.is_flying());
    }
}
