//! JSONL record parser: `from_reader` and per-record decoders.

use super::align::period_ns_from_hz;
use super::json::{
    int_field, invalid, json_complete, json_finite, json_i64, json_u64, next_record, parse_json,
    string_field,
};
use super::{
    valid_iana_timezone, HeaderChrome, JsonlRecording, SidecarGroup, SidecarHeader,
    JSONL_EXT_VERSION, JSONL_VERSION,
};
use crate::write::TelemetryFormatError;
use motorsport_telemetry_core::{
    parse_timespan_ms, timespan_ms_in_range, AbsoluteTimeRange, AppliedPass, Channel,
    ChannelDisplay, ChannelLabel, ChannelPlot, Chunk, LapMetadata, SampleType, SourceIdentity,
    Span, SpanMetaValue, SpanPrimary, UnitSource, VideoFileRef, TIMESPAN_MS,
};
use serde_json::{Map, Number, Value};
use std::io::BufRead;

impl JsonlRecording {
    pub(super) fn from_reader(
        path: String,
        reader: impl BufRead,
    ) -> Result<Self, TelemetryFormatError> {
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
        let (quantum_ns, duration_ns, origin_ns) = parse_group_header(header, "header")?;

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
        let sidecar_header = if extension {
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
            if !valid_iana_timezone(&timezone) {
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
            if !timezone.is_empty() && !valid_iana_timezone(&timezone) {
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
        let schema_hash = parse_schema_hash(header)?;

        let laps = if extension {
            Vec::new()
        } else {
            let laps_line = next_record(&mut lines, "laps")?;
            parse_laps(&parse_json(&laps_line)?, quantum_ns)?
        };

        let mut channels = Vec::new();
        let mut values = Vec::new();
        let mut channel_visible = Vec::new();
        let mut channel_labels = Vec::new();
        let mut channel_display = Vec::new();
        let mut spans = Vec::new();
        let mut names = std::collections::BTreeSet::new();
        let mut channel_index = 0u32;
        let mut sidecar_groups = Vec::new();
        let mut current_group = sidecar_header.map(|header| SidecarGroup {
            header,
            quantum_ns,
            origin_ns,
            duration_ns,
            schema_hash,
            channel_range: 0..0,
            span_range: 0..0,
        });
        for line in lines {
            let line = line?;
            if line.is_empty() {
                return Err(invalid("blank lines are not allowed"));
            }
            let record = parse_json(&line)?;
            let object = record
                .as_object()
                .ok_or_else(|| invalid("record must be a JSON object"))?;
            if object.contains_key("mtx") {
                if !extension {
                    return Err(invalid("mtx header is not allowed in an mtj recording"));
                }
                let mut finished = current_group
                    .take()
                    .ok_or_else(|| invalid("mtx extension has no current group"))?;
                finished.channel_range.end = channels.len();
                finished.span_range.end = spans.len();
                sidecar_groups.push(finished);
                current_group = Some(parse_sidecar_group_header(
                    object,
                    channels.len(),
                    spans.len(),
                )?);
                continue;
            }
            if object.contains_key("mtj") {
                return Err(invalid("mtj header is only allowed on the first line"));
            }
            let (record_origin_ns, record_quantum_ns, record_duration_ns) = current_group
                .as_ref()
                .map(|group| (group.origin_ns, group.quantum_ns, group.duration_ns))
                .unwrap_or((origin_ns, quantum_ns, duration_ns));
            match record_kind(object)? {
                RecordKind::Channel => {
                    let parsed = parse_channel(
                        object,
                        record_origin_ns,
                        record_quantum_ns,
                        record_duration_ns,
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
                    channel_labels.push(parsed.labels);
                    channel_display.push(parsed.display);
                    channel_index += 1;
                }
                RecordKind::Span => {
                    spans.push(parse_span(object, record_quantum_ns, extension)?);
                }
            }
        }
        if let Some(mut finished) = current_group {
            finished.channel_range.end = channels.len();
            finished.span_range.end = spans.len();
            sidecar_groups.push(finished);
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
            sidecar_groups,
            channel_visible,
            channel_labels,
            channel_display,
            spans,
            videos,
            video_times,
            video_offset_ns,
        })
    }
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
fn parse_schema_hash(header: &Map<String, Value>) -> Result<Option<u64>, TelemetryFormatError> {
    let hash = string_field(header, "hash");
    if hash.is_empty() {
        return Ok(None);
    }
    if hash.len() != 16
        || !hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(invalid("hash must be 16-digit lowercase hex"));
    }
    u64::from_str_radix(&hash, 16)
        .map(Some)
        .map_err(|_| invalid("hash must be 16-digit lowercase hex"))
}
fn parse_sidecar_group_header(
    object: &Map<String, Value>,
    channel_start: usize,
    span_start: usize,
) -> Result<SidecarGroup, TelemetryFormatError> {
    if object.contains_key("mtj") {
        return Err(invalid("mtx header cannot contain mtj"));
    }
    let version = int_field(object, "mtx")?.ok_or_else(|| invalid("mtx header is missing mtx"))?;
    if version != u64::from(JSONL_EXT_VERSION) {
        return Err(invalid(format!("unsupported mtx version {version}")));
    }
    let (quantum_ns, duration_ns, origin_ns) = parse_group_header(object, "mtx header")?;
    let name = object
        .get("n")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("mtx header is missing n"))?;
    if name.is_empty() {
        return Err(invalid("mtx header n must be non-empty"));
    }
    let utc_start_ns =
        int_field(object, "utc")?.ok_or_else(|| invalid("mtx header is missing utc"))?;
    if utc_start_ns == 0 {
        return Err(invalid(
            "mtx header utc must be Unix-epoch nanoseconds at t=0",
        ));
    }
    let timezone = string_field(object, "tz");
    if timezone.is_empty() {
        return Err(invalid("mtx header is missing tz"));
    }
    if !valid_iana_timezone(&timezone) {
        return Err(invalid(format!(
            "mtx header tz is not an IANA timezone: {timezone}"
        )));
    }
    Ok(SidecarGroup {
        header: SidecarHeader {
            name: name.to_owned(),
            visible: parse_vis(object, true, "mtx header")?,
            right: parse_right(object.get("r"))?,
            utc_start_ns,
            timezone,
        },
        quantum_ns,
        origin_ns,
        duration_ns,
        schema_hash: parse_schema_hash(object)?,
        channel_range: channel_start..channel_start,
        span_range: span_start..span_start,
    })
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
fn parse_meta(value: Option<&Value>) -> Result<Vec<(String, SpanMetaValue)>, TelemetryFormatError> {
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
        meta.push((key.to_owned(), parse_meta_value(&pair[1])?));
    }
    Ok(meta)
}
fn parse_meta_value(value: &Value) -> Result<SpanMetaValue, TelemetryFormatError> {
    match value {
        Value::Number(number) => parse_timespan_number(number),
        Value::String(text) => Ok(SpanMetaValue::from_stored_text(text.clone())),
        Value::Object(object) => {
            let unit = object
                .get("u")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("typed meta value is missing u"))?;
            if unit != TIMESPAN_MS
                && motorsport_telemetry_core::normalize_unit(unit) != Some(TIMESPAN_MS)
            {
                return Err(invalid(format!(
                    "typed meta unit {unit} is not timespan_ms"
                )));
            }
            let raw = object
                .get("v")
                .ok_or_else(|| invalid("typed meta value is missing v"))?;
            match raw {
                Value::Number(number) => parse_timespan_number(number),
                Value::String(text) => parse_timespan_ms(text)
                    .map(SpanMetaValue::TimeMs)
                    .ok_or_else(|| invalid("timespan_ms string is not M:SS.FFF")),
                _ => Err(invalid("timespan_ms v must be an integer or M:SS.FFF")),
            }
        }
        _ => Err(invalid(
            "meta value must be a string, millisecond integer, or {v,u}",
        )),
    }
}
fn parse_timespan_number(number: &Number) -> Result<SpanMetaValue, TelemetryFormatError> {
    let ms = number
        .as_u64()
        .ok_or_else(|| invalid("timespan_ms must be an integer"))?;
    if !timespan_ms_in_range(ms) {
        return Err(invalid("timespan_ms exceeds 100 hours"));
    }
    Ok(SpanMetaValue::TimeMs(ms as u32))
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
    let display = parse_display(record, name)?;
    let labels = parse_labels(record, name, origin_ns, quantum_ns)?;
    if !labels.is_empty() && !display.plot.is_trace() {
        return Err(invalid(format!(
            "channel {name} labels are only allowed on plt=trace"
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
        labels,
        display,
    })
}
struct ParsedChannel {
    channel: Channel,
    values: Vec<f64>,
    visible: bool,
    labels: Vec<ChannelLabel>,
    display: ChannelDisplay,
}
fn parse_display(
    record: &Map<String, Value>,
    name: &str,
) -> Result<ChannelDisplay, TelemetryFormatError> {
    let plot = match record.get("plt").and_then(Value::as_str) {
        None | Some("") => ChannelPlot::Trace,
        Some(value) => ChannelPlot::parse(value).ok_or_else(|| {
            invalid(format!(
                "channel {name} plt must be trace, gauge, or compass"
            ))
        })?,
    };
    let (scale_min, scale_max) = match record.get("sc") {
        None => (None, None),
        Some(Value::Array(pair)) if !pair.is_empty() => {
            let min = match pair.first() {
                Some(Value::Null) | None => None,
                Some(value) => Some(json_finite(value, name, "sc min")?),
            };
            let max = match pair.get(1) {
                None | Some(Value::Null) => None,
                Some(value) => Some(json_finite(value, name, "sc max")?),
            };
            if let (Some(low), Some(high)) = (min, max) {
                if low >= high {
                    return Err(invalid(format!(
                        "channel {name} sc min must be less than max"
                    )));
                }
            }
            (min, max)
        }
        Some(_) => {
            return Err(invalid(format!(
                "channel {name} sc must be [min, max] numbers"
            )))
        }
    };
    let decimals = match record.get("rnd") {
        None => None,
        Some(value) => {
            let number = json_u64(value)?
                .ok_or_else(|| invalid(format!("channel {name} rnd must be an integer ≥ 0")))?;
            if number > 15 {
                return Err(invalid(format!("channel {name} rnd must be 0..=15")));
            }
            Some(number as u8)
        }
    };
    let format = string_field(record, "fmt");
    if format.chars().any(char::is_whitespace) {
        return Err(invalid(format!(
            "channel {name} fmt must not contain whitespace (use 0.0°C not '0.0 °C')"
        )));
    }
    if format.chars().count() > 16 {
        return Err(invalid(format!(
            "channel {name} fmt is longer than 16 characters"
        )));
    }
    Ok(ChannelDisplay {
        plot,
        scale_min,
        scale_max,
        decimals,
        format,
    })
}
fn parse_labels(
    record: &Map<String, Value>,
    name: &str,
    origin_ns: u64,
    quantum_ns: u64,
) -> Result<Vec<ChannelLabel>, TelemetryFormatError> {
    let Some(value) = record.get("lbl") else {
        return Ok(Vec::new());
    };
    let rows = value
        .as_array()
        .ok_or_else(|| invalid(format!("channel {name} lbl must be an array")))?;
    let mut labels = Vec::with_capacity(rows.len());
    let mut previous = None;
    for row in rows {
        let pair = row
            .as_array()
            .ok_or_else(|| invalid(format!("channel {name} lbl entry must be [ns, text]")))?;
        if pair.len() < 2 {
            return Err(invalid(format!(
                "channel {name} lbl entry must be [ns, text]"
            )));
        }
        let time_ns = json_u64(&pair[0])?
            .ok_or_else(|| invalid(format!("channel {name} label time must be an integer")))?;
        if time_ns < origin_ns || time_ns % quantum_ns != 0 {
            return Err(invalid(format!(
                "channel {name} label time is not on the time lattice"
            )));
        }
        if previous.is_some_and(|prev| time_ns <= prev) {
            return Err(invalid(format!(
                "channel {name} labels must be in increasing time order"
            )));
        }
        let text = pair[1]
            .as_str()
            .ok_or_else(|| invalid(format!("channel {name} label text must be a string")))?;
        if text.is_empty() {
            return Err(invalid(format!(
                "channel {name} label text must be non-empty"
            )));
        }
        previous = Some(time_ns);
        labels.push(ChannelLabel {
            time_ns,
            text: text.to_owned(),
        });
    }
    Ok(labels)
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
/// Extracts and validates the lattice fields shared by mtj recording headers
/// and mtx sidecar group headers (`q`, `dur`, `o`), returning
/// `(quantum_ns, duration_ns, origin_ns)`. `label` prefixes the missing-field
/// messages (e.g. `"header"` or `"mtx header"`).
fn parse_group_header(
    header: &Map<String, Value>,
    label: &str,
) -> Result<(u64, u64, u64), TelemetryFormatError> {
    let quantum_ns =
        int_field(header, "q")?.ok_or_else(|| invalid(format!("{label} is missing q")))?;
    if quantum_ns == 0 {
        return Err(invalid("q must be greater than 0"));
    }
    let duration_ns =
        int_field(header, "dur")?.ok_or_else(|| invalid(format!("{label} is missing dur")))?;
    let origin_ns = int_field(header, "o")?.unwrap_or(0);
    validate_lattice(quantum_ns, duration_ns, origin_ns)?;
    Ok((quantum_ns, duration_ns, origin_ns))
}
/// Checks that `origin_ns` and `duration_ns` sit on the `quantum_ns` lattice.
fn validate_lattice(
    quantum_ns: u64,
    duration_ns: u64,
    origin_ns: u64,
) -> Result<(), TelemetryFormatError> {
    if origin_ns % quantum_ns != 0 {
        return Err(invalid("o is not on the time lattice"));
    }
    if duration_ns < origin_ns || (duration_ns - origin_ns) % quantum_ns != 0 {
        return Err(invalid("dur is not on the time lattice"));
    }
    Ok(())
}
