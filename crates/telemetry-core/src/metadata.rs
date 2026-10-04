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

/// What a lap interval is, once stints are known.
///
/// A vendor lap counter is really a *stint* lap counter: an `AiM` dash resets
/// `Lap_Number` to 0 when the car stops in the pits, a Cosworth logger keeps
/// counting across a stop, a power-cycled logger starts again at 1. The
/// intervals between crossings are therefore not all laps of the same kind,
/// and a session commonly holds several in/out pairs. [`LapKind`] is the
/// normalised answer, so consumers can filter flying laps without knowing
/// which logger wrote the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LapKind {
    /// Not yet classified. Readers that store laps without a kind leave
    /// this; [`read_source_metadata`] resolves it.
    #[default]
    Unknown,
    /// Beacon to beacon with the car moving throughout.
    Flying,
    /// Starts at a stint start (pit exit, logger start) and ends at the
    /// first beacon. No start beacon.
    Out,
    /// Starts at a beacon and ends where the stint ends (pit box, counter
    /// reset, logger stop). No end beacon.
    In,
    /// A stint with no beacon at all: one fragment from start to stop.
    OutIn,
    /// Beacon to beacon, but the car stood still for a pit-stop's worth of
    /// time inside it: an in-lap and out-lap the counter did not separate.
    Pit,
}

impl LapKind {
    /// Stable lowercase token used in labels and on disk.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Flying => "flying",
            Self::Out => "out",
            Self::In => "in",
            Self::OutIn => "out-in",
            Self::Pit => "pit",
        }
    }

    /// Inverse of [`Self::as_str`]; anything else is [`LapKind::Unknown`].
    pub fn parse(token: &str) -> Self {
        match token {
            "flying" => Self::Flying,
            "out" => Self::Out,
            "in" => Self::In,
            "out-in" => Self::OutIn,
            "pit" => Self::Pit,
            _ => Self::Unknown,
        }
    }

    /// True for a complete beacon-to-beacon lap without a stop.
    pub fn is_flying(self) -> bool {
        self == Self::Flying
    }
}

/// One lap boundary derived from source channels or reported lap timing.
///
/// `number` is the **virtual session lap number**: 1-based and strictly
/// increasing across the whole recording, through every stint, so two
/// consumers of the same file always mean the same interval by "lap 7".
/// The vendor counter's value lives in `stint_lap`; the stint index in
/// `stint`; the normalised role in `kind`; and [`Self::label`] renders the
/// three as one human string (`S2 L3`, `S1 in`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LapMetadata {
    /// Virtual session lap number (1-based, monotonic across stints). Zero
    /// only on a lap that has not been through [`read_source_metadata`].
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
    /// 1-based stint index within the recording. Zero when unassigned.
    pub stint: u32,
    /// Lap number as the source counted it within the stint (the raw
    /// `Lap_Number` value), or the 1-based position in the stint when no
    /// counter exists.
    pub stint_lap: i64,
    /// Normalised role of this interval.
    pub kind: LapKind,
}

impl LapMetadata {
    /// A lap with only its interval known; stint, kind and virtual number
    /// are filled in by [`read_source_metadata`].
    pub fn interval(number: i64, start_ns: u64, end_ns: u64, complete: bool) -> Self {
        Self {
            number,
            start_ns,
            end_ns,
            duration_ns: end_ns.saturating_sub(start_ns),
            complete,
            first_video_frame: None,
            stint: 0,
            stint_lap: number,
            kind: LapKind::Unknown,
        }
    }

    /// Human label normalised across loggers: `S1 out`, `S1 L2`, `S1 L3`,
    /// `S1 in`, `S2 out`, …; a stint without a beacon is `S3 out-in`; a
    /// beacon-to-beacon lap containing a stop is `S1 pit L5`. `L{n}` uses
    /// the stint-local lap number so it matches the dash display.
    pub fn label(&self) -> String {
        let stint = self.stint.max(1);
        match self.kind {
            LapKind::Flying | LapKind::Unknown => format!("S{stint} L{}", self.stint_lap),
            LapKind::Out => format!("S{stint} out"),
            LapKind::In => format!("S{stint} in"),
            LapKind::OutIn => format!("S{stint} out-in"),
            LapKind::Pit => format!("S{stint} pit L{}", self.stint_lap),
        }
    }
}

/// Authoritative lap information supplied directly by a source format.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceLapMetadata {
    /// Known lap intervals in file-relative time.
    pub laps: Vec<LapMetadata>,
    /// Fastest lap explicitly reported by the source, when available.
    pub fastest_lap: Option<LapMetadata>,
}

