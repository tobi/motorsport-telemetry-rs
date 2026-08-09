use crate::TelemetrySource;
use std::collections::{BTreeMap, BTreeSet};

const GPS_WEEK_MS: u64 = 604_800_000;
const GPS_UNIX_EPOCH_MS: u64 = 315_964_800_000;

/// One lap boundary derived from source channels or reported lap timing.
#[derive(Debug, Clone, PartialEq)]
pub struct LapMetadata {
    /// Source lap number, or a conservative inferred number.
    pub number: i64,
    /// File- or session-relative lap start in nanoseconds.
    pub start_ns: u64,
    /// File- or session-relative lap end in nanoseconds.
    pub end_ns: u64,
    /// Lap duration in nanoseconds.
    pub duration_ns: u64,
    /// Whether both lap boundaries are known to fall within the recording.
    pub complete: bool,
}

/// Authoritative lap information supplied directly by a source format.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SourceLapMetadata {
    /// Known lap intervals in file-relative time.
    pub laps: Vec<LapMetadata>,
    /// Fastest lap explicitly reported by the source, when available.
    pub fastest_lap: Option<LapMetadata>,
}

/// A contiguous interval attributed to one internal driver identifier.
#[derive(Debug, Clone, PartialEq)]
pub struct DriverStint {
    /// Format-specific numeric driver identifier.
    pub driver_id: i64,
    /// File- or session-relative stint start in nanoseconds.
    pub start_ns: u64,
    /// File- or session-relative stint end in nanoseconds.
    pub end_ns: u64,
}

/// Reliable absolute clock coverage reported by a source format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbsoluteTimeRange {
    /// Clock name, such as `gps` or `utc`.
    pub clock: String,
    /// Inclusive absolute start timestamp in nanoseconds.
    pub start_ns: u64,
    /// Absolute end timestamp in nanoseconds.
    pub end_ns: u64,
    /// Format-provided identity used as one component of a session key.
    pub session_hint: String,
}

/// Human-readable identity embedded in a telemetry source.
///
/// Empty strings represent unavailable fields.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceIdentity {
    /// Driver name.
    pub driver: String,
    /// Vehicle name or identifier.
    pub vehicle: String,
    /// Circuit or venue name.
    pub venue: String,
    /// Event name.
    pub event: String,
    /// Session name.
    pub session: String,
    /// Recording date in the source's original representation.
    pub date: String,
    /// Recording time in the source's original representation.
    pub time: String,
}

/// Video linkage available at one telemetry timestamp.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VideoReference {
    /// Source video file index, when the recording spans multiple files.
    pub file_index: Option<u32>,
    /// Source-exact video synchronization time.
    pub sync_time: Option<f64>,
    /// Presentation-order video frame index, when available.
    pub frame_index: Option<u64>,
}

/// Format-neutral summary derived for one telemetry file.
#[derive(Debug, Clone, PartialEq)]
pub struct FileMetadata {
    /// Source path or caller-supplied name.
    pub path: String,
    /// Stable lowercase format identifier.
    pub format: String,
    /// Total number of declared channels.
    pub channel_count: usize,
    /// Number of channels containing at least one sample.
    pub sampled_channel_count: usize,
    /// Sum of sample counts across all channels.
    pub sample_count: u64,
    /// Longest channel duration in nanoseconds.
    pub duration_ns: u64,
    /// Stable hash of channel names, units, and scalar types.
    pub schema_hash: u64,
    /// Internal session candidate key, when a reliable clock is available.
    pub session_key: Option<String>,
    /// Name of the absolute clock used by this file.
    pub absolute_clock: Option<String>,
    /// Absolute recording start in nanoseconds.
    pub absolute_start_ns: Option<u64>,
    /// Absolute recording end in nanoseconds.
    pub absolute_end_ns: Option<u64>,
    /// Offset satisfying `absolute_ns = file_relative_ns + clock_offset_ns`.
    pub clock_offset_ns: Option<i128>,
    /// Human-readable identity embedded in the source.
    pub identity: SourceIdentity,
    /// Distinct internal driver identifiers in ascending order.
    pub driver_ids: Vec<i64>,
    /// Driver intervals in file-relative time.
    pub driver_stints: Vec<DriverStint>,
    /// Lap intervals in file-relative time.
    pub laps: Vec<LapMetadata>,
    /// Fastest complete or explicitly reported lap.
    pub fastest_lap: Option<LapMetadata>,
    /// Linked or embedded video frame count, when available.
    pub video_frame_count: Option<u64>,
}

