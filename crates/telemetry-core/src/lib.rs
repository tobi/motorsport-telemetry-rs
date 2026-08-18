#![doc = include_str!("../README.md")]
#![deny(missing_docs)]

use std::sync::Arc;

/// Errors, warnings, and notes reported while reading a recording.
pub mod diag;
/// How a channel should be drawn.
pub mod display;
mod laps;
/// Format-neutral file and session metadata derivation.
pub mod metadata;
/// Punctuation- and case-insensitive channel-name matching.
pub mod names;
/// Provenance records for named, lossless processing passes.
pub mod pass;
/// UTC start-of-file and venue timezone for absolute placement.
pub mod placement;
/// Interval annotations on the file-relative timeline.
pub mod span;
/// Byte storage shared by memory-mapped and in-memory parsers.
pub mod storage;
/// Race-time durations stored as integer milliseconds.
pub mod timespan;
pub mod units;
/// Physical plausibility checks over a loaded source.
pub mod validate;
/// A window over another source: retained channels plus appended mirrors.
pub mod view;

pub use diag::{Diagnostic, Diagnostics, Severity};
pub use display::{ChannelDisplay, ChannelPlot};
pub use metadata::{
    driver_histogram, group_sessions, read_source_metadata, schema_hash, AbsoluteTimeRange,
    DriverStint, FileMetadata, LapMetadata, SessionMetadata, SourceIdentity, SourceLapMetadata,
    VideoFileRef, VideoReference,
};
pub use pass::{AppliedPass, SourceOrigin};
pub use placement::{
    civil_ns_to_utc_ns, resolve_timezone, resolve_utc_start_ns, utc_from_clock, utc_from_metadata,
};
pub use span::{Span, SpanMetaValue, SpanPrimary};
pub use storage::Storage;
pub use timespan::{
    average_timespan_ms, format_timespan_ms, parse_timespan_ms, timespan_ms_in_range, TIMESPAN_MS,
    TIMESPAN_MS_MAX,
};
pub use units::{
    can_convert, convert, lookup as lookup_unit, normalize as normalize_unit, ConvertError,
    Dimension, UnitDef, UNITS,
};
pub use validate::{implies_decode_fault, validate_source, validate_source_with, ValidateOptions};
pub use view::{ViewError, ViewSource};

/// Native scalar representation used by a telemetry channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleType {
    /// Signed 8-bit integer.
    I8,
    /// Unsigned 8-bit integer.
    U8,
    /// Signed 16-bit integer.
    I16,
    /// Unsigned 16-bit integer.
    U16,
    /// Signed 32-bit integer.
    I32,
    /// Unsigned 32-bit integer.
    U32,
    /// IEEE-754 32-bit floating point.
    F32,
    /// IEEE-754 64-bit floating point.
    F64,
}

impl SampleType {
    /// Maps a Pi/Cosworth PDS type code to its scalar representation.
    ///
    /// Unrecognized codes fall back to [`Self::F32`], matching the PDS
    /// reader's compatibility behavior.
    pub fn from_pds_code(code: u32) -> Self {
        match code {
            0 => Self::I8,
            1 => Self::U8,
            2 => Self::I16,
            3 => Self::U16,
            4 => Self::I32,
            5 => Self::U32,
            7 => Self::F64,
            _ => Self::F32,
        }
    }

    /// Returns the stable numeric code used by schema hashing and PDS types.
    pub fn code(self) -> u32 {
        match self {
            Self::I8 => 0,
            Self::U8 => 1,
            Self::I16 => 2,
            Self::U16 => 3,
            Self::I32 => 4,
            Self::U32 => 5,
            Self::F32 => 6,
            Self::F64 => 7,
        }
    }

    /// Returns the encoded width of one native sample in bytes.
    pub fn byte_width(self) -> usize {
        match self {
            Self::I8 | Self::U8 => 1,
            Self::I16 | Self::U16 => 2,
            Self::I32 | Self::U32 | Self::F32 => 4,
            Self::F64 => 8,
        }
    }

