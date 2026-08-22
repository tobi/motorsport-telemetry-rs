//! Time-alignment helpers: regular-channel collection and lattice snapping.

use super::json::invalid;
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
/// Lays one channel onto a single `hz`/`t0` lattice, which is all MTJ can
/// express for a channel.
///
/// Readers now fit each gap-free run with its own period so the chunk model
/// follows the logger's stamps (see `aim_telemetry::period_chunks`), which
/// means a real recording almost never arrives as one exact lattice. Refusing
/// such channels — the old behaviour — silently dropped nearly every channel
/// of an AiM export. Instead every sample is placed in the lattice slot
/// nearest its own timestamp: a sample is never more than half a period from
/// where the logger put it, a slot nothing landed in is `null`, and when two
/// samples contend for one slot the earlier one is kept. That is the loss
/// inherent to the MTJ schema, not a property of the channel.
pub(super) fn collect_aligned(
    source: &dyn TelemetrySource,
    index: usize,
    channel: &Channel,
) -> Option<AlignedSeries> {
    if channel.sample_count == 0 || channel.chunks.is_empty() {
        return None;
    }
    let period_ns = lattice_period_ns(channel)?;
    let t0_ns = channel
        .chunks
        .iter()
        .map(|chunk| chunk.time_base_ns)
        .min()?;
    let mut last_ns = t0_ns;
    for (chunk_index, chunk) in channel.chunks.iter().enumerate() {
        if chunk.sample_count == 0 {
            continue;
        }
        last_ns = last_ns.max(source.sample_time_ns(index, chunk_index, chunk.sample_count - 1));
    }
    let count = usize::try_from(nearest_slot(last_ns, t0_ns, period_ns)? + 1).ok()?;
    let mut values = vec![None; count];
    let mut taken = vec![false; count];
    for (chunk_index, chunk) in channel.chunks.iter().enumerate() {
        for local in 0..chunk.sample_count {
            let time = source.sample_time_ns(index, chunk_index, local);
            let slot = usize::try_from(nearest_slot(time, t0_ns, period_ns)?).ok()?;
            if slot >= count || taken[slot] {
                continue;
            }
            taken[slot] = true;
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

/// Index of the lattice slot nearest `time_ns`; `None` when it precedes `t0`.
fn nearest_slot(time_ns: u64, t0_ns: u64, period_ns: u64) -> Option<u64> {
    let offset = time_ns.checked_sub(t0_ns)?;
    Some((offset + period_ns / 2) / period_ns)
}

/// The lattice period for a channel: the sample-weighted dominant chunk
/// period, rounded to the nearest millisecond when it is within 2 % of one so
/// a fitted 9 999 213 ns run is written as `hz: 100` rather than
/// `100.007870…`. A period that is not close to a millisecond multiple is
/// kept exactly.
fn lattice_period_ns(channel: &Channel) -> Option<u64> {
    let mut weights: Vec<(u64, u64)> = Vec::new();
    for chunk in &channel.chunks {
        if chunk.sample_period_ns == 0 || chunk.sample_count == 0 {
            continue;
        }
        match weights
            .iter_mut()
            .find(|(period, _)| *period == chunk.sample_period_ns)
        {
            Some((_, weight)) => *weight += chunk.sample_count,
            None => weights.push((chunk.sample_period_ns, chunk.sample_count)),
        }
    }
    let (dominant, _) = weights
        .into_iter()
        .max_by_key(|&(period, weight)| (weight, std::cmp::Reverse(period)))?;
    const MILLISECOND: u64 = 1_000_000;
    if dominant >= MILLISECOND {
        let rounded = ((dominant + MILLISECOND / 2) / MILLISECOND) * MILLISECOND;
        if rounded > 0 && rounded.abs_diff(dominant) * 50 <= dominant {
            return Some(rounded);
        }
    }
    Some(dominant)
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
