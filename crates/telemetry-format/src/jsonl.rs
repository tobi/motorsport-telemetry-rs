//! Motorsport Telemetry JSONL (MTJ): compact, time-aligned interchange.
//!
//! The normative rules live in `JSONL.md`. This module is a conforming
//! reader and writer.

use crate::write::TelemetryFormatError;
use motorsport_telemetry_core::{
    read_source_metadata, schema_hash, AbsoluteTimeRange, AppliedPass, Channel, Chunk,
    FileMetadata, LapMetadata, SampleType, SourceIdentity, SourceLapMetadata, SourceOrigin,
    TelemetrySource, UnitSource, VideoFileRef,
};
use serde_json::{Map, Number, Value};
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

/// zstd frame magic (`0x28B52FFD`).
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];

/// Default zstd level for compressed MTJ documents. This one goes to 11.
pub const JSONL_ZSTD_LEVEL: i32 = 11;

/// Document version written and accepted by this module.
pub const JSONL_VERSION: u16 = 1;

/// Extension (`mtx`) document version written and accepted by this module.
pub const JSONL_EXT_VERSION: u16 = 1;

pub use motorsport_telemetry_core::{Span, SpanPrimary};

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

/// Recommended lattice quantum when a recording has no regular channels.
const DEFAULT_QUANTUM_NS: u64 = 1_000_000;

/// Jitter allowed when deciding that a native channel is already regular.
const ALIGN_JITTER_NS: u64 = 2_000_000;

/// An opened MTJ document.
#[derive(Debug, Clone)]
pub struct JsonlRecording {
    path: String,
    source_format: String,
    source_path: String,
    identity: SourceIdentity,
    clock: Option<AbsoluteTimeRange>,
    utc_start_ns: Option<u64>,
    timezone: String,
    laps: Vec<LapMetadata>,
    channels: Vec<Channel>,
    values: Vec<Vec<f64>>,
    quantum_ns: u64,
    origin_ns: u64,
    duration_ns: u64,
    schema_hash: Option<u64>,
    extension: bool,
    sidecar: Option<SidecarHeader>,
    channel_visible: Vec<bool>,
    spans: Vec<Span>,
    passes: Vec<AppliedPass>,
    videos: Vec<VideoFileRef>,
    video_times: Vec<u64>,
    video_offset_ns: Option<i128>,
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

    /// MTX group header. `None` on a full recording.
    pub fn sidecar(&self) -> Option<&SidecarHeader> {
        self.sidecar.as_ref()
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
        if let (Some(host_hash), Some(ext_hash)) = (self.schema_hash, extension.schema_hash) {
            if host_hash != ext_hash {
                return Err(invalid(
                    "extension hash does not match the host schema hash",
                ));
            }
        }
        let shift = join_shift_ns(self, extension)?;
        let mut names: std::collections::BTreeSet<String> =
            self.channels.iter().map(|ch| ch.name.clone()).collect();
        let mut channels = self.channels.clone();
        let mut values = self.values.clone();
        let mut channel_visible = self.channel_visible.clone();
        if channel_visible.len() < channels.len() {
            channel_visible.resize(channels.len(), true);
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
        }
        if quantum_ns == 0 {
            quantum_ns = self.quantum_ns;
        }
        let mut spans = self.spans.clone();
        for span in &extension.spans {
            let shifted = shift_span(span, shift)?;
            duration_ns = duration_ns.max(shifted.end_ns);
            spans.push(shifted);
        }
        let mut attached = self.clone();
        attached.channels = channels;
        attached.values = values;
        attached.channel_visible = channel_visible;
        attached.spans = spans;
        attached.duration_ns = duration_ns;
        attached.quantum_ns = quantum_ns;
        Ok(attached)
    }

    fn from_reader(path: String, reader: impl BufRead) -> Result<Self, TelemetryFormatError> {
        let mut lines = reader.lines();
        let header_line = next_record(&mut lines, "header")?;
        let header = parse_json(&header_line)?;
        let header = header
            .as_object()
            .ok_or_else(|| invalid("header must be a JSON object"))?;
        let has_mtj = header.contains_key("mtj");
        let has_mtx = header.contains_key("mtx");
        if has_mtj && has_mtx {
            return Err(invalid("header cannot contain both mtj and mtx"));
        }
        let extension = has_mtx;
        let version_key = if extension { "mtx" } else { "mtj" };
        let version = int_field(header, version_key)?
            .ok_or_else(|| invalid(format!("header is missing {version_key}")))?;
        let expected = if extension {
            u64::from(JSONL_EXT_VERSION)
        } else {
            u64::from(JSONL_VERSION)
        };
        if version != expected {
            return Err(invalid(format!(
                "unsupported {version_key} version {version}"
            )));
        }
        let quantum_ns = int_field(header, "q")?.ok_or_else(|| invalid("header is missing q"))?;
        if quantum_ns == 0 {
            return Err(invalid("q must be greater than 0"));
        }
        let duration_ns =
            int_field(header, "dur")?.ok_or_else(|| invalid("header is missing dur"))?;
        let origin_ns = int_field(header, "o")?.unwrap_or(0);
        if origin_ns % quantum_ns != 0 {
            return Err(invalid("o is not on the time lattice"));
        }
        if duration_ns < origin_ns || (duration_ns - origin_ns) % quantum_ns != 0 {
            return Err(invalid("dur is not on the time lattice"));
        }

        let identity = SourceIdentity {
            driver: string_field(header, "drv"),
            vehicle: string_field(header, "veh"),
            venue: string_field(header, "ven"),
            event: string_field(header, "evt"),
            session: string_field(header, "ses"),
            date: string_field(header, "date"),
            time: string_field(header, "time"),
        };
        let session_hint = string_field(header, "hint");
        let clock = match (
            string_field(header, "clk"),
            int_field(header, "abs")?,
            int_field(header, "abe")?,
        ) {
            (name, Some(start_ns), end_ns) if !name.is_empty() => Some(AbsoluteTimeRange {
                clock: name,
                start_ns,
                end_ns: end_ns.unwrap_or(start_ns.saturating_add(duration_ns)),
                session_hint,
            }),
            _ => None,
        };
        let timezone = string_field(header, "tz");
        let utc_start_ns = int_field(header, "utc")?;
        let sidecar = if extension {
            let name = header
                .get("n")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("mtx header is missing n"))?;
            if name.is_empty() {
                return Err(invalid("mtx header n must be non-empty"));
            }
            let utc = utc_start_ns.ok_or_else(|| invalid("mtx header is missing utc"))?;
            if utc == 0 {
                return Err(invalid(
                    "mtx header utc must be Unix-epoch nanoseconds at t=0",
                ));
            }
            if timezone.is_empty() {
                return Err(invalid("mtx header is missing tz"));
            }
            if jiff::tz::TimeZone::get(&timezone).is_err() {
                return Err(invalid(format!(
                    "mtx header tz is not an IANA timezone: {timezone}"
                )));
            }
            Some(SidecarHeader {
                name: name.to_owned(),
                visible: parse_vis(header, true, "mtx header")?,
                right: parse_right(header.get("r"))?,
                utc_start_ns: utc,
                timezone: timezone.clone(),
            })
        } else {
            if !timezone.is_empty() && jiff::tz::TimeZone::get(&timezone).is_err() {
                return Err(invalid(format!(
                    "header tz is not an IANA timezone: {timezone}"
                )));
            }
            None
        };
        let source_format = string_field(header, "src");
        let source_path = string_field(header, "srcp");
        let passes = parse_passes(header)?;
        let (videos, video_times, video_offset_ns) = parse_videos(header, extension)?;
        let schema_hash = match string_field(header, "hash") {
            hash if hash.is_empty() => None,
            hash => u64::from_str_radix(&hash, 16)
                .map_err(|_| invalid("hash must be 16-digit lowercase hex"))
                .map(Some)?,
        };

        let laps = if extension {
            Vec::new()
        } else {
            let laps_line = next_record(&mut lines, "laps")?;
            parse_laps(&parse_json(&laps_line)?, quantum_ns)?
        };

        let mut channels = Vec::new();
        let mut values = Vec::new();
        let mut channel_visible = Vec::new();
        let mut spans = Vec::new();
        let mut names = std::collections::BTreeSet::new();
        let mut channel_index = 0u32;
        for line in lines {
            let line = line?;
            if line.is_empty() {
                return Err(invalid("blank lines are not allowed"));
            }
            let record = parse_json(&line)?;
            let object = record
                .as_object()
                .ok_or_else(|| invalid("record must be a JSON object"))?;
            match record_kind(object)? {
                RecordKind::Channel => {
                    let parsed = parse_channel(
                        object,
                        origin_ns,
                        quantum_ns,
                        duration_ns,
                        channel_index,
                        extension,
                    )?;
                    if !names.insert(parsed.channel.name.clone()) {
                        return Err(invalid(format!(
                            "duplicate channel name {}",
                            parsed.channel.name
                        )));
                    }
                    channels.push(parsed.channel);
                    values.push(parsed.values);
                    channel_visible.push(parsed.visible);
                    channel_index += 1;
                }
                RecordKind::Span => {
                    spans.push(parse_span(object, quantum_ns, extension)?);
                }
            }
        }

        Ok(Self {
            path,
            source_format,
            source_path,
            identity,
            clock,
            utc_start_ns,
            timezone,
            laps,
            channels,
            values,
            quantum_ns,
            origin_ns,
            duration_ns,
            schema_hash,
            extension,
            passes,
            sidecar,
            channel_visible,
            spans,
            videos,
            video_times,
            video_offset_ns,
        })
    }
}

/// Writes an MTJ document from any [`TelemetrySource`].
///
/// Compression is on: the file is a zstd frame at [`JSONL_ZSTD_LEVEL`].
/// Only regular, lattice-aligned channels are emitted. Irregular streams are
/// dropped rather than given per-sample timestamps.
pub fn write_jsonl_from_source(
    source: &dyn TelemetrySource,
    dest: impl AsRef<Path>,
) -> Result<(), TelemetryFormatError> {
    write_jsonl_from_source_with(source, dest, true)
}

/// Writes an MTJ document, with `compress` defaulting to on via
/// [`write_jsonl_from_source`].
///
/// When `compress` is true the payload is a zstd frame at [`JSONL_ZSTD_LEVEL`],
/// regardless of the destination suffix. When false, raw UTF-8 JSONL is written.
pub fn write_jsonl_from_source_with(
    source: &dyn TelemetrySource,
    dest: impl AsRef<Path>,
    compress: bool,
) -> Result<(), TelemetryFormatError> {
    let dest = dest.as_ref();
    let file = File::create(dest).map_err(TelemetryFormatError::from)?;
    if compress {
        let mut encoder =
            zstd::Encoder::new(BufWriter::new(file), JSONL_ZSTD_LEVEL).map_err(zstd_err)?;
        write_jsonl_document(source, &mut encoder, false)?;
        encoder.finish().map_err(zstd_err)?;
        Ok(())
    } else {
        write_jsonl_document(source, BufWriter::new(file), false)
    }
}

/// Writes an MTX extension from any [`TelemetrySource`].
///
/// Compression is on at [`JSONL_ZSTD_LEVEL`]. The document has an `mtx`
/// header and channel lines only — no laps.
pub fn write_jsonl_extension_from_source(
    source: &dyn TelemetrySource,
    dest: impl AsRef<Path>,
) -> Result<(), TelemetryFormatError> {
    write_jsonl_extension_from_source_with(source, dest, true)
}