    /// Returns a stable lowercase display name such as `float32`.
    pub fn name(self) -> &'static str {
        match self {
            Self::I8 => "int8",
            Self::U8 => "uint8",
            Self::I16 => "int16",
            Self::U16 => "uint16",
            Self::I32 => "int32",
            Self::U32 => "uint32",
            Self::F32 => "float32",
            Self::F64 => "float64",
        }
    }

    /// Returns whether this is a floating-point representation.
    pub fn is_float(self) -> bool {
        matches!(self, Self::F32 | Self::F64)
    }

    /// Decodes one little-endian native sample from `bytes` as `f64`.
    ///
    /// Returns `None` when `bytes` is shorter than [`Self::byte_width`].
    pub fn decode_le(self, bytes: &[u8]) -> Option<f64> {
        let width = self.byte_width();
        if bytes.len() < width {
            return None;
        }
        let slice = &bytes[..width];
        Some(match self {
            Self::I8 => i8::from_le_bytes(slice.try_into().ok()?) as f64,
            Self::U8 => u8::from_le_bytes(slice.try_into().ok()?) as f64,
            Self::I16 => i16::from_le_bytes(slice.try_into().ok()?) as f64,
            Self::U16 => u16::from_le_bytes(slice.try_into().ok()?) as f64,
            Self::I32 => i32::from_le_bytes(slice.try_into().ok()?) as f64,
            Self::U32 => u32::from_le_bytes(slice.try_into().ok()?) as f64,
            Self::F32 => f32::from_le_bytes(slice.try_into().ok()?) as f64,
            Self::F64 => f64::from_le_bytes(slice.try_into().ok()?),
        })
    }

    /// Encodes one `f64` to native little-endian bytes into `out`.
    ///
    /// Floating-point types store the value directly (narrowed to `f32` for
    /// [`Self::F32`]). Integer types round to the nearest integer and clamp to
    /// the type's representable range. Returns the number of bytes written,
    /// always [`Self::byte_width`] (at most 8).
    pub fn encode_le(self, value: f64, out: &mut [u8; 8]) -> usize {
        let width = self.byte_width();
        match self {
            Self::I8 => {
                let v = clamp_to_int(value, i8::MIN as i64, i8::MAX as i64) as i8;
                out[..width].copy_from_slice(&v.to_le_bytes());
            }
            Self::U8 => {
                let v = clamp_to_int(value, 0, u8::MAX as i64) as u8;
                out[..width].copy_from_slice(&v.to_le_bytes());
            }
            Self::I16 => {
                let v = clamp_to_int(value, i16::MIN as i64, i16::MAX as i64) as i16;
                out[..width].copy_from_slice(&v.to_le_bytes());
            }
            Self::U16 => {
                let v = clamp_to_int(value, 0, u16::MAX as i64) as u16;
                out[..width].copy_from_slice(&v.to_le_bytes());
            }
            Self::I32 => {
                let v = clamp_to_int(value, i32::MIN as i64, i32::MAX as i64) as i32;
                out[..width].copy_from_slice(&v.to_le_bytes());
            }
            Self::U32 => {
                let v = clamp_to_int(value, 0, u32::MAX as i64) as u32;
                out[..width].copy_from_slice(&v.to_le_bytes());
            }
            Self::F32 => {
                out[..width].copy_from_slice(&(value as f32).to_le_bytes());
            }
            Self::F64 => {
                out[..width].copy_from_slice(&value.to_le_bytes());
            }
        }
        width
    }
}

/// Rounds `value` and clamps it to the inclusive `[min, max]` integer range.
///
/// Non-finite values map to the nearest bound: `+inf` to `max`, anything else
/// (including NaN) to `min`.
fn clamp_to_int(value: f64, min: i64, max: i64) -> i64 {
    let rounded = value.round();
    if !rounded.is_finite() {
        return if rounded > 0.0 { max } else { min };
    }
    rounded.clamp(min as f64, max as f64) as i64
}

/// One contiguous, constant-rate run of samples within a channel.
#[derive(Debug, Clone)]
pub struct Chunk {
    /// Time between adjacent samples in nanoseconds.
    pub sample_period_ns: u64,
    /// Number of samples in this chunk.
    pub sample_count: u64,
    /// Byte offset for binary formats, column-local offset for text formats.
    pub data_ptr: u64,
    /// Channel-global sample index of this chunk's first sample.
    pub sample_base: u64,
    /// File-relative timestamp of this chunk's first sample.
    pub time_base_ns: u64,
}

/// Where a channel's unit string came from.
///
/// Units drive downstream conversion and display, so a guess must never be
/// indistinguishable from a value the file actually declared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UnitSource {
    /// The file stored an explicit unit string for this channel.
    Declared,
    /// The unit is fixed by the file format's specification for this channel
    /// (for example Pi/Cosworth PDS storing SI base units, or a VBOX builtin
    /// column that is always km/h).
    SpecDefault,
    /// No unit information is available. `unit` is empty.
    #[default]
    Unknown,
}

impl UnitSource {
    /// Returns a stable lowercase provenance label.
    pub fn name(self) -> &'static str {
        match self {
            Self::Declared => "declared",
            Self::SpecDefault => "spec_default",
            Self::Unknown => "unknown",
        }
    }
}

/// Source-exact metadata and chunk layout for one telemetry signal.
#[derive(Debug, Clone)]
pub struct Channel {
    /// Format-specific channel or record identifier.
    pub id: u32,
    /// Channel name reported by the source.
    pub name: String,
    /// Unit reported or specified by the source format; empty if unknown.
    pub unit: String,
    /// Provenance of `unit`. Never infer a unit from a channel name and report
    /// it as [`UnitSource::Declared`].
    pub unit_source: UnitSource,
    /// Native scalar representation.
    pub sample_type: SampleType,
    /// Constant-rate sample runs ordered by time.
    pub chunks: Vec<Chunk>,
    /// Total number of samples across all chunks.
    pub sample_count: u64,
    /// File-relative end time of the channel in nanoseconds.
    pub duration_ns: u64,
}

