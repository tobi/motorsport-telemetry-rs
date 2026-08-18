//! Motorsport Telemetry JSONL (MTJ): compact, time-aligned interchange.
//!
//! The normative rules live in `JSONL.md`. This module is a conforming
//! reader and writer.

use crate::write::TelemetryFormatError;
use motorsport_telemetry_core::{
    read_source_metadata, AbsoluteTimeRange, AppliedPass, Channel, ChannelDisplay, ChannelLabel,
    FileMetadata, LapMetadata, SourceIdentity, VideoFileRef,
};
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

mod align;
mod json;
mod read;
mod source;
mod write;

#[cfg(test)]
mod tests;

use align::gcd;
use json::invalid;
use write::{join_shift_ns, shift_channel, shift_span};

pub use align::period_ns_from_hz;
pub use motorsport_telemetry_core::{Span, SpanPrimary};
pub use source::{HeaderChrome, SidecarGroup, SidecarHeader};
pub use write::{
    write_jsonl_extension_from_source, write_jsonl_extension_from_source_with,
    write_jsonl_from_source, write_jsonl_from_source_with, write_jsonl_timeline,
    write_jsonl_timeline_with, write_jsonl_to,
};

/// zstd frame magic (`0x28B52FFD`).
pub(super) const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];
/// Default zstd level for compressed MTJ documents. This one goes to 11.
pub const JSONL_ZSTD_LEVEL: i32 = 11;
/// Document version written and accepted by this module.
pub const JSONL_VERSION: u16 = 1;
/// Extension (`mtx`) document version written and accepted by this module.
pub const JSONL_EXT_VERSION: u16 = 1;
/// Recommended lattice quantum when a recording has no regular channels.
pub(super) const DEFAULT_QUANTUM_NS: u64 = 1_000_000;
/// Jitter allowed when deciding that a native channel is already regular.
pub(super) const ALIGN_JITTER_NS: u64 = 2_000_000;
/// An opened MTJ document.
#[derive(Debug, Clone)]
pub struct JsonlRecording {
    pub(super) path: String,
    pub(super) source_format: String,
    pub(super) source_path: String,
    pub(super) identity: SourceIdentity,
    pub(super) clock: Option<AbsoluteTimeRange>,
    pub(super) utc_start_ns: Option<u64>,
    pub(super) timezone: String,
    pub(super) laps: Vec<LapMetadata>,
    pub(super) channels: Vec<Channel>,
    pub(super) values: Vec<Vec<f64>>,
    pub(super) quantum_ns: u64,
    pub(super) origin_ns: u64,
    pub(super) duration_ns: u64,
    pub(super) schema_hash: Option<u64>,
    pub(super) extension: bool,
    pub(super) sidecar_groups: Vec<SidecarGroup>,
    pub(super) channel_visible: Vec<bool>,
    pub(super) channel_labels: Vec<Vec<ChannelLabel>>,
    pub(super) channel_display: Vec<ChannelDisplay>,
    pub(super) spans: Vec<Span>,
    pub(super) passes: Vec<AppliedPass>,
    pub(super) videos: Vec<VideoFileRef>,
    pub(super) video_times: Vec<u64>,
    pub(super) video_offset_ns: Option<i128>,
}
impl JsonlRecording {
    /// Reads an MTJ file from `path`.
    ///
    /// Accepts `.telemetry.jsonl`, `.jsonl`, `.mtj`, and the same names with a
    /// `.zstd` or `.zst` suffix. A zstd frame is also detected by magic bytes,
    /// so a compressed document named without the suffix still opens.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, TelemetryFormatError> {
        let path = path.as_ref();
        let mut file = File::open(path)?;
        let display = path.to_string_lossy().into_owned();
        if starts_with_zstd(&mut file)? {
            let decoder = zstd::Decoder::new(file).map_err(zstd_err)?;
            Self::from_reader(display, BufReader::new(decoder))
        } else {
            Self::from_reader(display, BufReader::new(file))
        }
    }

    /// Parses an owned MTJ buffer, decompressing a zstd frame when present.
    pub fn from_bytes(path: impl Into<String>, bytes: &[u8]) -> Result<Self, TelemetryFormatError> {
        if bytes.starts_with(&ZSTD_MAGIC) {
            let decoder = zstd::Decoder::new(bytes).map_err(zstd_err)?;
            Self::from_reader(path.into(), BufReader::new(decoder))
        } else {
            Self::from_reader(path.into(), BufReader::new(bytes))
        }
    }

    /// Format-neutral summary. Laps come from the document, not a rescan.
    pub fn metadata(&self) -> FileMetadata {
        self.metadata_impl()
    }

    /// Shared body for the inherent and trait `metadata` so the trait override
    /// (used by `Box<dyn TelemetrySource>`) does not shadow and recurse into
    /// the inherent method.
    pub(super) fn metadata_impl(&self) -> FileMetadata {
        let mut metadata = read_source_metadata(self);
        if let Some(hash) = self.schema_hash {
            metadata.schema_hash = hash;
        }
        metadata.utc_start_ns = self.utc_start_ns;
        metadata.timezone = self.timezone.clone();
        metadata
    }

    /// Unix-epoch nanoseconds (UTC) at file `t = 0`.
    pub fn utc_start_ns(&self) -> Option<u64> {
        self.utc_start_ns
    }

    /// IANA timezone stamped on the header. Empty when unknown.
    pub fn timezone(&self) -> &str {
        &self.timezone
    }

    /// Lattice quantum declared by the header.
    pub fn quantum_ns(&self) -> u64 {
        self.quantum_ns
    }

    /// Lattice origin declared by the header.
    pub fn origin_ns(&self) -> u64 {
        self.origin_ns
    }

    /// Exclusive file-relative duration declared by the header.
    pub fn duration_ns(&self) -> u64 {
        self.duration_ns
    }

    /// True when this document is an MTX sidecar, not a full recording.
    pub fn is_extension(&self) -> bool {
        self.extension
    }

    /// MTX folders in file order. Empty on a full recording.
    pub fn sidecar_groups(&self) -> &[SidecarGroup] {
        &self.sidecar_groups
    }

    /// Default visibility of each sample channel, aligned with [`TelemetrySource::channels`].
    pub fn channel_visible(&self) -> &[bool] {
        &self.channel_visible
    }

    /// Spans in file order.
    pub fn spans(&self) -> &[Span] {
        &self.spans
    }

    /// Appends `extension` channels onto this recording, joined by
    /// integer nanoseconds (the primary key).
    ///
    /// File-relative nanoseconds are the default. If both documents stamp
    /// `utc`, sidecar times shift by `host_file = ext_file + ext.utc −
    /// host.utc`. `clk`/`abs` is not a join key. Duplicate names and a
    /// disagreeing host `hash` are errors.
    pub fn attach(&self, extension: &Self) -> Result<Self, TelemetryFormatError> {
        if !extension.extension {
            return Err(invalid("attach requires an mtx extension"));
        }
        if self.extension {
            return Err(invalid("cannot attach onto an extension"));
        }
        if extension.sidecar_groups.is_empty() {
            return Err(invalid("mtx extension has no groups"));
        }
        let mut channel_shifts = vec![0; extension.channels.len()];
        let mut span_shifts = vec![0; extension.spans.len()];
        for group in &extension.sidecar_groups {
            if let (Some(host_hash), Some(ext_hash)) = (self.schema_hash, group.schema_hash) {
                if host_hash != ext_hash {
                    return Err(invalid(
                        "extension hash does not match the host schema hash",
                    ));
                }
            }
            let shift = join_shift_ns(self, group.header.utc_start_ns);
            channel_shifts[group.channel_range.clone()].fill(shift);
            span_shifts[group.span_range.clone()].fill(shift);
        }
        let mut names: std::collections::BTreeSet<String> =
            self.channels.iter().map(|ch| ch.name.clone()).collect();
        let mut channels = self.channels.clone();
        let mut values = self.values.clone();
        let mut channel_visible = self.channel_visible.clone();
        if channel_visible.len() < channels.len() {
            channel_visible.resize(channels.len(), true);
        }
        let mut channel_labels = self.channel_labels.clone();
        if channel_labels.len() < channels.len() {
            channel_labels.resize(channels.len(), Vec::new());
        }
        let mut channel_display = self.channel_display.clone();
        if channel_display.len() < channels.len() {
            channel_display.resize(channels.len(), ChannelDisplay::trace());
        }
        let mut duration_ns = self.duration_ns;
        let mut quantum_ns = self.quantum_ns;
        for (index, (channel, series)) in
            extension.channels.iter().zip(&extension.values).enumerate()
        {
            if !names.insert(channel.name.clone()) {
                return Err(invalid(format!(
                    "extension channel {} already exists on the host",
                    channel.name
                )));
            }
            let shift = channel_shifts[index];
            let shifted = shift_channel(channel, shift)?;
            duration_ns = duration_ns.max(shifted.duration_ns);
            if let Some(period) = shifted.first_period_ns() {
                quantum_ns = gcd(quantum_ns, period);
                quantum_ns = gcd(quantum_ns, shifted.chunks[0].time_base_ns);
            }
            channels.push(shifted);
            values.push(series.clone());
            channel_visible.push(
                extension
                    .channel_visible
                    .get(index)
                    .copied()
                    .unwrap_or(true),
            );
            let display = extension
                .channel_display
                .get(index)
                .cloned()
                .unwrap_or_default();
            let mut labels = if display.plot.is_trace() {
                extension
                    .channel_labels
                    .get(index)
                    .cloned()
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            for label in &mut labels {
                let time = i128::from(label.time_ns) + shift;
                if time < 0 {
                    return Err(invalid(format!(
                        "extension label on {} starts before host t=0",
                        channel.name
                    )));
                }
                label.time_ns =
                    u64::try_from(time).map_err(|_| invalid("extension time overflow"))?;
            }
            channel_labels.push(labels);
            channel_display.push(display);
        }
        if quantum_ns == 0 {
            quantum_ns = self.quantum_ns;
        }
        let mut spans = self.spans.clone();
        for (index, span) in extension.spans.iter().enumerate() {
            let shifted = shift_span(span, span_shifts[index])?;
            duration_ns = duration_ns.max(shifted.end_ns);
            spans.push(shifted);
        }
        let mut attached = self.clone();
        attached.channels = channels;
        attached.values = values;
        attached.channel_visible = channel_visible;
        attached.channel_labels = channel_labels;
        attached.channel_display = channel_display;
        attached.spans = spans;
        attached.duration_ns = duration_ns;
        attached.quantum_ns = quantum_ns;
        Ok(attached)
    }
}
/// True when `path` should be treated as an MTJ document, compressed or not.
pub fn is_jsonl_path(path: impl AsRef<Path>) -> bool {
    jsonl_suffix(path.as_ref()).is_some()
}

