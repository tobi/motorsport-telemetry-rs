//! Format-neutral file and session metadata derivation.
//!
//! [`read_source_metadata`] is a small pipeline of named strategy functions:
//!
//! 1. [`derive_clock`] resolves the absolute clock (GPS week/ITOW, an explicit
//!    source range, or nothing) plus the session candidate key.
//! 2. [`driver_stints`] splits the recording by internal driver identifier.
//! 3. Lap recovery lives in [`crate::laps`] and follows the precedence
//!    `authoritative > counter > timer` (see the README "How laps are
//!    recovered" section): [`crate::laps::authoritative_laps`],
//!    [`crate::laps::counter_laps`], [`crate::laps::timer_reset_laps`], and
//!    [`crate::laps::pick_laps`]; [`crate::laps::fastest_lap`] derives the
//!    fastest lap from the chosen set.
//! 4. [`video_summary`] collects linked-video counts and file references.
//!
//! Timestamp and lap arithmetic uses `checked_*` primitives and skips a value
//! on overflow (a dropped sample is noted at each site) rather than saturating.
//! File-derived floats are narrowed to integers through [`finite_i64`] /
//! [`finite_u64`], which return `None` for non-finite or out-of-range inputs.

use crate::names;
use crate::{AppliedPass, TelemetrySource};
use std::collections::{BTreeMap, BTreeSet};

use crate::laps;

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
    /// Presentation-order video frame at [`Self::start_ns`], when known.
    pub first_video_frame: Option<u64>,
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

/// A linked video file referenced by a telemetry recording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoFileRef {
    /// Basename only (`foo_0001.mp4`). Never a session key.
    pub filename: String,
    /// Source file index (`avifileindex`), when the recording spans files.
    pub index: u32,
    /// BLAKE3-256 of the video file, when it was present at convert time.
    pub blake3: Option<[u8; 32]>,
    /// Presentation-order frame count, when known.
    pub frame_count: u64,
    /// Offset satisfying `video_presentation_ns = file_relative_ns + offset`.
    pub presentation_offset_ns: Option<i128>,
}

/// Video linkage available at one telemetry timestamp.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VideoReference {
    /// Source video file index, when the recording spans multiple files.
    pub file_index: Option<u32>,
    /// Source-exact video synchronization time.
    pub sync_time: Option<f64>,
    /// Presentation timestamp on the linked video's movie timeline.
    pub presentation_time_ns: Option<u64>,
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
    /// Format identifier of the original recording this file was converted
    /// from. Equals [`Self::format`] when the file is itself the origin.
    pub source_format: String,
    /// Path of the original recording as seen at first conversion. Equals
    /// [`Self::path`] when the file is itself the origin.
    pub source_path: String,
    /// `.telemetry` catalog version. Absent on vendor source files.
    pub format_version: Option<u16>,
    /// Processing passes applied to this file, in application order.
    ///
    /// Empty on raw vendor files and raw conversions. Every listed pass only
    /// appended the channels in [`AppliedPass::outputs`].
    pub passes: Vec<AppliedPass>,
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
    /// Unix-epoch nanoseconds (UTC) at file `t = 0`.
    ///
    /// `utc_epoch_ns = file_relative_ns + utc_start_ns`. Absent when the
    /// source never stored a UTC-based clock. Do not invent this from civil
    /// `date`/`time` strings alone.
    pub utc_start_ns: Option<u64>,
    /// IANA timezone of the venue, e.g. `America/New_York`.
    ///
    /// Empty when unknown. Used to format a civil wall time from
    /// [`Self::utc_start_ns`]. Never used as a join key.
    pub timezone: String,
    /// Human-readable identity embedded in the source.
    pub identity: SourceIdentity,
    /// Distinct internal driver identifiers in ascending order.
    pub driver_ids: Vec<i64>,
    /// Driver intervals in file-relative time.
    pub driver_stints: Vec<DriverStint>,
    /// Lap intervals in file-relative time.
    pub laps: Vec<LapMetadata>,
    /// Number of complete flying laps. Stored as a header scalar in `.telemetry`.
    pub valid_laps: u32,
    /// Fastest complete or explicitly reported lap.
    pub fastest_lap: Option<LapMetadata>,
    /// Linked or embedded video frame count, when available.
    pub video_frame_count: Option<u64>,
    /// Offset satisfying `video_presentation_ns = file_relative_ns + offset`.
    pub video_presentation_offset_ns: Option<i128>,
    /// Linked video files in index order. Empty when the recording has no video.
    pub videos: Vec<VideoFileRef>,
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

