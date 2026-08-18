//! A window over another source: retained channels plus appended mirrors.
//!
//! [`ViewSource`] wraps any [`crate::TelemetrySource`] and exposes a subset of
//! its channels (in any order) plus in-memory appended channels that mirror an
//! existing channel's chunk layout and sample times. It is the foundation for
//! lossless derived channels: a pass appends its outputs as packed bytes that
//! share the mirrored channel's timeline, and the view decodes them on demand.

use crate::names;
use crate::{
    chunk_bytes, sample_bytes, AbsoluteTimeRange, AppliedPass, Channel, ChannelDisplay,
    ChannelLabel, Chunk, FileMetadata, SampleTimes, SampleType, SourceIdentity, SourceLapMetadata,
    SourceOrigin, Span, TelemetrySource, UnitSource, VideoFileRef, VideoReference,
};

/// How one view channel is backed.
#[derive(Debug, Clone)]
enum Slot {
    /// A channel retained from the inner source, by inner index.
    Inner(usize),
    /// An appended channel mirroring an inner channel's layout.
    Append {
        /// The resolved inner channel whose chunk layout and sample times are mirrored.
        inner_mirror: usize,
        /// Packed native bytes for the appended channel, laid out as
        /// `chunk.sample_count * sample_type.byte_width()` per chunk, in chunk order.
        data: Vec<u8>,
    },
}

/// A [`TelemetrySource`] over `inner`: any subset of inner channels (in any
/// order) plus in-memory appended channels that mirror an existing channel's
/// layout and sample times.
///
/// Construct with [`ViewSource::new`] (keeps every inner channel) then
/// [`ViewSource::retain`] to drop channels and [`ViewSource::append`] to add
/// derived ones. `retain` is only valid before the first `append`.
pub struct ViewSource<'a> {
    inner: &'a dyn TelemetrySource,
    channels: Vec<Channel>,
    slots: Vec<Slot>,
    visible: Vec<bool>,
    passes: Vec<AppliedPass>,
}

impl std::fmt::Debug for ViewSource<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ViewSource")
            .field("path", &self.inner.path())
            .field("format", &self.inner.format())
            .field("channels", &self.channels.len())
            .field("appended", &self.appended_len())
            .field("visible", &self.visible)
            .field("passes", &self.passes)
            .finish()
    }
}

/// Errors raised while building a [`ViewSource`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewError {
    /// `append` was given a `mirrors` view index that does not exist.
    BadMirror {
        /// The requested mirror view index.
        mirrors: usize,
        /// The number of channels currently in the view.
        channel_count: usize,
    },
    /// `append` was given a `data` buffer whose length does not match the
    /// mirrored channel's sample footprint.
    OutputShape {
        /// Name of the appended channel.
        channel: String,
        /// Expected byte length: `sample_count * sample_type.byte_width()`.
        expected: usize,
        /// Actual `data` length passed.
        actual: usize,
    },
    /// `append` was given a name that already exists in the view.
    DuplicateName(String),
}

impl<'a> ViewSource<'a> {
    /// Creates a view over `inner` keeping every inner channel, with visibility
    /// and applied passes copied from `inner`.
    pub fn new(inner: &'a dyn TelemetrySource) -> Self {
        let channels = inner.channels().to_vec();
        let slots = (0..channels.len()).map(Slot::Inner).collect();
        let inner_visible = inner.channel_visible();
        let visible = (0..channels.len())
            .map(|index| inner_visible.get(index).copied().unwrap_or(true))
            .collect();
        let passes = inner.applied_passes().to_vec();
        Self {
            inner,
            channels,
            slots,
            visible,
            passes,
        }
    }

    /// Drops channels for which `keep` returns `false`.
    ///
    /// `keep` receives the **pre-view inner index** (equal to the view index
    /// before any append) and the channel. Only valid before the first
    /// [`Self::append`]; calling it after appends panics.
    pub fn retain(&mut self, keep: impl FnMut(usize, &Channel) -> bool) {
        assert!(
            self.appended_len() == 0,
            "ViewSource::retain is only valid before any append"
        );
        let mut keep = keep;
        let mut next_channels = Vec::new();
        let mut next_slots = Vec::new();
        let mut next_visible = Vec::new();
        for (view_index, (channel, slot)) in self.channels.iter().zip(&self.slots).enumerate() {
            if keep(view_index, channel) {
                next_channels.push(channel.clone());
                next_slots.push(slot.clone());
                next_visible.push(self.visible.get(view_index).copied().unwrap_or(true));
            }
        }
        self.channels = next_channels;
        self.slots = next_slots;
        self.visible = next_visible;
    }

