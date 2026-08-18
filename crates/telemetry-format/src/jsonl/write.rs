//! MTJ/MTX document writers and JSON serialization helpers.

use super::align::{collect_aligned, gcd, snap_laps, snap_spans, snap_up, AlignedSeries};
use super::json::invalid;
use super::{
    valid_iana_timezone, zstd_err, HeaderChrome, JsonlRecording, SidecarHeader, DEFAULT_QUANTUM_NS,
    JSONL_EXT_VERSION, JSONL_VERSION, JSONL_ZSTD_LEVEL,
};
use crate::write::TelemetryFormatError;
use motorsport_telemetry_core::{
    read_source_metadata, schema_hash, AppliedPass, Channel, ChannelDisplay, FileMetadata,
    LapMetadata, Span, SpanMetaValue, TelemetrySource, TIMESPAN_MS,
};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

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
    if !valid_iana_timezone(&header.timezone) {
        return Err(invalid(format!(
            "mtx header tz is not an IANA timezone: {}",
            header.timezone
        )));
    }
    if quantum_ns == 0 {
        return Err(invalid("q must be greater than 0"));
    }
    let spans = snap_spans(spans, quantum_ns)?;
    let mut dur = duration_ns;
    for span in &spans {
        dur = dur.max(span.end_ns);
    }
    if dur % quantum_ns != 0 {
        dur = snap_up(dur, quantum_ns)?;
    }
    let dest = dest.as_ref();
    let file = File::create(dest).map_err(TelemetryFormatError::from)?;
    if compress {
        let mut encoder =
            zstd::Encoder::new(BufWriter::new(file), JSONL_ZSTD_LEVEL).map_err(zstd_err)?;
        write_timeline_document(&mut encoder, header, quantum_ns, dur, &spans)?;
        encoder.finish().map_err(zstd_err)?;
        Ok(())
    } else {
        write_timeline_document(BufWriter::new(file), header, quantum_ns, dur, &spans)
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
pub(super) fn write_jsonl_document(
    source: &dyn TelemetrySource,
    mut writer: impl Write,
    extension: bool,
) -> Result<(), TelemetryFormatError> {
    let mut metadata = read_source_metadata(source);
    let timezone = motorsport_telemetry_core::placement::resolve_timezone(source);
    metadata.timezone = timezone.clone();
    metadata.utc_start_ns = source
        .utc_start_ns()
        .or_else(|| motorsport_telemetry_core::placement::utc_from_metadata(&metadata, &timezone));
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

    let mut duration_ns = snap_up(metadata.duration_ns, quantum_ns)?.max(origin_ns);
    for series in &aligned {
        duration_ns = duration_ns.max(series.end_ns());
    }

    let laps = if extension {
        Vec::new()
    } else {
        let laps = snap_laps(&metadata.laps, quantum_ns)?;
        for lap in &laps {
            duration_ns = duration_ns.max(snap_up(lap.end_ns, quantum_ns)?);
        }
        laps
    };
    let spans = snap_spans(source.spans(), quantum_ns)?;
    for span in &spans {
        duration_ns = duration_ns.max(span.end_ns);
    }
    if duration_ns < origin_ns || (duration_ns - origin_ns) % quantum_ns != 0 {
        duration_ns = snap_up(duration_ns.max(origin_ns), quantum_ns)?;
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
    write_display_fields(writer, &series.display)?;
    if !series.labels.is_empty() {
        writer.write_all(b",\"lbl\":[")?;
        for (index, label) in series.labels.iter().enumerate() {
            if index > 0 {
                writer.write_all(b",")?;
            }
            write!(writer, "[{},", label.time_ns)?;
            write_json_string(writer, &label.text)?;
            writer.write_all(b"]")?;
        }
        writer.write_all(b"]")?;
    }
    writer.write_all(b"}")?;
    Ok(())
}
fn write_display_fields(
    writer: &mut impl Write,
    display: &ChannelDisplay,
) -> Result<(), TelemetryFormatError> {
    if !display.plot.is_trace() {
        writer.write_all(b",\"plt\":")?;
        write_json_string(writer, display.plot.as_str())?;
    }
    match (display.scale_min, display.scale_max) {
        (Some(min), Some(max)) => {
            writer.write_all(b",\"sc\":[")?;
            write_number(writer, min)?;
            writer.write_all(b",")?;
            write_number(writer, max)?;
            writer.write_all(b"]")?;
        }
        (Some(min), None) => {
            writer.write_all(b",\"sc\":[")?;
            write_number(writer, min)?;
            writer.write_all(b"]")?;
        }
        (None, Some(max)) => {
            writer.write_all(b",\"sc\":[null,")?;
            write_number(writer, max)?;
            writer.write_all(b"]")?;
        }
        (None, None) => {}
    }
    if let Some(decimals) = display.decimals {
        write!(writer, ",\"rnd\":{decimals}")?;
    }
    if !display.format.is_empty() {
        writer.write_all(b",\"fmt\":")?;
        write_json_string(writer, &display.format)?;
    }
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
pub(super) fn write_number(
    writer: &mut impl Write,
    value: f64,
) -> Result<(), TelemetryFormatError> {
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
            write_meta_value(writer, value)?;
            writer.write_all(b"]")?;
        }
        writer.write_all(b"]")?;
    }
    writer.write_all(b"}")?;
    Ok(())
}
fn write_meta_value(
    writer: &mut impl Write,
    value: &SpanMetaValue,
) -> Result<(), TelemetryFormatError> {
    match value {
        SpanMetaValue::Text(text) => write_json_string(writer, text),
        SpanMetaValue::TimeMs(ms) => {
            write!(writer, "{{\"v\":{ms},\"u\":\"{}\"}}", TIMESPAN_MS)?;
            Ok(())
        }
    }
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
pub(super) fn join_shift_ns(host: &JsonlRecording, ext_utc: u64) -> i128 {
    host.utc_start_ns
        .map(|host_utc| i128::from(ext_utc) - i128::from(host_utc))
        .unwrap_or(0)
}
pub(super) fn shift_channel(
    channel: &Channel,
    shift_ns: i128,
) -> Result<Channel, TelemetryFormatError> {
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
pub(super) fn shift_span(span: &Span, shift_ns: i128) -> Result<Span, TelemetryFormatError> {
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