/// Sparse comment on a channel trace: file-relative nanoseconds plus text.
///
/// Drawn as a dot on that channel at `time_ns`. On hover the viewer expands
/// a dotted vertical across the full trace height. Not a sample.
/// Allowed only on [`ChannelPlot::Trace`] channels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelLabel {
    /// File-relative nanoseconds.
    pub time_ns: u64,
    /// Comment text shown on hover.
    pub text: String,
}

impl Channel {
    /// Returns the first chunk's sample period, if the channel has samples.
    pub fn first_period_ns(&self) -> Option<u64> {
        self.chunks.first().map(|chunk| chunk.sample_period_ns)
    }

    /// Returns the first chunk's sampling frequency in hertz.
    pub fn frequency_hz(&self) -> Option<f64> {
        self.first_period_ns().map(|period| 1e9 / period as f64)
    }

    /// Returns whether sampling should preserve discrete values instead of interpolating.
    ///
    /// Integer channels always step. Floating-point channels with known state,
    /// counter, flag, or gear names also step.
    pub fn uses_step_interpolation(&self) -> bool {
        if !self.sample_type.is_float() {
            return true;
        }
        [
            "gear",
            "lapnumber",
            "lapbeacon",
            "laptrigger",
            "beaconeventcount",
            "beaconcount",
            "switch",
            "status",
            "state",
            "flag",
            "alarm",
            "satellites",
            "solutiontype",
        ]
        .iter()
        .any(|token| names::contains(&self.name, token))
    }
}

/// Where a channel's sample instants come from.
#[derive(Debug, Clone, Copy)]
pub enum SampleTimes<'a> {
    /// Grid instants: `time = chunk.time_base_ns + local * chunk.sample_period_ns`.
    Grid,
    /// One stamp per sample, channel-global index `chunk.sample_base + local`,
    /// ascending, with `len == channel.sample_count`.
    Explicit(&'a [u64]),
}

/// Checked slice of one packed sample:
/// `data[data_ptr + local*width .. + width]`.
///
/// Returns `None` when the range is out of bounds or the offset arithmetic
/// overflows.
pub fn sample_bytes<'a>(
    data: &'a [u8],
    chunk: &Chunk,
    local_index: u64,
    width: usize,
) -> Option<&'a [u8]> {
    let data_ptr = usize::try_from(chunk.data_ptr).ok()?;
    let local = usize::try_from(local_index).ok()?;
    let start = data_ptr.checked_add(local.checked_mul(width)?)?;
    let end = start.checked_add(width)?;
    data.get(start..end)
}

/// Checked slice of a whole packed chunk:
/// `data[data_ptr .. data_ptr + sample_count*width]`.
///
/// Returns `None` when the range is out of bounds or the offset arithmetic
/// overflows.
pub fn chunk_bytes<'a>(data: &'a [u8], chunk: &Chunk, width: usize) -> Option<&'a [u8]> {
    let data_ptr = usize::try_from(chunk.data_ptr).ok()?;
    let count = usize::try_from(chunk.sample_count).ok()?;
    let start = data_ptr;
    let end = start.checked_add(count.checked_mul(width)?)?;
    data.get(start..end)
}

/// Shared random-access interface implemented by every format reader.
///
/// Implementations expose native channel metadata and decode samples on
/// demand. Timestamps are file-relative nanoseconds unless explicitly stated.
pub trait TelemetrySource: Send + Sync {
    /// Returns the source path or caller-supplied input name.
    fn path(&self) -> &str;
    /// Returns a stable lowercase format identifier.
    fn format(&self) -> &'static str;
    /// Returns source-exact channel metadata in decode-index order.
    fn channels(&self) -> &[Channel];
    /// Decodes one native sample as `f64`.
    ///
    /// The three indexes must identify an existing channel, chunk, and sample.
    fn decode(&self, channel_index: usize, chunk_index: usize, local_index: u64) -> f64;

    /// Returns the packed native bytes of one chunk, when they are contiguous.
    ///
    /// The slice length must be `sample_count * sample_type.byte_width()`.
    /// Sources that only expose decoded scalars return `None`.
    fn chunk_bytes(&self, _channel_index: usize, _chunk_index: usize) -> Option<&[u8]> {
        None
    }

    /// Returns the affine `(scale, bias)` applied after a native integer load.
    ///
    /// `decode ≈ raw * scale + bias`. Floating-point encodings and sources
    /// that already store engineering values return `(1.0, 0.0)`.
    fn sample_affine(&self, _channel_index: usize) -> (f64, f64) {
        (1.0, 0.0)
    }

    /// Returns the source's reliable absolute clock range, when available.
    fn absolute_time_range(&self) -> Option<AbsoluteTimeRange> {
        None
    }

    /// Unix-epoch nanoseconds (UTC) at file `t = 0`, when the source stored one.
    ///
    /// Default is `None`. Native `.telemetry` v4 and MTJ/MTX headers expose
    /// the stamped value. Do not invent this from decorative `date`/`time`.
    fn utc_start_ns(&self) -> Option<u64> {
        None
    }

    /// IANA timezone of the venue, empty when unknown.
    fn timezone(&self) -> String {
        String::new()
    }

