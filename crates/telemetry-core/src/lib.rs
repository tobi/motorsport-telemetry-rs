#![doc = include_str!("../README.md")]
#![deny(missing_docs)]

use std::sync::Arc;

/// How a channel should be drawn.
pub mod display;
/// Format-neutral file and session metadata derivation.
pub mod metadata;
/// Interval annotations on the file-relative timeline.
pub mod span;
/// Race-time durations stored as integer milliseconds.
pub mod timespan;
pub mod units;

pub use display::{ChannelDisplay, ChannelPlot};
pub use metadata::{
    driver_histogram, group_sessions, read_source_metadata, schema_hash, AbsoluteTimeRange,
    DriverStint, FileMetadata, LapMetadata, SessionMetadata, SourceIdentity, SourceLapMetadata,
    VideoFileRef, VideoReference,
};
pub use span::{Span, SpanMetaValue, SpanPrimary};
pub use timespan::{
    average_timespan_ms, format_timespan_ms, parse_timespan_ms, timespan_ms_in_range, TIMESPAN_MS,
    TIMESPAN_MS_MAX,
};
pub use units::{
    can_convert, convert, lookup as lookup_unit, normalize as normalize_unit, ConvertError,
    Dimension, UnitDef, UNITS,
};

/// Native scalar representation used by a telemetry channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleType {
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
            Self::U8 => 1,
            Self::I16 | Self::U16 => 2,
            Self::I32 | Self::U32 | Self::F32 => 4,
            Self::F64 => 8,
        }
    }

    /// Returns a stable lowercase display name such as `float32`.
    pub fn name(self) -> &'static str {
        match self {
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
        .any(|token| normalized_contains(&self.name, token))
    }
}

fn normalized_contains(value: &str, needle: &str) -> bool {
    let needle = needle.as_bytes();
    value
        .char_indices()
        .filter(|(_, character)| character.is_ascii_alphanumeric())
        .any(|(start, _)| {
            let mut matched = 0;
            for byte in value[start..].bytes().filter(u8::is_ascii_alphanumeric) {
                if byte.to_ascii_lowercase() != needle[matched] {
                    return false;
                }
                matched += 1;
                if matched == needle.len() {
                    return true;
                }
            }
            false
        })
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
    fn metadata(&self) -> FileMetadata
    where
        Self: Sized,
    {
        read_source_metadata(self)
    }

    /// Returns the file-relative timestamp for one native sample.
    fn sample_time_ns(&self, channel_index: usize, chunk_index: usize, local_index: u64) -> u64 {
        let chunk = &self.channels()[channel_index].chunks[chunk_index];
        chunk.time_base_ns + local_index * chunk.sample_period_ns
    }

    /// Samples a channel at a file-relative timestamp.
    ///
    /// When `linear` is true, continuous floating-point signals are linearly
    /// interpolated. Discrete channels selected by
    /// [`Channel::uses_step_interpolation`] always use step sampling. Returns
    /// `None` for an absent channel, an empty channel, or a timestamp at or
    /// beyond the channel duration.
    fn sample_at(&self, channel_index: usize, time_ns: u64, linear: bool) -> Option<f64> {
        let channel = self.channels().get(channel_index)?;
        if time_ns >= channel.duration_ns || channel.chunks.is_empty() {
            return None;
        }
        let chunk_index = channel.chunks.partition_point(|chunk| {
            chunk
                .time_base_ns
                .saturating_add(chunk.sample_count.saturating_mul(chunk.sample_period_ns))
                <= time_ns
        });
        let chunk = channel.chunks.get(chunk_index)?;
        let relative = time_ns.saturating_sub(chunk.time_base_ns);
        let sample = (relative / chunk.sample_period_ns).min(chunk.sample_count - 1);
        let a = self.decode(channel_index, chunk_index, sample);
        if !linear || channel.uses_step_interpolation() {
            return Some(a);
        }

        let sample_time = chunk.time_base_ns + sample * chunk.sample_period_ns;
        let (b, next_time) = if sample + 1 < chunk.sample_count {
            (
                self.decode(channel_index, chunk_index, sample + 1),
                sample_time + chunk.sample_period_ns,
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
}

/// Shared, thread-safe ownership of a format-neutral telemetry source.
pub type SourceRef = Arc<dyn TelemetrySource>;

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
    fn discrete_channels_use_step_interpolation_even_when_stored_as_float() {
        assert!(channel("Gear_Pos", SampleType::F32).uses_step_interpolation());
        assert!(channel("Lap Beacon", SampleType::F32).uses_step_interpolation());
        assert!(!channel("Speed_Ref", SampleType::F32).uses_step_interpolation());
        assert!(!channel("Speed_Ref", SampleType::F64).uses_step_interpolation());
        for sample_type in [
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
}
