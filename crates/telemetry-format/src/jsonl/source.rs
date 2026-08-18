//! `TelemetrySource` impl and MTX sidecar header/group types for `JsonlRecording`.

use super::JsonlRecording;
use motorsport_telemetry_core::{
    AbsoluteTimeRange, AppliedPass, Channel, ChannelDisplay, ChannelLabel, FileMetadata,
    SourceIdentity, SourceLapMetadata, SourceOrigin, Span, TelemetrySource, VideoFileRef,
};
use std::ops::Range;

/// MTX group chrome: the sidecar is the folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidecarHeader {
    /// Folder title, e.g. `Sebring 12H 2025`.
    pub name: String,
    /// When false the group starts collapsed.
    pub visible: bool,
    /// Right-aligned text and fact pills, in draw order.
    pub right: Vec<HeaderChrome>,
    /// Unix-epoch nanoseconds (UTC) at this sidecar's `t = 0`.
    pub utc_start_ns: u64,
    /// IANA timezone, e.g. `America/New_York`.
    pub timezone: String,
}
/// One folder in an MTX sidecar and the records governed by its header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidecarGroup {
    /// Folder title, visibility, chrome, and absolute placement.
    pub header: SidecarHeader,
    /// Lattice quantum for this group's records.
    pub quantum_ns: u64,
    /// Lattice origin for this group's records.
    pub origin_ns: u64,
    /// Exclusive end of this group's timeline.
    pub duration_ns: u64,
    /// Optional host schema hash declared by this group.
    pub schema_hash: Option<u64>,
    /// Sample channels governed by this group's header.
    pub channel_range: Range<usize>,
    /// Spans governed by this group's header.
    pub span_range: Range<usize>,
}
/// One right-aligned header element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeaderChrome {
    /// Description text.
    Text(String),
    /// Fact pill (`label`, `value`).
    Pill {
        /// Short fact name, e.g. `Avg lap`.
        label: String,
        /// Fact value, e.g. `1:52.1`.
        value: String,
    },
}
impl TelemetrySource for JsonlRecording {
    fn path(&self) -> &str {
        &self.path
    }

    fn format(&self) -> &'static str {
        match self.source_format.as_str() {
            "aimd" => "aimd",
            "pds" => "pds",
            "motec" => "motec",
            "vbo" => "vbo",
            "telemetry" => "telemetry",
            "jsonl" => "jsonl",
            _ => "jsonl",
        }
    }

    fn channels(&self) -> &[Channel] {
        &self.channels
    }

    fn decode(&self, channel_index: usize, _chunk_index: usize, local_index: u64) -> f64 {
        let local = local_index as usize;
        self.values
            .get(channel_index)
            .and_then(|values| values.get(local))
            .copied()
            .unwrap_or(f64::NAN)
    }

    fn applied_passes(&self) -> &[AppliedPass] {
        &self.passes
    }
    fn identity(&self) -> SourceIdentity {
        self.identity.clone()
    }

    /// A JSONL file whose recorded `src` is a real vendor format is a
    /// conversion, so it reports that origin; a bare `jsonl`/empty `src`
    /// has no upstream origin.
    fn source_origin(&self) -> Option<SourceOrigin> {
        (!matches!(self.source_format.as_str(), "" | "jsonl")).then(|| SourceOrigin {
            format: self.source_format.clone(),
            path: self.source_path.clone(),
        })
    }

    fn source_lap_metadata(&self) -> Option<SourceLapMetadata> {
        if self.extension {
            return Some(SourceLapMetadata::default());
        }
        Some(SourceLapMetadata {
            laps: self.laps.clone(),
            fastest_lap: self
                .laps
                .iter()
                .filter(|lap| lap.complete)
                .min_by_key(|lap| lap.duration_ns)
                .cloned(),
        })
    }

    fn absolute_time_range(&self) -> Option<AbsoluteTimeRange> {
        self.clock.clone()
    }

    fn utc_start_ns(&self) -> Option<u64> {
        self.utc_start_ns
    }

    fn timezone(&self) -> String {
        self.timezone.clone()
    }

    fn channel_visible(&self) -> &[bool] {
        &self.channel_visible
    }

    fn spans(&self) -> &[Span] {
        &self.spans
    }

    fn channel_labels(&self, channel_index: usize) -> &[ChannelLabel] {
        self.channel_labels
            .get(channel_index)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    fn channel_display(&self, channel_index: usize) -> ChannelDisplay {
        self.channel_display
            .get(channel_index)
            .cloned()
            .unwrap_or_default()
    }

    fn video_files(&self) -> &[VideoFileRef] {
        &self.videos
    }

    fn video_presentation_times_ns(&self) -> Option<&[u64]> {
        (!self.video_times.is_empty()).then_some(self.video_times.as_slice())
    }

    fn video_frame_count(&self) -> Option<u64> {
        (!self.video_times.is_empty()).then_some(self.video_times.len() as u64)
    }

    fn video_frame_at(&self, time_ns: u64) -> Option<u64> {
        if self.video_times.is_empty() {
            return None;
        }
        let stamp = self.video_presentation_time_ns(time_ns)?;
        let index = self.video_times.partition_point(|time| *time <= stamp);
        Some(index.saturating_sub(1) as u64)
    }

    fn video_presentation_offset_ns(&self) -> Option<i128> {
        self.video_offset_ns
    }

    fn metadata(&self) -> FileMetadata {
        self.metadata_impl()
    }
}