/// Writes an MTX extension, with `compress` defaulting to on via
/// [`write_jsonl_extension_from_source`].
pub fn write_jsonl_extension_from_source_with(
    source: &dyn TelemetrySource,
    dest: impl AsRef<Path>,
    compress: bool,
) -> Result<(), TelemetryFormatError> {
    let dest = dest.as_ref();
    let file = File::create(dest).map_err(TelemetryFormatError::from)?;
    if compress {
        let mut encoder =
            zstd::Encoder::new(BufWriter::new(file), JSONL_ZSTD_LEVEL).map_err(zstd_err)?;
        write_jsonl_document(source, &mut encoder, true)?;
        encoder.finish().map_err(zstd_err)?;
        Ok(())
    } else {
        write_jsonl_document(source, BufWriter::new(file), true)
    }
}

/// Writes an MTX sidecar of spans (no sample channels).
///
/// Compression is on at [`JSONL_ZSTD_LEVEL`].
pub fn write_jsonl_timeline(
    dest: impl AsRef<Path>,
    header: &SidecarHeader,
    quantum_ns: u64,
    duration_ns: u64,
    spans: &[Span],
) -> Result<(), TelemetryFormatError> {
    write_jsonl_timeline_with(dest, header, quantum_ns, duration_ns, spans, true)
}

/// Writes a span sidecar, with `compress` defaulting to on via
/// [`write_jsonl_timeline`].
pub fn write_jsonl_timeline_with(
    dest: impl AsRef<Path>,
    header: &SidecarHeader,
    quantum_ns: u64,
    duration_ns: u64,
    spans: &[Span],
    compress: bool,
) -> Result<(), TelemetryFormatError> {
    if header.name.is_empty() {
        return Err(invalid("mtx header n must be non-empty"));
    }
    if header.utc_start_ns == 0 {
        return Err(invalid(
            "mtx header utc must be Unix-epoch nanoseconds at t=0",
        ));
    }
    if header.timezone.is_empty() {
        return Err(invalid("mtx header tz is required"));
    }
    if jiff::tz::TimeZone::get(&header.timezone).is_err() {
        return Err(invalid(format!(
            "mtx header tz is not an IANA timezone: {}",
            header.timezone
        )));
    }
    if quantum_ns == 0 {
        return Err(invalid("q must be greater than 0"));
    }
    let mut dur = duration_ns;
    for span in spans {
        dur = dur.max(span.end_ns);
    }
    if dur % quantum_ns != 0 {
        dur = snap_up(dur, quantum_ns);
    }
    let dest = dest.as_ref();
    let file = File::create(dest).map_err(TelemetryFormatError::from)?;
    if compress {
        let mut encoder =
            zstd::Encoder::new(BufWriter::new(file), JSONL_ZSTD_LEVEL).map_err(zstd_err)?;
        write_timeline_document(&mut encoder, header, quantum_ns, dur, spans)?;
        encoder.finish().map_err(zstd_err)?;
        Ok(())
    } else {
        write_timeline_document(BufWriter::new(file), header, quantum_ns, dur, spans)
    }
}

fn write_timeline_document(
    mut writer: impl Write,
    header: &SidecarHeader,
    quantum_ns: u64,
    duration_ns: u64,
    spans: &[Span],
) -> Result<(), TelemetryFormatError> {
    write_sidecar_header(&mut writer, header, quantum_ns, 0, duration_ns, None)?;
    writer.write_all(b"\n")?;
    for span in spans {
        write_span(&mut writer, span)?;
        writer.write_all(b"\n")?;
    }
    writer.flush()?;
    Ok(())
}

/// Writes an MTJ recording document to any `Write`.
pub fn write_jsonl_to(
    source: &dyn TelemetrySource,
    writer: impl Write,
) -> Result<(), TelemetryFormatError> {
    write_jsonl_document(source, writer, false)
}

