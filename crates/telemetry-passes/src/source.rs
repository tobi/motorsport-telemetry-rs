//! Accumulating source view: base channels plus pass-derived channels.

use crate::{PassError, PassOutput};
use motorsport_telemetry_core::{
    AbsoluteTimeRange, AppliedPass, Channel, Chunk, SampleType, SourceIdentity, SourceLapMetadata,
    SourceOrigin, Span, TelemetrySource, UnitSource, VideoFileRef, VideoReference,
};

/// Packed samples for one derived channel.
struct DerivedStore {
    /// Base-source channel index whose sample times this channel copies.
    /// Always resolved to the base source, even when a pass mirrored an
    /// earlier derived channel.
    mirrors: usize,
    sample_type: SampleType,
    data: Vec<u8>,
}

/// A [`TelemetrySource`] presenting a base source plus channels derived by
/// passes, with merged provenance.
///
/// The base source is untouched: derived samples live in memory here and
/// only become part of a file when the view is written out. Derived
/// channels answer `sample_time_ns` through the channel they mirror, so
/// event-timed channels keep exact timestamps end to end.
pub struct PassedSource<'a> {
    inner: &'a dyn TelemetrySource,
    inner_len: usize,
    channels: Vec<Channel>,
    derived: Vec<DerivedStore>,
    passes: Vec<AppliedPass>,
}

impl std::fmt::Debug for PassedSource<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PassedSource")
            .field("path", &self.inner.path())
            .field("base_channels", &self.inner_len)
            .field("derived_channels", &self.derived.len())
            .field("passes", &self.passes)
            .finish()
    }
}

impl<'a> PassedSource<'a> {
    /// Wraps `inner` with no derived channels yet.
    pub fn new(inner: &'a dyn TelemetrySource) -> Self {
        Self {
            inner,
            inner_len: inner.channels().len(),
            channels: inner.channels().to_vec(),
            derived: Vec::new(),
            passes: inner.applied_passes().to_vec(),
        }
    }

    /// Number of channels derived on top of the base source.
    pub fn derived_len(&self) -> usize {
        self.derived.len()
    }

    /// Appends a pass's output channels and records its provenance.
    /// Returns the appended channel names.
    pub(crate) fn push(
        &mut self,
        name: &str,
        version: u32,
        output: PassOutput,
    ) -> Result<Vec<String>, PassError> {
        let pass_label = format!("{name}@{version}");
        let mut outputs = Vec::with_capacity(output.channels.len());
        for derived in output.channels {
            if derived.mirrors >= self.channels.len() {
                return Err(PassError::BadMirror {
                    pass: pass_label,
                    mirrors: derived.mirrors,
                    channel_count: self.channels.len(),
                });
            }
            // Resolve chained mirrors to the base source so sample_time_ns
            // can always be answered by `inner`.
            let root = if derived.mirrors < self.inner_len {
                derived.mirrors
            } else {
                self.derived[derived.mirrors - self.inner_len].mirrors
            };
            let mirror = &self.channels[derived.mirrors];
            let width = derived.sample_type.byte_width();
            let expected = mirror.sample_count as usize * width;
            if derived.data.len() != expected {
                return Err(PassError::OutputShape {
                    pass: pass_label,
                    channel: derived.name,
                    expected,
                    actual: derived.data.len(),
                });
            }
            let mut chunks = Vec::with_capacity(mirror.chunks.len());
            let mut offset = 0u64;
            for chunk in &mirror.chunks {
                chunks.push(Chunk {
                    sample_period_ns: chunk.sample_period_ns,
                    sample_count: chunk.sample_count,
                    data_ptr: offset,
                    sample_base: chunk.sample_base,
                    time_base_ns: chunk.time_base_ns,
                });
                offset += chunk.sample_count * width as u64;
            }
            let sample_count = mirror.sample_count;
            let duration_ns = mirror.duration_ns;
            let id = self
                .channels
                .iter()
                .map(|channel| channel.id)
                .max()
                .unwrap_or(0)
                .saturating_add(1);
            self.channels.push(Channel {
                id,
                name: derived.name.clone(),
                unit: derived.unit,
                unit_source: UnitSource::Declared,
                sample_type: derived.sample_type,
                chunks,
                sample_count,
                duration_ns,
            });
            self.derived.push(DerivedStore {
                mirrors: root,
                sample_type: derived.sample_type,
                data: derived.data,
            });
            outputs.push(derived.name);
        }
        let mut params = output.params;
        params.sort_by(|a, b| a.0.cmp(&b.0));
        self.passes.push(AppliedPass {
            name: name.to_owned(),
            version,
            params,
            inputs: output.inputs,
            outputs: outputs.clone(),
        });
        Ok(outputs)
    }