    /// Default visibility of each sample channel, aligned with [`Self::channels`].
    ///
    /// Empty means every channel is visible. A shorter slice treats the
    /// remaining channels as visible.
    fn channel_visible(&self) -> &[bool] {
        &[]
    }

    /// Problems this source recovered from while it was read, in the order
    /// they were encountered.
    ///
    /// A reader that assumed, clamped, substituted, or dropped anything MUST
    /// report it here. An empty slice is a positive claim: everything returned
    /// is what the file stated. Recovery that is not reported here is
    /// indistinguishable from correct data, which is how a misread sample
    /// width once turned into speeds of 1.5e308 m/s.
    ///
    /// This reports what *reading* found. Physical plausibility of the values
    /// is a separate judgement made by [`crate::validate::validate_source`].
    fn diagnostics(&self) -> &[crate::Diagnostic] {
        &[]
    }

    /// Interval annotations on the file-relative timeline. Empty when none.
    fn spans(&self) -> &[crate::Span] {
        &[]
    }

    /// Sparse comment labels on one sample channel, oldest first.
    ///
    /// Empty when the channel has none. Times are file-relative nanoseconds.
    fn channel_labels(&self, _channel_index: usize) -> &[crate::ChannelLabel] {
        &[]
    }

    /// Display class, optional scale, and rounding for one sample channel.
    fn channel_display(&self, _channel_index: usize) -> crate::ChannelDisplay {
        crate::ChannelDisplay::trace()
    }

    /// Processing passes recorded as applied to this source, in order.
    ///
    /// Vendor recordings return an empty slice: their channels are raw
    /// conversions. Converted artifacts report the chain of named passes
    /// that appended their derived channels. Every listed pass is lossless:
    /// dropping the channels named in [`AppliedPass::outputs`] recovers the
    /// raw conversion byte for byte.
    fn applied_passes(&self) -> &[crate::AppliedPass] {
        &[]
    }

    /// Identity of the original vendor recording, when this source is
    /// itself a converted artifact.
    ///
    /// `None` means this source *is* the origin. Writers persist this so a
    /// `.telemetry` file always remembers the name and format it was
    /// converted from, even across rewrites.
    fn source_origin(&self) -> Option<crate::SourceOrigin> {
        None
    }

    /// Returns identity fields embedded in the source.
    fn identity(&self) -> SourceIdentity {
        SourceIdentity::default()
    }

    /// Returns authoritative source-provided lap intervals, when available.
    ///
    /// Format readers can use this hook for sidecars or native lap packets.
    /// The generic metadata derivation falls back to conventional channels
    /// only when the source returns `None`. This hook is intentionally optional
    /// and is not the cross-format lap-summary API; use the `laps` field from
    /// [`FileMetadata`] for the complete format-neutral result.
    fn source_lap_metadata(&self) -> Option<SourceLapMetadata> {
        None
    }

    /// Linked video files. Empty when the recording has no video.
    fn video_files(&self) -> &[crate::VideoFileRef] {
        &[]
    }

    /// Presentation-order movie-timeline timestamps for each video frame.
    ///
    /// Values are nanoseconds on the same timeline
    /// [`Self::video_presentation_time_ns`] uses. Empty/`None` when unknown.
    fn video_presentation_times_ns(&self) -> Option<&[u64]> {
        None
    }

    /// Returns the number of frames in linked or embedded video, when known.
    fn video_frame_count(&self) -> Option<u64> {
        self.video_presentation_times_ns()
            .map(|times| times.len() as u64)
    }

    /// Maps a file-relative telemetry timestamp to a video frame index.
    fn video_frame_at(&self, _time_ns: u64) -> Option<u64> {
        None
    }

    /// Returns the offset from file-relative time to the video movie timeline.
    fn video_presentation_offset_ns(&self) -> Option<i128> {
        None
    }

    /// Maps a file-relative telemetry timestamp to the video's movie timeline.
    fn video_presentation_time_ns(&self, time_ns: u64) -> Option<u64> {
        u64::try_from(i128::from(time_ns) + self.video_presentation_offset_ns()?).ok()
    }

    /// Returns all video linkage available at a file-relative timestamp.
    ///
    /// The default implementation samples conventional VBOX AVI linkage
    /// channels and calls [`Self::video_frame_at`].
    fn video_reference_at(&self, time_ns: u64) -> VideoReference {
        let file_index = self
            .channels()
            .iter()
            .position(|channel| {
                matches!(
                    channel.name.to_ascii_lowercase().as_str(),
                    "avifileindex" | "avi file index"
                )
            })
            .and_then(|index| self.sample_at(index, time_ns, false))
            .filter(|value| value.is_finite() && *value >= 0.0)
            .map(|value| value.round() as u32);
        let sync_time = self
            .channels()
            .iter()
            .position(|channel| {
                matches!(
                    channel.name.to_ascii_lowercase().as_str(),
                    "avisynctime" | "avitime" | "avi sync time"
                )
            })
            .and_then(|index| self.sample_at(index, time_ns, false))
            .filter(|value| value.is_finite());
        VideoReference {
            file_index,
            sync_time,
            presentation_time_ns: self.video_presentation_time_ns(time_ns),
            frame_index: self.video_frame_at(time_ns),
        }
    }