/// Metadata merged across files that belong to one recording session.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionMetadata {
    /// Unique derived key for this grouped session.
    pub session_key: String,
    /// Indexes into the [`FileMetadata`] slice passed to [`group_sessions`].
    pub files: Vec<usize>,
    /// Earliest absolute file start in nanoseconds.
    pub absolute_start_ns: Option<u64>,
    /// Latest absolute file end in nanoseconds.
    pub absolute_end_ns: Option<u64>,
    /// Span from absolute start to absolute end in nanoseconds.
    pub duration_ns: u64,
    /// Driver intervals translated to session-relative time.
    pub driver_stints: Vec<DriverStint>,
    /// Lap intervals translated and merged in session-relative time.
    pub laps: Vec<LapMetadata>,
    /// Fastest complete or explicitly reported session lap.
    pub fastest_lap: Option<LapMetadata>,
}

fn normalized_eq(value: &str, wanted: &str) -> bool {
    value
        .bytes()
        .filter(u8::is_ascii_alphanumeric)
        .map(|byte| byte.to_ascii_lowercase())
        .eq(wanted.bytes())
}

fn channel_index(source: &dyn TelemetrySource, names: &[&str]) -> Option<usize> {
    source.channels().iter().position(|channel| {
        channel.sample_count > 0
            && names
                .iter()
                .any(|wanted| normalized_eq(&channel.name, wanted))
    })
}

fn samples(source: &dyn TelemetrySource, channel_index: usize) -> Vec<(u64, f64)> {
    let channel = &source.channels()[channel_index];
    let mut values = Vec::with_capacity(channel.sample_count as usize);
    for (chunk_index, chunk) in channel.chunks.iter().enumerate() {
        for local_index in 0..chunk.sample_count {
            values.push((
                source.sample_time_ns(channel_index, chunk_index, local_index),
                source.decode(channel_index, chunk_index, local_index),
            ));
        }
    }
    values
}

fn integer_runs(values: &[(u64, f64)], duration_ns: u64) -> Vec<(i64, u64, u64)> {
    let mut runs = Vec::new();
    let mut current: Option<(i64, u64)> = None;
    for &(time_ns, value) in values {
        if !value.is_finite() {
            continue;
        }
        let integer = value.round() as i64;
        if current.is_some_and(|(before, _)| before == integer) {
            continue;
        }
        if let Some((before, start_ns)) = current.replace((integer, time_ns)) {
            runs.push((before, start_ns, time_ns));
        }
    }
    if let Some((value, start_ns)) = current {
        runs.push((value, start_ns, duration_ns.max(start_ns)));
    }
    runs
}

fn is_completed_lap_counter(channel: &crate::Channel) -> bool {
    ["beaconeventcount", "beaconcount", "lapbeaconcount"]
        .iter()
        .any(|wanted| normalized_eq(&channel.name, wanted))
}

fn counter_lap_number_at(
    source: &dyn TelemetrySource,
    channel_index: usize,
    time_ns: u64,
    previous: bool,
) -> Option<i64> {
    let value = source.sample_at(channel_index, time_ns, false)?;
    if !value.is_finite() {
        return None;
    }
    let value = value.round() as i64;
    let completed_count = is_completed_lap_counter(&source.channels()[channel_index]);
    Some(match (completed_count, previous) {
        (true, false) => value.saturating_add(1),
        (true, true) | (false, false) => value,
        (false, true) => value.saturating_sub(1),
    })
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
            if !value.is_finite() {
                continue;
            }
            let counter = value.round() as i64;
            if counter < 0 {
                continue;
            }
            let time_ns = source.sample_time_ns(channel_index, chunk_index, local_index);
            let Some(before) = high_water else {
                high_water = Some(counter);
                current = Some((counter.saturating_add(number_offset), time_ns, false));
                continue;
            };
            if counter <= before {
                // Shutdown resets and transient backwards values are not lap
                // crossings. Keep the high-water mark so a later 0 -> 1 does
                // not create a second, overlapping lap sequence.
                continue;
            }
            if let Some((number, start_ns, start_known)) =
                current.replace((counter.saturating_add(number_offset), time_ns, true))
            {
                if number > 0 && time_ns > start_ns {
                    laps.push(LapMetadata {
                        number,
                        start_ns,
                        end_ns: time_ns,
                        duration_ns: time_ns - start_ns,
                        complete: start_known,
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
            });
        }
    }
    (laps, crossings)
}