fn write_jsonl_document(
    source: &dyn TelemetrySource,
    mut writer: impl Write,
    extension: bool,
) -> Result<(), TelemetryFormatError> {
    let mut metadata = read_source_metadata(source);
    let timezone = crate::placement::resolve_timezone(source);
    metadata.timezone = timezone.clone();
    metadata.utc_start_ns = source
        .utc_start_ns()
        .or_else(|| crate::placement::utc_from_metadata(&metadata, &timezone));
    if extension {
        if metadata.utc_start_ns.is_none() {
            return Err(invalid(
                "mtx requires utc start-of-file (Unix-epoch nanoseconds at t=0)",
            ));
        }
        if metadata.timezone.is_empty() {
            return Err(invalid(
                "mtx requires tz (IANA timezone, e.g. America/New_York)",
            ));
        }
    }
    let mut aligned = Vec::new();
    for (index, channel) in source.channels().iter().enumerate() {
        if let Some(series) = collect_aligned(source, index, channel) {
            aligned.push(series);
        }
    }

    let origin_ns = 0u64;
    let mut quantum_ns = 0u64;
    for series in &aligned {
        quantum_ns = gcd(quantum_ns, series.period_ns);
        quantum_ns = gcd(quantum_ns, series.t0_ns.saturating_sub(origin_ns));
    }
    if quantum_ns == 0 {
        quantum_ns = DEFAULT_QUANTUM_NS;
    }

    let mut duration_ns = snap_up(metadata.duration_ns, quantum_ns).max(origin_ns);
    for series in &aligned {
        duration_ns = duration_ns.max(series.end_ns());
    }

    let laps = if extension {
        Vec::new()
    } else {
        let laps = snap_laps(&metadata.laps, quantum_ns);
        for lap in &laps {
            duration_ns = duration_ns.max(snap_up(lap.end_ns, quantum_ns));
        }
        laps
    };
    let spans = snap_spans(source.spans(), quantum_ns);
    for span in &spans {
        duration_ns = duration_ns.max(span.end_ns);
    }
    if duration_ns < origin_ns || (duration_ns - origin_ns) % quantum_ns != 0 {
        duration_ns = snap_up(duration_ns.max(origin_ns), quantum_ns);
    }

    if extension {
        let sidecar = sidecar_header_from_source(source, &metadata);
        write_sidecar_header(
            &mut writer,
            &sidecar,
            quantum_ns,
            origin_ns,
            duration_ns,
            Some(&metadata),
        )?;
        writer.write_all(b"\n")?;
    } else {
        write_header(
            &mut writer,
            &metadata,
            source,
            quantum_ns,
            origin_ns,
            duration_ns,
        )?;
        writer.write_all(b"\n")?;
        write_laps(&mut writer, &laps)?;
        writer.write_all(b"\n")?;
    }
    for series in &aligned {
        let vis = if extension {
            Some(series.visible)
        } else if !series.visible {
            Some(false)
        } else {
            None
        };
        write_channel(&mut writer, series, origin_ns, vis)?;
        writer.write_all(b"\n")?;
    }
    for span in &spans {
        write_span(&mut writer, span)?;
        writer.write_all(b"\n")?;
    }
    writer.flush()?;
    Ok(())
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
    if name.contains(".ext.jsonl.zstd")
        || name.contains(".ext.jsonl.zst")
        || name.contains(".mtx.jsonl.zstd")
        || name.contains(".mtx.jsonl.zst")
        || name.ends_with(".ext.jsonl.zstd")
        || name.ends_with(".mtx.jsonl.zstd")
    {
        Some(JsonlSuffix::ExtZstd)
    } else if name.contains(".ext.jsonl") || name.contains(".mtx.jsonl") {
        Some(JsonlSuffix::Ext)
    } else if name.ends_with(".telemetry.jsonl.zstd")
        || name.ends_with(".telemetry.jsonl.zst")
        || name.ends_with(".jsonl.zstd")
        || name.ends_with(".jsonl.zst")
        || name.ends_with(".mtj.zstd")
        || name.ends_with(".mtj.zst")
    {
        Some(JsonlSuffix::Zstd)
    } else if name.ends_with(".telemetry.jsonl")
        || name.ends_with(".jsonl")
        || name.ends_with(".mtj")
    {
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

fn zstd_err(err: std::io::Error) -> TelemetryFormatError {
    TelemetryFormatError::Invalid(format!("zstd: {err}"))
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

    fn applied_passes(&self) -> &[AppliedPass] {
        &self.passes
    }

    fn source_origin(&self) -> Option<SourceOrigin> {
        (!self.source_format.is_empty() || !self.source_path.is_empty()).then(|| SourceOrigin {
            format: self.source_format.clone(),
            path: self.source_path.clone(),
        })
    }

    fn decode(&self, channel_index: usize, _chunk_index: usize, local_index: u64) -> f64 {
        self.values[channel_index][local_index as usize]
    }

    fn sample_at(&self, channel_index: usize, time_ns: u64, linear: bool) -> Option<f64> {
        let channel = self.channels.get(channel_index)?;
        if time_ns < channel.chunks.first()?.time_base_ns || time_ns >= channel.duration_ns {
            return None;
        }
        let chunk_index = channel.chunks.partition_point(|chunk| {
            chunk
                .time_base_ns
                .saturating_add(chunk.sample_count.saturating_mul(chunk.sample_period_ns))
                <= time_ns
        });
        let chunk = channel.chunks.get(chunk_index)?;
        let sample = (time_ns.saturating_sub(chunk.time_base_ns) / chunk.sample_period_ns)
            .min(chunk.sample_count - 1);
        let a = self.decode(channel_index, chunk_index, sample);
        if !a.is_finite() {
            return None;
        }
        if !linear || channel.uses_step_interpolation() {
            return Some(a);
        }
        if sample + 1 >= chunk.sample_count {
            return Some(a);
        }
        let b = self.decode(channel_index, chunk_index, sample + 1);
        if !b.is_finite() {
            return Some(a);
        }
        let interval = chunk.sample_period_ns;
        if interval == 0 {
            return Some(a);
        }
        let fraction =
            time_ns.saturating_sub(chunk.time_base_ns + sample * interval) as f64 / interval as f64;
        Some(a + (b - a) * fraction)
    }

    fn identity(&self) -> SourceIdentity {
        self.identity.clone()
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
}

struct AlignedSeries {
    name: String,
    unit: String,
    t0_ns: u64,
    period_ns: u64,
    values: Vec<Option<f64>>,
    visible: bool,
}

impl AlignedSeries {
    fn end_ns(&self) -> u64 {
        self.t0_ns + self.values.len() as u64 * self.period_ns
    }
}

struct ParsedChannel {
    channel: Channel,
    values: Vec<f64>,
    visible: bool,
}

fn collect_aligned(
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
            let expected = chunk.time_base_ns + local * period_ns;
            if actual.abs_diff(expected) > jitter {
                return None;
            }
        }
    }
    let t0_ns = channel.chunks[0].time_base_ns;
    let last = {
        let chunk = channel.chunks.last()?;
        chunk.time_base_ns + chunk.sample_count.saturating_sub(1) * period_ns
    };
    if last < t0_ns {
        return None;
    }
    let count = ((last - t0_ns) / period_ns + 1) as usize;
    let mut values = vec![None; count];
    for (chunk_index, chunk) in channel.chunks.iter().enumerate() {
        for local in 0..chunk.sample_count {
            let time = chunk.time_base_ns + local * period_ns;
            let slot = ((time - t0_ns) / period_ns) as usize;
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
    })
}

fn snap_spans(spans: &[Span], quantum_ns: u64) -> Vec<Span> {
    if quantum_ns == 0 {
        return spans.to_vec();
    }
    spans
        .iter()
        .map(|span| {
            let start_ns = snap_nearest(span.start_ns, quantum_ns);
            let mut end_ns = snap_nearest(span.end_ns, quantum_ns);
            if end_ns <= start_ns {
                end_ns = start_ns.saturating_add(quantum_ns);
            }
            Span {
                name: span.name.clone(),
                start_ns,
                end_ns,
                visible: span.visible,
                color: span.color.clone(),
                primary: span.primary.clone(),
                meta: span.meta.clone(),
            }
        })
        .collect()
}

fn snap_laps(laps: &[LapMetadata], quantum_ns: u64) -> Vec<LapMetadata> {
    laps.iter()
        .map(|lap| {
            let mut start_ns = snap_nearest(lap.start_ns, quantum_ns);
            let mut end_ns = snap_nearest(lap.end_ns, quantum_ns);
            if end_ns <= start_ns {
                end_ns = start_ns.saturating_add(quantum_ns);
            }
            if start_ns == end_ns {
                start_ns = 0;
                end_ns = quantum_ns;
            }
            LapMetadata {
                number: lap.number,
                start_ns,
                end_ns,
                duration_ns: end_ns - start_ns,
                complete: lap.complete,
                first_video_frame: lap.first_video_frame,
            }
        })
        .collect()
}

fn write_header(
    writer: &mut impl Write,
    metadata: &FileMetadata,
    source: &dyn TelemetrySource,
    quantum_ns: u64,
    origin_ns: u64,
    duration_ns: u64,
) -> Result<(), TelemetryFormatError> {
    write!(
        writer,
        "{{\"mtj\":{JSONL_VERSION},\"q\":{quantum_ns},\"dur\":{duration_ns}"
    )?;
    if origin_ns != 0 {
        write!(writer, ",\"o\":{origin_ns}")?;
    }
    // The original vendor identity: for converted artifacts this is what
    // the chain started from, not the immediate input.
    let src = metadata.source_format.as_str();
    if !src.is_empty() && src != "jsonl" {
        writer.write_all(b",\"src\":")?;
        write_json_string(writer, src)?;
    }
    let origin_path = source
        .source_origin()
        .map(|origin| origin.path)
        .filter(|path| !path.is_empty())
        .unwrap_or_else(|| match source.format() {
            // These are containers of conversions, never origins themselves.
            "jsonl" | "telemetry" => String::new(),
            _ => source.path().to_owned(),
        });
    if !origin_path.is_empty() {
        writer.write_all(b",\"srcp\":")?;
        write_json_string(writer, &origin_path)?;
    }
    write_opt_string(writer, "drv", &metadata.identity.driver)?;
    write_opt_string(writer, "veh", &metadata.identity.vehicle)?;
    write_opt_string(writer, "ven", &metadata.identity.venue)?;
    write_opt_string(writer, "evt", &metadata.identity.event)?;
    write_opt_string(writer, "ses", &metadata.identity.session)?;
    write_opt_string(writer, "date", &metadata.identity.date)?;
    write_opt_string(writer, "time", &metadata.identity.time)?;
    write_placement_fields(writer, metadata.utc_start_ns, &metadata.timezone)?;
    write_clock_fields(writer, metadata)?;
    if let Some(hint) = metadata
        .session_key
        .as_deref()
        .and_then(|key| key.rsplit_once(':').map(|(hint, _)| hint))
    {
        if !hint.is_empty() {
            write_opt_string(writer, "hint", hint)?;
        }
    }
    write_videos(writer, source)?;
    write_passes(writer, &metadata.passes)?;
    write!(writer, ",\"hash\":\"{:016x}\"}}", schema_hash(source))?;
    Ok(())
}

/// Writes the applied-pass provenance into an MTJ header:
/// `"passes":[{"n":name,"v":version,"p":{key:value},"in":[..],"out":[..]}]`.
fn write_passes(
    writer: &mut impl Write,
    passes: &[AppliedPass],
) -> Result<(), TelemetryFormatError> {
    if passes.is_empty() {
        return Ok(());
    }
    writer.write_all(b",\"passes\":[")?;
    for (index, pass) in passes.iter().enumerate() {
        if index > 0 {
            writer.write_all(b",")?;
        }
        writer.write_all(b"{\"n\":")?;
        write_json_string(writer, &pass.name)?;
        write!(writer, ",\"v\":{}", pass.version)?;
        if !pass.params.is_empty() {
            writer.write_all(b",\"p\":{")?;
            for (position, (key, value)) in pass.params.iter().enumerate() {
                if position > 0 {
                    writer.write_all(b",")?;
                }
                write_json_string(writer, key)?;
                writer.write_all(b":")?;
                write_json_string(writer, value)?;
            }
            writer.write_all(b"}")?;
        }
        for (key, names) in [("in", &pass.inputs), ("out", &pass.outputs)] {
            if names.is_empty() {
                continue;
            }
            write!(writer, ",\"{key}\":[")?;
            for (position, name) in names.iter().enumerate() {
                if position > 0 {
                    writer.write_all(b",")?;
                }
                write_json_string(writer, name)?;
            }
            writer.write_all(b"]")?;
        }
        writer.write_all(b"}")?;
    }
    writer.write_all(b"]")?;
    Ok(())
}

/// Writes the optional video-linkage header keys: `vo` (recording-level
/// presentation offset), `vf` (linked video files), and `vpts` (the
/// presentation-order frame timestamp table). Uses the same
/// [`crate::write::linked_videos`] collection as the native catalog so both
/// formats stamp identical linkage. Sidecar documents never call this: video
/// belongs to the host recording.
fn write_videos(
    writer: &mut impl Write,
    source: &dyn TelemetrySource,
) -> Result<(), TelemetryFormatError> {
    if let Some(offset) = source.video_presentation_offset_ns() {
        write!(writer, ",\"vo\":{offset}")?;
    }
    let videos = crate::write::linked_videos(source);
    if videos.is_empty() {
        return Ok(());
    }
    writer.write_all(b",\"vf\":[")?;
    for (position, video) in videos.iter().enumerate() {
        if position > 0 {
            writer.write_all(b",")?;
        }
        writer.write_all(b"{\"n\":")?;
        write_json_string(writer, &video.filename)?;
        write!(
            writer,
            ",\"i\":{},\"fc\":{}",
            video.index, video.frame_count
        )?;
        if let Some(hash) = &video.blake3 {
            writer.write_all(b",\"b3\":\"")?;
            for byte in hash {
                write!(writer, "{byte:02x}")?;
            }
            writer.write_all(b"\"")?;
        }
        if let Some(offset) = video.presentation_offset_ns {
            write!(writer, ",\"po\":{offset}")?;
        }
        writer.write_all(b"}")?;
    }
    writer.write_all(b"]")?;
    if let Some(times) = source.video_presentation_times_ns() {
        if !times.is_empty() {
            writer.write_all(b",\"vpts\":[")?;
            for (position, time) in times.iter().enumerate() {
                if position > 0 {
                    writer.write_all(b",")?;
                }
                write!(writer, "{time}")?;
            }
            writer.write_all(b"]")?;
        }
    }
    Ok(())
}

/// Parsed video-linkage header keys: file references, the frame timestamp
/// table, and the recording-level presentation offset.
type ParsedVideos = (Vec<VideoFileRef>, Vec<u64>, Option<i128>);

/// Parses the optional video-linkage header keys back into file references,
/// the frame timestamp table, and the recording-level presentation offset.
///
/// Sidecar (`mtx`) documents reject all three keys, `vpts` requires `vf`
/// (otherwise a native rewrite would have to invent a file reference), and
/// the timestamp table must be non-decreasing because readers binary-search
/// it in presentation order.
fn parse_videos(
    header: &Map<String, Value>,
    extension: bool,
) -> Result<ParsedVideos, TelemetryFormatError> {
    if extension {
        for key in ["vo", "vf", "vpts"] {
            if header.contains_key(key) {
                return Err(invalid(format!(
                    "mtx sidecars cannot carry video linkage ({key}); video belongs to the host recording"
                )));
            }
        }
        return Ok((Vec::new(), Vec::new(), None));
    }
    let video_offset_ns = match header.get("vo") {
        None => None,
        Some(value) => Some(i128::from(
            json_i64(value).ok_or_else(|| invalid("vo must be an integer"))?,
        )),
    };
    let videos = match header.get("vf") {
        None => Vec::new(),
        Some(value) => {
            let entries = value
                .as_array()
                .ok_or_else(|| invalid("vf must be an array"))?;
            if entries.is_empty() {
                return Err(invalid("vf must not be empty"));
            }
            let mut videos = Vec::with_capacity(entries.len());
            for entry in entries {
                let object = entry
                    .as_object()
                    .ok_or_else(|| invalid("vf entries must be objects"))?;
                let filename = object.get("n").and_then(Value::as_str).unwrap_or_default();
                if filename.is_empty() {
                    return Err(invalid("vf entry is missing n"));
                }
                let index = int_field(object, "i")?
                    .ok_or_else(|| invalid("vf entry is missing i"))
                    .and_then(|index| {
                        u32::try_from(index).map_err(|_| invalid("vf entry i does not fit u32"))
                    })?;
                let frame_count = int_field(object, "fc")?.unwrap_or(0);
                let blake3 = match object.get("b3").and_then(Value::as_str) {
                    None => None,
                    Some(hex) => Some(decode_blake3_hex(hex)?),
                };
                let presentation_offset_ns = match object.get("po") {
                    None => None,
                    Some(value) => Some(i128::from(
                        json_i64(value).ok_or_else(|| invalid("vf entry po must be an integer"))?,
                    )),
                };
                videos.push(VideoFileRef {
                    filename: filename.to_owned(),
                    index,
                    blake3,
                    frame_count,
                    presentation_offset_ns,
                });
            }
            videos
        }
    };
    let video_times = match header.get("vpts") {
        None => Vec::new(),
        Some(value) => {
            if videos.is_empty() {
                return Err(invalid("vpts requires vf"));
            }
            let entries = value
                .as_array()
                .ok_or_else(|| invalid("vpts must be an array"))?;
            if entries.is_empty() {
                return Err(invalid("vpts must not be empty"));
            }
            let mut times = Vec::with_capacity(entries.len());
            for entry in entries {
                let stamp = json_u64(entry)?
                    .ok_or_else(|| invalid("vpts entries must be non-negative integers"))?;
                if times.last().is_some_and(|last| stamp < *last) {
                    return Err(invalid("vpts must be non-decreasing"));
                }
                times.push(stamp);
            }
            times
        }
    };
    Ok((videos, video_times, video_offset_ns))
}

/// Decodes a 64-digit hex string into a BLAKE3-256 digest.
fn decode_blake3_hex(hex: &str) -> Result<[u8; 32], TelemetryFormatError> {
    if hex.len() != 64 {
        return Err(invalid("vf entry b3 must be 64 hex digits"));
    }
    let mut digest = [0u8; 32];
    for (index, slot) in digest.iter_mut().enumerate() {
        let pair = hex
            .get(index * 2..index * 2 + 2)
            .ok_or_else(|| invalid("vf entry b3 must be 64 hex digits"))?;
        *slot = u8::from_str_radix(pair, 16)
            .map_err(|_| invalid("vf entry b3 must be 64 hex digits"))?;
    }
    Ok(digest)
}

/// Parses the optional `passes` header key back into provenance records.
fn parse_passes(
    header: &serde_json::Map<String, Value>,
) -> Result<Vec<AppliedPass>, TelemetryFormatError> {
    let Some(value) = header.get("passes") else {
        return Ok(Vec::new());
    };
    let list = value
        .as_array()
        .ok_or_else(|| invalid("passes must be an array"))?;
    let mut passes = Vec::with_capacity(list.len());
    for entry in list {
        let object = entry
            .as_object()
            .ok_or_else(|| invalid("passes entries must be objects"))?;
        let name = object
            .get("n")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| invalid("pass entry is missing n"))?;
        let version = object
            .get("v")
            .and_then(Value::as_u64)
            .ok_or_else(|| invalid("pass entry is missing v"))?;
        let mut params = Vec::new();
        if let Some(map) = object.get("p") {
            let map = map
                .as_object()
                .ok_or_else(|| invalid("pass p must be an object"))?;
            for (key, value) in map {
                let value = value
                    .as_str()
                    .ok_or_else(|| invalid("pass p values must be strings"))?;
                params.push((key.clone(), value.to_owned()));
            }
        }
        let names = |key: &str| -> Result<Vec<String>, TelemetryFormatError> {
            match object.get(key) {
                None => Ok(Vec::new()),
                Some(value) => value
                    .as_array()
                    .ok_or_else(|| invalid(format!("pass {key} must be an array")))?
                    .iter()
                    .map(|entry| {
                        entry
                            .as_str()
                            .map(str::to_owned)
                            .ok_or_else(|| invalid(format!("pass {key} entries must be strings")))
                    })
                    .collect(),
            }
        };
        passes.push(AppliedPass {
            name: name.to_owned(),
            version: version as u32,
            params,
            inputs: names("in")?,
            outputs: names("out")?,
        });
    }
    Ok(passes)
}