    /// Appends a derived channel that mirrors `mirrors` (a view index).
    ///
    /// `data` must be exactly `mirror.sample_count * sample_type.byte_width()`
    /// bytes, laid out per chunk in the mirror's chunk order. The new channel
    /// shares the mirror's chunk layout and sample times (resolved to the inner
    /// source), uses affine `(1.0, 0.0)`, a trace display, and empty labels.
    /// Its id is one greater than the largest existing channel id; its unit is
    /// recorded as [`UnitSource::Declared`]. Returns the new view index.
    pub fn append(
        &mut self,
        name: &str,
        unit: &str,
        sample_type: SampleType,
        mirrors: usize,
        data: Vec<u8>,
    ) -> Result<usize, ViewError> {
        if mirrors >= self.channels.len() {
            return Err(ViewError::BadMirror {
                mirrors,
                channel_count: self.channels.len(),
            });
        }
        let normalized = names::normalize(name);
        if self
            .channels
            .iter()
            .any(|channel| names::eq(&channel.name, &normalized))
        {
            return Err(ViewError::DuplicateName(name.to_owned()));
        }
        let mirror_channel = &self.channels[mirrors];
        let inner_mirror = match &self.slots[mirrors] {
            Slot::Inner(inner_index) => *inner_index,
            Slot::Append { inner_mirror, .. } => *inner_mirror,
        };
        let width = sample_type.byte_width();
        let expected = mirror_channel
            .sample_count
            .checked_mul(width as u64)
            .and_then(|bytes| usize::try_from(bytes).ok())
            .unwrap_or(usize::MAX);
        if data.len() != expected {
            return Err(ViewError::OutputShape {
                channel: name.to_owned(),
                expected,
                actual: data.len(),
            });
        }
        let max_id = self
            .channels
            .iter()
            .map(|channel| channel.id)
            .max()
            .unwrap_or(0);
        let chunks = mirror_chunks(mirror_channel, width);
        let channel = Channel {
            id: max_id + 1,
            name: name.to_owned(),
            unit: unit.to_owned(),
            unit_source: UnitSource::Declared,
            sample_type,
            chunks,
            sample_count: mirror_channel.sample_count,
            duration_ns: mirror_channel.duration_ns,
        };
        self.channels.push(channel);
        self.slots.push(Slot::Append { inner_mirror, data });
        self.visible.push(true);
        Ok(self.channels.len() - 1)
    }

    /// Returns a mutable handle to the view's applied-pass provenance vector.
    pub fn passes_mut(&mut self) -> &mut Vec<AppliedPass> {
        &mut self.passes
    }

    /// Returns a mutable handle to the per-channel visibility flags.
    ///
    /// Length equals [`TelemetrySource::channels`] length.
    pub fn visible_mut(&mut self) -> &mut Vec<bool> {
        &mut self.visible
    }

    /// Returns the inner source this view wraps.
    pub fn inner(&self) -> &'a dyn TelemetrySource {
        self.inner
    }

    /// Returns the number of appended (derived) channels in the view.
    pub fn appended_len(&self) -> usize {
        self.slots
            .iter()
            .filter(|slot| matches!(slot, Slot::Append { .. }))
            .count()
    }
}

/// Builds chunk metadata for an appended channel: same sample layout as the
/// mirror, with `data_ptr` as a cumulative byte offset into the appended buffer.
fn mirror_chunks(mirror: &Channel, width: usize) -> Vec<Chunk> {
    let mut chunks = Vec::with_capacity(mirror.chunks.len());
    let mut data_ptr: u64 = 0;
    for chunk in &mirror.chunks {
        chunks.push(Chunk {
            sample_period_ns: chunk.sample_period_ns,
            sample_count: chunk.sample_count,
            data_ptr,
            sample_base: chunk.sample_base,
            time_base_ns: chunk.time_base_ns,
        });
        let chunk_bytes_len = chunk.sample_count.saturating_mul(width as u64);
        data_ptr = data_ptr.saturating_add(chunk_bytes_len);
    }
    chunks
}