/// Rounds a finite f64 to `i64`, returning `None` for NaN, infinity, or values
/// outside the `i64` range. Used for file-derived counter and driver values.
pub(crate) fn finite_i64(value: f64) -> Option<i64> {
    if value.is_finite() && (i64::MIN as f64..=i64::MAX as f64).contains(&value) {
        Some(value.round() as i64)
    } else {
        None
    }
}

/// Rounds a finite, non-negative f64 to `u64`, returning `None` for NaN,
/// infinity, negative, or out-of-range values. Used for file-derived timestamps.
pub(crate) fn finite_u64(value: f64) -> Option<u64> {
    if value.is_finite() && (0.0..=u64::MAX as f64).contains(&value) {
        Some(value.round() as u64)
    } else {
        None
    }
}

/// All native samples of one channel as `(time_ns, value)` pairs.
pub(crate) fn samples(source: &dyn TelemetrySource, channel_index: usize) -> Vec<(u64, f64)> {
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

/// Runs of consecutive equal integer values, as `(value, start_ns, end_ns)`.
fn integer_runs(values: &[(u64, f64)], duration_ns: u64) -> Vec<(i64, u64, u64)> {
    let mut runs = Vec::new();
    let mut current: Option<(i64, u64)> = None;
    for &(time_ns, value) in values {
        let Some(integer) = finite_i64(value) else {
            continue;
        };
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

/// Absolute clock coverage and session candidate key for one source.
struct ClockInfo {
    clock: Option<String>,
    start_ns: Option<u64>,
    end_ns: Option<u64>,
    offset_ns: Option<i128>,
    session_key: Option<String>,
}

/// Resolves the absolute clock (GPS week/ITOW or an explicit source range) and
/// the internal session candidate key.
///
/// GPS week and ITOW are narrowed through [`finite_u64`]; any non-finite or
/// out-of-range value leaves the GPS clock unset. The millisecond-to-nanosecond
/// chain uses `checked_*` arithmetic, so an overflow drops the whole GPS clock
/// rather than saturating to a wrong instant.
fn derive_clock(source: &dyn TelemetrySource, hash: u64) -> ClockInfo {
    let explicit_absolute = source.absolute_time_range();
    let absolute = names::find(source.channels(), &["gpsweek"]).and_then(|week_index| {
        let week = samples(source, week_index)
            .into_iter()
            .find_map(|(_, value)| finite_u64(value))?;
        let itow_index = names::find(source.channels(), &["gpsitow"])?;
        let itow = samples(source, itow_index);
        let &(first_time, first_value) = itow.first()?;
        let &(_last_time, last_value) = itow.last()?;
        let first_itow = finite_u64(first_value)?;
        let last_itow = finite_u64(last_value)?;
        // GPS clock overflow: leave the absolute clock unset on any failure.
        let start_ns = week
            .checked_mul(GPS_WEEK_MS)?
            .checked_add(first_itow)?
            .checked_add(GPS_UNIX_EPOCH_MS)?
            .checked_mul(1_000_000)?;
        let end_ns = week
            .checked_mul(GPS_WEEK_MS)?
            .checked_add(last_itow)?
            .checked_add(GPS_UNIX_EPOCH_MS)?
            .checked_mul(1_000_000)?;
        Some((week, first_time, start_ns, end_ns))
    });
    if let Some(range) = explicit_absolute {
        ClockInfo {
            clock: Some(range.clock),
            start_ns: Some(range.start_ns),
            end_ns: Some(range.end_ns),
            offset_ns: Some(i128::from(range.start_ns)),
            session_key: Some(format!("{}:{hash:016x}", range.session_hint)),
        }
    } else if let Some((week, first_time, start_ns, end_ns)) = absolute {
        ClockInfo {
            clock: Some("gps".into()),
            start_ns: Some(start_ns),
            end_ns: Some(end_ns),
            offset_ns: Some(i128::from(start_ns) - i128::from(first_time)),
            session_key: Some(format!("gps:{week}:{hash:016x}")),
        }
    } else {
        ClockInfo {
            clock: None,
            start_ns: None,
            end_ns: None,
            offset_ns: None,
            session_key: None,
        }
    }
}

/// Distinct driver identifiers and their contiguous stints.
///
/// When several runs exist, stints shorter than one second are dropped as
/// transient noise; a single run is always kept. `checked_sub` makes an
/// inverted run (end before start) fail the duration test instead of
/// saturating to zero.
fn driver_stints(source: &dyn TelemetrySource, duration_ns: u64) -> (Vec<i64>, Vec<DriverStint>) {
    let raw_driver_runs = names::find(source.channels(), &["driverid", "driver", "driverindex"])
        .map(|index| integer_runs(&samples(source, index), duration_ns))
        .unwrap_or_default();
    let driver_runs = raw_driver_runs
        .iter()
        .copied()
        .filter(|(_, start_ns, end_ns)| {
            raw_driver_runs.len() == 1
                || end_ns
                    .checked_sub(*start_ns)
                    .is_some_and(|duration| duration >= 1_000_000_000)
        })
        .collect::<Vec<_>>();
    let driver_ids = driver_runs
        .iter()
        .map(|(driver, _, _)| *driver)
        .filter(|driver| *driver >= 0)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let stints = driver_runs
        .into_iter()
        .filter(|(driver, _, _)| *driver >= 0)
        .map(|(driver_id, start_ns, end_ns)| DriverStint {
            driver_id,
            start_ns,
            end_ns,
        })
        .collect::<Vec<_>>();
    (driver_ids, stints)
}

/// Linked-video frame count, presentation offset, and file references.
fn video_summary(source: &dyn TelemetrySource) -> (Option<u64>, Option<i128>, Vec<VideoFileRef>) {
    let offset = source.video_presentation_offset_ns();
    let videos = source
        .video_files()
        .iter()
        .cloned()
        .map(|mut video| {
            if video.presentation_offset_ns.is_none() {
                video.presentation_offset_ns = offset;
            }
            if video.frame_count == 0 {
                if let Some(count) = source.video_frame_count() {
                    video.frame_count = count;
                }
            }
            video
        })
        .collect();
    (source.video_frame_count(), offset, videos)
}

/// Stable FNV-1a of lowercased channel names, raw units, and sample-type codes.
pub fn schema_hash(source: &dyn TelemetrySource) -> u64 {
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
/// The pipeline is documented in the module-level comment.
pub fn read_source_metadata(source: &dyn TelemetrySource) -> FileMetadata {
    let duration_ns = source
        .channels()
        .iter()
        .map(|channel| channel.duration_ns)
        .max()
        .unwrap_or(0);
    let hash = schema_hash(source);
    let clock = derive_clock(source, hash);
    let (driver_ids, driver_stints) = driver_stints(source, duration_ns);

    let authoritative = laps::authoritative_laps(source);
    let (lap_channel_index, counter_laps, counter_crossings, timer_laps) = match &authoritative {
        Some(_) => (None, Vec::new(), 0, Vec::new()),
        None => {
            let (index, counter_laps, crossings) = laps::counter_laps(source, duration_ns);
            let timer_laps = laps::timer_reset_laps(source, duration_ns, index);
            (index, counter_laps, crossings, timer_laps)
        }
    };
    let mut laps = laps::pick_laps(
        authoritative.as_ref(),
        counter_laps,
        counter_crossings,
        timer_laps,
    );
    let mut fastest_lap =
        laps::fastest_lap(source, &laps, authoritative.as_ref(), lap_channel_index);

    stamp_lap_video_frames(source, &mut laps);
    if let Some(fastest) = &mut fastest_lap {
        if fastest.first_video_frame.is_none() {
            fastest.first_video_frame = source.video_frame_at(fastest.start_ns);
        }
    }

    let (video_frame_count, video_presentation_offset_ns, videos) = video_summary(source);
    let origin = source.source_origin();
    let mut metadata = FileMetadata {
        path: source.path().to_owned(),
        format: source.format().to_owned(),
        source_format: origin
            .as_ref()
            .map(|origin| origin.format.clone())
            .filter(|format| !format.is_empty())
            .unwrap_or_else(|| source.format().to_owned()),
        source_path: origin
            .map(|origin| origin.path)
            .filter(|path| !path.is_empty())
            .unwrap_or_else(|| source.path().to_owned()),
        passes: source.applied_passes().to_vec(),
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
        format_version: None,
        session_key: clock.session_key,
        absolute_clock: clock.clock,
        absolute_start_ns: clock.start_ns,
        absolute_end_ns: clock.end_ns,
        clock_offset_ns: clock.offset_ns,
        utc_start_ns: None,
        timezone: String::new(),
        identity: source.identity(),
        driver_ids,
        driver_stints,
        valid_laps: laps.iter().filter(|lap| lap.complete).count() as u32,
        laps,
        fastest_lap,
        video_frame_count,
        video_presentation_offset_ns,
        videos,
    };
    let timezone = crate::placement::resolve_timezone(source);
    let utc_start_ns = source
        .utc_start_ns()
        .or_else(|| crate::placement::utc_from_metadata(&metadata, &timezone));
    metadata.utc_start_ns = utc_start_ns;
    metadata.timezone = timezone;
    metadata
}

fn stamp_lap_video_frames(source: &dyn TelemetrySource, laps: &mut [LapMetadata]) {
    for lap in laps {
        if lap.first_video_frame.is_none() {
            lap.first_video_frame = source.video_frame_at(lap.start_ns);
        }
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
                            first_video_frame: lap.first_video_frame,
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
/// Returns an empty map if no recognized sampled channel exists. Finite,
/// in-range values are rounded to the nearest integer identifier; out-of-range
/// values are skipped.
pub fn driver_histogram(source: &dyn TelemetrySource) -> BTreeMap<i64, u64> {
    let Some(index) = names::find(source.channels(), &["driverid", "driver", "driverindex"]) else {
        return BTreeMap::new();
    };
    let mut counts = BTreeMap::new();
    for (_, value) in samples(source, index) {
        if let Some(id) = finite_i64(value) {
            *counts.entry(id).or_default() += 1;
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
    fn beacon_counter_wins_over_binary_lap_number_flag() {
        let mut source = counter_source("Lap Number", vec![0.0, 0.0, 1.0, 1.0, 1.0, 1.0, 0.0]);
        let count = source.channels[0].sample_count;
        source.channels.push(Channel {
            id: 1,
            name: "beaconEventCount".into(),
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
        });
        source.values.push(vec![0.0, 0.0, 1.0, 2.0, 3.0, 4.0, 4.0]);

        let metadata = read_source_metadata(&source);
        assert_eq!(
            metadata
                .laps
                .iter()
                .map(|lap| (lap.number, lap.complete))
                .collect::<Vec<_>>(),
            [(1, false), (2, true), (3, true), (4, true), (5, false)]
        );
        assert_eq!(metadata.valid_laps, 3);
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