fn sidecar_header_from_source(
    source: &dyn TelemetrySource,
    metadata: &FileMetadata,
) -> SidecarHeader {
    let name = [
        &metadata.identity.event,
        &metadata.identity.session,
        &metadata.identity.venue,
    ]
    .into_iter()
    .find(|value| !value.is_empty())
    .cloned()
    .or_else(|| {
        std::path::Path::new(source.path())
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
    })
    .filter(|value| !value.is_empty())
    .unwrap_or_else(|| "Extension".into());
    SidecarHeader {
        name,
        visible: true,
        right: Vec::new(),
        utc_start_ns: metadata.utc_start_ns.unwrap_or(0),
        timezone: metadata.timezone.clone(),
    }
}

fn write_sidecar_header(
    writer: &mut impl Write,
    sidecar: &SidecarHeader,
    quantum_ns: u64,
    origin_ns: u64,
    duration_ns: u64,
    metadata: Option<&FileMetadata>,
) -> Result<(), TelemetryFormatError> {
    write!(writer, "{{\"mtx\":{JSONL_EXT_VERSION},\"n\":")?;
    write_json_string(writer, &sidecar.name)?;
    write!(
        writer,
        ",\"q\":{quantum_ns},\"dur\":{duration_ns},\"vis\":{}",
        u8::from(sidecar.visible)
    )?;
    if origin_ns != 0 {
        write!(writer, ",\"o\":{origin_ns}")?;
    }
    if !sidecar.right.is_empty() {
        writer.write_all(b",\"r\":[")?;
        for (index, chrome) in sidecar.right.iter().enumerate() {
            if index > 0 {
                writer.write_all(b",")?;
            }
            match chrome {
                HeaderChrome::Text(text) => {
                    writer.write_all(b"{\"t\":")?;
                    write_json_string(writer, text)?;
                    writer.write_all(b"}")?;
                }
                HeaderChrome::Pill { label, value } => {
                    writer.write_all(b"{\"p\":[")?;
                    write_json_string(writer, label)?;
                    writer.write_all(b",")?;
                    write_json_string(writer, value)?;
                    writer.write_all(b"]}")?;
                }
            }
        }
        writer.write_all(b"]")?;
    }
    write_placement_fields(writer, Some(sidecar.utc_start_ns), &sidecar.timezone)?;
    if let Some(metadata) = metadata {
        write_clock_fields(writer, metadata)?;
    }
    writer.write_all(b"}")?;
    Ok(())
}

fn write_placement_fields(
    writer: &mut impl Write,
    utc_start_ns: Option<u64>,
    timezone: &str,
) -> Result<(), TelemetryFormatError> {
    if let Some(utc) = utc_start_ns {
        write!(writer, ",\"utc\":{utc}")?;
    }
    write_opt_string(writer, "tz", timezone)?;
    Ok(())
}

fn write_clock_fields(
    writer: &mut impl Write,
    metadata: &FileMetadata,
) -> Result<(), TelemetryFormatError> {
    let Some(clock) = metadata.absolute_clock.as_deref() else {
        return Ok(());
    };
    if clock.is_empty() {
        return Ok(());
    }
    // abs is the clock reading at file t=0 (clock_offset_ns), not the first
    // GPS sample if that sample is later.
    let Some(abs) = metadata
        .clock_offset_ns
        .and_then(|offset| u64::try_from(offset).ok())
        .or(metadata.absolute_start_ns)
    else {
        return Ok(());
    };
    writer.write_all(b",\"clk\":")?;
    write_json_string(writer, clock)?;
    write!(writer, ",\"abs\":{abs}")?;
    if let Some(end) = metadata.absolute_end_ns {
        write!(writer, ",\"abe\":{end}")?;
    }
    Ok(())
}

fn join_shift_ns(
    host: &JsonlRecording,
    ext: &JsonlRecording,
) -> Result<i128, TelemetryFormatError> {
    if let (Some(host_utc), Some(ext_utc)) = (host.utc_start_ns, ext.utc_start_ns) {
        return Ok(i128::from(ext_utc) - i128::from(host_utc));
    }
    Ok(0)
}

fn shift_channel(channel: &Channel, shift_ns: i128) -> Result<Channel, TelemetryFormatError> {
    if shift_ns == 0 {
        return Ok(channel.clone());
    }
    let mut shifted = channel.clone();
    for chunk in &mut shifted.chunks {
        let time = i128::from(chunk.time_base_ns) + shift_ns;
        if time < 0 {
            return Err(invalid(format!(
                "extension channel {} starts before host t=0",
                channel.name
            )));
        }
        chunk.time_base_ns = u64::try_from(time).map_err(|_| invalid("extension time overflow"))?;
    }
    let end = i128::from(channel.duration_ns) + shift_ns;
    if end < 0 {
        return Err(invalid(format!(
            "extension channel {} ends before host t=0",
            channel.name
        )));
    }
    shifted.duration_ns = u64::try_from(end).map_err(|_| invalid("extension time overflow"))?;
    Ok(shifted)
}

fn shift_span(span: &Span, shift_ns: i128) -> Result<Span, TelemetryFormatError> {
    if shift_ns == 0 {
        return Ok(span.clone());
    }
    let start = i128::from(span.start_ns) + shift_ns;
    let end = i128::from(span.end_ns) + shift_ns;
    if start < 0 {
        return Err(invalid(format!(
            "extension span {} starts before host t=0",
            span_label(span)
        )));
    }
    Ok(Span {
        name: span.name.clone(),
        start_ns: u64::try_from(start).map_err(|_| invalid("extension time overflow"))?,
        end_ns: u64::try_from(end).map_err(|_| invalid("extension time overflow"))?,
        visible: span.visible,
        color: span.color.clone(),
        primary: span.primary.clone(),
        meta: span.meta.clone(),
    })
}

fn span_label(span: &Span) -> &str {
    if !span.name.is_empty() {
        &span.name
    } else if !span.primary.title.is_empty() {
        &span.primary.title
    } else {
        "span"
    }
}

fn write_laps(writer: &mut impl Write, laps: &[LapMetadata]) -> Result<(), TelemetryFormatError> {
    writer.write_all(b"[")?;
    for (index, lap) in laps.iter().enumerate() {
        if index > 0 {
            writer.write_all(b",")?;
        }
        write!(
            writer,
            "[{},{},{},{}",
            lap.number,
            lap.start_ns,
            lap.end_ns,
            u8::from(lap.complete)
        )?;
        if let Some(frame) = lap.first_video_frame {
            write!(writer, ",{frame}")?;
        }
        writer.write_all(b"]")?;
    }
    writer.write_all(b"]")?;
    Ok(())
}

fn write_channel(
    writer: &mut impl Write,
    series: &AlignedSeries,
    origin_ns: u64,
    visible: Option<bool>,
) -> Result<(), TelemetryFormatError> {
    writer.write_all(b"{\"n\":")?;
    write_json_string(writer, &series.name)?;
    writer.write_all(b",\"hz\":")?;
    write_hz(writer, series.period_ns)?;
    if !series.unit.is_empty() {
        writer.write_all(b",\"u\":")?;
        write_json_string(writer, &series.unit)?;
    }
    if let Some(visible) = visible {
        write!(writer, ",\"vis\":{}", u8::from(visible))?;
    }
    writer.write_all(b",\"v\":[")?;
    for (index, value) in series.values.iter().enumerate() {
        if index > 0 {
            writer.write_all(b",")?;
        }
        match value {
            Some(number) => write_number(writer, *number)?,
            None => writer.write_all(b"null")?,
        }
    }
    writer.write_all(b"]")?;
    if series.t0_ns != origin_ns {
        write!(writer, ",\"t0\":{}", series.t0_ns)?;
    }
    writer.write_all(b"}")?;
    Ok(())
}

fn write_hz(writer: &mut impl Write, period_ns: u64) -> Result<(), TelemetryFormatError> {
    if period_ns > 0 && 1_000_000_000 % period_ns == 0 {
        write!(writer, "{}", 1_000_000_000 / period_ns)?;
    } else {
        write_number(writer, 1e9 / period_ns as f64)?;
    }
    Ok(())
}

fn write_number(writer: &mut impl Write, value: f64) -> Result<(), TelemetryFormatError> {
    if !value.is_finite() {
        writer.write_all(b"null")?;
        return Ok(());
    }
    if value == 0.0 {
        writer.write_all(b"0")?;
        return Ok(());
    }
    if value.fract() == 0.0 && value >= i64::MIN as f64 && value <= i64::MAX as f64 {
        let integer = value as i64;
        if integer as f64 == value {
            write!(writer, "{integer}")?;
            return Ok(());
        }
    }
    let as_f32 = value as f32;
    let rendered = if as_f32.is_finite() && as_f32 as f64 == value {
        format!("{as_f32}")
    } else {
        format!("{value}")
    };
    writer.write_all(rendered.as_bytes())?;
    Ok(())
}

fn write_opt_string(
    writer: &mut impl Write,
    key: &str,
    value: &str,
) -> Result<(), TelemetryFormatError> {
    if value.is_empty() {
        return Ok(());
    }
    write!(writer, ",\"{key}\":")?;
    write_json_string(writer, value)
}