fn schema_hash(source: &dyn TelemetrySource) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for channel in source.channels() {
        for byte in channel.name.bytes().map(|byte| byte.to_ascii_lowercase()) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        for byte in channel.unit.bytes().map(|byte| byte.to_ascii_lowercase()) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash ^= u64::from(channel.sample_type.code());
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Derives counts, identity, clocks, stints, laps, and video metadata.
///
/// Channel names are matched conservatively after punctuation and case
/// normalization. Missing evidence remains absent rather than being guessed.
pub fn read_source_metadata(source: &dyn TelemetrySource) -> FileMetadata {
    let duration_ns = source
        .channels()
        .iter()
        .map(|channel| channel.duration_ns)
        .max()
        .unwrap_or(0);
    let hash = schema_hash(source);
    let explicit_absolute = source.absolute_time_range();
    let absolute = channel_index(source, &["gpsweek"]).and_then(|week_index| {
        let week = samples(source, week_index)
            .into_iter()
            .find_map(|(_, value)| value.is_finite().then_some(value.round() as u64))?;
        let itow_index = channel_index(source, &["gpsitow"])?;
        let itow = samples(source, itow_index);
        let &(first_time, first_value) = itow.first()?;
        let &(_last_time, last_value) = itow.last()?;
        let start_ns = week
            .saturating_mul(GPS_WEEK_MS)
            .saturating_add(first_value.round().max(0.0) as u64)
            .saturating_add(GPS_UNIX_EPOCH_MS)
            .saturating_mul(1_000_000);
        let end_ns = week
            .saturating_mul(GPS_WEEK_MS)
            .saturating_add(last_value.round().max(0.0) as u64)
            .saturating_add(GPS_UNIX_EPOCH_MS)
            .saturating_mul(1_000_000);
        Some((week, first_time, start_ns, end_ns))
    });
    let (absolute_clock, absolute_start_ns, absolute_end_ns, clock_offset_ns, session_key) =
        if let Some(range) = explicit_absolute {
            (
                Some(range.clock),
                Some(range.start_ns),
                Some(range.end_ns),
                Some(i128::from(range.start_ns)),
                Some(format!("{}:{hash:016x}", range.session_hint)),
            )
        } else if let Some((week, first_time, start_ns, end_ns)) = absolute {
            (
                Some("gps".into()),
                Some(start_ns),
                Some(end_ns),
                Some(i128::from(start_ns) - i128::from(first_time)),
                Some(format!("gps:{week}:{hash:016x}")),
            )
        } else {
            (None, None, None, None, None)
        };
    let raw_driver_runs = channel_index(source, &["driverid", "driver", "driverindex"])
        .map(|index| integer_runs(&samples(source, index), duration_ns))
        .unwrap_or_default();
    let driver_runs = raw_driver_runs
        .iter()
        .copied()
        .filter(|(_, start_ns, end_ns)| {
            raw_driver_runs.len() == 1 || end_ns.saturating_sub(*start_ns) >= 1_000_000_000
        })
        .collect::<Vec<_>>();
    let driver_ids = driver_runs
        .iter()
        .map(|(driver, _, _)| *driver)
        .filter(|driver| *driver >= 0)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let driver_stints = driver_runs
        .into_iter()
        .filter(|(driver, _, _)| *driver >= 0)
        .map(|(driver_id, start_ns, end_ns)| DriverStint {
            driver_id,
            start_ns,
            end_ns,
        })
        .collect::<Vec<_>>();

    let source_laps = source.source_lap_metadata();
    let lap_channel_index = source_laps
        .is_none()
        .then(|| {
            channel_index(
                source,
                &[
                    "lapnumber",
                    "lapnum",
                    "lapcount",
                    "lapcounter",
                    "currentlap",
                    "lap",
                    "beaconeventcount",
                    "beaconcount",
                    "lapbeaconcount",
                ],
            )
        })
        .flatten();
    let (counter_laps, counter_crossings) = source_laps
        .is_none()
        .then(|| {
            lap_channel_index
                .map(|index| increasing_counter_laps(source, index, duration_ns))
                .unwrap_or_default()
        })
        .unwrap_or_default();
    let timer_resets = source_laps
        .is_none()
        .then(|| {
            channel_index(
                source,
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
                    .any(|wanted| normalized_eq(&source.channels()[index].name, wanted));
                let max_value = values
                    .iter()
                    .map(|(_, value)| *value)
                    .filter(|value| value.is_finite())
                    .fold(0.0_f64, f64::max);
                let reset_threshold = if max_value > 1_000.0 { 5_000.0 } else { 5.0 };
                values
                    .windows(2)
                    .filter_map(|pair| {
                        let before = pair[0].1;
                        let after = pair[1].1;
                        let reset = if is_progress {
                            let full_lap = if max_value > 2.0 { 100.0 } else { 1.0 };
                            before >= full_lap * 0.75 && after <= full_lap * 0.25
                        } else {
                            before - after > reset_threshold
                        };
                        (before.is_finite() && after.is_finite() && reset).then_some(pair[1].0)
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
        })
        .unwrap_or_default();
    let laps = if let Some(source_laps) = &source_laps {
        source_laps.laps.clone()
    } else if counter_crossings > 0 {
        counter_laps
    } else if !timer_resets.is_empty() {
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
                (number > 0).then_some(LapMetadata {
                    number,
                    start_ns: pair[0],
                    end_ns: pair[1],
                    duration_ns: pair[1].saturating_sub(pair[0]),
                    complete: index > 0 && index + 1 < count,
                })
            })
            .collect::<Vec<_>>()
    } else {
        counter_laps
    };
    let reference_lap_ns = source_laps
        .is_none()
        .then(|| {
            channel_index(source, &["reflaptime", "referencelaptime"]).and_then(|index| {
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
                    .map(|value| (value * scale).round() as u64)
            })
        })
        .flatten();
    let plausible_lap = |duration_ns: u64| {
        duration_ns >= 10_000_000_000
            && reference_lap_ns.is_none_or(|reference| {
                duration_ns >= reference / 2 && duration_ns <= reference.saturating_mul(3) / 2
            })
    };
    let mut fastest_lap = source_laps
        .as_ref()
        .and_then(|source| source.fastest_lap.clone())
        .or_else(|| {
            laps.iter()
                .filter(|lap| lap.complete && plausible_lap(lap.duration_ns))
                .min_by_key(|lap| lap.duration_ns)
                .cloned()
        });
    if let Some(previous_lap_index) = source_laps
        .is_none()
        .then(|| channel_index(source, &["previouslt", "previouslaptime", "lastlaptime"]))
        .flatten()
    {
        let values = samples(source, previous_lap_index);
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
        let reported = values
            .into_iter()
            .filter(|(_, value)| value.is_finite() && *value > 0.0)
            .map(|(time_ns, value)| (time_ns, (value * scale).round() as u64))
            .filter(|(_, duration_ns)| plausible_lap(*duration_ns))
            .min_by_key(|(_, duration_ns)| *duration_ns);
        if let Some((time_ns, duration_ns)) = reported {
            let number = lap_channel_index
                .and_then(|channel| counter_lap_number_at(source, channel, time_ns, true))
                .unwrap_or(0);
            fastest_lap = Some(LapMetadata {
                number,
                start_ns: time_ns.saturating_sub(duration_ns),
                end_ns: time_ns,
                duration_ns,
                complete: true,
            });
        }
    }

    FileMetadata {
        path: source.path().to_owned(),
        format: source.format().to_owned(),
        channel_count: source.channels().len(),
        sampled_channel_count: source
            .channels()
            .iter()
            .filter(|channel| channel.sample_count > 0)
            .count(),
        sample_count: source
            .channels()
            .iter()
            .map(|channel| channel.sample_count)
            .sum(),
        duration_ns,
        schema_hash: hash,
        session_key,
        absolute_clock,
        absolute_start_ns,
        absolute_end_ns,
        clock_offset_ns,
        identity: source.identity(),
        driver_ids,
        driver_stints,
        laps,
        fastest_lap,
        video_frame_count: source.video_frame_count(),
    }
}

fn absolute_time(metadata: &FileMetadata, relative_ns: u64) -> Option<u64> {
    let offset = metadata.clock_offset_ns?;
    u64::try_from(i128::from(relative_ns) + offset).ok()
}

/// Groups files with equal internal session keys and compatible absolute clocks.
///
/// `max_gap_ns` is the largest allowed gap between adjacent files. Unkeyed
/// files form separate sessions. Filenames are never used for identity.
pub fn group_sessions(files: &[FileMetadata], max_gap_ns: u64) -> Vec<SessionMetadata> {
    let mut indexed = files
        .iter()
        .enumerate()
        .collect::<Vec<(usize, &FileMetadata)>>();
    indexed.sort_by_key(|(_, file)| (file.session_key.clone(), file.absolute_start_ns));
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for (index, file) in indexed {
        let joins_previous = groups
            .last()
            .and_then(|group| group.last())
            .is_some_and(|before| {
                let previous = &files[*before];
                previous.session_key == file.session_key
                    && previous.session_key.is_some()
                    && previous
                        .absolute_end_ns
                        .zip(file.absolute_start_ns)
                        .is_some_and(|(end, start)| start <= end.saturating_add(max_gap_ns))
            });
        if joins_previous {
            groups.last_mut().unwrap().push(index);
        } else {
            groups.push(vec![index]);
        }
    }

    groups
        .into_iter()
        .enumerate()
        .map(|(group_index, indices)| {
            let start = indices
                .iter()
                .filter_map(|index| files[*index].absolute_start_ns)
                .min();
            let end = indices
                .iter()
                .filter_map(|index| files[*index].absolute_end_ns)
                .max();
            let base = start.unwrap_or(0);

            let mut driver_segments = Vec::new();
            for index in &indices {
                let file = &files[*index];
                for stint in &file.driver_stints {
                    if let (Some(from), Some(to)) = (
                        absolute_time(file, stint.start_ns),
                        absolute_time(file, stint.end_ns),
                    ) {
                        driver_segments.push(DriverStint {
                            driver_id: stint.driver_id,
                            start_ns: from.saturating_sub(base),
                            end_ns: to.saturating_sub(base),
                        });
                    }
                }
            }
            driver_segments.sort_by_key(|stint| stint.start_ns);
            let mut driver_stints: Vec<DriverStint> = Vec::new();
            for stint in driver_segments {
                if let Some(previous) = driver_stints.last_mut() {
                    if previous.driver_id == stint.driver_id
                        && stint.start_ns <= previous.end_ns.saturating_add(max_gap_ns)
                    {
                        previous.end_ns = previous.end_ns.max(stint.end_ns);
                        continue;
                    }
                }
                driver_stints.push(stint);
            }

            let mut lap_segments = Vec::new();
            for index in &indices {
                let file = &files[*index];
                for lap in &file.laps {
                    if let (Some(from), Some(to)) = (
                        absolute_time(file, lap.start_ns),
                        absolute_time(file, lap.end_ns),
                    ) {
                        lap_segments.push(LapMetadata {
                            number: lap.number,
                            start_ns: from.saturating_sub(base),
                            end_ns: to.saturating_sub(base),
                            duration_ns: to.saturating_sub(from),
                            complete: false,
                        });
                    }
                }
            }
            lap_segments.sort_by_key(|lap| lap.start_ns);
            let mut laps: Vec<LapMetadata> = Vec::new();
            for lap in lap_segments {
                if let Some(previous) = laps.last_mut() {
                    if previous.number == lap.number
                        && lap.start_ns <= previous.end_ns.saturating_add(max_gap_ns)
                    {
                        previous.end_ns = previous.end_ns.max(lap.end_ns);
                        previous.duration_ns = previous.end_ns.saturating_sub(previous.start_ns);
                        continue;
                    }
                }
                laps.push(lap);
            }
            let lap_count = laps.len();
            for (index, lap) in laps.iter_mut().enumerate() {
                lap.complete = index > 0 && index + 1 < lap_count;
            }
            let inferred_fastest = laps
                .iter()
                .filter(|lap| lap.complete && lap.duration_ns >= 10_000_000_000)
                .min_by_key(|lap| lap.duration_ns)
                .cloned();
            let reported_fastest = indices
                .iter()
                .filter_map(|index| files[*index].fastest_lap.as_ref())
                .min_by_key(|lap| lap.duration_ns)
                .cloned();
            let fastest_lap = reported_fastest.or(inferred_fastest);

            let candidate = indices
                .first()
                .and_then(|index| files[*index].session_key.clone())
                .unwrap_or_else(|| format!("unkeyed:{group_index}"));
            SessionMetadata {
                session_key: format!("{candidate}:{group_index}"),
                files: indices,
                absolute_start_ns: start,
                absolute_end_ns: end,
                duration_ns: end
                    .zip(start)
                    .map_or(0, |(end, start)| end.saturating_sub(start)),
                driver_stints,
                laps,
                fastest_lap,
            }
        })
        .collect()
}

/// Counts native samples for each value of a recognized driver-ID channel.
///
/// Returns an empty map if no recognized sampled channel exists. Finite values
/// are rounded to the nearest integer identifier.
pub fn driver_histogram(source: &dyn TelemetrySource) -> BTreeMap<i64, u64> {
    let Some(index) = channel_index(source, &["driverid", "driver", "driverindex"]) else {
        return BTreeMap::new();
    };
    let mut counts = BTreeMap::new();
    for (_, value) in samples(source, index) {
        if value.is_finite() {
            *counts.entry(value.round() as i64).or_default() += 1;
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Channel, Chunk, SampleType, UnitSource};

    struct MetadataSource {
        path: String,
        channels: Vec<Channel>,
        values: Vec<Vec<f64>>,
        absolute_start_ns: u64,
    }

    impl TelemetrySource for MetadataSource {
        fn path(&self) -> &str {
            &self.path
        }

        fn format(&self) -> &'static str {
            "synthetic"
        }

        fn channels(&self) -> &[Channel] {
            &self.channels
        }

        fn decode(&self, channel_index: usize, _chunk_index: usize, local_index: u64) -> f64 {
            self.values[channel_index][local_index as usize]
        }

        fn absolute_time_range(&self) -> Option<AbsoluteTimeRange> {
            Some(AbsoluteTimeRange {
                clock: "test".into(),
                start_ns: self.absolute_start_ns,
                end_ns: self.absolute_start_ns + 40_000_000_000,
                session_hint: "test-session".into(),
            })
        }
    }

    fn metadata_source(path: &str, start_ns: u64, driver: i64) -> MetadataSource {
        let names = ["DRIVER_ID", "Lap_Number", "Previous_LT", "Ref_Lap_Time"];
        let values = vec![
            vec![driver as f64; 4],
            vec![1.0, 2.0, 3.0, 4.0],
            vec![0.0, 20_000.0, 18_000.0, 19_000.0],
            vec![20_000.0; 4],
        ];
        let channels = names
            .into_iter()
            .enumerate()
            .map(|(index, name)| Channel {
                id: index as u32,
                name: name.into(),
                unit: String::new(),
                unit_source: UnitSource::Unknown,
                sample_type: SampleType::F64,
                chunks: vec![Chunk {
                    sample_period_ns: 10_000_000_000,
                    sample_count: 4,
                    data_ptr: 0,
                    sample_base: 0,
                    time_base_ns: 0,
                }],
                sample_count: 4,
                duration_ns: 40_000_000_000,
            })
            .collect();
        MetadataSource {
            path: path.into(),
            channels,
            values,
            absolute_start_ns: start_ns,
        }
    }

    #[test]
    fn summarizes_driver_laps_and_fastest_complete_lap() {
        let source = metadata_source("part-1", 1_000_000_000_000, 3);
        let metadata = read_source_metadata(&source);
        assert_eq!(metadata.driver_ids, [3]);
        assert_eq!(metadata.laps.len(), 4);
        assert_eq!(
            metadata.fastest_lap.as_ref().unwrap().duration_ns,
            18_000_000_000
        );
        assert!(metadata
            .session_key
            .as_deref()
            .unwrap()
            .starts_with("test-session:"));
    }

    #[test]
    fn lap_progression_wraps_produce_lap_boundaries() {
        let mut source = metadata_source("progress", 1_000_000_000_000, 3);
        source.channels = vec![Channel {
            id: 0,
            name: "Lap Progression".into(),
            unit: "%".into(),
            unit_source: UnitSource::Declared,
            sample_type: SampleType::F64,
            chunks: vec![Chunk {
                sample_period_ns: 1_000_000_000,
                sample_count: 7,
                data_ptr: 0,
                sample_base: 0,
                time_base_ns: 0,
            }],
            sample_count: 7,
            duration_ns: 7_000_000_000,
        }];
        source.values = vec![vec![80.0, 99.0, 2.0, 50.0, 98.0, 1.0, 30.0]];

        let metadata = read_source_metadata(&source);
        assert_eq!(metadata.laps.len(), 3);
        assert_eq!(metadata.laps[1].start_ns, 2_000_000_000);
        assert_eq!(metadata.laps[1].end_ns, 5_000_000_000);
        assert!(metadata.laps[1].complete);
    }

    fn counter_source(name: &str, values: Vec<f64>) -> MetadataSource {
        let count = values.len() as u64;
        MetadataSource {
            path: "counter".into(),
            channels: vec![Channel {
                id: 0,
                name: name.into(),
                unit: String::new(),
                unit_source: UnitSource::Unknown,
                sample_type: SampleType::F64,
                chunks: vec![Chunk {
                    sample_period_ns: 10_000_000_000,
                    sample_count: count,
                    data_ptr: 0,
                    sample_base: 0,
                    time_base_ns: 0,
                }],
                sample_count: count,
                duration_ns: count * 10_000_000_000,
            }],
            values: vec![values],
            absolute_start_ns: 1_000_000_000_000,
        }
    }

    #[test]
    fn upward_lap_counter_is_preferred_and_shutdown_reset_is_ignored() {
        let source = counter_source("Lap Number", vec![1.0, 1.0, 2.0, 2.0, 3.0, 3.0, 0.0, 1.0]);
        let metadata = read_source_metadata(&source);
        assert_eq!(
            metadata
                .laps
                .iter()
                .map(|lap| (lap.number, lap.start_ns, lap.end_ns, lap.complete))
                .collect::<Vec<_>>(),
            [
                (1, 0, 20_000_000_000, false),
                (2, 20_000_000_000, 40_000_000_000, true),
                (3, 40_000_000_000, 80_000_000_000, false),
            ]
        );
    }

    #[test]
    fn completed_beacon_counter_offsets_the_active_lap_number() {
        let source = counter_source(
            "Beacon Event Count",
            vec![0.0, 0.0, 1.0, 1.0, 2.0, 2.0, 3.0],
        );
        let metadata = read_source_metadata(&source);
        assert_eq!(
            metadata
                .laps
                .iter()
                .map(|lap| (lap.number, lap.complete))
                .collect::<Vec<_>>(),
            [(1, false), (2, true), (3, true), (4, false)]
        );
    }

    #[test]
    fn constant_counter_falls_through_to_other_lap_signals() {
        let mut source = counter_source("Lap Number", vec![1.0; 7]);
        let count = source.channels[0].sample_count;
        source.channels.push(Channel {
            id: 1,
            name: "Lap Progression".into(),
            unit: "%".into(),
            unit_source: UnitSource::Declared,
            sample_type: SampleType::F64,
            chunks: vec![Chunk {
                sample_period_ns: 10_000_000_000,
                sample_count: count,
                data_ptr: 0,
                sample_base: 0,
                time_base_ns: 0,
            }],
            sample_count: count,
            duration_ns: count * 10_000_000_000,
        });
        source
            .values
            .push(vec![80.0, 99.0, 2.0, 50.0, 98.0, 1.0, 30.0]);

        let metadata = read_source_metadata(&source);
        assert_eq!(metadata.laps.len(), 3);
        assert_eq!(metadata.laps[1].start_ns, 20_000_000_000);
        assert_eq!(metadata.laps[1].end_ns, 50_000_000_000);
    }

    #[test]
    fn groups_contiguous_files_and_merges_driver_stints() {
        let first = read_source_metadata(&metadata_source("part-1", 1_000_000_000_000, 3));
        let second = read_source_metadata(&metadata_source("part-2", 1_045_000_000_000, 3));
        let sessions = group_sessions(&[first, second], 10_000_000_000);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].files, [0, 1]);
        assert_eq!(sessions[0].driver_stints.len(), 1);
        assert_eq!(sessions[0].driver_stints[0].driver_id, 3);
    }
}