    /// Derives a format-neutral metadata summary from this source.
    fn metadata(&self) -> FileMetadata {
        read_source_metadata(&self)
    }

    /// Returns where this channel's sample instants come from.
    ///
    /// Grid channels derive instants from chunk time bases and periods;
    /// Explicit channels carry one ascending stamp per sample. The default is
    /// [`SampleTimes::Grid`].
    fn sample_times(&self, _channel_index: usize) -> SampleTimes<'_> {
        SampleTimes::Grid
    }

    /// Returns the file-relative timestamp for one native sample.
    ///
    /// Dispatches on [`Self::sample_times`]: Grid uses the chunk time base and
    /// period; Explicit uses `times[chunk.sample_base + local]`, falling back
    /// to the grid formula when the stamp is missing.
    fn sample_time_ns(&self, channel_index: usize, chunk_index: usize, local_index: u64) -> u64 {
        let chunk = &self.channels()[channel_index].chunks[chunk_index];
        match self.sample_times(channel_index) {
            SampleTimes::Grid => chunk
                .time_base_ns
                .saturating_add(local_index.saturating_mul(chunk.sample_period_ns)),
            SampleTimes::Explicit(times) => {
                let global = chunk.sample_base.saturating_add(local_index);
                usize::try_from(global)
                    .ok()
                    .and_then(|index| times.get(index).copied())
                    .unwrap_or_else(|| {
                        chunk
                            .time_base_ns
                            .saturating_add(local_index.saturating_mul(chunk.sample_period_ns))
                    })
            }
        }
    }

    /// Samples a channel at a file-relative timestamp.
    ///
    /// When `linear` is true, continuous floating-point signals are linearly
    /// interpolated. Discrete channels selected by
    /// [`Channel::uses_step_interpolation`] always use step sampling. Returns
    /// `None` for an absent channel, an empty channel, a timestamp before the
    /// first sample, or a timestamp at or beyond the channel duration.
    ///
    /// Grid channels use the chunk time base and period; Explicit channels
    /// bisect the per-sample stamps and interpolate between adjacent stamps.
    fn sample_at(&self, channel_index: usize, time_ns: u64, linear: bool) -> Option<f64> {
        let channel = self.channels().get(channel_index)?;
        if channel.chunks.is_empty() || time_ns >= channel.duration_ns {
            return None;
        }
        match self.sample_times(channel_index) {
            SampleTimes::Grid => {
                let first_start = channel.chunks.first()?.time_base_ns;
                if time_ns < first_start {
                    return None;
                }
                let chunk_index = channel.chunks.partition_point(|chunk| {
                    chunk
                        .time_base_ns
                        .saturating_add(chunk.sample_count.saturating_mul(chunk.sample_period_ns))
                        <= time_ns
                });
                let chunk = channel.chunks.get(chunk_index)?;
                if chunk.sample_count == 0 || chunk.sample_period_ns == 0 {
                    return None;
                }
                let relative = time_ns.saturating_sub(chunk.time_base_ns);
                let sample = (relative / chunk.sample_period_ns).min(chunk.sample_count - 1);
                let a = self.decode(channel_index, chunk_index, sample);
                if !linear || channel.uses_step_interpolation() {
                    return Some(a);
                }
                let sample_time = chunk
                    .time_base_ns
                    .saturating_add(sample.saturating_mul(chunk.sample_period_ns));
                let next_sample = sample.saturating_add(1);
                let (b, next_time) = if next_sample < chunk.sample_count {
                    (
                        self.decode(channel_index, chunk_index, next_sample),
                        sample_time.saturating_add(chunk.sample_period_ns),
                    )
                } else if let Some(next_chunk) = channel.chunks.get(chunk_index + 1) {
                    (
                        self.decode(channel_index, chunk_index + 1, 0),
                        next_chunk.time_base_ns,
                    )
                } else {
                    return Some(a);
                };
                let interval = next_time.saturating_sub(sample_time);
                if interval == 0 {
                    return Some(a);
                }
                let fraction = time_ns.saturating_sub(sample_time) as f64 / interval as f64;
                Some(a + (b - a) * fraction)
            }
            SampleTimes::Explicit(times) => {
                let first = times.first()?;
                if time_ns < *first {
                    return None;
                }
                let upper = times.partition_point(|stamp| *stamp <= time_ns);
                if upper == 0 {
                    return None;
                }
                let index = upper - 1;
                let stamp = times[index];
                let (chunk_index, local_index) = chunk_for_global(channel, index as u64)?;
                let a = self.decode(channel_index, chunk_index, local_index);
                if !linear || channel.uses_step_interpolation() || index + 1 >= times.len() {
                    return Some(a);
                }
                let next_stamp = times[index + 1];
                let (next_chunk, next_local) = chunk_for_global(channel, (index + 1) as u64)?;
                let b = self.decode(channel_index, next_chunk, next_local);
                let interval = next_stamp.saturating_sub(stamp);
                if interval == 0 {
                    return Some(a);
                }
                let fraction = time_ns.saturating_sub(stamp) as f64 / interval as f64;
                Some(a + (b - a) * fraction)
            }
        }
    }
}