/// A contiguous interval attributed to one internal driver identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// Native identity before descriptive metadata overrides.
    pub source_identity: SourceIdentity,
    /// Human-readable identity, with descriptive metadata overrides applied.
    pub identity: SourceIdentity,
    /// Additional file-level metadata, including resolved `TRACK.yml` fields.
    /// Never changes numeric samples, clocks, video timing, or session keys.
    pub extra: crate::MetadataMap,
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
#[derive(Debug, Clone, PartialEq, Eq)]
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

/// Resolves the absolute clock (an explicit source range, GPS week/ITOW, or a
/// Unix-seconds channel such as Cosworth `Global Time`) and the internal
/// session candidate key.
///
/// GPS week and ITOW are narrowed through [`finite_u64`]; any non-finite or
/// out-of-range value leaves the GPS clock unset. The millisecond-to-nanosecond
/// chain uses `checked_*` arithmetic, so an overflow drops the whole GPS clock
/// rather than saturating to a wrong instant.
fn derive_clock(source: &dyn TelemetrySource, hash: u64) -> ClockInfo {
    let explicit_absolute = source.absolute_time_range();
    let absolute = names::find(source.channels(), &["gpsweek"]).and_then(|week_index| {
        // A recording never spans a GPS week boundary (the week lasts seven
        // days), so the week is a constant. Take the *dominant* value rather
        // than the first sample: u-blox receivers can emit a stale week for a
        // few seconds at power-on (an AiM SmartyCam reported 2117 for 4.5 s
        // before correcting to 2437), and anchoring the whole recording's UTC
        // on that startup sample dates it six years early. The time-of-week
        // (`GPS iTOW`) is unaffected by the stale week, so pairing the modal
        // week with the first/last iTOW recovers the true span.
        let week = dominant_gps_week(source, week_index)?;
        let itow_index = names::find(source.channels(), &["gpsitow"])?;
        let itow = samples(source, itow_index);
        let &(first_time, first_value) = itow.first()?;
        let &(_last_time, last_value) = itow.last()?;
        let first_itow = finite_u64(first_value)?;
        let last_itow = finite_u64(last_value)?;
        // An iTOW that wrapped past the week boundary belongs to the next week.
        let end_week = if last_itow < first_itow {
            week.checked_add(1)?
        } else {
            week
        };
        // GPS clock overflow: leave the absolute clock unset on any failure.
        let start_ns = week
            .checked_mul(GPS_WEEK_MS)?
            .checked_add(first_itow)?
            .checked_add(GPS_UNIX_EPOCH_MS)?
            .checked_mul(1_000_000)?;
        let end_ns = end_week
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
    } else if let Some((first_time, start_ns, end_ns)) = unix_clock(source) {
        ClockInfo {
            clock: Some("utc".into()),
            start_ns: Some(start_ns),
            end_ns: Some(end_ns),
            offset_ns: Some(i128::from(start_ns) - i128::from(first_time)),
            session_key: Some(format!(
                "utc:{}:{hash:016x}",
                start_ns / 1_000_000_000 / 86_400
            )),
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

/// GPS week that covers most of a recording.
///
/// A receiver can report a stale week for the first few seconds after a cold
/// start; the corrected week then holds for the rest of the recording. Because
/// a recording cannot cross a real week boundary, the modal value is the
/// authoritative one. Ties are broken by the last occurrence so the corrected
/// week wins over the startup artifact.
fn dominant_gps_week(source: &dyn TelemetrySource, week_index: usize) -> Option<u64> {
    let mut tally: BTreeMap<u64, (usize, usize)> = BTreeMap::new();
    for (position, (_, value)) in samples(source, week_index).into_iter().enumerate() {
        if let Some(week) = finite_u64(value) {
            let entry = tally.entry(week).or_insert((0, 0));
            entry.0 += 1;
            entry.1 = position;
        }
    }
    tally
        .into_iter()
        .max_by_key(|(_, (count, last))| (*count, *last))
        .map(|(week, _)| week)
}

/// Earliest instant a logger could honestly report: 2000-01-01T00:00:00Z.
const UNIX_CLOCK_MIN_S: f64 = 946_684_800.0;
/// Latest: 2100-01-01T00:00:00Z. Anything outside is a counter, not a clock.
const UNIX_CLOCK_MAX_S: f64 = 4_102_444_800.0;

/// An absolute clock from a channel that logs Unix-epoch seconds directly —
/// Cosworth's `Global Time`, for instance.
///
/// Returns `(first_sample_time_ns, start_ns, end_ns)`, where `start_ns` is the
/// wall clock at the first sample. The channel is trusted only when every
/// finite value is a plausible date, the values never run backwards, and the
/// clock advances at the same rate as the sample timeline (within 2 % or two
/// seconds, whichever is larger): a channel that fails any of those is
/// counting something else.
fn unix_clock(source: &dyn TelemetrySource) -> Option<(u64, u64, u64)> {
    let index = names::find(source.channels(), &["globaltime", "unixtime", "epochtime"])?;
    let values: Vec<(u64, f64)> = samples(source, index)
        .into_iter()
        .filter(|(_, value)| value.is_finite())
        .collect();
    let &(first_time, first_value) = values.first()?;
    let &(last_time, last_value) = values.last()?;
    if values.len() < 2
        || values
            .iter()
            .any(|(_, value)| !(UNIX_CLOCK_MIN_S..=UNIX_CLOCK_MAX_S).contains(value))
        || values.windows(2).any(|pair| pair[1].1 < pair[0].1)
    {
        return None;
    }
    let clock_span_s = last_value - first_value;
    let sample_span_s = last_time.saturating_sub(first_time) as f64 / 1e9;
    let tolerance_s = (sample_span_s * 0.02).max(2.0);
    if (clock_span_s - sample_span_s).abs() > tolerance_s {
        return None;
    }
    let start_ns = finite_u64(first_value * 1e9)?;
    let end_ns = finite_u64(last_value * 1e9)?;
    Some((first_time, start_ns, end_ns))
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
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for channel in source.channels() {
        for byte in channel.name.bytes().map(|byte| byte.to_ascii_lowercase()) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        for byte in channel.unit.bytes().map(|byte| byte.to_ascii_lowercase()) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        hash ^= u64::from(channel.sample_type.code());
        hash = hash.wrapping_mul(0x0100_0000_01b3);
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
    let (counter_laps, counter_crossings, timer_laps, snap_window_ns) = if authoritative.is_some() {
        (Vec::new(), 0, Vec::new(), 0)
    } else {
        let (index, counter_laps, crossings) = laps::counter_laps(source, duration_ns);
        let timer_laps = laps::timer_reset_laps(source, duration_ns, index);
        let snap_window_ns = laps::snap_window_ns(
            laps::channel_period_ns(source, index),
            laps::channel_period_ns(source, laps::timer_channel(source)),
        );
        (counter_laps, crossings, timer_laps, snap_window_ns)
    };
    let mut laps = laps::pick_laps(
        authoritative.as_ref(),
        counter_laps,
        counter_crossings,
        timer_laps,
        snap_window_ns,
    );
    laps::classify_laps(source, &mut laps);
    let mut fastest_lap = laps::fastest_lap(source, &laps, authoritative.as_ref());

    stamp_lap_video_frames(source, &mut laps);
    if let Some(fastest) = &mut fastest_lap {
        if fastest.first_video_frame.is_none() {
            fastest.first_video_frame = source.video_frame_at(fastest.start_ns);
        }
    }

    let (video_frame_count, video_presentation_offset_ns, videos) = video_summary(source);
    let origin = source.source_origin();
    let identity = source.identity();
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
        session_key: clock.session_key,
        absolute_clock: clock.clock,
        absolute_start_ns: clock.start_ns,
        absolute_end_ns: clock.end_ns,
        clock_offset_ns: clock.offset_ns,
        utc_start_ns: None,
        timezone: String::new(),
        source_identity: identity.clone(),
        identity,
        extra: source.extra_metadata(),
        driver_ids,
        driver_stints,
        valid_laps: laps.iter().filter(|lap| lap.kind.is_flying()).count() as u32,
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
    metadata.apply_extra_metadata();
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
        if let Some(group) = groups.last_mut().filter(|_| joins_previous) {
            group.push(index);
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
                            stint: lap.stint,
                            stint_lap: lap.stint_lap,
                            kind: lap.kind,
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
        // The fastest lap is one of the laps, never an interval rebuilt from
        // a `Previous_LT` report (18 s here) that matches no lap boundary; a
        // source and its `.telemetry` conversion must name the same lap.
        let fastest = metadata.fastest_lap.as_ref().unwrap();
        assert_eq!(fastest.duration_ns, 10_000_000_000);
        assert!(metadata.laps.iter().any(|lap| lap == fastest));
        assert!(metadata
            .session_key
            .as_deref()
            .unwrap()
            .starts_with("test-session:"));
    }

    fn with_clock_channel(name: &str, values: Vec<f64>) -> MetadataSource {
        let mut source = metadata_source("clock", 0, 3);
        source.channels = vec![Channel {
            id: 0,
            name: name.into(),
            unit: "s".into(),
            unit_source: UnitSource::Declared,
            sample_type: SampleType::F64,
            chunks: vec![Chunk {
                sample_period_ns: 1_000_000_000,
                sample_count: values.len() as u64,
                data_ptr: 0,
                sample_base: 0,
                time_base_ns: 5_000_000_000,
            }],
            sample_count: values.len() as u64,
            duration_ns: 5_000_000_000 + values.len() as u64 * 1_000_000_000,
        }];
        source.values = vec![values];
        source
    }

    struct NoClock(MetadataSource);
    impl TelemetrySource for NoClock {
        fn path(&self) -> &str {
            self.0.path()
        }
        fn format(&self) -> &'static str {
            self.0.format()
        }
        fn channels(&self) -> &[Channel] {
            self.0.channels()
        }
        fn decode(&self, channel_index: usize, chunk_index: usize, local_index: u64) -> f64 {
            self.0.decode(channel_index, chunk_index, local_index)
        }
    }

    fn gps_clock_source(weeks: Vec<f64>, itows: Vec<f64>) -> MetadataSource {
        let names = ["GPS Week", "GPS iTOW"];
        let count = weeks.len() as u64;
        let channels = names
            .into_iter()
            .enumerate()
            .map(|(index, name)| Channel {
                id: index as u32,
                name: name.into(),
                unit: if index == 0 {
                    "count".into()
                } else {
                    "ms".into()
                },
                unit_source: UnitSource::Unknown,
                sample_type: SampleType::F64,
                chunks: vec![Chunk {
                    sample_period_ns: 1_000_000_000,
                    sample_count: count,
                    data_ptr: 0,
                    sample_base: 0,
                    time_base_ns: 0,
                }],
                sample_count: count,
                duration_ns: count * 1_000_000_000,
            })
            .collect();
        MetadataSource {
            path: "gps".into(),
            channels,
            values: vec![weeks, itows],
            absolute_start_ns: 0,
        }
    }

    #[test]
    fn a_stale_startup_gps_week_does_not_date_the_recording() {
        // AiM SmartyCam SCHD0215 reported GPS week 2117 for the first ~4.5 s
        // before correcting to 2437. The modal week must win: anchoring on the
        // first sample dates the recording 2020-08-02 instead of 2026-09-20.
        let weeks = vec![2117.0, 2117.0, 2437.0, 2437.0, 2437.0, 2437.0];
        let itows = vec![
            0.0,
            900_000.0,
            1_000_000.0,
            2_000_000.0,
            3_000_000.0,
            4_000_000.0,
        ];
        let gps = gps_clock_source(weeks, itows);
        let metadata = read_source_metadata(&NoClock(gps));
        assert_eq!(metadata.absolute_clock.as_deref(), Some("gps"));
        let expected = (2437u64 * 604_800_000 + 315_964_800_000) * 1_000_000;
        assert_eq!(metadata.absolute_start_ns, Some(expected));
        assert!(metadata
            .session_key
            .as_deref()
            .unwrap()
            .starts_with("gps:2437:"));
    }

    #[test]
    fn unix_seconds_channel_becomes_the_absolute_clock() {
        // Cosworth `Global Time`: Unix seconds at 1 Hz, first sample 5 s into
        // the file. The wall clock at t = 0 is therefore five seconds earlier.
        let first = 1_737_644_480.0; // 2025-01-23T15:01:20Z
        let values: Vec<f64> = (0..20).map(|i| first + f64::from(i)).collect();
        let source = NoClock(with_clock_channel("Global Time", values));
        let metadata = read_source_metadata(&source);
        assert_eq!(metadata.absolute_clock.as_deref(), Some("utc"));
        assert_eq!(metadata.absolute_start_ns, Some(1_737_644_480_000_000_000));
        assert_eq!(
            metadata.clock_offset_ns,
            Some(1_737_644_480_000_000_000 - 5_000_000_000)
        );
        assert!(metadata
            .session_key
            .as_deref()
            .unwrap()
            .starts_with("utc:20111:"));
    }

    #[test]
    fn a_counter_that_is_not_wall_time_is_not_a_clock() {
        // Plausible magnitude but advancing ten seconds per sample: that is
        // not a clock running alongside the timeline.
        let values: Vec<f64> = (0..20)
            .map(|i| 1_737_644_480.0 + 10.0 * f64::from(i))
            .collect();
        let racing = NoClock(with_clock_channel("Global Time", values));
        assert_eq!(read_source_metadata(&racing).absolute_clock, None);
        // Right rate, impossible date.
        let values: Vec<f64> = (0..20).map(|i| 12_345.0 + f64::from(i)).collect();
        let early = NoClock(with_clock_channel("Global Time", values));
        assert_eq!(read_source_metadata(&early).absolute_clock, None);
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
    fn declared_timer_units_override_magnitude() {
        // Cosworth's long installation runs use seconds even above 1000.
        // A corrupt/high first value must not turn all subsequent seconds
        // into milliseconds and hide ordinary 100-second laps.
        for (unit, multiplier) in [("s", 1.0), ("ms", 1000.0)] {
            let mut source = counter_source(
                "Lap Time",
                [1158.0, 0.5, 90.5, 100.5, 0.5, 10.5]
                    .map(|v| v * multiplier)
                    .to_vec(),
            );
            source.channels[0].unit = unit.into();
            let laps = read_source_metadata(&source).laps;
            assert_eq!(laps.len(), 3, "{unit}: {laps:?}");
            assert_eq!(laps[0].end_ns, 9_500_000_000);
            assert_eq!(laps[1].end_ns, 39_500_000_000);
        }
    }

    #[test]
    fn timer_boundary_before_the_recording_is_not_a_zero_length_lap() {
        let mut source = counter_source("Lap Time", vec![20.0, 10.0, 15.0, 0.5, 10.5]);
        source.channels[0].unit = "s".into();
        let laps = read_source_metadata(&source).laps;
        assert_eq!(laps.len(), 2, "{laps:?}");
        assert!(laps.iter().all(|lap| lap.duration_ns > 0));
        assert_eq!(laps[0].end_ns, 29_500_000_000);
    }

    #[test]
    fn timer_reset_after_a_lagging_counter_advances_the_lap_number() {
        // Real Cosworth race fragment: `beaconEventCount` sits at 22 for the
        // whole 65 s file (active lap 23); `Lap Time` resets 0.6 s before
        // the end and the 10 Hz counter never caught up. The tail fragment
        // is lap 24, not a second lap 23.
        let mut source = counter_source("beaconEventCount", vec![22.0; 7]);
        let count = source.channels[0].sample_count;
        source.channels.push(Channel {
            id: 1,
            name: "Lap Time".into(),
            unit: "s".into(),
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
            .push(vec![30.0, 40.0, 50.0, 60.0, 70.0, 80.0, 0.5]);
        let metadata = read_source_metadata(&source);
        assert_eq!(
            metadata
                .laps
                .iter()
                .map(|lap| (lap.stint_lap, lap.complete))
                .collect::<Vec<_>>(),
            [(23, false), (24, false)]
        );
        assert!(metadata.laps.windows(2).all(|w| w[1].number > w[0].number));
    }

    #[test]
    fn corrupt_counter_spike_does_not_swallow_later_laps() {
        // Radio-received Cosworth log: one bit-flipped `Lap Number` sample
        // reads 1_009_840_763 mid-file. It is not a crossing, and the laps
        // counted after it must still be found.
        let source = counter_source(
            "Lap Number",
            vec![1.0, 1.0, 2.0, 1_009_840_763.0, 2.0, 3.0, 3.0, 4.0],
        );
        let metadata = read_source_metadata(&source);
        assert_eq!(
            metadata
                .laps
                .iter()
                .map(|lap| (lap.stint_lap, lap.complete))
                .collect::<Vec<_>>(),
            [(1, false), (2, true), (3, true), (4, false)]
        );
        // A genuine multi-lap jump that the next sample confirms (counter
        // resumed after a logging gap) is still a crossing.
        let source = counter_source("Lap Number", vec![1.0, 1.0, 4.0, 4.0, 5.0, 5.0]);
        let metadata = read_source_metadata(&source);
        assert_eq!(
            metadata
                .laps
                .iter()
                .map(|lap| lap.stint_lap)
                .collect::<Vec<_>>(),
            [1, 4, 5]
        );
    }

    #[test]
    fn timer_resync_to_a_large_value_is_not_a_lap_boundary() {
        // AiM `Current_Lap_Time` (ms, 10 s period here): a power-on count is
        // resynced to 126.65 s at sample 2, then genuinely resets (60 ms) at
        // sample 5. Only the reset is a boundary; the resync would otherwise
        // put one at `t = 0` (a zero-length lap) and another mid-lap.
        let mut source = counter_source("Lap Number", vec![0.0; 8]);
        source.channels[0].name = "Current_Lap_Time".into();
        source.values[0] = vec![
            31_458_000.0,
            31_468_000.0,
            126_650.0,
            136_650.0,
            146_650.0,
            60.0,
            10_060.0,
            20_060.0,
        ];
        let metadata = read_source_metadata(&source);
        let bounds: Vec<(u64, u64)> = metadata
            .laps
            .iter()
            .map(|lap| (lap.start_ns, lap.end_ns))
            .collect();
        assert_eq!(
            bounds,
            [(0, 49_940_000_000), (49_940_000_000, 80_000_000_000)]
        );
        assert!(metadata.laps.iter().all(|lap| lap.duration_ns > 0));
    }

    #[test]
    fn upward_lap_counter_is_preferred_and_a_reset_starts_a_new_stint() {
        // 10 s samples: the drop to 0 at 60 s is not recovered within
        // RESET_CONFIRM_NS, so it is a stint boundary, not a glitch. Lap 3
        // becomes stint 1's in-lap; the 0 -> 1 climb is stint 2's out-lap
        // and its tail fragment. Virtual numbers run straight through.
        let source = counter_source("Lap Number", vec![1.0, 1.0, 2.0, 2.0, 3.0, 3.0, 0.0, 1.0]);
        let metadata = read_source_metadata(&source);
        assert_eq!(
            metadata
                .laps
                .iter()
                .map(|lap| (
                    lap.number,
                    lap.stint,
                    lap.stint_lap,
                    lap.start_ns,
                    lap.end_ns,
                    lap.kind
                ))
                .collect::<Vec<_>>(),
            [
                (1, 1, 1, 0, 20_000_000_000, LapKind::Out),
                (2, 1, 2, 20_000_000_000, 40_000_000_000, LapKind::Flying),
                (3, 1, 3, 40_000_000_000, 60_000_000_000, LapKind::In),
                (4, 2, 0, 60_000_000_000, 70_000_000_000, LapKind::Out),
                (5, 2, 1, 70_000_000_000, 80_000_000_000, LapKind::In),
            ]
        );
        assert_eq!(
            metadata
                .laps
                .iter()
                .map(LapMetadata::label)
                .collect::<Vec<_>>(),
            ["S1 out", "S1 L2", "S1 in", "S2 out", "S2 in"]
        );
        assert_eq!(metadata.valid_laps, 1);
    }

    /// Builds a multi-channel synthetic source; every channel shares one
    /// sample period.
    fn channels_source(period_ns: u64, channels: Vec<(&str, &str, Vec<f64>)>) -> MetadataSource {
        let built = channels
            .iter()
            .enumerate()
            .map(|(index, (name, unit, values))| {
                let count = values.len() as u64;
                Channel {
                    id: index as u32,
                    name: (*name).into(),
                    unit: (*unit).into(),
                    unit_source: if unit.is_empty() {
                        UnitSource::Unknown
                    } else {
                        UnitSource::Declared
                    },
                    sample_type: SampleType::F64,
                    chunks: vec![Chunk {
                        sample_period_ns: period_ns,
                        sample_count: count,
                        data_ptr: 0,
                        sample_base: 0,
                        time_base_ns: 0,
                    }],
                    sample_count: count,
                    duration_ns: count * period_ns,
                }
            })
            .collect();
        MetadataSource {
            path: "stints".into(),
            channels: built,
            values: channels.into_iter().map(|(_, _, values)| values).collect(),
            absolute_start_ns: 1_000_000_000_000,
        }
    }

    /// Indianapolis 2026 test, CT3 Run2 (`AiM` `SmartyCam` `.MP4.telemetry`):
    /// `Lap_Number` ran 1,2,3,4 then dropped to 0 when the car stopped in
    /// the pit box (t = 264 s), climbed 1..5 in the second stint and dropped
    /// to 0 again at the end; `Current_Lap_Time` reset at every beacon *and*
    /// at both pit resets. The old counter walk ignored the drop as a
    /// "shutdown reset", so 262 -> 626 s (four flying laps) became one
    /// 364 s "complete" lap and the 73 s in-lap fragment 189 -> 262 s, cut
    /// short by the pit-box reset with the car standing still inside it,
    /// was reported as the fastest lap at a 1:16 circuit. Modelled here at
    /// 1/5 scale (one channel sample per second, laps of 16-20 s so they
    /// clear the 10 s plausibility floor).
    #[test]
    fn aim_pit_box_counter_reset_splits_stints_and_never_wins_fastest_lap() {
        // seconds: 0..20 lap 1 | 20..36 lap 2 | 36..43 in-lap | 43 the dash
        // closes the lap as the car stops in the box (counter -> 4, timer
        // reset) | 44 counter reset to 0 | 46 dash arms 0 -> 1, still parked
        // | 60 pit exit | 70 beacon -> 2 | 86 -> 3 | 102 -> 4, tail. Exactly
        // the CT3 Run2 sequence (262.4 s +1, 264.0 s -> 0, 303.4 s -> 1).
        let mut lap_number = Vec::new();
        let mut lap_time = Vec::new();
        let mut speed = Vec::new();
        for t in 0..108u32 {
            let (lap, since) = match t {
                0..=19 => (1.0, t),
                20..=35 => (2.0, t - 20),
                36..=42 => (3.0, t - 36),
                43 => (4.0, 0),
                44..=45 => (0.0, t - 43),
                46..=69 => (1.0, t - 43),
                70..=85 => (2.0, t - 70),
                86..=101 => (3.0, t - 86),
                _ => (4.0, t - 102),
            };
            lap_number.push(lap);
            lap_time.push(f64::from(since) * 1000.0 + 50.0); // ms, first sample 50 ms after the reset
            speed.push(if (42..=59).contains(&t) || t >= 104 {
                0.0
            } else {
                40.0
            });
        }
        let source = channels_source(
            1_000_000_000,
            vec![
                ("Lap_Number", "", lap_number),
                ("Current_Lap_Time", "", lap_time),
                ("Speed_Wspd_App", "km/h", speed),
            ],
        );
        let metadata = read_source_metadata(&source);
        let shape: Vec<(i64, u32, i64, LapKind, bool)> = metadata
            .laps
            .iter()
            .map(|lap| (lap.number, lap.stint, lap.stint_lap, lap.kind, lap.complete))
            .collect();
        assert_eq!(
            shape,
            [
                (1, 1, 1, LapKind::Out, false),
                (2, 1, 2, LapKind::Flying, true),
                (3, 1, 3, LapKind::In, false),
                (4, 2, 1, LapKind::Out, false),
                (5, 2, 2, LapKind::Flying, true),
                (6, 2, 3, LapKind::Flying, true),
                (7, 2, 4, LapKind::In, false),
            ]
        );
        assert_eq!(
            metadata
                .laps
                .iter()
                .map(LapMetadata::label)
                .collect::<Vec<_>>(),
            ["S1 out", "S1 L2", "S1 in", "S2 out", "S2 L2", "S2 L3", "S2 in"]
        );
        // No 1 s fragment between the dash's pit-event increment (43 s) and
        // the counter reset (44 s): the in-lap absorbs it. Its end sits on
        // the pit-event timer reset (first post-reset sample 50 ms -> 42.95
        // s), which the counter drop a second later snaps onto.
        assert_eq!(metadata.laps[2].end_ns, 42_950_000_000);
        assert_eq!(metadata.laps[3].start_ns, metadata.laps[2].end_ns);
        assert_eq!(metadata.valid_laps, 3);
        // The 8 s in-lap fragment (36 -> 44 s) is shorter than every flying
        // lap and must not be the fastest lap.
        let fastest = metadata.fastest_lap.as_ref().unwrap();
        assert!(fastest.kind.is_flying(), "{fastest:?}");
        assert_eq!(fastest.stint_lap, 2);
        assert_eq!(fastest.stint, 1);
        // Timer resets place the beacons: the first post-reset timer sample
        // reads 50 ms, so the crossings are recovered 50 ms before the 1 s
        // sample that carried them. Lap 2 of stint 1 is 19.95 -> 35.95 s.
        assert_eq!(
            (fastest.start_ns, fastest.end_ns),
            (19_950_000_000, 35_950_000_000)
        );
    }

    /// A Cosworth-style logger keeps counting across a pit stop: the
    /// beacon-to-beacon interval that contains the stop is one long
    /// "complete" lap. It is a pit lap, closes its stint, and is never the
    /// fastest lap even when a broken beacon made it short.
    #[test]
    fn a_complete_lap_with_a_pit_length_stop_is_a_pit_lap_and_closes_the_stint() {
        let mut lap_number = Vec::new();
        let mut speed = Vec::new();
        for t in 0..60u32 {
            lap_number.push(match t {
                0..=9 => 1.0,
                10..=19 => 2.0,
                20..=39 => 3.0, // the stop 24..=41 puts 16 s of standing still inside it
                40..=49 => 4.0,
                _ => 5.0,
            });
            speed.push(if (24..=41).contains(&t) { 0.0 } else { 45.0 });
        }
        let source = channels_source(
            1_000_000_000,
            vec![
                ("Lap Number", "", lap_number),
                ("Ground Speed", "m/s", speed),
            ],
        );
        let metadata = read_source_metadata(&source);
        let shape: Vec<(u32, i64, LapKind, &str)> = metadata
            .laps
            .iter()
            .map(|lap| (lap.stint, lap.stint_lap, lap.kind, lap.kind.as_str()))
            .collect();
        assert_eq!(
            shape,
            [
                (1, 1, LapKind::Out, "out"),
                (1, 2, LapKind::Flying, "flying"),
                (1, 3, LapKind::Pit, "pit"),
                (2, 4, LapKind::Flying, "flying"),
                (2, 5, LapKind::In, "in"),
            ]
        );
        assert_eq!(metadata.laps[2].label(), "S1 pit L3");
        assert_eq!(metadata.valid_laps, 2);
        assert_ne!(metadata.fastest_lap.as_ref().unwrap().kind, LapKind::Pit);
    }

    /// When the counter itself restarted right after the pit lap, the stint
    /// split is already there; the pit rule must not add a second one.
    #[test]
    fn a_pit_lap_followed_by_a_counter_reset_starts_one_stint_not_two() {
        let mut lap_number = Vec::new();
        let mut speed = Vec::new();
        for t in 0..60u32 {
            lap_number.push(match t {
                10..=19 => 2.0,
                20..=39 => 3.0,
                40..=49 => 0.0, // dash reset after the stop
                _ => 1.0,
            });
            speed.push(if (24..=41).contains(&t) { 0.0 } else { 45.0 });
        }
        let source = channels_source(
            1_000_000_000,
            vec![
                ("Lap Number", "", lap_number),
                ("Ground Speed", "m/s", speed),
            ],
        );
        let metadata = read_source_metadata(&source);
        let stints: Vec<u32> = metadata.laps.iter().map(|lap| lap.stint).collect();
        assert_eq!(stints, [1, 1, 1, 2, 2], "{:?}", metadata.laps);
    }

    /// A source-reported fastest lap (VBO gate, LDX details) that classifies
    /// as a pit lap is not the fastest lap.
    #[test]
    fn authoritative_fastest_lap_that_is_a_pit_lap_is_rejected() {
        struct WithFastest(MetadataSource);
        impl TelemetrySource for WithFastest {
            fn path(&self) -> &str {
                self.0.path()
            }
            fn format(&self) -> &'static str {
                "synthetic"
            }
            fn channels(&self) -> &[Channel] {
                self.0.channels()
            }
            fn decode(&self, c: usize, k: usize, i: u64) -> f64 {
                self.0.decode(c, k, i)
            }
            fn source_lap_metadata(&self) -> Option<SourceLapMetadata> {
                let laps = vec![
                    LapMetadata::interval(1, 0, 20_000_000_000, true),
                    LapMetadata::interval(2, 20_000_000_000, 36_000_000_000, true), // 16 s, stop inside
                    LapMetadata::interval(3, 36_000_000_000, 56_000_000_000, true),
                ];
                Some(SourceLapMetadata {
                    fastest_lap: Some(laps[1].clone()),
                    laps,
                })
            }
        }
        let speed: Vec<f64> = (0..60u32)
            .map(|t| if (21..=35).contains(&t) { 0.0 } else { 45.0 })
            .collect();
        let source = WithFastest(channels_source(
            1_000_000_000,
            vec![("Ground Speed", "m/s", speed)],
        ));
        let metadata = read_source_metadata(&source);
        assert_eq!(metadata.laps[1].kind, LapKind::Pit);
        let fastest = metadata.fastest_lap.as_ref().unwrap();
        assert!(fastest.kind.is_flying(), "{fastest:?}");
        assert_eq!(fastest.stint_lap, 1);
    }

    #[test]
    fn a_counter_drop_that_recovers_within_seconds_is_a_glitch_not_a_stint() {
        // A stale dash frame re-sends lap 2 for one 10 s sample inside lap 3.
        // RESET_CONFIRM_NS is 5 s, so make the sample period short enough for
        // the recovery to land inside the window.
        let mut source = counter_source("Lap Number", vec![1.0, 2.0, 3.0, 3.0, 2.0, 3.0, 3.0, 4.0]);
        for chunk in &mut source.channels[0].chunks {
            chunk.sample_period_ns = 1_000_000_000;
        }
        source.channels[0].duration_ns = 8_000_000_000;
        let metadata = read_source_metadata(&source);
        assert_eq!(
            metadata
                .laps
                .iter()
                .map(|lap| (lap.stint, lap.stint_lap, lap.kind))
                .collect::<Vec<_>>(),
            [
                (1, 1, LapKind::Out),
                (1, 2, LapKind::Flying),
                (1, 3, LapKind::Flying),
                (1, 4, LapKind::In),
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