    fn decode_derived(&self, store: &DerivedStore, chunk: &Chunk, local_index: u64) -> f64 {
        let width = store.sample_type.byte_width();
        let at = chunk.data_ptr as usize + local_index as usize * width;
        let bytes = &store.data[at..at + width];
        match store.sample_type {
            SampleType::I8 => bytes[0] as i8 as f64,
            SampleType::U8 => bytes[0] as f64,
            SampleType::I16 => i16::from_le_bytes(bytes.try_into().unwrap()) as f64,
            SampleType::U16 => u16::from_le_bytes(bytes.try_into().unwrap()) as f64,
            SampleType::I32 => i32::from_le_bytes(bytes.try_into().unwrap()) as f64,
            SampleType::U32 => u32::from_le_bytes(bytes.try_into().unwrap()) as f64,
            SampleType::F32 => f32::from_le_bytes(bytes.try_into().unwrap()) as f64,
            SampleType::F64 => f64::from_le_bytes(bytes.try_into().unwrap()),
        }
    }
}

impl TelemetrySource for PassedSource<'_> {
    fn path(&self) -> &str {
        self.inner.path()
    }

    fn format(&self) -> &'static str {
        self.inner.format()
    }

    fn channels(&self) -> &[Channel] {
        &self.channels
    }

    fn diagnostics(&self) -> &[motorsport_telemetry_core::Diagnostic] {
        self.inner.diagnostics()
    }

    fn decode(&self, channel_index: usize, chunk_index: usize, local_index: u64) -> f64 {
        if channel_index < self.inner_len {
            return self.inner.decode(channel_index, chunk_index, local_index);
        }
        let store = &self.derived[channel_index - self.inner_len];
        let chunk = &self.channels[channel_index].chunks[chunk_index];
        self.decode_derived(store, chunk, local_index)
    }

    fn chunk_bytes(&self, channel_index: usize, chunk_index: usize) -> Option<&[u8]> {
        if channel_index < self.inner_len {
            return self.inner.chunk_bytes(channel_index, chunk_index);
        }
        let store = &self.derived[channel_index - self.inner_len];
        let chunk = self.channels[channel_index].chunks.get(chunk_index)?;
        let width = store.sample_type.byte_width();
        let start = chunk.data_ptr as usize;
        let end = start + chunk.sample_count as usize * width;
        store.data.get(start..end)
    }

    fn sample_affine(&self, channel_index: usize) -> (f64, f64) {
        if channel_index < self.inner_len {
            self.inner.sample_affine(channel_index)
        } else {
            (1.0, 0.0)
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
        // Shorter-than-channels slices treat the remainder — including every
        // derived channel — as visible.
        self.inner.channel_visible()
    }

    fn spans(&self) -> &[Span] {
        self.inner.spans()
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

    fn video_reference_at(&self, time_ns: u64) -> VideoReference {
        self.inner.video_reference_at(time_ns)
    }

    fn sample_time_ns(&self, channel_index: usize, chunk_index: usize, local_index: u64) -> u64 {
        if channel_index < self.inner_len {
            return self
                .inner
                .sample_time_ns(channel_index, chunk_index, local_index);
        }
        let store = &self.derived[channel_index - self.inner_len];
        self.inner
            .sample_time_ns(store.mirrors, chunk_index, local_index)
    }

    fn sample_at(&self, channel_index: usize, time_ns: u64, linear: bool) -> Option<f64> {
        if channel_index < self.inner_len {
            return self.inner.sample_at(channel_index, time_ns, linear);
        }
        let channel = self.channels.get(channel_index)?;
        if time_ns >= channel.duration_ns || channel.chunks.is_empty() {
            return None;
        }
        let mirrors = self.derived[channel_index - self.inner_len].mirrors;
        // Locate by true sample times through the mirror, so event-timed
        // mirrors resolve exactly rather than through the chunk grid.
        let chunk_index = channel
            .chunks
            .partition_point(|chunk| chunk.time_base_ns <= time_ns)
            .saturating_sub(1);
        let chunk = &channel.chunks[chunk_index];
        let time_of =
            |chunk_index: usize, local: u64| self.inner.sample_time_ns(mirrors, chunk_index, local);
        let sample = if time_ns <= time_of(chunk_index, 0) {
            0
        } else {
            let mut low = 0u64;
            let mut high = chunk.sample_count - 1;
            while high > low {
                let middle = low + (high - low).div_ceil(2);
                if time_of(chunk_index, middle) <= time_ns {
                    low = middle;
                } else {
                    high = middle - 1;
                }
            }
            low
        };
        let a = self.decode(channel_index, chunk_index, sample);
        if !linear || channel.uses_step_interpolation() {
            return Some(a);
        }
        let sample_time = time_of(chunk_index, sample);
        let (b, next_time) = if sample + 1 < chunk.sample_count {
            (
                self.decode(channel_index, chunk_index, sample + 1),
                time_of(chunk_index, sample + 1),
            )
        } else if chunk_index + 1 < channel.chunks.len() {
            (
                self.decode(channel_index, chunk_index + 1, 0),
                time_of(chunk_index + 1, 0),
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