fn write_json_string(writer: &mut impl Write, value: &str) -> Result<(), TelemetryFormatError> {
    writer.write_all(b"\"")?;
    for character in value.chars() {
        match character {
            '"' => writer.write_all(b"\\\"")?,
            '\\' => writer.write_all(b"\\\\")?,
            '\n' => writer.write_all(b"\\n")?,
            '\r' => writer.write_all(b"\\r")?,
            '\t' => writer.write_all(b"\\t")?,
            character if character.is_control() => {
                write!(writer, "\\u{:04x}", u32::from(character))?;
            }
            character => {
                let mut buf = [0u8; 4];
                writer.write_all(character.encode_utf8(&mut buf).as_bytes())?;
            }
        }
    }
    writer.write_all(b"\"")?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecordKind {
    Channel,
    Span,
}

fn record_kind(object: &Map<String, Value>) -> Result<RecordKind, TelemetryFormatError> {
    match object.get("k").and_then(Value::as_str).unwrap_or("") {
        "" | "c" => Ok(RecordKind::Channel),
        "s" => Ok(RecordKind::Span),
        "f" => Err(invalid(
            "folder records are not used; the sidecar header is the group",
        )),
        other => Err(invalid(format!("unknown record kind {other}"))),
    }
}

fn parse_vis(
    object: &Map<String, Value>,
    required: bool,
    what: &str,
) -> Result<bool, TelemetryFormatError> {
    match object.get("vis") {
        None if required => Err(invalid(format!("{what} is missing vis"))),
        None => Ok(true),
        Some(value) => json_complete(value),
    }
}

fn parse_right(value: Option<&Value>) -> Result<Vec<HeaderChrome>, TelemetryFormatError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let rows = value
        .as_array()
        .ok_or_else(|| invalid("header r must be an array"))?;
    let mut right = Vec::with_capacity(rows.len());
    for row in rows {
        let object = row
            .as_object()
            .ok_or_else(|| invalid("each r element must be an object"))?;
        if let Some(text) = object.get("t").and_then(Value::as_str) {
            right.push(HeaderChrome::Text(text.to_owned()));
        } else if let Some(pair) = object.get("p").and_then(Value::as_array) {
            if pair.len() < 2 {
                return Err(invalid("pill must be [label, value]"));
            }
            let label = pair[0]
                .as_str()
                .ok_or_else(|| invalid("pill label must be a string"))?;
            let value = pair[1]
                .as_str()
                .ok_or_else(|| invalid("pill value must be a string"))?;
            right.push(HeaderChrome::Pill {
                label: label.to_owned(),
                value: value.to_owned(),
            });
        } else {
            return Err(invalid("r element must have t or p"));
        }
    }
    Ok(right)
}

fn parse_span(
    object: &Map<String, Value>,
    quantum_ns: u64,
    require_vis: bool,
) -> Result<Span, TelemetryFormatError> {
    let start_ns = int_field(object, "s")?.ok_or_else(|| invalid("span is missing s"))?;
    let end_ns = int_field(object, "e")?.ok_or_else(|| invalid("span is missing e"))?;
    if end_ns <= start_ns {
        return Err(invalid("span end must be greater than start"));
    }
    if start_ns % quantum_ns != 0 || end_ns % quantum_ns != 0 {
        return Err(invalid("span boundary is not on the time lattice"));
    }
    let color = string_field(object, "c");
    if !color.is_empty() {
        validate_color(&color)?;
    }
    let (title, subtitle) = match object.get("p").and_then(Value::as_object) {
        Some(primary) => (string_field(primary, "title"), string_field(primary, "sub")),
        None => (String::new(), String::new()),
    };
    let meta = parse_meta(object.get("m"))?;
    Ok(Span {
        name: string_field(object, "n"),
        start_ns,
        end_ns,
        visible: parse_vis(object, require_vis, "span")?,
        color,
        primary: SpanPrimary { title, subtitle },
        meta,
    })
}

fn parse_meta(value: Option<&Value>) -> Result<Vec<(String, String)>, TelemetryFormatError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let rows = value
        .as_array()
        .ok_or_else(|| invalid("span m must be an array of [name, value]"))?;
    let mut meta = Vec::with_capacity(rows.len());
    for row in rows {
        let pair = row
            .as_array()
            .ok_or_else(|| invalid("each meta entry must be [name, value]"))?;
        if pair.len() < 2 {
            return Err(invalid("each meta entry must be [name, value]"));
        }
        let key = pair[0]
            .as_str()
            .ok_or_else(|| invalid("meta name must be a string"))?;
        let text = pair[1]
            .as_str()
            .ok_or_else(|| invalid("meta value must be a string"))?;
        meta.push((key.to_owned(), text.to_owned()));
    }
    Ok(meta)
}

fn validate_color(color: &str) -> Result<(), TelemetryFormatError> {
    let hex = color
        .strip_prefix('#')
        .ok_or_else(|| invalid("span color must be #RRGGBB"))?;
    if hex.len() != 6 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid("span color must be #RRGGBB"));
    }
    Ok(())
}

fn write_span(writer: &mut impl Write, span: &Span) -> Result<(), TelemetryFormatError> {
    write!(writer, "{{\"k\":\"s\"")?;
    if !span.name.is_empty() {
        writer.write_all(b",\"n\":")?;
        write_json_string(writer, &span.name)?;
    }
    write!(
        writer,
        ",\"s\":{},\"e\":{},\"vis\":{}",
        span.start_ns,
        span.end_ns,
        u8::from(span.visible)
    )?;
    if !span.color.is_empty() {
        writer.write_all(b",\"c\":")?;
        write_json_string(writer, &span.color)?;
    }
    if !span.primary.title.is_empty() || !span.primary.subtitle.is_empty() {
        writer.write_all(b",\"p\":{")?;
        let mut first = true;
        if !span.primary.title.is_empty() {
            writer.write_all(b"\"title\":")?;
            write_json_string(writer, &span.primary.title)?;
            first = false;
        }
        if !span.primary.subtitle.is_empty() {
            if !first {
                writer.write_all(b",")?;
            }
            writer.write_all(b"\"sub\":")?;
            write_json_string(writer, &span.primary.subtitle)?;
        }
        writer.write_all(b"}")?;
    }
    if !span.meta.is_empty() {
        writer.write_all(b",\"m\":[")?;
        for (index, (key, value)) in span.meta.iter().enumerate() {
            if index > 0 {
                writer.write_all(b",")?;
            }
            writer.write_all(b"[")?;
            write_json_string(writer, key)?;
            writer.write_all(b",")?;
            write_json_string(writer, value)?;
            writer.write_all(b"]")?;
        }
        writer.write_all(b"]")?;
    }
    writer.write_all(b"}")?;
    Ok(())
}

fn parse_laps(value: &Value, quantum_ns: u64) -> Result<Vec<LapMetadata>, TelemetryFormatError> {
    let rows = value
        .as_array()
        .ok_or_else(|| invalid("laps record must be a JSON array"))?;
    let mut laps = Vec::with_capacity(rows.len());
    let mut previous_start = None;
    for row in rows {
        let fields = row
            .as_array()
            .ok_or_else(|| invalid("each lap must be an array"))?;
        if fields.len() < 4 {
            return Err(invalid("lap tuple must be [number, start, end, complete]"));
        }
        let number =
            json_i64(&fields[0]).ok_or_else(|| invalid("lap number must be an integer"))?;
        let start_ns =
            json_u64(&fields[1])?.ok_or_else(|| invalid("lap start must be an integer"))?;
        let end_ns = json_u64(&fields[2])?.ok_or_else(|| invalid("lap end must be an integer"))?;
        if end_ns <= start_ns {
            return Err(invalid("lap end must be greater than start"));
        }
        if start_ns % quantum_ns != 0 || end_ns % quantum_ns != 0 {
            return Err(invalid("lap boundary is not on the time lattice"));
        }
        if previous_start.is_some_and(|before| start_ns < before) {
            return Err(invalid("laps must be in non-decreasing start order"));
        }
        previous_start = Some(start_ns);
        laps.push(LapMetadata {
            number,
            start_ns,
            end_ns,
            duration_ns: end_ns - start_ns,
            complete: json_complete(&fields[3])?,
            first_video_frame: fields.get(4).map(json_u64).transpose()?.flatten(),
        });
    }
    Ok(laps)
}

fn parse_channel(
    record: &Map<String, Value>,
    origin_ns: u64,
    quantum_ns: u64,
    duration_ns: u64,
    id: u32,
    require_vis: bool,
) -> Result<ParsedChannel, TelemetryFormatError> {
    let name = record
        .get("n")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("channel is missing n"))?;
    if name.is_empty() {
        return Err(invalid("channel name must be non-empty"));
    }
    let hz = record
        .get("hz")
        .and_then(Value::as_f64)
        .ok_or_else(|| invalid(format!("channel {name} is missing hz")))?;
    let period_ns = period_ns_from_hz(hz)
        .ok_or_else(|| invalid(format!("channel {name} has a non-positive hz")))?;
    if period_ns % quantum_ns != 0 {
        return Err(invalid(format!(
            "channel {name} period {period_ns} is not a multiple of q={quantum_ns}"
        )));
    }
    let t0_ns = match record.get("t0") {
        Some(value) => json_u64(value)?.ok_or_else(|| invalid(format!("channel {name} t0")))?,
        None => origin_ns,
    };
    if t0_ns < origin_ns || t0_ns % quantum_ns != 0 {
        return Err(invalid(format!(
            "channel {name} t0 is not on the time lattice"
        )));
    }
    let unit = record
        .get("u")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let raw_values = record
        .get("v")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid(format!("channel {name} is missing v")))?;
    if raw_values.is_empty() {
        return Err(invalid(format!("channel {name} has no samples")));
    }
    let mut values = Vec::with_capacity(raw_values.len());
    for value in raw_values {
        values.push(match value {
            Value::Null => f64::NAN,
            Value::Number(number) => number
                .as_f64()
                .ok_or_else(|| invalid(format!("channel {name} has a non-finite value")))?,
            _ => {
                return Err(invalid(format!(
                    "channel {name} values must be numbers or null"
                )))
            }
        });
    }
    let sample_count = values.len() as u64;
    let channel_end = t0_ns.saturating_add(sample_count.saturating_mul(period_ns));
    if channel_end > duration_ns.saturating_add(period_ns) {
        return Err(invalid(format!(
            "channel {name} extends beyond dur + one period"
        )));
    }
    Ok(ParsedChannel {
        channel: Channel {
            id,
            name: name.to_owned(),
            unit,
            unit_source: if record
                .get("u")
                .and_then(Value::as_str)
                .is_some_and(|u| !u.is_empty())
            {
                UnitSource::Declared
            } else {
                UnitSource::Unknown
            },
            sample_type: SampleType::F64,
            chunks: vec![Chunk {
                sample_period_ns: period_ns,
                sample_count,
                data_ptr: 0,
                sample_base: 0,
                time_base_ns: t0_ns,
            }],
            sample_count,
            duration_ns: channel_end,
        },
        values,
        visible: parse_vis(record, require_vis, &format!("channel {name}"))?,
    })
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

fn next_record(
    lines: &mut impl Iterator<Item = std::io::Result<String>>,
    what: &str,
) -> Result<String, TelemetryFormatError> {
    let line = lines
        .next()
        .ok_or_else(|| invalid(format!("missing {what} record")))??;
    if line.is_empty() {
        return Err(invalid(format!("{what} record is empty")));
    }
    Ok(line)
}