/// Finds the chunk and local index holding channel-global sample `global`.
///
/// Chunks are ordered by `sample_base`; returns the last chunk whose
/// `sample_base <= global` when `global - sample_base < sample_count`.
fn chunk_for_global(channel: &Channel, global: u64) -> Option<(usize, u64)> {
    let chunk_index = channel
        .chunks
        .partition_point(|chunk| chunk.sample_base <= global);
    if chunk_index == 0 {
        return None;
    }
    let idx = chunk_index - 1;
    let chunk = channel.chunks.get(idx)?;
    let local = global.checked_sub(chunk.sample_base)?;
    (local < chunk.sample_count).then_some((idx, local))
}

/// Shared, thread-safe ownership of a format-neutral telemetry source.
pub type SourceRef = Arc<dyn TelemetrySource>;

macro_rules! impl_telemetry_source_for_wrapper {
    ($($wrapper:ty),+ $(,)?) => {
        $(
            impl<T: TelemetrySource + ?Sized> TelemetrySource for $wrapper {
                fn path(&self) -> &str {
                    (**self).path()
                }
                fn format(&self) -> &'static str {
                    (**self).format()
                }
                fn channels(&self) -> &[Channel] {
                    (**self).channels()
                }
                fn decode(
                    &self,
                    channel_index: usize,
                    chunk_index: usize,
                    local_index: u64,
                ) -> f64 {
                    (**self).decode(channel_index, chunk_index, local_index)
                }
                fn chunk_bytes(
                    &self,
                    channel_index: usize,
                    chunk_index: usize,
                ) -> Option<&[u8]> {
                    (**self).chunk_bytes(channel_index, chunk_index)
                }
                fn sample_affine(&self, channel_index: usize) -> (f64, f64) {
                    (**self).sample_affine(channel_index)
                }
                fn absolute_time_range(&self) -> Option<AbsoluteTimeRange> {
                    (**self).absolute_time_range()
                }
                fn utc_start_ns(&self) -> Option<u64> {
                    (**self).utc_start_ns()
                }
                fn timezone(&self) -> String {
                    (**self).timezone()
                }
                fn channel_visible(&self) -> &[bool] {
                    (**self).channel_visible()
                }
                fn diagnostics(&self) -> &[crate::Diagnostic] {
                    (**self).diagnostics()
                }
                fn spans(&self) -> &[crate::Span] {
                    (**self).spans()
                }
                fn channel_labels(&self, channel_index: usize) -> &[crate::ChannelLabel] {
                    (**self).channel_labels(channel_index)
                }
                fn channel_display(&self, channel_index: usize) -> crate::ChannelDisplay {
                    (**self).channel_display(channel_index)
                }
                fn applied_passes(&self) -> &[crate::AppliedPass] {
                    (**self).applied_passes()
                }
                fn source_origin(&self) -> Option<crate::SourceOrigin> {
                    (**self).source_origin()
                }
                fn identity(&self) -> SourceIdentity {
                    (**self).identity()
                }
                fn source_lap_metadata(&self) -> Option<SourceLapMetadata> {
                    (**self).source_lap_metadata()
                }
                fn video_files(&self) -> &[crate::VideoFileRef] {
                    (**self).video_files()
                }
                fn video_presentation_times_ns(&self) -> Option<&[u64]> {
                    (**self).video_presentation_times_ns()
                }
                fn video_frame_count(&self) -> Option<u64> {
                    (**self).video_frame_count()
                }
                fn video_frame_at(&self, time_ns: u64) -> Option<u64> {
                    (**self).video_frame_at(time_ns)
                }
                fn video_presentation_offset_ns(&self) -> Option<i128> {
                    (**self).video_presentation_offset_ns()
                }
                fn video_presentation_time_ns(&self, time_ns: u64) -> Option<u64> {
                    (**self).video_presentation_time_ns(time_ns)
                }
                fn video_reference_at(&self, time_ns: u64) -> crate::VideoReference {
                    (**self).video_reference_at(time_ns)
                }
                fn metadata(&self) -> FileMetadata {
                    (**self).metadata()
                }
                fn sample_times(&self, channel_index: usize) -> SampleTimes<'_> {
                    (**self).sample_times(channel_index)
                }
                fn sample_time_ns(
                    &self,
                    channel_index: usize,
                    chunk_index: usize,
                    local_index: u64,
                ) -> u64 {
                    (**self).sample_time_ns(channel_index, chunk_index, local_index)
                }
                fn sample_at(
                    &self,
                    channel_index: usize,
                    time_ns: u64,
                    linear: bool,
                ) -> Option<f64> {
                    (**self).sample_at(channel_index, time_ns, linear)
                }
            }
        )+
    };
}

impl_telemetry_source_for_wrapper!(&T, Box<T>, std::sync::Arc<T>);

#[cfg(test)]
mod tests {
    use super::*;