/// True when `path` names a zstd-compressed MTJ document.
pub fn is_jsonl_zstd_path(path: impl AsRef<Path>) -> bool {
    matches!(
        jsonl_suffix(path.as_ref()),
        Some(JsonlSuffix::Zstd | JsonlSuffix::ExtZstd)
    )
}

/// True when `path` names an MTX extension document.
pub fn is_jsonl_ext_path(path: impl AsRef<Path>) -> bool {
    matches!(
        jsonl_suffix(path.as_ref()),
        Some(JsonlSuffix::Ext | JsonlSuffix::ExtZstd)
    )
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JsonlSuffix {
    Plain,
    Zstd,
    Ext,
    ExtZstd,
}
fn jsonl_suffix(path: &Path) -> Option<JsonlSuffix> {
    let name = path.file_name()?.to_str()?.to_ascii_lowercase();
    // Strict suffix table, checked longest-first so a more-specific extension
    // wins over a shorter suffix it ends with (e.g. `.telemetry.ext.jsonl.zstd`
    // before `.ext.jsonl.zstd`). No substring matching: a name must actually
    // end with the suffix.
    const EXT_ZSTD: &[&str] = &[
        ".telemetry.ext.jsonl.zstd",
        ".telemetry.ext.jsonl.zst",
        ".ext.jsonl.zstd",
        ".ext.jsonl.zst",
        ".mtx.jsonl.zstd",
        ".mtx.jsonl.zst",
    ];
    const EXT: &[&str] = &[
        ".telemetry.ext.jsonl",
        ".telemetry.mtx.jsonl",
        ".ext.jsonl",
        ".mtx.jsonl",
    ];
    const ZSTD: &[&str] = &[
        ".telemetry.jsonl.zstd",
        ".telemetry.jsonl.zst",
        ".jsonl.zstd",
        ".jsonl.zst",
        ".mtj.zstd",
        ".mtj.zst",
    ];
    const PLAIN: &[&str] = &[".telemetry.jsonl", ".jsonl", ".mtj"];
    if EXT_ZSTD.iter().any(|suffix| name.ends_with(suffix)) {
        Some(JsonlSuffix::ExtZstd)
    } else if EXT.iter().any(|suffix| name.ends_with(suffix)) {
        Some(JsonlSuffix::Ext)
    } else if ZSTD.iter().any(|suffix| name.ends_with(suffix)) {
        Some(JsonlSuffix::Zstd)
    } else if PLAIN.iter().any(|suffix| name.ends_with(suffix)) {
        Some(JsonlSuffix::Plain)
    } else {
        None
    }
}
fn starts_with_zstd(file: &mut File) -> Result<bool, TelemetryFormatError> {
    let mut magic = [0u8; 4];
    let read = file.read(&mut magic)?;
    file.seek(SeekFrom::Start(0))?;
    Ok(read == 4 && magic == ZSTD_MAGIC)
}
pub(super) fn zstd_err(err: std::io::Error) -> TelemetryFormatError {
    TelemetryFormatError::Invalid(format!("zstd: {err}"))
}
/// True when `timezone` is a real IANA zone, validated through core placement
/// (which owns the `jiff` dependency) rather than this crate.
pub(super) fn valid_iana_timezone(timezone: &str) -> bool {
    motorsport_telemetry_core::placement::civil_ns_to_utc_ns(0, timezone).is_some()
}