fn parse_json(line: &str) -> Result<Value, TelemetryFormatError> {
    serde_json::from_str(line).map_err(|err| invalid(err.to_string()))
}

fn int_field(object: &Map<String, Value>, key: &str) -> Result<Option<u64>, TelemetryFormatError> {
    match object.get(key) {
        None => Ok(None),
        Some(value) => {
            Ok(Some(json_u64(value)?.ok_or_else(|| {
                invalid(format!("{key} must be an integer"))
            })?))
        }
    }
}

fn string_field(object: &Map<String, Value>, key: &str) -> String {
    object
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

fn json_u64(value: &Value) -> Result<Option<u64>, TelemetryFormatError> {
    match value {
        Value::Number(number) => number
            .as_u64()
            .or_else(|| number.as_i64().and_then(|value| u64::try_from(value).ok()))
            .or_else(|| {
                number.as_f64().and_then(|value| {
                    (value.fract() == 0.0 && value >= 0.0 && value <= u64::MAX as f64)
                        .then_some(value as u64)
                })
            })
            .map(Some)
            .ok_or_else(|| invalid("expected a non-negative integer")),
        Value::Null => Ok(None),
        _ => Err(invalid("expected a non-negative integer")),
    }
}

fn json_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Number(number) => number.as_i64().or_else(|| {
            number.as_f64().and_then(|value| {
                (value.fract() == 0.0 && value >= i64::MIN as f64 && value <= i64::MAX as f64)
                    .then_some(value as i64)
            })
        }),
        _ => None,
    }
}

fn json_complete(value: &Value) -> Result<bool, TelemetryFormatError> {
    match value {
        Value::Bool(flag) => Ok(*flag),
        Value::Number(number) if number.as_u64() == Some(1) || as_one(number) => Ok(true),
        Value::Number(number) if number.as_u64() == Some(0) || as_zero(number) => Ok(false),
        _ => Err(invalid("lap complete must be 0 or 1")),
    }
}

fn as_one(number: &Number) -> bool {
    number.as_i64() == Some(1) || number.as_f64() == Some(1.0)
}

fn as_zero(number: &Number) -> bool {
    number.as_i64() == Some(0) || number.as_f64() == Some(0.0)
}

fn snap_nearest(value: u64, quantum_ns: u64) -> u64 {
    if quantum_ns <= 1 {
        return value;
    }
    let rem = value % quantum_ns;
    if rem * 2 < quantum_ns {
        value - rem
    } else {
        value + (quantum_ns - rem)
    }
}

fn snap_up(value: u64, quantum_ns: u64) -> u64 {
    if quantum_ns <= 1 {
        return value;
    }
    let rem = value % quantum_ns;
    if rem == 0 {
        value
    } else {
        value + (quantum_ns - rem)
    }
}

fn gcd(mut left: u64, mut right: u64) -> u64 {
    while right != 0 {
        let rest = left % right;
        left = right;
        right = rest;
    }
    left
}

