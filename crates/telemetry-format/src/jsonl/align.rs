//! Time-alignment helpers: regular-channel collection and lattice snapping.

use super::json::invalid;
use super::ALIGN_JITTER_NS;
use crate::write::TelemetryFormatError;
use motorsport_telemetry_core::{
    Channel, ChannelDisplay, ChannelLabel, LapMetadata, Span, TelemetrySource,
};

pub(super) struct AlignedSeries {
    pub(super) name: String,
    pub(super) unit: String,
    pub(super) t0_ns: u64,
    pub(super) period_ns: u64,
    pub(super) values: Vec<Option<f64>>,
    pub(super) visible: bool,
    pub(super) labels: Vec<ChannelLabel>,
    pub(super) display: ChannelDisplay,
}
impl AlignedSeries {
    pub(super) fn end_ns(&self) -> u64 {
        self.t0_ns + self.values.len() as u64 * self.period_ns
    }
}
pub(super) fn collect_aligned(
    source: &dyn TelemetrySource,
    index: usize,
    channel: &Channel,
) -> Option<AlignedSeries> {
    if channel.sample_count == 0 || channel.chunks.is_empty() {
        return None;
    }
    let period_ns = channel.first_period_ns().filter(|period| *period > 0)?;
    if channel
        .chunks
        .iter()
        .any(|chunk| chunk.sample_period_ns != period_ns)
    {
        return None;
    }
    let jitter = ALIGN_JITTER_NS.min(period_ns / 2).max(1);
    for (chunk_index, chunk) in channel.chunks.iter().enumerate() {
        for local in 0..chunk.sample_count {
            let actual = source.sample_time_ns(index, chunk_index, local);
            let expected = chunk
                .time_base_ns
                .checked_add(local.checked_mul(period_ns)?)?;
            if actual.abs_diff(expected) > jitter {
                return None;
            }
        }
    }
    let t0_ns = channel.chunks[0].time_base_ns;
    let last = {
        let chunk = channel.chunks.last()?;
        chunk.time_base_ns.checked_add(
            chunk
                .sample_count
                .saturating_sub(1)
                .checked_mul(period_ns)?,
        )?
    };
    if last < t0_ns {
        return None;
    }
    let count = usize::try_from((last - t0_ns) / period_ns + 1).ok()?;
    let mut values = vec![None; count];
    for (chunk_index, chunk) in channel.chunks.iter().enumerate() {
        for local in 0..chunk.sample_count {
            let time = chunk
                .time_base_ns
                .checked_add(local.checked_mul(period_ns)?)?;
            let slot = usize::try_from((time - t0_ns) / period_ns).ok()?;
            let value = source.decode(index, chunk_index, local);
            values[slot] = value.is_finite().then_some(value);
        }
    }
    if values.iter().all(Option::is_none) {
        return None;
    }
    Some(AlignedSeries {
        name: channel.name.clone(),
        unit: channel.unit.clone(),
        t0_ns,
        period_ns,
        values,
        visible: source.channel_visible().get(index).copied().unwrap_or(true),
        display: source.channel_display(index),
        labels: if source.channel_display(index).plot.is_trace() {
            source.channel_labels(index).to_vec()
        } else {
            Vec::new()
        },
    })
}
pub(super) fn snap_spans(
    spans: &[Span],
    quantum_ns: u64,
) -> Result<Vec<Span>, TelemetryFormatError> {
    if quantum_ns == 0 {
        return Ok(spans.to_vec());
    }
    spans
        .iter()
        .map(|span| {
            let start_ns = snap_nearest(span.start_ns, quantum_ns)?;
            let mut end_ns = snap_nearest(span.end_ns, quantum_ns)?;
            if end_ns <= start_ns {
                end_ns = start_ns
                    .checked_add(quantum_ns)
                    .ok_or_else(|| invalid("snapped span end overflows u64"))?;
            }
            Ok(Span {
                name: span.name.clone(),
                start_ns,
                end_ns,
                visible: span.visible,
                color: span.color.clone(),
                primary: span.primary.clone(),
                meta: span.meta.clone(),
            })
        })
        .collect()
}
pub(super) fn snap_laps(
    laps: &[LapMetadata],
    quantum_ns: u64,
) -> Result<Vec<LapMetadata>, TelemetryFormatError> {
    laps.iter()
        .map(|lap| {
            let mut start_ns = snap_nearest(lap.start_ns, quantum_ns)?;
            let mut end_ns = snap_nearest(lap.end_ns, quantum_ns)?;
            if end_ns <= start_ns {
                end_ns = start_ns
                    .checked_add(quantum_ns)
                    .ok_or_else(|| invalid("snapped lap end overflows u64"))?;
            }
            if start_ns == end_ns {
                start_ns = 0;
                end_ns = quantum_ns;
            }
            let duration_ns = end_ns
                .checked_sub(start_ns)
                .ok_or_else(|| invalid("snapped lap duration underflows u64"))?;
            Ok(LapMetadata {
                number: lap.number,
                start_ns,
                end_ns,
                duration_ns,
                complete: lap.complete,
                first_video_frame: lap.first_video_frame,
            })
        })
        .collect()
}
/// Converts a JSON `hz` number into a nanosecond sample period.
pub fn period_ns_from_hz(hz: f64) -> Option<u64> {
    if !hz.is_finite() || hz <= 0.0 {
        return None;
    }
    if hz.fract() == 0.0 && hz <= 1_000_000_000.0 {
        let hz_u = hz as u64;
        if hz_u > 0 && 1_000_000_000 % hz_u == 0 {
            return Some(1_000_000_000 / hz_u);
        }
    }
    let period = (1e9 / hz).round();
    if !(1.0..=u64::MAX as f64).contains(&period) {
        return None;
    }
    Some(period as u64)
}
fn snap_nearest(value: u64, quantum_ns: u64) -> Result<u64, TelemetryFormatError> {
    if quantum_ns <= 1 {
        return Ok(value);
    }
    let rem = value % quantum_ns;
    if rem * 2 < quantum_ns {
        Ok(value - rem)
    } else {
        value
            .checked_add(quantum_ns - rem)
            .ok_or_else(|| invalid("snapped timestamp overflows u64"))
    }
}
pub(super) fn snap_up(value: u64, quantum_ns: u64) -> Result<u64, TelemetryFormatError> {
    if quantum_ns <= 1 {
        return Ok(value);
    }
    let rem = value % quantum_ns;
    if rem == 0 {
        Ok(value)
    } else {
        value
            .checked_add(quantum_ns - rem)
            .ok_or_else(|| invalid("snapped timestamp overflows u64"))
    }
}
pub(super) fn gcd(mut left: u64, mut right: u64) -> u64 {
    while right != 0 {
        let rest = left % right;
        left = right;
        right = rest;
    }
    left
}