impl TelemetrySource for ViewSource<'_> {
    fn path(&self) -> &str {
        self.inner.path()
    }
    fn format(&self) -> &'static str {
        self.inner.format()
    }
    fn channels(&self) -> &[Channel] {
        &self.channels
    }
    fn decode(&self, channel_index: usize, chunk_index: usize, local_index: u64) -> f64 {
        match &self.slots[channel_index] {
            Slot::Inner(inner_index) => self.inner.decode(*inner_index, chunk_index, local_index),
            Slot::Append { data, .. } => {
                let channel = &self.channels[channel_index];
                let chunk = &channel.chunks[chunk_index];
                let width = channel.sample_type.byte_width();
                sample_bytes(data, chunk, local_index, width)
                    .and_then(|bytes| channel.sample_type.decode_le(bytes))
                    .unwrap_or(0.0)
            }
        }
    }
    fn chunk_bytes(&self, channel_index: usize, chunk_index: usize) -> Option<&[u8]> {
        match &self.slots[channel_index] {
            Slot::Inner(inner_index) => self.inner.chunk_bytes(*inner_index, chunk_index),
            Slot::Append { data, .. } => {
                let channel = &self.channels[channel_index];
                let chunk = channel.chunks.get(chunk_index)?;
                chunk_bytes(data, chunk, channel.sample_type.byte_width())
            }
        }
    }
    fn sample_affine(&self, channel_index: usize) -> (f64, f64) {
        match &self.slots[channel_index] {
            Slot::Inner(inner_index) => self.inner.sample_affine(*inner_index),
            Slot::Append { .. } => (1.0, 0.0),
        }
    }
    fn absolute_time_range(&self) -> Option<AbsoluteTimeRange> {
        self.inner.absolute_time_range()
    }
    fn utc_start_ns(&self) -> Option<u64> {
        self.inner.utc_start_ns()
    }
    fn timezone(&self) -> String {
        self.inner.timezone()
    }
    fn channel_visible(&self) -> &[bool] {
        &self.visible
    }
    fn diagnostics(&self) -> &[crate::Diagnostic] {
        self.inner.diagnostics()
    }
    fn spans(&self) -> &[Span] {
        self.inner.spans()
    }
    fn channel_labels(&self, channel_index: usize) -> &[ChannelLabel] {
        match &self.slots[channel_index] {
            Slot::Inner(inner_index) => self.inner.channel_labels(*inner_index),
            Slot::Append { .. } => &[],
        }
    }
    fn channel_display(&self, channel_index: usize) -> ChannelDisplay {
        match &self.slots[channel_index] {
            Slot::Inner(inner_index) => self.inner.channel_display(*inner_index),
            Slot::Append { .. } => ChannelDisplay::trace(),
        }
    }
    fn applied_passes(&self) -> &[AppliedPass] {
        &self.passes
    }
    fn source_origin(&self) -> Option<SourceOrigin> {
        self.inner.source_origin()
    }
    fn identity(&self) -> SourceIdentity {
        self.inner.identity()
    }
    fn source_lap_metadata(&self) -> Option<SourceLapMetadata> {
        self.inner.source_lap_metadata()
    }
    fn video_files(&self) -> &[VideoFileRef] {
        self.inner.video_files()
    }
    fn video_presentation_times_ns(&self) -> Option<&[u64]> {
        self.inner.video_presentation_times_ns()
    }
    fn video_frame_count(&self) -> Option<u64> {
        self.inner.video_frame_count()
    }
    fn video_frame_at(&self, time_ns: u64) -> Option<u64> {
        self.inner.video_frame_at(time_ns)
    }
    fn video_presentation_offset_ns(&self) -> Option<i128> {
        self.inner.video_presentation_offset_ns()
    }
    fn video_presentation_time_ns(&self, time_ns: u64) -> Option<u64> {
        self.inner.video_presentation_time_ns(time_ns)
    }
    fn video_reference_at(&self, time_ns: u64) -> VideoReference {
        self.inner.video_reference_at(time_ns)
    }
    fn metadata(&self) -> FileMetadata {
        self.inner.metadata()
    }
    fn sample_times(&self, channel_index: usize) -> SampleTimes<'_> {
        match &self.slots[channel_index] {
            Slot::Inner(inner_index) => self.inner.sample_times(*inner_index),
            Slot::Append { inner_mirror, .. } => self.inner.sample_times(*inner_mirror),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Chunk, UnitSource};

    /// A minimal two-channel inner source with grid sample times.
    struct GridInner {
        channels: Vec<Channel>,
        speed: [f64; 4],
        rpm: [f64; 4],
    }

    impl GridInner {
        fn new() -> Self {
            let chunk = Chunk {
                sample_period_ns: 1_000_000_000,
                sample_count: 4,
                data_ptr: 0,
                sample_base: 0,
                time_base_ns: 1_000_000_000,
            };
            let speed = Channel {
                id: 10,
                name: "Speed".into(),
                unit: "km/h".into(),
                unit_source: UnitSource::Declared,
                sample_type: SampleType::F32,
                chunks: vec![chunk.clone()],
                sample_count: 4,
                duration_ns: 5_000_000_000,
            };
            let rpm = Channel {
                id: 11,
                name: "RPM".into(),
                unit: "rpm".into(),
                unit_source: UnitSource::Declared,
                sample_type: SampleType::F32,
                chunks: vec![chunk],
                sample_count: 4,
                duration_ns: 5_000_000_000,
            };
            Self {
                channels: vec![speed, rpm],
                speed: [0.0, 10.0, 20.0, 30.0],
                rpm: [1000.0, 2000.0, 3000.0, 4000.0],
            }
        }
    }

    impl TelemetrySource for GridInner {
        fn path(&self) -> &str {
            "grid"
        }
        fn format(&self) -> &'static str {
            "grid"
        }
        fn channels(&self) -> &[Channel] {
            &self.channels
        }
        fn decode(&self, channel_index: usize, _chunk_index: usize, local_index: u64) -> f64 {
            let local = local_index as usize;
            if channel_index == 0 {
                self.speed[local]
            } else {
                self.rpm[local]
            }
        }
    }

    #[test]
    fn retain_reorders_and_remaps_decode() {
        let inner = GridInner::new();
        let mut view = ViewSource::new(&inner);
        // Keep only RPM (inner index 1) — view index 0 now maps to inner 1.
        view.retain(|index, _| index == 1);
        assert_eq!(view.channels().len(), 1);
        assert_eq!(view.channels()[0].name, "RPM");
        // decode(view 0, ...) must remap to inner RPM.
        assert_eq!(view.decode(0, 0, 0), 1000.0);
        assert_eq!(view.decode(0, 0, 3), 4000.0);
        // sample_at uses the view's chunk layout and decode.
        assert_eq!(view.sample_at(0, 3_000_000_000, false), Some(3000.0));
    }

    #[test]
    fn append_grid_mirror_decodes_from_data() {
        let inner = GridInner::new();
        let mut view = ViewSource::new(&inner);
        // Append a derived channel mirroring Speed (view index 0).
        let mut buf = [0u8; 8];
        let data: Vec<u8> = [1.0_f32, 2.0, 3.0, 4.0]
            .iter()
            .flat_map(|v| {
                let n = SampleType::F32.encode_le(*v as f64, &mut buf);
                buf[..n].to_vec()
            })
            .collect();
        let index = view
            .append("Derived", "m/s", SampleType::F32, 0, data)
            .unwrap();
        assert_eq!(index, 2);
        assert_eq!(view.appended_len(), 1);
        assert_eq!(view.channels()[2].name, "Derived");
        assert_eq!(view.channels()[2].unit_source, UnitSource::Declared);
        assert_eq!(view.channels()[2].id, 12);
        assert_eq!(view.decode(2, 0, 0), 1.0);
        assert_eq!(view.decode(2, 0, 3), 4.0);
        // sample_at interpolates the derived float channel at the grid instants.
        assert_eq!(view.sample_at(2, 1_500_000_000, true), Some(1.5));
        assert_eq!(view.sample_at(2, 2_500_000_000, true), Some(2.5));
        // before the first sample time (time_base_ns = 1e9) -> None.
        assert_eq!(view.sample_at(2, 500_000_000, true), None);
        // chunk_bytes serves the appended buffer.
        let bytes = view.chunk_bytes(2, 0).unwrap();
        assert_eq!(bytes.len(), 4 * 4);
    }

    /// An inner source with explicit per-sample timestamps.
    struct ExplicitInner {
        channels: Vec<Channel>,
        values: Vec<f64>,
        times: Vec<u64>,
    }

    impl ExplicitInner {
        fn new() -> Self {
            let times = vec![100u64, 300, 700, 1500];
            let chunk = Chunk {
                sample_period_ns: 0,
                sample_count: 4,
                data_ptr: 0,
                sample_base: 0,
                time_base_ns: 100,
            };
            let channel = Channel {
                id: 1,
                name: "X".into(),
                unit: String::new(),
                unit_source: UnitSource::Unknown,
                sample_type: SampleType::F64,
                chunks: vec![chunk],
                sample_count: 4,
                duration_ns: 2000,
            };
            Self {
                channels: vec![channel],
                values: vec![0.0, 10.0, 20.0, 30.0],
                times,
            }
        }
    }

    impl TelemetrySource for ExplicitInner {
        fn path(&self) -> &str {
            "explicit"
        }
        fn format(&self) -> &'static str {
            "explicit"
        }
        fn channels(&self) -> &[Channel] {
            &self.channels
        }
        fn decode(&self, _channel_index: usize, _chunk_index: usize, local_index: u64) -> f64 {
            self.values[local_index as usize]
        }
        fn sample_times(&self, _channel_index: usize) -> SampleTimes<'_> {
            SampleTimes::Explicit(&self.times)
        }
    }

    #[test]
    fn append_explicit_mirror_keeps_exact_stamps() {
        let inner = ExplicitInner::new();
        let mut view = ViewSource::new(&inner);
        // Append a derived channel mirroring X (view index 0).
        let mut buf = [0u8; 8];
        let data: Vec<u8> = [100.0_f64, 200.0, 300.0, 400.0]
            .iter()
            .flat_map(|v| {
                let n = SampleType::F64.encode_le(*v, &mut buf);
                buf[..n].to_vec()
            })
            .collect();
        view.append("Y", "unit", SampleType::F64, 0, data).unwrap();
        // The appended channel's sample_times resolve to the inner Explicit stamps.
        match view.sample_times(1) {
            SampleTimes::Explicit(times) => assert_eq!(times, &[100, 300, 700, 1500]),
            SampleTimes::Grid => panic!("expected Explicit sample times"),
        }
        // sample_time_ns uses the explicit stamps.
        assert_eq!(view.sample_time_ns(1, 0, 2), 700);
        // sample_at bisects the stamps and interpolates.
        assert_eq!(view.sample_at(1, 100, true), Some(100.0));
        assert_eq!(view.sample_at(1, 200, true), Some(150.0));
        assert_eq!(view.sample_at(1, 500, true), Some(250.0));
        assert_eq!(view.sample_at(1, 1100, true), Some(350.0));
        // before the first stamp -> None.
        assert_eq!(view.sample_at(1, 50, true), None);
        // at or beyond duration -> None.
        assert_eq!(view.sample_at(1, 2000, true), None);
    }

    #[test]
    fn append_errors() {
        let inner = GridInner::new();
        let mut view = ViewSource::new(&inner);
        // BadMirror: mirrors out of range.
        assert_eq!(
            view.append("D", "u", SampleType::F32, 5, Vec::new()),
            Err(ViewError::BadMirror {
                mirrors: 5,
                channel_count: 2
            })
        );
        // OutputShape: wrong data length (expected 4*4=16, got 3).
        assert_eq!(
            view.append("D", "u", SampleType::F32, 0, vec![0u8; 3]),
            Err(ViewError::OutputShape {
                channel: "D".into(),
                expected: 16,
                actual: 3
            })
        );
        // DuplicateName: "Speed" already exists.
        let data = vec![0u8; 16];
        assert_eq!(
            view.append("Speed", "u", SampleType::F32, 0, data),
            Err(ViewError::DuplicateName("Speed".into()))
        );
    }

    #[test]
    fn passes_and_visible_vectors() {
        let inner = GridInner::new();
        let mut view = ViewSource::new(&inner);
        assert_eq!(view.visible_mut().len(), 2);
        view.visible_mut()[0] = false;
        assert_eq!(view.channel_visible(), &[false, true]);
        let pass = AppliedPass {
            name: "test".into(),
            ..AppliedPass::default()
        };
        view.passes_mut().push(pass.clone());
        assert_eq!(view.applied_passes(), &[pass]);
        // Inner accessor still reaches the wrapped source.
        assert_eq!(view.inner().format(), "grid");
    }
}