fn invalid(message: impl Into<String>) -> TelemetryFormatError {
    TelemetryFormatError::Invalid(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NativeRecording;
    use motorsport_telemetry_core::UnitSource;

    struct TinySource {
        identity: SourceIdentity,
        channels: Vec<Channel>,
        values: Vec<Vec<f64>>,
        laps: Vec<LapMetadata>,
        utc_start_ns: Option<u64>,
        timezone: String,
        videos: Vec<VideoFileRef>,
        video_times: Vec<u64>,
        video_offset_ns: Option<i128>,
    }

    impl TelemetrySource for TinySource {
        fn path(&self) -> &str {
            "tiny"
        }
        fn format(&self) -> &'static str {
            "pds"
        }
        fn channels(&self) -> &[Channel] {
            &self.channels
        }
        fn decode(&self, channel_index: usize, chunk_index: usize, local_index: u64) -> f64 {
            let base = self.channels[channel_index].chunks[chunk_index].sample_base;
            self.values[channel_index][(base + local_index) as usize]
        }
        fn identity(&self) -> SourceIdentity {
            self.identity.clone()
        }
        fn utc_start_ns(&self) -> Option<u64> {
            self.utc_start_ns
        }
        fn timezone(&self) -> String {
            self.timezone.clone()
        }
        fn source_lap_metadata(&self) -> Option<SourceLapMetadata> {
            Some(SourceLapMetadata {
                laps: self.laps.clone(),
                fastest_lap: None,
            })
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
    }

    fn channel(name: &str, unit: &str, period_ns: u64, count: u64, t0: u64) -> Channel {
        Channel {
            id: 1,
            name: name.into(),
            unit: unit.into(),
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
                time_base_ns: t0,
            }],
            sample_count: count,
            duration_ns: t0 + count * period_ns,
        }
    }

    fn tiny() -> TinySource {
        TinySource {
            identity: SourceIdentity {
                driver: "Tobi".into(),
                venue: "Road America".into(),
                ..SourceIdentity::default()
            },
            channels: vec![
                channel("Speed", "km/h", 10_000_000, 4, 0),
                channel("GPS Speed", "m/s", 40_000_000, 1, 0),
            ],
            values: vec![vec![10.0, 11.0, 12.5, 13.0], vec![2.8]],
            utc_start_ns: None,
            timezone: String::new(),
            videos: Vec::new(),
            video_times: Vec::new(),
            video_offset_ns: None,
            laps: vec![LapMetadata {
                number: 1,
                start_ns: 0,
                end_ns: 40_000_000,
                duration_ns: 40_000_000,
                complete: false,
                first_video_frame: None,
            }],
        }
    }

    #[test]
    fn promoted_f32_values_write_short_decimals() {
        let mut short = Vec::new();
        write_number(&mut short, f64::from(0.2f32)).unwrap();
        assert_eq!(String::from_utf8(short).unwrap(), "0.2");
        let mut exact = Vec::new();
        write_number(&mut exact, 1.0 / 3.0).unwrap();
        assert_eq!(String::from_utf8(exact).unwrap(), format!("{}", 1.0 / 3.0));
    }

    #[test]
    fn period_from_integer_hz_is_exact() {
        assert_eq!(period_ns_from_hz(100.0), Some(10_000_000));
        assert_eq!(period_ns_from_hz(25.0), Some(40_000_000));
        assert_eq!(period_ns_from_hz(1.0), Some(1_000_000_000));
    }

    #[test]
    fn writes_header_laps_then_compact_channels() {
        let source = tiny();
        let mut bytes = Vec::new();
        write_jsonl_to(&source, &mut bytes).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 4);
        assert!(lines[0].starts_with("{\"mtj\":1,\"q\":10000000,\"dur\":40000000"));
        assert!(lines[0].contains("\"src\":\"pds\""));
        assert!(lines[0].contains("\"drv\":\"Tobi\""));
        assert!(
            !lines[0].contains(": ") && !lines[0].contains(", "),
            "header has insignificant whitespace: {}",
            lines[0]
        );
        assert_eq!(lines[1], "[[1,0,40000000,0]]");
        assert_eq!(
            lines[2],
            "{\"n\":\"Speed\",\"hz\":100,\"u\":\"km/h\",\"v\":[10,11,12.5,13]}"
        );
        assert_eq!(
            lines[3],
            "{\"n\":\"GPS Speed\",\"hz\":25,\"u\":\"m/s\",\"v\":[2.8]}"
        );
    }

    #[test]
    fn round_trip_preserves_alignment_and_values() {
        let source = tiny();
        let mut bytes = Vec::new();
        write_jsonl_to(&source, &mut bytes).unwrap();
        let opened = JsonlRecording::from_bytes("tiny.jsonl", &bytes).unwrap();
        assert_eq!(opened.quantum_ns(), 10_000_000);
        assert_eq!(opened.format(), "pds");
        assert_eq!(opened.identity().driver, "Tobi");
        assert_eq!(opened.channels().len(), 2);
        assert_eq!(opened.decode(0, 0, 0), 10.0);
        assert_eq!(opened.decode(0, 0, 2), 12.5);
        assert_eq!(opened.sample_time_ns(0, 0, 1), 10_000_000);
        assert_eq!(opened.sample_time_ns(1, 0, 0), 0);
        assert_eq!(opened.metadata().laps[0].end_ns, 40_000_000);
    }

    #[test]
    fn rejects_channel_off_the_lattice() {
        let text = concat!(
            "{\"mtj\":1,\"q\":10000000,\"dur\":40000000}\n",
            "[]\n",
            "{\"n\":\"Speed\",\"hz\":30,\"v\":[1,2,3]}\n",
        );
        let err = JsonlRecording::from_bytes("bad.jsonl", text.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("not a multiple of q"));
    }

    #[test]
    fn video_linkage_round_trips() {
        let mut source = tiny();
        source.videos = vec![VideoFileRef {
            filename: "SCHD0060.MP4".into(),
            index: 1,
            blake3: Some([0xab; 32]),
            frame_count: 4,
            presentation_offset_ns: Some(101_333_333),
        }];
        source.video_times = vec![101_333_333, 134_700_000, 168_066_666, 201_433_333];
        source.video_offset_ns = Some(101_333_333);

        let mut bytes = Vec::new();
        write_jsonl_to(&source, &mut bytes).unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();
        let header = text.lines().next().unwrap();
        assert!(header.contains(",\"vo\":101333333,"));
        assert!(header.contains(&format!(
            ",\"vf\":[{{\"n\":\"SCHD0060.MP4\",\"i\":1,\"fc\":4,\"b3\":\"{}\",\"po\":101333333}}]",
            "ab".repeat(32)
        )));
        assert!(header.contains(",\"vpts\":[101333333,134700000,168066666,201433333]"));
        assert!(header.ends_with('}'));
        assert!(
            header.rfind("\"hash\":").unwrap() > header.rfind("\"vpts\":").unwrap(),
            "hash must stay the last header key"
        );
        // The lap line picks up the first video frame (5th element).
        assert_eq!(text.lines().nth(1).unwrap(), "[[1,0,40000000,0,0]]");

        let opened = JsonlRecording::from_bytes("tiny.mtj", &bytes).unwrap();
        assert_eq!(opened.video_files(), source.videos.as_slice());
        assert_eq!(
            opened.video_presentation_times_ns().unwrap(),
            source.video_times.as_slice()
        );
        assert_eq!(opened.video_presentation_offset_ns(), Some(101_333_333));
        assert_eq!(opened.video_frame_count(), Some(4));
        // Same binary-search semantics as the native reader.
        assert_eq!(opened.video_frame_at(0), Some(0));
        assert_eq!(opened.video_frame_at(40_000_000), Some(1));
        assert_eq!(opened.video_frame_at(u64::MAX / 2), Some(3));
        assert_eq!(opened.metadata().video_frame_count, Some(4));

        // The native hop keeps the linkage bit for bit.
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("tiny.telemetry");
        crate::write_from_source(&opened, &dest).unwrap();
        let native = NativeRecording::open(&dest).unwrap();
        assert_eq!(native.video_files(), source.videos.as_slice());
        assert_eq!(
            native.video_presentation_times_ns().unwrap(),
            source.video_times.as_slice()
        );
        assert_eq!(native.video_presentation_offset_ns(), Some(101_333_333));
        assert_eq!(native.video_frame_at(40_000_000), Some(1));
    }

    #[test]
    fn sidecars_reject_video_linkage() {
        let text = concat!(
            "{\"mtx\":1,\"q\":10000000,\"dur\":40000000,",
            "\"n\":\"tires\",\"vis\":true,\"utc\":1700000000000000000,\"tz\":\"UTC\",",
            "\"vf\":[{\"n\":\"clip.mp4\",\"i\":1,\"fc\":1}]}\n",
        );
        let err = JsonlRecording::from_bytes("bad.mtjx", text.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("video belongs to the host"));
    }

    #[test]
    fn rejects_vpts_without_vf() {
        let text = concat!(
            "{\"mtj\":1,\"q\":10000000,\"dur\":40000000,\"vpts\":[0,1]}\n",
            "[]\n",
        );
        let err = JsonlRecording::from_bytes("bad.mtj", text.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("vpts requires vf"));
    }

    #[test]
    fn rejects_decreasing_vpts() {
        let text = concat!(
            "{\"mtj\":1,\"q\":10000000,\"dur\":40000000,",
            "\"vf\":[{\"n\":\"clip.mp4\",\"i\":1,\"fc\":2}],\"vpts\":[5,4]}\n",
            "[]\n",
        );
        let err = JsonlRecording::from_bytes("bad.mtj", text.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("non-decreasing"));
    }

    #[test]
    fn rejects_unaligned_lap() {
        let text = concat!(
            "{\"mtj\":1,\"q\":10000000,\"dur\":40000000}\n",
            "[[1,1,40000000,0]]\n",
        );
        let err = JsonlRecording::from_bytes("bad.jsonl", text.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("not on the time lattice"));
    }

    #[test]
    fn fills_gaps_with_null_and_keeps_indexes() {
        let mut source = tiny();
        source.channels[0].chunks = vec![
            Chunk {
                sample_period_ns: 10_000_000,
                sample_count: 2,
                data_ptr: 0,
                sample_base: 0,
                time_base_ns: 0,
            },
            Chunk {
                sample_period_ns: 10_000_000,
                sample_count: 1,
                data_ptr: 0,
                sample_base: 2,
                time_base_ns: 30_000_000,
            },
        ];
        source.channels[0].sample_count = 3;
        source.values[0] = vec![10.0, 11.0, 13.0];
        let mut bytes = Vec::new();
        write_jsonl_to(&source, &mut bytes).unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(text.contains("\"v\":[10,11,null,13]"));
        let opened = JsonlRecording::from_bytes("gap.jsonl", &bytes).unwrap();
        assert!(opened.decode(0, 0, 2).is_nan());
        assert_eq!(opened.decode(0, 0, 3), 13.0);
        assert_eq!(opened.sample_time_ns(0, 0, 2), 20_000_000);
    }

    #[test]
    fn drops_irregular_channels() {
        let mut source = tiny();
        source.channels.push(Channel {
            id: 3,
            name: "Beacon".into(),
            unit: String::new(),
            unit_source: UnitSource::Unknown,
            sample_type: SampleType::F64,
            chunks: vec![Chunk {
                sample_period_ns: 0,
                sample_count: 2,
                data_ptr: 0,
                sample_base: 0,
                time_base_ns: 0,
            }],
            sample_count: 2,
            duration_ns: 1,
        });
        source.values.push(vec![1.0, 2.0]);
        let mut bytes = Vec::new();
        write_jsonl_to(&source, &mut bytes).unwrap();
        let opened = JsonlRecording::from_bytes("skip.jsonl", &bytes).unwrap();
        assert_eq!(opened.channels().len(), 2);
        assert!(opened.channels().iter().all(|ch| ch.name != "Beacon"));
    }

    #[test]
    fn recognizes_telemetry_jsonl_and_zstd_names() {
        assert!(is_jsonl_path("run.telemetry.jsonl"));
        assert!(is_jsonl_path("run.telemetry.jsonl.zstd"));
        assert!(is_jsonl_path("run.TELEMETRY.JSONL.ZST"));
        assert!(is_jsonl_zstd_path("run.telemetry.jsonl.zstd"));
        assert!(is_jsonl_zstd_path("run.jsonl.zst"));
        assert!(!is_jsonl_zstd_path("run.telemetry.jsonl"));
        assert!(!is_jsonl_path("run.telemetry"));
        assert!(!is_jsonl_path("run.pds"));
        assert!(is_jsonl_ext_path("run.telemetry.ext.jsonl"));
        assert!(is_jsonl_ext_path("run.telemetry.ext.jsonl.zstd"));
        assert!(is_jsonl_zstd_path("run.telemetry.ext.jsonl.zstd"));
        assert!(!is_jsonl_ext_path("run.telemetry.jsonl"));
    }

    #[test]
    fn write_jsonl_from_source_compresses_at_level_11_by_default() {
        assert_eq!(JSONL_ZSTD_LEVEL, 11);
        let source = tiny();
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("tiny.telemetry.jsonl");
        write_jsonl_from_source(&source, &dest).unwrap();
        let bytes = std::fs::read(&dest).unwrap();
        assert_eq!(&bytes[..4], &ZSTD_MAGIC);
        let opened = JsonlRecording::open(&dest).unwrap();
        assert_eq!(opened.decode(0, 0, 0), 10.0);
        write_jsonl_from_source_with(&source, &dest, false).unwrap();
        let plain = std::fs::read(&dest).unwrap();
        assert_eq!(plain[0], b'{');
    }

    #[test]
    fn zstd_round_trip_is_sniffed_by_magic() {
        let source = tiny();
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("tiny.telemetry.jsonl.zstd");
        write_jsonl_from_source(&source, &dest).unwrap();
        let bytes = std::fs::read(&dest).unwrap();
        assert_eq!(&bytes[..4], &ZSTD_MAGIC);
        assert!(bytes.len() < 400);
        let opened = JsonlRecording::open(&dest).unwrap();
        assert_eq!(opened.decode(0, 0, 0), 10.0);
        assert_eq!(opened.channels()[0].name, "Speed");
        let renamed = dir.path().join("tiny.telemetry.jsonl");
        std::fs::copy(&dest, &renamed).unwrap();
        let sniffed = JsonlRecording::open(&renamed).unwrap();
        assert_eq!(sniffed.decode(0, 0, 2), 12.5);
    }

    #[test]
    fn jsonl_and_zstd_decompress_to_identical_bytes_and_payload() {
        let source = tiny();
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("tiny.telemetry.jsonl");
        let zstd = dir.path().join("tiny.telemetry.jsonl.zstd");
        write_jsonl_from_source_with(&source, &plain, false).unwrap();
        write_jsonl_from_source(&source, &zstd).unwrap();

        let plain_bytes = std::fs::read(&plain).unwrap();
        let mut decoded = Vec::new();
        zstd::Decoder::new(std::fs::File::open(&zstd).unwrap())
            .unwrap()
            .read_to_end(&mut decoded)
            .unwrap();
        assert_eq!(
            decoded, plain_bytes,
            "zstd frame must be the same UTF-8 document"
        );

        let a = JsonlRecording::open(&plain).unwrap();
        let b = JsonlRecording::open(&zstd).unwrap();
        assert_ne!(a.path(), b.path());
        assert_eq!(a.source_format, b.source_format);
        assert_eq!(a.identity, b.identity);
        assert_eq!(a.clock, b.clock);
        assert_eq!(a.utc_start_ns, b.utc_start_ns);
        assert_eq!(a.timezone, b.timezone);
        assert_eq!(a.laps, b.laps);
        assert_eq!(a.quantum_ns, b.quantum_ns);
        assert_eq!(a.origin_ns, b.origin_ns);
        assert_eq!(a.duration_ns, b.duration_ns);
        assert_eq!(a.schema_hash, b.schema_hash);
        assert_eq!(a.channels.len(), b.channels.len());
        for (left, right) in a.channels.iter().zip(&b.channels) {
            assert_eq!(left.name, right.name);
            assert_eq!(left.unit, right.unit);
            assert_eq!(left.sample_count, right.sample_count);
            assert_eq!(left.duration_ns, right.duration_ns);
            assert_eq!(
                left.chunks[0].sample_period_ns,
                right.chunks[0].sample_period_ns
            );
            assert_eq!(left.chunks[0].time_base_ns, right.chunks[0].time_base_ns);
        }
        assert_eq!(a.values.len(), b.values.len());
        for (left, right) in a.values.iter().zip(&b.values) {
            assert_eq!(left.len(), right.len());
            for (l, r) in left.iter().zip(right) {
                assert_eq!(l.to_bits(), r.to_bits());
            }
        }
    }

    #[test]
    fn extension_is_header_then_channels() {
        let text = concat!(
            "{\"mtx\":1,\"n\":\"Ride height\",\"q\":10000000,\"dur\":50000000,\"vis\":1,\"utc\":1700000000000000000,\"tz\":\"America/New_York\"}\n",
            "{\"n\":\"Ride Height FL\",\"hz\":100,\"u\":\"mm\",\"vis\":1,\"v\":[42,41,40,39],\"t0\":10000000}\n",
        );
        let opened =
            JsonlRecording::from_bytes("ride.telemetry.ext.jsonl", text.as_bytes()).unwrap();
        assert!(opened.is_extension());
        assert_eq!(opened.sidecar().unwrap().name, "Ride height");
        assert!(opened.sidecar().unwrap().visible);
        assert!(opened.metadata().laps.is_empty());
        assert_eq!(opened.channels().len(), 1);
        assert_eq!(opened.channel_visible(), [true]);
        assert_eq!(opened.channels()[0].name, "Ride Height FL");
        assert_eq!(opened.sample_time_ns(0, 0, 0), 10_000_000);
        assert_eq!(opened.decode(0, 0, 0), 42.0);
        assert_eq!(opened.decode(0, 0, 3), 39.0);
    }

    #[test]
    fn writes_extension_without_laps() {
        let mut source = tiny();
        source.utc_start_ns = Some(1_700_000_000_000_000_000);
        source.timezone = "America/Chicago".into();
        let mut bytes = Vec::new();
        write_jsonl_document(&source, &mut bytes, true).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        let mut lines = text.lines();
        let header = lines.next().unwrap();
        assert!(header.starts_with("{\"mtx\":1,\"n\":"));
        assert!(header.contains("\"vis\":1"));
        assert!(header.contains("\"utc\":1700000000000000000"));
        assert!(header.contains("\"tz\":\"America/Chicago\""));
        assert!(!header.contains("\"mtj\""));
        let second = lines.next().unwrap();
        assert!(second.contains("\"vis\":1"), "{second}");
    }

    #[test]
    fn attach_joins_on_file_relative_time() {
        let host = JsonlRecording::from_bytes(
            "host.jsonl",
            concat!(
                "{\"mtj\":1,\"q\":10000000,\"dur\":40000000}\n",
                "[]\n",
                "{\"n\":\"Speed\",\"hz\":100,\"u\":\"km/h\",\"v\":[10,11,12,13]}\n",
            )
            .as_bytes(),
        )
        .unwrap();
        let ext = JsonlRecording::from_bytes(
            "extra.telemetry.ext.jsonl",
            concat!(
                "{\"mtx\":1,\"n\":\"Ride\",\"q\":10000000,\"dur\":40000000,\"vis\":1,\"utc\":1700000000000000000,\"tz\":\"America/New_York\"}\n",
                "{\"n\":\"Ride Height FL\",\"hz\":100,\"u\":\"mm\",\"vis\":1,\"v\":[42,41],\"t0\":20000000}\n",
            )
            .as_bytes(),
        )
        .unwrap();
        let merged = host.attach(&ext).unwrap();
        assert!(!merged.is_extension());
        assert_eq!(merged.channels().len(), 2);
        assert_eq!(merged.sample_time_ns(1, 0, 0), 20_000_000);
        assert_eq!(merged.decode(1, 0, 0), 42.0);
        assert_eq!(merged.sample_at(1, 20_000_000, false), Some(42.0));
        assert_eq!(merged.sample_at(1, 0, false), None);
    }

    #[test]
    fn attach_translates_on_utc_nanoseconds() {
        let host = JsonlRecording::from_bytes(
            "host.jsonl",
            concat!(
                "{\"mtj\":1,\"q\":10000000,\"dur\":40000000,\"utc\":1000,\"tz\":\"UTC\"}\n",
                "[]\n",
                "{\"n\":\"Speed\",\"hz\":100,\"v\":[1,2,3,4]}\n",
            )
            .as_bytes(),
        )
        .unwrap();
        let ext = JsonlRecording::from_bytes(
            "extra.telemetry.ext.jsonl",
            concat!(
                "{\"mtx\":1,\"n\":\"Extra group\",\"q\":10000000,\"dur\":20000000,\"vis\":1,\"utc\":20001000,\"tz\":\"UTC\"}\n",
                "{\"n\":\"Extra\",\"hz\":100,\"vis\":1,\"v\":[9,8]}\n",
            )
            .as_bytes(),
        )
        .unwrap();
        let merged = host.attach(&ext).unwrap();
        assert_eq!(merged.sample_time_ns(1, 0, 0), 20_000_000);
        assert_eq!(merged.decode(1, 0, 0), 9.0);
    }

    #[test]
    fn attach_joins_on_utc_ns_not_clk_abs() {
        let host = JsonlRecording::from_bytes(
            "host.jsonl",
            concat!(
                "{\"mtj\":1,\"q\":10000000,\"dur\":40000000,\"utc\":1000,\"tz\":\"UTC\",",
                "\"clk\":\"gps\",\"abs\":1}\n",
                "[]\n",
                "{\"n\":\"Speed\",\"hz\":100,\"v\":[1,2,3,4]}\n",
            )
            .as_bytes(),
        )
        .unwrap();
        let ext = JsonlRecording::from_bytes(
            "extra.telemetry.ext.jsonl",
            concat!(
                "{\"mtx\":1,\"n\":\"Extra group\",\"q\":10000000,\"dur\":20000000,\"vis\":1,",
                "\"utc\":1000,\"tz\":\"UTC\",\"clk\":\"gps\",\"abs\":999999}\n",
                "{\"n\":\"Extra\",\"hz\":100,\"vis\":1,\"v\":[9,8],\"t0\":10000000}\n",
            )
            .as_bytes(),
        )
        .unwrap();
        let merged = host.attach(&ext).unwrap();
        assert_eq!(merged.sample_time_ns(1, 0, 0), 10_000_000);
    }

    #[test]
    fn attach_rejects_duplicate_channel_names() {
        let host = JsonlRecording::from_bytes(
            "host.jsonl",
            concat!(
                "{\"mtj\":1,\"q\":10000000,\"dur\":40000000}\n",
                "[]\n",
                "{\"n\":\"Speed\",\"hz\":100,\"v\":[1,2]}\n",
            )
            .as_bytes(),
        )
        .unwrap();
        let ext = JsonlRecording::from_bytes(
            "extra.telemetry.ext.jsonl",
            concat!(
                "{\"mtx\":1,\"n\":\"Dup\",\"q\":10000000,\"dur\":20000000,\"vis\":1,\"utc\":1700000000000000000,\"tz\":\"UTC\"}\n",
                "{\"n\":\"Speed\",\"hz\":100,\"vis\":0,\"v\":[9,8]}\n",
            )
            .as_bytes(),
        )
        .unwrap();
        let err = host.attach(&ext).unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn parses_sidecar_group_of_stint_spans() {
        let text = concat!(
            "{\"mtx\":1,\"n\":\"Sebring 12H 2025\",\"q\":1000000,\"dur\":12600000000000,\"vis\":0,",
            "\"utc\":1742040000000000000,\"tz\":\"America/New_York\",",
            "\"r\":[{\"t\":\"LMP2 stints during the race\"},{\"p\":[\"Avg lap\",\"1:52.1\"]}]}\n",
            "{\"k\":\"s\",\"n\":\"443-1\",\"s\":0,\"e\":5400000000000,\"vis\":1,\"c\":\"#e11d48\",",
            "\"p\":{\"title\":\"#443\",\"sub\":\"EL · 1:52.1\"},",
            "\"m\":[[\"Laps\",\"28\"],[\"Best\",\"1:50.332\"],[\"Avg\",\"1:52.104\"],[\"License\",\"IMSA\"]]}\n",
            "{\"k\":\"s\",\"n\":\"443-2\",\"s\":5400000000000,\"e\":12600000000000,\"vis\":0,\"c\":\"#2563eb\",",
            "\"p\":{\"title\":\"#443\",\"sub\":\"MB · 1:51.8\"},",
            "\"m\":[[\"Laps\",\"38\"],[\"Best\",\"1:50.110\"],[\"Avg\",\"1:51.804\"],[\"License\",\"IMSA\"]]}\n",
        );
        let opened =
            JsonlRecording::from_bytes("lmp2.telemetry.ext.jsonl", text.as_bytes()).unwrap();
        assert!(opened.is_extension());
        let group = opened.sidecar().unwrap();
        assert_eq!(group.name, "Sebring 12H 2025");
        assert!(!group.visible);
        assert_eq!(
            group.right,
            [
                HeaderChrome::Text("LMP2 stints during the race".into()),
                HeaderChrome::Pill {
                    label: "Avg lap".into(),
                    value: "1:52.1".into()
                }
            ]
        );
        assert!(opened.channels().is_empty());
        assert_eq!(opened.spans().len(), 2);
        let stint = &opened.spans()[0];
        assert!(stint.visible);
        assert!(!opened.spans()[1].visible);
        assert_eq!(stint.primary.title, "#443");
        assert_eq!(stint.primary.subtitle, "EL · 1:52.1");
        assert_eq!(stint.color, "#e11d48");
        assert_eq!(
            stint.meta,
            [
                ("Laps".into(), "28".into()),
                ("Best".into(), "1:50.332".into()),
                ("Avg".into(), "1:52.104".into()),
                ("License".into(), "IMSA".into()),
            ]
        );
        assert_eq!(stint.end_ns - stint.start_ns, 5_400_000_000_000);
    }

    #[test]
    fn timeline_round_trip_preserves_primary_and_meta() {
        let header = SidecarHeader {
            name: "Sebring 12H 2025".into(),
            visible: true,
            right: vec![
                HeaderChrome::Text("LMP2 stints during the race".into()),
                HeaderChrome::Pill {
                    label: "Avg lap".into(),
                    value: "1:52.1".into(),
                },
            ],
            utc_start_ns: 1_742_040_000_000_000_000,
            timezone: "America/New_York".into(),
        };
        let spans = [Span {
            name: "443-1".into(),
            start_ns: 0,
            end_ns: 1_000_000_000,
            visible: true,
            color: "#e11d48".into(),
            primary: SpanPrimary {
                title: "#443".into(),
                subtitle: "EL".into(),
            },
            meta: vec![
                ("Laps".into(), "18".into()),
                ("Total drive time".into(), "1:30:00".into()),
                ("Driver License".into(), "IMSA".into()),
            ],
        }];
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("stints.telemetry.ext.jsonl");
        write_jsonl_timeline_with(&dest, &header, 1_000_000, 0, &spans, false).unwrap();
        let text = std::fs::read_to_string(&dest).unwrap();
        assert!(text.starts_with("{\"mtx\":1,\"n\":\"Sebring 12H 2025\""));
        assert!(text.contains("\"k\":\"s\""));
        assert!(!text.contains("\"k\":\"f\""));
        assert!(text.contains("\"title\":\"#443\""));
        assert!(text.contains("[\"Driver License\",\"IMSA\"]"));
        let opened = JsonlRecording::open(&dest).unwrap();
        assert_eq!(opened.sidecar(), Some(&header));
        assert_eq!(opened.spans(), spans.as_slice());
    }

    #[test]
    fn attach_shifts_span_times_with_utc_ns() {
        let host = JsonlRecording::from_bytes(
            "host.jsonl",
            concat!(
                "{\"mtj\":1,\"q\":1000000,\"dur\":4000000000,\"utc\":1000,\"tz\":\"UTC\"}\n",
                "[]\n",
                "{\"n\":\"Speed\",\"hz\":1,\"v\":[1,2,3,4]}\n",
            )
            .as_bytes(),
        )
        .unwrap();
        let ext = JsonlRecording::from_bytes(
            "stints.telemetry.ext.jsonl",
            concat!(
                "{\"mtx\":1,\"n\":\"Sebring 12H 2025\",\"q\":1000000,\"dur\":2000000,\"vis\":1,\"utc\":2001000,\"tz\":\"UTC\"}\n",
                "{\"k\":\"s\",\"s\":0,\"e\":1000000,\"vis\":1,\"p\":{\"title\":\"#443\"}}\n",
            )
            .as_bytes(),
        )
        .unwrap();
        let merged = host.attach(&ext).unwrap();
        let span = &merged.spans()[0];
        assert_eq!(span.start_ns, 2_000_000);
        assert_eq!(span.end_ns, 3_000_000);
        assert_eq!(span.primary.title, "#443");
    }

    #[test]
    fn rejects_invalid_span_color() {
        let text = concat!(
            "{\"mtx\":1,\"n\":\"Bad\",\"q\":1000000,\"dur\":2000000,\"vis\":1,\"utc\":1700000000000000000,\"tz\":\"UTC\"}\n",
            "{\"k\":\"s\",\"s\":0,\"e\":1000000,\"vis\":1,\"c\":\"red\"}\n",
        );
        let err = JsonlRecording::from_bytes("bad.ext.jsonl", text.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("#RRGGBB"));
    }

    #[test]
    fn rejects_mtx_missing_utc_or_tz() {
        let missing_utc =
            "{\"mtx\":1,\"n\":\"X\",\"q\":1000000,\"dur\":1000000,\"vis\":1,\"tz\":\"UTC\"}\n";
        let err = JsonlRecording::from_bytes("bad.ext.jsonl", missing_utc.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("missing utc"), "{err}");
        let missing_tz =
            "{\"mtx\":1,\"n\":\"X\",\"q\":1000000,\"dur\":1000000,\"vis\":1,\"utc\":1700000000000000000}\n";
        let err = JsonlRecording::from_bytes("bad.ext.jsonl", missing_tz.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("missing tz"), "{err}");
    }

    #[test]
    fn rejects_folder_records() {
        let text = concat!(
            "{\"mtx\":1,\"n\":\"X\",\"q\":1000000,\"dur\":1000000,\"vis\":1,\"utc\":1700000000000000000,\"tz\":\"UTC\"}\n",
            "{\"k\":\"f\",\"n\":\"LMP2\",\"x\":[]}\n",
        );
        let err = JsonlRecording::from_bytes("bad.ext.jsonl", text.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("folder records are not used"));
    }
}