    fn channel(name: &str, sample_type: SampleType) -> Channel {
        Channel {
            id: 1,
            name: name.into(),
            unit: String::new(),
            unit_source: UnitSource::Unknown,
            sample_type,
            chunks: Vec::new(),
            sample_count: 0,
            duration_ns: 0,
        }
    }

    struct TestSource {
        channel: Channel,
        values: [f64; 2],
    }

    impl TelemetrySource for TestSource {
        fn path(&self) -> &str {
            "test"
        }
        fn format(&self) -> &'static str {
            "test"
        }
        fn channels(&self) -> &[Channel] {
            std::slice::from_ref(&self.channel)
        }
        fn decode(&self, _channel_index: usize, _chunk_index: usize, local_index: u64) -> f64 {
            self.values[local_index as usize]
        }
    }

    fn two_sample_source(sample_type: SampleType) -> TestSource {
        TestSource {
            channel: Channel {
                id: 1,
                name: "Speed".into(),
                unit: String::new(),
                unit_source: UnitSource::Unknown,
                sample_type,
                chunks: vec![Chunk {
                    sample_period_ns: 1_000_000_000,
                    sample_count: 2,
                    data_ptr: 0,
                    sample_base: 0,
                    time_base_ns: 0,
                }],
                sample_count: 2,
                duration_ns: 2_000_000_000,
            },
            values: [10.0, 20.0],
        }
    }

    #[test]
    fn pds_type_code_zero_is_signed_byte() {
        let sample_type = SampleType::from_pds_code(0);
        assert_eq!(sample_type, SampleType::I8);
        assert_eq!(sample_type.code(), 0);
        assert_eq!(sample_type.byte_width(), 1);
        assert_eq!(sample_type.name(), "int8");
    }

    #[test]
    fn discrete_channels_use_step_interpolation_even_when_stored_as_float() {
        assert!(channel("Gear_Pos", SampleType::F32).uses_step_interpolation());
        assert!(channel("Lap Beacon", SampleType::F32).uses_step_interpolation());
        assert!(!channel("Speed_Ref", SampleType::F32).uses_step_interpolation());
        assert!(!channel("Speed_Ref", SampleType::F64).uses_step_interpolation());
        for sample_type in [
            SampleType::I8,
            SampleType::U8,
            SampleType::I16,
            SampleType::U16,
            SampleType::I32,
            SampleType::U32,
        ] {
            assert!(channel("Speed_Ref", sample_type).uses_step_interpolation());
        }
    }

    #[test]
    fn linear_mode_never_interpolates_integer_source_channels() {
        let integer = two_sample_source(SampleType::I32);
        let float = two_sample_source(SampleType::F32);
        assert_eq!(integer.sample_at(0, 500_000_000, true), Some(10.0));
        assert_eq!(float.sample_at(0, 500_000_000, true), Some(15.0));
    }
    #[test]
    fn corrupt_zero_period_and_count_do_not_panic_sampling() {
        let mut zero_period = two_sample_source(SampleType::F32);
        zero_period.channel.chunks[0].sample_period_ns = 0;
        assert_eq!(zero_period.sample_at(0, 0, true), None);

        let mut zero_count = two_sample_source(SampleType::F32);
        zero_count.channel.chunks[0].sample_count = 0;
        assert_eq!(zero_count.sample_at(0, 0, true), None);
    }

    #[test]
    fn corrupt_sample_timestamp_arithmetic_saturates() {
        let mut source = two_sample_source(SampleType::F32);
        source.channel.chunks[0].time_base_ns = u64::MAX - 5;
        source.channel.chunks[0].sample_period_ns = 10;
        assert_eq!(source.sample_time_ns(0, 0, u64::MAX), u64::MAX);
    }

    struct ExplicitTimesSource {
        channel: Channel,
        values: Vec<f64>,
        times: Vec<u64>,
    }

    impl TelemetrySource for ExplicitTimesSource {
        fn path(&self) -> &str {
            "explicit"
        }
        fn format(&self) -> &'static str {
            "explicit"
        }
        fn channels(&self) -> &[Channel] {
            std::slice::from_ref(&self.channel)
        }
        fn decode(&self, _channel_index: usize, _chunk_index: usize, local_index: u64) -> f64 {
            self.values[local_index as usize]
        }
        fn sample_times(&self, _channel_index: usize) -> SampleTimes<'_> {
            SampleTimes::Explicit(&self.times)
        }
    }

    fn explicit_source() -> ExplicitTimesSource {
        ExplicitTimesSource {
            channel: Channel {
                id: 1,
                name: "X".into(),
                unit: String::new(),
                unit_source: UnitSource::Unknown,
                sample_type: SampleType::F64,
                chunks: vec![Chunk {
                    sample_period_ns: 0,
                    sample_count: 4,
                    data_ptr: 0,
                    sample_base: 0,
                    time_base_ns: 100,
                }],
                sample_count: 4,
                duration_ns: 2_000,
            },
            values: vec![0.0, 10.0, 20.0, 30.0],
            times: vec![100, 300, 700, 1500],
        }
    }

    #[test]
    fn explicit_sample_time_ns_uses_stamps() {
        let source = explicit_source();
        assert_eq!(source.sample_time_ns(0, 0, 0), 100);
        assert_eq!(source.sample_time_ns(0, 0, 2), 700);
    }

    #[test]
    fn explicit_sample_at_bisects_and_lerps() {
        let source = explicit_source();
        // Exact stamp hits.
        assert_eq!(source.sample_at(0, 100, true), Some(0.0));
        assert_eq!(source.sample_at(0, 300, true), Some(10.0));
        assert_eq!(source.sample_at(0, 700, true), Some(20.0));
        assert_eq!(source.sample_at(0, 1500, true), Some(30.0));
        // Bisect + lerp between stamps.
        assert_eq!(source.sample_at(0, 200, true), Some(5.0));
        assert_eq!(source.sample_at(0, 500, true), Some(15.0));
        assert_eq!(source.sample_at(0, 1100, true), Some(25.0));
        // Step mode (linear=false) returns the held sample.
        assert_eq!(source.sample_at(0, 500, false), Some(10.0));
    }

    #[test]
    fn explicit_sample_at_before_first_is_none() {
        let source = explicit_source();
        assert_eq!(source.sample_at(0, 50, true), None);
        assert_eq!(source.sample_at(0, 99, false), None);
        // At or beyond duration is None.
        assert_eq!(source.sample_at(0, 2_000, true), None);
    }

    #[test]
    fn grid_sample_at_before_first_is_none() {
        let mut source = two_sample_source(SampleType::F32);
        source.channel.chunks[0].time_base_ns = 1_000_000_000;
        source.channel.duration_ns = 3_000_000_000;
        assert_eq!(source.sample_at(0, 500_000_000, true), None);
        assert_eq!(source.sample_at(0, 1_000_000_000, true), Some(10.0));
    }

    #[test]
    fn decode_le_and_encode_le_round_trip() {
        let mut buf = [0u8; 8];
        for (sample_type, value) in [
            (SampleType::I8, -42.0),
            (SampleType::U8, 200.0),
            (SampleType::I16, -1234.0),
            (SampleType::U16, 60_000.0),
            (SampleType::I32, -999_999.0),
            (SampleType::U32, 4_000_000_000.0),
            (SampleType::F32, 1.5),
            (SampleType::F64, -6.25e-3),
        ] {
            let n = sample_type.encode_le(value, &mut buf);
            assert_eq!(n, sample_type.byte_width());
            let decoded = sample_type.decode_le(&buf[..n]).unwrap();
            if sample_type.is_float() {
                let expected = if matches!(sample_type, SampleType::F32) {
                    value as f32 as f64
                } else {
                    value
                };
                assert!(
                    (decoded - expected).abs() < 1e-6,
                    "{sample_type:?}: {decoded} != {expected}"
                );
            } else {
                assert_eq!(decoded, value);
            }
        }
    }

    #[test]
    fn decode_le_returns_none_for_short_buffer() {
        assert_eq!(SampleType::I16.decode_le(&[0]), None);
        assert_eq!(SampleType::F64.decode_le(&[0; 7]), None);
    }

    #[test]
    fn encode_le_clamps_integer_out_of_range() {
        let mut buf = [0u8; 8];
        SampleType::I8.encode_le(300.0, &mut buf);
        assert_eq!(SampleType::I8.decode_le(&buf[..1]), Some(127.0));
        SampleType::U8.encode_le(-5.0, &mut buf);
        assert_eq!(SampleType::U8.decode_le(&buf[..1]), Some(0.0));
        SampleType::U8.encode_le(f64::INFINITY, &mut buf);
        assert_eq!(SampleType::U8.decode_le(&buf[..1]), Some(255.0));
    }

    #[test]
    fn sample_bytes_and_chunk_bytes_are_checked() {
        let data = [0u8; 32];
        let chunk = Chunk {
            sample_period_ns: 1,
            sample_count: 4,
            data_ptr: 8,
            sample_base: 0,
            time_base_ns: 0,
        };
        assert_eq!(sample_bytes(&data, &chunk, 0, 4).unwrap().len(), 4);
        assert_eq!(sample_bytes(&data, &chunk, 1, 4).unwrap(), &data[12..16]);
        assert_eq!(sample_bytes(&data, &chunk, 2, 4).unwrap(), &data[16..20]);
        // Beyond the data buffer.
        assert_eq!(sample_bytes(&data, &chunk, 6, 4), None);
        // Overflow via huge local_index.
        assert_eq!(sample_bytes(&data, &chunk, u64::MAX, 4), None);
        assert_eq!(chunk_bytes(&data, &chunk, 4).unwrap().len(), 16);
        assert_eq!(chunk_bytes(&data, &chunk, 4), Some(&data[8..24] as &[u8]));
        // Chunk end beyond data.
        let big = Chunk {
            sample_period_ns: 1,
            sample_count: 10,
            data_ptr: 8,
            sample_base: 0,
            time_base_ns: 0,
        };
        assert_eq!(chunk_bytes(&data, &big, 4), None);
    }
}
