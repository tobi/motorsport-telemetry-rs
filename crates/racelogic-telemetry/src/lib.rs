#![doc = include_str!("../README.md")]
#![deny(missing_docs)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::print_stdout,
        clippy::print_stderr,
        clippy::unreadable_literal,
        clippy::float_cmp,
        reason = "unit tests: fail loudly, print freely, exact fixture values"
    )
)]

use motorsport_telemetry_core::{
    names, Channel, Chunk, Diagnostic, LapMetadata, SampleTimes, SampleType, SourceLapMetadata,
    Storage, TelemetrySource, UnitSource, VideoFileRef, VideoSyncPoint, VideoSyncSegment,
    VideoTimeline,
};
use std::path::Path;
use thiserror::Error;

/// Opens a VBO file and derives its format-neutral metadata summary.
pub fn read_metadata(
    path: impl AsRef<Path>,
) -> Result<motorsport_telemetry_core::FileMetadata, RacelogicError> {
    RacelogicFile::open_mode(path, true)
        .map(|file| motorsport_telemetry_core::read_source_metadata(&file))
}

/// Derives format-neutral metadata from an owned VBO byte buffer.
#[allow(
    clippy::needless_pass_by_value,
    reason = "owned-buffer metadata entry point matches the other format readers"
)]
pub fn read_metadata_from_bytes(
    path: impl Into<String>,
    data: Vec<u8>,
) -> Result<motorsport_telemetry_core::FileMetadata, RacelogicError> {
    RacelogicFile::from_slice_mode(path.into(), &data, true)
        .map(|file| motorsport_telemetry_core::read_source_metadata(&file))
}

const BUILTIN_NAMES: [&str; 12] = [
    "satellites",
    "time",
    "latitude",
    "longitude",
    "velocity kmh",
    "heading",
    "height",
    "vertical velocity m/s",
    "sampleperiod",
    "solution type",
    "avifileindex",
    "avisynctime",
];
const BUILTIN_SHORT: [&str; 12] = [
    "sats",
    "time",
    "lat",
    "long",
    "velocity",
    "heading",
    "height",
    "vert-vel",
    "Tsample",
    "solution_type",
    "avifileindex",
    "avitime",
];

/// Errors returned while opening or parsing Racelogic VBOX telemetry.
#[derive(Debug, Error)]
pub enum RacelogicError {
    /// The VBO file could not be opened or memory-mapped.
    #[error("I/O error for {path}: {source}")]
    Io {
        /// Path that was being opened.
        path: String,
        /// Underlying filesystem error.
        source: std::io::Error,
    },
    /// The VBO structure or a sample value is malformed.
    #[error("invalid VBO file {path}: {message}")]
    Invalid {
        /// Path or caller-supplied input name.
        path: String,
        /// Specific validation failure.
        message: String,
    },
}

/// Sections borrow directly out of the mapped file: a VBO's `[data]` block is
/// one line per sample, so owning each line would allocate once per sample for
/// text we only ever read.
#[derive(Default)]
struct Sections<'a> {
    created_date: String,
    created_time: String,
    header: Vec<&'a str>,
    units: Vec<&'a str>,
    column_names: Vec<&'a str>,
    data: Vec<&'a str>,
    avi: Vec<&'a str>,
    laptiming: Vec<&'a str>,
}

/// An opened Racelogic VBOX telemetry source.
#[derive(Debug)]
pub struct RacelogicFile {
    /// Source path or caller-supplied name.
    pub path: String,
    /// Source-exact telemetry channel metadata.
    pub channels: Vec<Channel>,
    /// File-relative timestamp for each VBO data row.
    pub time_ns: Vec<u64>,
    /// Recording date from the VBOX preamble, when present.
    pub date: String,
    /// Recording time from the VBOX preamble, when present.
    pub recording_time: String,
    /// Linked video files discovered next to the VBO (`prefixNNNN.ext`).
    pub videos: Vec<VideoFileRef>,
    video_timeline: Option<VideoTimeline>,
    values: Vec<Vec<f64>>,
    absolute_start_ns: u64,
    /// Laps from GPS crossings of the `[laptiming]` start/finish gate.
    laps: Option<SourceLapMetadata>,
    /// Recovery diagnostics collected during parse.
    pub diagnostics: Vec<Diagnostic>,
}

fn invalid(path: &str, message: impl Into<String>) -> RacelogicError {
    RacelogicError::Invalid {
        path: path.into(),
        message: message.into(),
    }
}

fn sections(text: &str) -> Sections<'_> {
    let mut result = Sections::default();
    let mut current = String::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(created) = trimmed.strip_prefix("File created on ") {
            // VBOX Tools writes `26/01/2025 @ 12:23:57`; older exports and
            // Circuit Tools write `31/07/2006 at 09:55:20`.
            if let Some((date, time)) = created
                .split_once(" at ")
                .or_else(|| created.split_once(" @ "))
            {
                date.trim().clone_into(&mut result.created_date);
                time.trim().clone_into(&mut result.created_time);
            }
        }
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            current = trimmed[1..trimmed.len() - 1].to_ascii_lowercase();
            continue;
        }
        if trimmed.is_empty() {
            continue;
        }
        match current.as_str() {
            "header" => result.header.push(trimmed),
            "channel units" => result.units.push(trimmed),
            "column names" => {
                result.column_names = trimmed.split_whitespace().collect();
                current.clear();
            }
            "data" => result.data.push(trimmed),
            "avi" => result.avi.push(trimmed),
            "laptiming" => result.laptiming.push(trimmed),
            _ => {}
        }
    }
    result
}

/// Days since the Unix epoch for a `dd/mm/yyyy` header date (proleptic
/// Gregorian, UTC). `None` for anything that does not parse as a real date.
fn header_date_days(date: &str) -> Option<i64> {
    let mut parts = date.trim().split('/');
    let day: i64 = parts.next()?.trim().parse().ok()?;
    let month: i64 = parts.next()?.trim().parse().ok()?;
    let year: i64 = parts.next()?.trim().parse().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    if !(1980..=2200).contains(&year) {
        return None;
    }
    // Howard Hinnant's days_from_civil.
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

fn time_seconds(raw: f64) -> f64 {
    let hours = (raw / 10000.0).floor();
    let minutes = ((raw % 10000.0) / 100.0).floor();
    hours * 3600.0 + minutes * 60.0 + raw % 100.0
}

fn builtin_unit(name: &str) -> &'static str {
    match name.to_ascii_lowercase().as_str() {
        "time" | "tsample" => "s",
        "lat" | "long" => "min",
        "velocity" => "km/h",
        "heading" => "deg",
        "height" => "m",
        "vert-vel" => "m/s",
        _ => "",
    }
}

fn metadata_channel(name: &str) -> bool {
    matches!(
        names::normalize(name).as_str(),
        "time"
            | "tsample"
            | "sats"
            | "satellites"
            | "latitude"
            | "longitude"
            | "lat"
            | "long"
            | "lon"
            | "driver"
            | "driverid"
            | "driverindex"
            | "lap"
            | "lapnumber"
            | "lapcount"
            | "lapcounter"
            | "currentlaptime"
            | "laptime"
            | "laptimerunning"
            | "previouslt"
            | "previouslaptime"
            | "lastlaptime"
            | "reflaptime"
            | "referencelaptime"
            | "avifileindex"
            | "avisynctime"
            | "avitime"
            | "cartype"
            | "vehicletype"
            | "vehiclemodel"
            | "carmodel"
            | "carnumber"
            | "vehiclenumber"
            | "racenumber"
            | "competitionnumber"
            | "carclass"
            | "vehicleclass"
            | "classid"
            | "competitionclass"
    )
}
impl RacelogicFile {
    /// Memory-maps the file and parses straight out of the mapping.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, RacelogicError> {
        Self::open_mode(path, false)
    }

    /// Memory-maps a file and retains only channels used for metadata reports.
    ///
    /// Channel declarations remain visible, but sample values for unrelated
    /// bulk signals are skipped while each text row is scanned.
    pub fn open_metadata(path: impl AsRef<Path>) -> Result<Self, RacelogicError> {
        Self::open_mode(path, true)
    }

    fn open_mode(path: impl AsRef<Path>, metadata_only: bool) -> Result<Self, RacelogicError> {
        let path = path.as_ref();
        let display = path.to_string_lossy().into_owned();
        let storage = Storage::open(path).map_err(|source| RacelogicError::Io {
            path: display.clone(),
            source,
        })?;
        Self::from_slice_mode(display, &storage, metadata_only)
    }

    /// Parses VBO telemetry from an owned byte buffer.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "owned-buffer entry point matches the other format readers; from_slice accepts borrowed input"
    )]
    pub fn from_bytes(path: impl Into<String>, bytes: Vec<u8>) -> Result<Self, RacelogicError> {
        Self::from_slice_mode(path.into(), &bytes, false)
    }

    /// Parses VBO telemetry from a borrowed byte slice.
    ///
    /// Parsed values are owned by the returned file; the input need not outlive
    /// the result.
    pub fn from_slice(path: impl Into<String>, bytes: &[u8]) -> Result<Self, RacelogicError> {
        Self::from_slice_mode(path.into(), bytes, false)
    }

    fn from_slice_mode(
        display: String,
        bytes: &[u8],
        metadata_only: bool,
    ) -> Result<Self, RacelogicError> {
        if bytes.is_empty() {
            return Err(invalid(&display, "empty file"));
        }
        // Borrow when the file is UTF-8 (the overwhelmingly common case) and
        // only allocate for the latin-1 fallback.
        let fallback;
        let text: &str = if let Ok(text) = std::str::from_utf8(bytes) {
            text
        } else {
            fallback = bytes
                .iter()
                .map(|&byte| char::from(byte))
                .collect::<String>();
            &fallback
        };
        let parsed = sections(text);
        let mut diagnostics = Vec::new();
        if parsed.data.is_empty() {
            return Err(invalid(&display, "missing or empty [data] section"));
        }
        let column_names_missing = parsed.column_names.is_empty();
        let short_names: Vec<&str> = if column_names_missing {
            parsed
                .header
                .iter()
                .enumerate()
                .map(|(index, name)| {
                    if index < BUILTIN_SHORT.len() {
                        BUILTIN_SHORT[index]
                    } else {
                        *name
                    }
                })
                .collect()
        } else {
            parsed.column_names.clone()
        };
        if short_names.is_empty() {
            return Err(invalid(&display, "no channel names"));
        }
        let count = short_names.len();
        let selected = short_names
            .iter()
            .map(|name| !metadata_only || metadata_channel(name))
            .collect::<Vec<_>>();
        let mut values = selected
            .iter()
            .map(|selected| {
                if *selected {
                    Vec::with_capacity(parsed.data.len())
                } else {
                    Vec::new()
                }
            })
            .collect::<Vec<Vec<f64>>>();
        let mut rows = 0usize;
        let mut unparsable_counts = vec![0u32; count];
        let mut skipped_rows = 0u32;
        for line in &parsed.data {
            if line.split_whitespace().nth(1).is_none() {
                skipped_rows += 1;
                continue;
            }
            rows += 1;
            let mut tokens = line.split_whitespace();
            for (column, output) in values.iter_mut().enumerate() {
                if !selected[column] {
                    tokens.next();
                    continue;
                }
                let token = tokens.next();
                if let Some(value) = token.and_then(|t| t.parse::<f64>().ok()) {
                    output.push(value);
                } else {
                    output.push(f64::NAN);
                    unparsable_counts[column] += 1;
                }
            }
        }
        if rows == 0 {
            return Err(invalid(&display, "no valid data rows"));
        }
        if column_names_missing {
            diagnostics.push(Diagnostic::warning(
                "vbo.column_names_missing",
                "no [column names] section; channel names fell back to [header] \
                 or builtin defaults",
            ));
        }
        for (column, &count) in unparsable_counts.iter().enumerate() {
            if count > 0 {
                diagnostics.push(
                    Diagnostic::warning(
                        "vbo.value_unparsable",
                        format!("{count} numeric token(s) failed to parse and became NaN"),
                    )
                    .with_channel(short_names[column]),
                );
            }
        }
        if skipped_rows > 0 {
            diagnostics.push(Diagnostic::warning(
                "vbo.row_too_few_tokens",
                format!(
                    "{skipped_rows} data row(s) skipped for having fewer than two \
                     whitespace-delimited tokens"
                ),
            ));
        }
        let time_column = short_names
            .iter()
            .position(|name| names::eq(name, "time"))
            .ok_or_else(|| invalid(&display, "no time column"))?;
        let first_raw = values[time_column][0];
        if !first_raw.is_finite() {
            return Err(invalid(
                &display,
                "first time value is not a finite number; cannot establish a timeline",
            ));
        }
        let first = time_seconds(first_raw);
        let mut time_ns = Vec::with_capacity(rows);
        let mut rollover_corrections = 0u32;
        for value in &mut values[time_column] {
            let mut seconds = time_seconds(*value);
            if seconds < first - 43200.0 {
                seconds += 86400.0;
                rollover_corrections += 1;
            }
            *value = seconds - first;
            time_ns.push((*value * 1e9).round().max(0.0) as u64);
        }
        if rollover_corrections > 0 {
            diagnostics.push(Diagnostic::info(
                "vbo.time_rollover_corrected",
                format!(
                    "{rollover_corrections} time value(s) wrapped past midnight; \
                     +86400 s correction applied"
                ),
            ));
        }
        let tsample_period = short_names
            .iter()
            .position(|name| names::eq(name, "tsample"))
            .and_then(|index| values[index].iter().copied().find(|value| *value > 0.0))
            .map(|seconds| (seconds * 1e9).round() as u64);
        let delta_period = time_ns
            .windows(2)
            .map(|pair| pair[1].saturating_sub(pair[0]))
            .find(|delta| *delta > 0);
        let sample_period = match (tsample_period, delta_period) {
            (Some(period), _) | (None, Some(period)) => period,
            (None, None) => {
                diagnostics.push(Diagnostic::warning(
                    "vbo.sample_period_defaulted",
                    "no tsample column and no positive time delta found; sample \
                     period defaulted to 100 ms",
                ));
                100_000_000
            }
        };
        let duration = time_ns
            .last()
            .copied()
            .unwrap_or(0)
            .saturating_add(sample_period);

        // `[channel units]` declares one unit per custom (non-builtin) column.
        // Real VBOX loggers (VBVDHD2 firmware 1.x) emit one extra leading entry
        // covering the last builtin column (`avisynctime`, "s"), so the list
        // is anchored at its *end*: the last custom column takes the last unit.
        // Front-aligning those files shifted every unit by one and labelled
        // `Vehicle_Speed` as `%`. When fewer units than custom columns are
        // declared the start is the only anchor available, so fall back to it.
        let custom_count = count.saturating_sub(BUILTIN_NAMES.len());
        let unit_offset = parsed.units.len() as isize - custom_count as isize;
        if custom_count > 0 && !parsed.units.is_empty() && unit_offset != 0 {
            diagnostics.push(Diagnostic::info(
                "vbo.channel_units_count_mismatch",
                format!(
                    "[channel units] lists {} entries for {custom_count} custom column(s); \
                     units were aligned to the {} of the list",
                    parsed.units.len(),
                    if unit_offset > 0 { "end" } else { "start" }
                ),
            ));
        }
        let declared_unit = |index: usize| -> Option<&str> {
            let position = index as isize - BUILTIN_NAMES.len() as isize + unit_offset.max(0);
            usize::try_from(position)
                .ok()
                .and_then(|position| parsed.units.get(position))
                .copied()
                .filter(|unit| *unit != "(null)" && !unit.is_empty())
        };

        let mut channels = Vec::with_capacity(count);
        for index in 0..count {
            let name: String = match parsed.header.get(index) {
                Some(declared) => (*declared).to_owned(),
                None if index < BUILTIN_NAMES.len() => BUILTIN_NAMES[index].to_owned(),
                None => short_names[index].to_owned(),
            };
            // Builtin VBOX columns have units fixed by the format spec; the
            // trailing custom columns declare theirs in [channel units].
            let (unit, unit_source) = if index < BUILTIN_NAMES.len() {
                let builtin = builtin_unit(short_names[index]);
                if !builtin.is_empty() {
                    (builtin.to_owned(), UnitSource::SpecDefault)
                } else if let Some(declared) = declared_unit(index) {
                    // A surplus leading entry names a builtin the spec leaves
                    // unitless (e.g. `avisynctime` -> "s").
                    (declared.to_owned(), UnitSource::Declared)
                } else {
                    (String::new(), UnitSource::Unknown)
                }
            } else {
                match declared_unit(index) {
                    Some(declared) => (declared.to_owned(), UnitSource::Declared),
                    None => (String::new(), UnitSource::Unknown),
                }
            };
            let sampled = selected[index];
            channels.push(Channel {
                id: index as u32,
                name,
                unit,
                unit_source,
                sample_type: SampleType::F64,
                chunks: sampled
                    .then_some(Chunk {
                        sample_period_ns: sample_period,
                        sample_count: rows as u64,
                        data_ptr: 0,
                        sample_base: 0,
                        time_base_ns: 0,
                    })
                    .into_iter()
                    .collect(),
                sample_count: if sampled { rows as u64 } else { 0 },
                duration_ns: if sampled { duration } else { 0 },
            });
        }
        let videos = discover_videos(&parsed.avi, &short_names, &values);
        let video_timeline = video_timeline(
            &short_names,
            &values,
            &time_ns,
            sample_period,
            &mut diagnostics,
        );
        let laps = gate_laps(
            &parsed.laptiming,
            &short_names,
            &values,
            &time_ns,
            duration,
            &mut diagnostics,
        );
        Ok(Self {
            path: display,
            channels,
            time_ns,
            laps,
            date: parsed.created_date,
            recording_time: parsed.created_time,
            videos,
            video_timeline,
            values,
            absolute_start_ns: (first * 1e9).round().max(0.0) as u64,
            diagnostics,
        })
    }
}

/// The start/finish marks declared in `[laptiming]`, as two points in the
/// file's own coordinate convention (arc-minutes, longitude first, west
/// positive — the same convention as the `lat` / `long` columns). See
/// [`gate_metres`] for what the two points mean.
///
/// A line reads `Start <long1> <lat1> <long2> <lat2> ¬ <name>`. `Split`
/// lines (sector gates) are ignored.
fn parse_gate(laptiming: &[&str]) -> Option<[(f64, f64); 2]> {
    laptiming.iter().find_map(|line| {
        let rest = line.strip_prefix("Start")?;
        let mut numbers = rest
            .split_whitespace()
            .take_while(|token| *token != "¬")
            .map(|token| token.trim_start_matches('+').parse::<f64>().ok());
        let long1 = numbers.next()??;
        let lat1 = numbers.next()??;
        let long2 = numbers.next()??;
        let lat2 = numbers.next()??;
        ((long1, lat1) != (long2, lat2)).then_some([(long1, lat1), (long2, lat2)])
    })
}

/// Fraction along `a -> b` at which it crosses the segment `c -> d`, if it
/// does (proper intersection, both parameters inside `0..=1`).
fn segment_crossing(a: (f64, f64), b: (f64, f64), c: (f64, f64), d: (f64, f64)) -> Option<f64> {
    let r = (b.0 - a.0, b.1 - a.1);
    let s = (d.0 - c.0, d.1 - c.1);
    let denominator = r.0 * s.1 - r.1 * s.0;
    if denominator.abs() < f64::EPSILON {
        return None;
    }
    let qp = (c.0 - a.0, c.1 - a.1);
    let t = (qp.0 * s.1 - qp.1 * s.0) / denominator;
    let u = (qp.0 * r.1 - qp.1 * r.0) / denominator;
    ((0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u)).then_some(t)
}

/// Two gate crossings closer than this are one crossing seen twice (GPS
/// jitter while sitting on the line). Well below any lap.
const GATE_DEBOUNCE_NS: u64 = 10_000_000_000;

/// Consecutive fixes further apart than this speed implies are a position
/// jump, not motion (150 m/s = 540 km/h). Such a pair can neither cross
/// the gate nor be trusted.
const GATE_MAX_SPEED_MPS: f64 = 150.0;

/// When more than this fraction of consecutive fix pairs are jumps the
/// receiver was not tracking and nothing it says about the gate is usable.
/// Healthy sessions sit at zero; a failing antenna produced 2.3 %.
const GATE_MAX_JUMP_FRACTION: f64 = 0.005;

/// Minimum satellites for a fix to take part in gate timing.
const GATE_MIN_SATELLITES: f64 = 4.0;

/// Half-width of the timing gate, in metres, either side of its centre.
///
/// Inference policy, not a field stored in the file: use a 50 m line centred
/// on the mark to cover a wide pit straight. Its width and the interpretation
/// of the two marks were checked against this collection, not a vendor spec.
const GATE_HALF_WIDTH_M: f64 = 25.0;

/// Metres per arc-minute of latitude (and of longitude at the equator).
const METRES_PER_ARCMIN: f64 = 1_852.0;

/// Local planar metres for a `(long, lat)` arc-minute pair, relative to
/// `origin`. Longitude is scaled by the cosine of the origin latitude so
/// distances (and therefore the gate width) are right; the sign convention
/// is irrelevant to intersection tests.
fn local_metres(point: (f64, f64), origin: (f64, f64)) -> (f64, f64) {
    let cos_lat = (origin.1 / 60.0).to_radians().cos();
    (
        (point.0 - origin.0) * METRES_PER_ARCMIN * cos_lat,
        (point.1 - origin.1) * METRES_PER_ARCMIN,
    )
}

/// The timing gate as a metric segment about `origin`.
///
/// The two `[laptiming]` points are not the gate's ends: on real files they
/// sit a couple of metres apart *along* the racing line (the first marks the
/// line's position, the second the direction of travel). The gate is the
/// line through the first point perpendicular to that direction, extended
/// [`GATE_HALF_WIDTH_M`] each side.
fn gate_metres(marks: [(f64, f64); 2], origin: (f64, f64)) -> Option<[(f64, f64); 2]> {
    let centre = local_metres(marks[0], origin);
    let ahead = local_metres(marks[1], origin);
    let travel = (ahead.0 - centre.0, ahead.1 - centre.1);
    let length = (travel.0 * travel.0 + travel.1 * travel.1).sqrt();
    if length == 0.0 || !length.is_finite() {
        return None;
    }
    let across = (-travel.1 / length, travel.0 / length);
    Some([
        (
            centre.0 - across.0 * GATE_HALF_WIDTH_M,
            centre.1 - across.1 * GATE_HALF_WIDTH_M,
        ),
        (
            centre.0 + across.0 * GATE_HALF_WIDTH_M,
            centre.1 + across.1 * GATE_HALF_WIDTH_M,
        ),
    ])
}

/// Laps from GPS crossings of the declared start/finish gate.
///
/// These are inferred laps, not logger-reported times. GPS crossings can
/// recover boundaries lost by the CAN `Lap_Number` channel during dash resets at driver changes and pit stops (the
/// counter drops to 0 and counts up again, so a high-water-mark reading of
/// it swallows every lap until the old maximum is passed — a 29-minute "lap"
/// at racing speed). Files without a gate, without GPS, or whose GPS never
/// crosses the gate return `None` and fall back to the generic counter and
/// timer recovery. Laps are numbered from 1 in crossing order; the head and
/// tail fragments are incomplete.
fn gate_laps(
    laptiming: &[&str],
    short_names: &[&str],
    values: &[Vec<f64>],
    time_ns: &[u64],
    duration_ns: u64,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<SourceLapMetadata> {
    let gate = parse_gate(laptiming)?;
    let column = |wanted: &[&str]| {
        short_names
            .iter()
            .position(|name| wanted.iter().any(|w| names::eq(name, w)))
            .filter(|index| values[*index].len() == time_ns.len())
    };
    let lat = column(&["lat", "latitude"])?;
    let long = column(&["long", "lon", "longitude"])?;
    let sats = column(&["sats", "satellites"]);
    let fix = |row: usize| -> Option<(f64, f64)> {
        let latitude = values[lat][row];
        let longitude = values[long][row];
        if !latitude.is_finite()
            || !longitude.is_finite()
            || latitude.abs() > 90.0 * 60.0
            || longitude.abs() > 180.0 * 60.0
            || (latitude == 0.0 && longitude == 0.0)
        {
            return None;
        }
        Some((longitude, latitude))
    };
    let usable = |row: usize| -> Option<(f64, f64)> {
        if sats.is_some_and(|sats| {
            !values[sats][row].is_finite() || values[sats][row] < GATE_MIN_SATELLITES
        }) {
            return None;
        }
        fix(row)
    };
    let origin = gate[0];
    let gate = gate_metres(gate, origin)?;
    let jump = |prev_row: usize, prev_point: (f64, f64), row: usize, point: (f64, f64)| {
        let dt_s = time_ns[row].saturating_sub(time_ns[prev_row]) as f64 / 1e9;
        let distance_m =
            ((point.0 - prev_point.0).powi(2) + (point.1 - prev_point.1).powi(2)).sqrt();
        dt_s <= 0.0 || distance_m > GATE_MAX_SPEED_MPS * dt_s
    };

    // Receiver health is judged on every reported fix, not just the ones
    // that pass the satellite filter: a failing antenna reports positions
    // jumping by degrees between samples, and a logger that still labels a
    // fraction of them with a satellite count is not one to time laps with.
    let mut pairs = 0usize;
    let mut jumps = 0usize;
    let mut previous: Option<(usize, (f64, f64))> = None;
    for row in 0..time_ns.len() {
        let Some(point) = fix(row).map(|point| local_metres(point, origin)) else {
            continue;
        };
        if let Some((prev_row, prev_point)) = previous {
            if prev_row + 1 == row {
                pairs += 1;
                if jump(prev_row, prev_point, row, point) {
                    jumps += 1;
                }
            }
        }
        previous = Some((row, point));
    }
    if pairs > 0 && jumps as f64 > pairs as f64 * GATE_MAX_JUMP_FRACTION {
        diagnostics.push(Diagnostic::warning(
            "vbo.gate_laps_skipped_gps_unreliable",
            format!(
                "{jumps} of {pairs} consecutive GPS fixes imply more than {GATE_MAX_SPEED_MPS:.0} \
                 m/s; the receiver was not tracking, so the [laptiming] gate was not used for \
                 laps"
            ),
        ));
        return None;
    }

    let mut crossings: Vec<u64> = Vec::new();
    let mut previous: Option<(usize, (f64, f64))> = None;
    for row in 0..time_ns.len() {
        let Some(point) = usable(row).map(|point| local_metres(point, origin)) else {
            continue;
        };
        if let Some((prev_row, prev_point)) = previous {
            // Only consecutive fixes form a path; a dropout in between means
            // the car could have crossed anywhere, and a jump is not a path.
            if prev_row + 1 == row
                && time_ns[row].saturating_sub(time_ns[prev_row]) <= 2_000_000_000
                && !jump(prev_row, prev_point, row, point)
            {
                if let Some(t) = segment_crossing(prev_point, point, gate[0], gate[1]) {
                    let t0 = time_ns[prev_row] as f64;
                    let t1 = time_ns[row] as f64;
                    let at = (t0 + (t1 - t0) * t).round() as u64;
                    if crossings
                        .last()
                        .is_none_or(|last| at.saturating_sub(*last) >= GATE_DEBOUNCE_NS)
                    {
                        crossings.push(at);
                    }
                }
            }
        }
        previous = Some((row, point));
    }
    let last_crossing = *crossings.last()?;
    let mut boundaries = Vec::with_capacity(crossings.len() + 2);
    boundaries.push(0);
    boundaries.extend(crossings.iter().copied());
    boundaries.push(duration_ns.max(last_crossing));
    let count = boundaries.len() - 1;
    let laps = boundaries
        .windows(2)
        .enumerate()
        .filter(|(_, pair)| pair[1] > pair[0])
        .map(|(index, pair)| {
            LapMetadata::interval(
                index as i64 + 1,
                pair[0],
                pair[1],
                index > 0 && index + 1 < count,
            )
        })
        .collect::<Vec<_>>();
    diagnostics.push(Diagnostic::info(
        "vbo.laps_from_gate",
        format!(
            "{} GPS crossing(s) of the [laptiming] start/finish gate define the laps; the CAN \
             lap counter (if any) was not used; inferred with a 50 m gate",
            crossings.len()
        ),
    ));
    Some(SourceLapMetadata {
        laps,
        fastest_lap: None,
    })
}

fn discover_videos(avi: &[&str], short_names: &[&str], values: &[Vec<f64>]) -> Vec<VideoFileRef> {
    let prefix = avi.first().copied().unwrap_or("");
    let ext = avi
        .get(1)
        .copied()
        .unwrap_or("avi")
        .trim_start_matches('.')
        .to_ascii_lowercase();
    let mut indices = std::collections::BTreeSet::new();
    if let Some(column) = short_names
        .iter()
        .position(|name| names::eq(name, "avifileindex"))
    {
        for value in &values[column] {
            if let Some(index) = native_integer(*value).and_then(|value| u32::try_from(value).ok())
            {
                indices.insert(index);
            }
        }
    }
    if indices.is_empty() && !prefix.is_empty() {
        indices.insert(1);
    }
    indices
        .into_iter()
        .filter(|index| *index > 0)
        .map(|index| {
            let filename = if prefix.is_empty() {
                format!("{index:04}.{ext}")
            } else {
                format!("{prefix}{index:04}.{ext}")
            };
            VideoFileRef {
                filename,
                index,
                blake3: None,
                frame_count: 0,
                presentation_offset_ns: None,
            }
        })
        .collect()
}

// HD1/HD2 Video Time (PTS) is integer milliseconds since this video file
// started. The surplus `[channel units]` entry sometimes says `s`; it does
// not change the native field's scale. Preserve the raw channel untouched.
// https://en.racelogic.support/automotive/data-loggers/vbvdhd2/technical/can-output/
#[allow(
    clippy::float_cmp,
    reason = "native video fields must be exact nonnegative integers"
)]
fn native_integer(value: f64) -> Option<u64> {
    (value.is_finite() && value >= 0.0 && value < u64::MAX as f64 && value.fract() == 0.0)
        .then_some(value as u64)
}

fn video_timeline(
    names: &[&str],
    values: &[Vec<f64>],
    times: &[u64],
    sample_period: u64,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<VideoTimeline> {
    let index_column = names
        .iter()
        .position(|name| names::eq(name, "avifileindex"))?;
    let sync_column = names
        .iter()
        .position(|name| names::eq(name, "avitime") || names::eq(name, "avisynctime"))?;
    let mut segments = Vec::<VideoSyncSegment>::new();
    let mut current = None::<VideoSyncSegment>;
    let mut previous_observation = None;
    let finish = |current: &mut Option<VideoSyncSegment>, segments: &mut Vec<VideoSyncSegment>| {
        if let Some(segment) = current.take() {
            segments.push(segment);
        }
    };
    for (row, &time) in times.iter().enumerate() {
        let index = values[index_column]
            .get(row)
            .copied()
            .and_then(native_integer)
            .and_then(|index| u32::try_from(index).ok())
            .filter(|index| *index > 0);
        let presentation = values[sync_column]
            .get(row)
            .copied()
            .and_then(native_integer)
            .and_then(|milliseconds| milliseconds.checked_mul(1_000_000));
        let observation = index.zip(presentation);
        // Invalid/no-video rows are evidence too: at the same telemetry
        // instant they contradict a valid sync observation in either order.
        if let Some((last_time, last_observation)) = previous_observation {
            if time == last_time && observation == last_observation {
                continue; // exact clock duplicate; preserve every raw row
            }
            if time <= last_time {
                diagnostics.push(Diagnostic::warning("vbo.video_clock_conflict",
                    "conflicting or reversed telemetry-time video observations; synchronization disabled"));
                return None;
            }
        }
        previous_observation = Some((time, observation));
        let (Some(index), Some(presentation)) = (index, presentation) else {
            finish(&mut current, &mut segments);
            continue;
        };
        let split = current.as_ref().is_some_and(|segment| {
            segment.file_index != index
                || segment.points.last().is_some_and(|last| {
                    presentation < last.presentation_time_ns
                        || time.saturating_sub(last.telemetry_time_ns)
                            > sample_period.saturating_add(sample_period / 2)
                })
        });
        if split {
            finish(&mut current, &mut segments);
        }
        current
            .get_or_insert_with(|| VideoSyncSegment {
                file_index: index,
                points: Vec::new(),
            })
            .points
            .push(VideoSyncPoint {
                telemetry_time_ns: time,
                presentation_time_ns: presentation,
            });
    }
    finish(&mut current, &mut segments);
    VideoTimeline::from_segments(segments).ok()
}

impl TelemetrySource for RacelogicFile {
    fn path(&self) -> &str {
        &self.path
    }
    fn format(&self) -> &'static str {
        "vbo"
    }
    fn channels(&self) -> &[Channel] {
        &self.channels
    }
    fn video_files(&self) -> &[VideoFileRef] {
        &self.videos
    }
    fn video_timeline(&self) -> Option<&VideoTimeline> {
        self.video_timeline.as_ref()
    }
    fn decode(&self, channel_index: usize, _chunk_index: usize, local_index: u64) -> f64 {
        let Some(values) = self.values.get(channel_index) else {
            return f64::NAN;
        };
        let Some(index) = usize::try_from(local_index).ok() else {
            return f64::NAN;
        };
        values.get(index).copied().unwrap_or(f64::NAN)
    }
    fn absolute_time_range(&self) -> Option<motorsport_telemetry_core::AbsoluteTimeRange> {
        let duration_ns = self
            .channels
            .iter()
            .map(|channel| channel.duration_ns)
            .max()
            .unwrap_or(0);
        Some(motorsport_telemetry_core::AbsoluteTimeRange {
            clock: "time_of_day".into(),
            start_ns: self.absolute_start_ns,
            end_ns: self.absolute_start_ns.saturating_add(duration_ns),
            session_hint: "vbo:time_of_day".into(),
        })
    }
    fn identity(&self) -> motorsport_telemetry_core::SourceIdentity {
        motorsport_telemetry_core::SourceIdentity {
            date: self.date.clone(),
            time: self.recording_time.clone(),
            ..Default::default()
        }
    }
    /// The VBOX `time` column is UTC time of day by specification; the header
    /// date names the day. Together they are a true Unix instant, so this is
    /// a derivation of stored values, not a decorative stamp. `None` without a
    /// parseable header date.
    fn utc_start_ns(&self) -> Option<u64> {
        let days = header_date_days(&self.date)?;
        let midnight_ns = u64::try_from(days.checked_mul(86_400_000_000_000)?).ok()?;
        midnight_ns.checked_add(self.absolute_start_ns)
    }
    fn sample_times(&self, channel_index: usize) -> SampleTimes<'_> {
        if self
            .channels
            .get(channel_index)
            .is_none_or(|c| c.sample_count == 0)
        {
            return SampleTimes::Explicit(&[]);
        }
        SampleTimes::Explicit(&self.time_ns)
    }

    fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    fn source_lap_metadata(&self) -> Option<SourceLapMetadata> {
        self.laps.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn clock_fixture(rows: &str) -> RacelogicFile {
        let text = format!("[header]\ntime\nsampleperiod\navifileindex\navisynctime\n[AVI]\nrun_\nmp4\n[column names]\ntime Tsample avifileindex avitime\n[data]\n{rows}");
        RacelogicFile::from_bytes("run.vbo", text.into_bytes()).unwrap()
    }

    #[test]
    fn native_video_clock_is_milliseconds_and_preserves_raw_samples() {
        let file = clock_fixture("120000.000 0.040 1 16233\n120000.040 0.040 1 16266\n120000.080 0.040 2 0\n120000.120 0.040 2 33\n");
        let clock = file.video_timeline().unwrap();
        assert_eq!(
            clock.presentation_at(0).unwrap().presentation_time_ns,
            16_233_000_000
        );
        assert_eq!(file.decode(3, 0, 0), 16233.0);
        assert_eq!(file.video_reference_at(80_000_000).file_index, Some(2));
        assert_eq!(
            file.video_reference_at(80_000_000).presentation_time_ns,
            Some(0)
        );
        assert_eq!(
            file.video_reference_at(60_000_000).presentation_time_ns,
            None
        );
        assert_eq!(file.metadata().video_timeline.as_ref(), Some(clock));
    }

    #[test]
    fn no_video_rows_gaps_and_resets_do_not_supply_interpolation() {
        let file = clock_fixture("120000.000 0.040 1 0\n120000.040 0.040 1 40\n120000.080 0.040 0 80\n120000.120 0.040 1 120\n120000.400 0.040 1 400\n120000.440 0.040 1 10\n120000.480 0.040 1 50\n");
        let clock = file.video_timeline().unwrap();
        for time in [
            60_000_000,
            80_000_000,
            100_000_000,
            200_000_000,
            420_000_000,
        ] {
            assert!(clock.presentation_at(time).is_err(), "{time}");
        }
        assert_eq!(
            clock.telemetry_at(1, 20_000_000),
            Err(motorsport_telemetry_core::VideoMappingError::Ambiguous)
        );
    }

    #[test]
    fn exact_duplicate_observations_are_deduplicated_but_conflicts_disable_sync() {
        let file = clock_fixture("120000.000 0.040 1 10\n120000.040 0.040 1 50\n120000.040 0.040 1 50\n120000.120 0.040 1 130\n");
        assert_eq!(file.channels[0].sample_count, 4);
        assert_eq!(file.video_timeline().unwrap().segments().len(), 2);
        assert!(file
            .video_timeline()
            .unwrap()
            .presentation_at(80_000_000)
            .is_err());
        let conflict =
            clock_fixture("120000.000 0.040 1 10\n120000.040 0.040 1 50\n120000.040 0.040 1 60\n");
        assert!(conflict.video_timeline().is_none());
        assert!(conflict
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "vbo.video_clock_conflict"));
    }

    #[test]
    fn invalid_duplicate_clock_rows_conflict_in_both_orders() {
        for rows in [
            "120000.000 0.040 1 10\n120000.000 0.040 0 0\n120000.040 0.040 1 50\n",
            "120000.000 0.040 0 0\n120000.000 0.040 1 10\n120000.040 0.040 1 50\n",
            "120000.000 0.040 1 10\n120000.000 0.040 1 -1\n120000.040 0.040 1 50\n",
        ] {
            let file = clock_fixture(rows);
            assert_eq!(file.channels[0].sample_count, 3);
            assert!(file.video_timeline().is_none());
            assert!(file
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "vbo.video_clock_conflict"));
        }
    }

    fn fixture(contents: &str) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(contents.as_bytes()).unwrap();
        file
    }

    #[test]
    fn parses_irregular_timestamps_and_interpolates_continuous_values() {
        let fixture = fixture("[header]\ntime\nvelocity kmh\n[column names]\ntime velocity\n[data]\n120000.0 10\n120000.5 20\n120001.5 40\n");
        let file = RacelogicFile::open(fixture.path()).unwrap();
        let in_memory =
            RacelogicFile::from_bytes("fixture.vbo", std::fs::read(fixture.path()).unwrap())
                .unwrap();
        assert_eq!(in_memory.channels.len(), 2);
        let metadata = read_metadata(fixture.path()).unwrap();
        assert_eq!(metadata.channel_count, 2);
        assert!(metadata.absolute_start_ns.is_some());
        assert_eq!(file.time_ns, [0, 500_000_000, 1_500_000_000]);
        assert_eq!(file.decode(1, 0, 2), 40.0);
        assert_eq!(file.sample_at(1, 1_000_000_000, true), Some(30.0));
    }

    #[test]
    fn metadata_mode_keeps_gps_and_skips_bulk_values() {
        let fixture = fixture("[header]\ntime\nlatitude\nlongitude\nthrottle\n[column names]\ntime lat long throttle\n[data]\n120000.0 2627.8 5279.3 10\n120000.5 2627.9 5279.4 20\n");
        let file = RacelogicFile::open_metadata(fixture.path()).unwrap();
        assert_eq!(file.channels[1].sample_count, 2, "latitude retained");
        assert_eq!(file.channels[2].sample_count, 2, "longitude retained");
        assert_eq!(file.channels[3].sample_count, 0, "throttle skipped");
        assert_eq!(file.values[3], Vec::<f64>::new());
    }

    #[test]
    fn preserves_recording_date_from_preamble() {
        let fixture = fixture("File created on 31/07/2006 at 09:55:20\n[column names]\ntime velocity\n[data]\n120000.0 10\n120000.5 20\n");
        let file = RacelogicFile::open(fixture.path()).unwrap();
        assert_eq!(file.identity().date, "31/07/2006");
        assert_eq!(file.identity().time, "09:55:20");
    }

    #[test]
    fn handles_midnight_rollover_and_stepwise_gear() {
        let fixture = fixture("[header]\ntime\ngear\n[column names]\ntime Gear\n[data]\n235959.5 3\n000000.0 4\n000000.5 4\n");
        let file = RacelogicFile::open(fixture.path()).unwrap();
        assert_eq!(file.time_ns, [0, 500_000_000, 1_000_000_000]);
        assert_eq!(file.sample_at(1, 250_000_000, true), Some(3.0));
    }

    #[test]
    fn discovers_two_avi_files_from_avifileindex() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("run_0001.mp4"), b"one").unwrap();
        std::fs::write(dir.path().join("run_0002.mp4"), b"two").unwrap();
        let vbo = dir.path().join("run.vbo");
        std::fs::write(
            &vbo,
            "[header]\ntime\navifileindex\navisynctime\n\
             [column names]\ntime avifileindex avitime\n\
             [AVI]\nrun_\nmp4\n\
             [data]\n120000.0 0001 10\n120000.5 0001 20\n120001.0 0002 0\n120001.5 0002 5\n",
        )
        .unwrap();
        let file = RacelogicFile::open(&vbo).unwrap();
        assert_eq!(file.videos.len(), 2);
        assert_eq!(file.videos[0].index, 1);
        assert_eq!(file.videos[0].filename, "run_0001.mp4");
        assert_eq!(file.videos[1].index, 2);
        assert_eq!(file.videos[1].filename, "run_0002.mp4");
        let at_first = file.video_reference_at(0);
        let at_second = file.video_reference_at(1_000_000_000);
        assert_eq!(at_first.file_index, Some(1));
        assert_eq!(at_second.file_index, Some(2));
    }

    /// A VBO whose car drives a straight line north past the gate at 1 Hz
    /// (rows 0..20), loses GPS for one row, then does it again (rows 21..41),
    /// with a CAN `Lap_Number` that resets to 0 at the gap (a dash reset) so
    /// the counter alone would swallow the second lap.
    fn gate_fixture(gate: &str, lap_numbers: &[i32]) -> String {
        // Gate at lat 1751.26125', long 4864.3685' (Daytona), direction of
        // travel marked 0.001' further north along the same longitude, so the
        // gate itself runs east-west.
        use std::fmt::Write;
        let mut data = String::new();
        for (row, lap) in lap_numbers.iter().enumerate() {
            let time = 120_000.0 + row as f64;
            if row == 20 {
                writeln!(data, "0 {time:.1} 0.0 0.0 0.0 0.0 0.0 0.0 1.0 0 0 0 {lap}").unwrap();
                continue;
            }
            // 20 rows per pass: lat climbs 0.0005'/row (~0.9 m) from 0.005'
            // south of the gate to 0.005' north of it.
            let pass_row = if row < 20 { row } else { row - 21 };
            let lat = 1751.26125 - 0.005 + pass_row as f64 * 0.000_5;
            writeln!(
                data,
                "8 {time:.1} {lat:.6} 4864.368500 100.0 0.0 10.0 0.0 1.0 1 0 0 {lap}"
            )
            .unwrap();
        }
        let builtin = BUILTIN_SHORT.join(" ");
        let builtin_header = BUILTIN_NAMES.join("\n");
        format!(
            "[header]\n{builtin_header}\nLap_Number\n[channel units]\n(null)\n\
             {gate}[column names]\n{builtin} Lap_Number\n[data]\n{data}"
        )
    }

    #[test]
    fn laps_come_from_gps_crossings_of_the_laptiming_gate() {
        let gate = "[laptiming]\nStart +4864.368500 +1751.261250 +4864.368500 +1751.262250 \u{ac} Start / Finish\n";
        // Two passes; the counter reads 1 for the first and resets to 0 then
        // 1 again for the second, which a high-water reading would ignore.
        let mut counter = vec![1; 20];
        counter.extend([0; 2]);
        counter.extend([1; 19]);
        let fixture = fixture(&gate_fixture(gate, &counter));
        let file = RacelogicFile::open(fixture.path()).unwrap();
        let laps = file.source_lap_metadata().expect("gate laps");
        // The gate latitude is reached exactly at pass row 10: 10 s into the
        // first pass and 31 s into the file for the second.
        let bounds: Vec<(i64, u64, u64, bool)> = laps
            .laps
            .iter()
            .map(|lap| (lap.number, lap.start_ns, lap.end_ns, lap.complete))
            .collect();
        assert_eq!(
            bounds,
            [
                (1, 0, 10_000_000_000, false),
                (2, 10_000_000_000, 31_000_000_000, true),
                (3, 31_000_000_000, 41_000_000_000, false),
            ]
        );
        assert!(file
            .diagnostics()
            .iter()
            .any(|d| d.code == "vbo.laps_from_gate"));
        // The generic recovery uses the authoritative laps, so the reset
        // counter no longer produces one 3 s lap.
        let metadata = motorsport_telemetry_core::read_source_metadata(&file);
        assert_eq!(metadata.laps.len(), 3);
        assert_eq!(metadata.valid_laps, 1);
    }

    #[test]
    fn metadata_only_gate_laps_respect_invalid_satellite_counts() {
        let gate = "[laptiming]\nStart +4864.368500 +1751.261250 +4864.368500 +1751.262250\n";
        for count in ["0", "NaN"] {
            let text = gate_fixture(gate, &[1; 41]).replace("\n8 ", &format!("\n{count} "));
            let fixture = fixture(&text);
            let full = RacelogicFile::open(fixture.path()).unwrap();
            let header = RacelogicFile::open_metadata(fixture.path()).unwrap();
            assert!(full.source_lap_metadata().is_none(), "{count}");
            assert_eq!(
                header.source_lap_metadata(),
                full.source_lap_metadata(),
                "{count}"
            );
        }
    }

    #[test]
    fn gate_far_from_the_car_falls_back_to_the_counter() {
        // Same drive, but the declared gate is at another circuit entirely.
        let gate = "[laptiming]\nStart +4881.221190 +1647.013530 +4881.227000 +1647.013560 \u{ac} Start / Finish\n";
        let fixture = fixture(&gate_fixture(gate, &[1; 41]));
        let file = RacelogicFile::open(fixture.path()).unwrap();
        assert!(file.source_lap_metadata().is_none());
        assert!(!file
            .diagnostics()
            .iter()
            .any(|d| d.code == "vbo.laps_from_gate"));
        // No [laptiming] at all: also no authoritative laps.
        let no_gate = self::fixture(&gate_fixture("", &[1; 41]));
        let file = RacelogicFile::open(no_gate.path()).unwrap();
        assert!(file.source_lap_metadata().is_none());
    }

    #[test]
    fn unreliable_gps_does_not_time_the_gate() {
        // Every other fix teleports 8 degrees north: a receiver that is not
        // tracking. The CAN counter must remain the lap source.
        let gate = "[laptiming]\nStart +4864.368500 +1751.261250 +4864.368500 +1751.262250 \u{ac} Start / Finish\n";
        let text = gate_fixture(gate, &[1; 41]);
        let broken = text
            .lines()
            .enumerate()
            .map(|(index, line)| {
                if index % 2 == 0 && line.starts_with("8 ") {
                    let mut tokens: Vec<String> = line.split(' ').map(str::to_owned).collect();
                    tokens[2] = format!("{:.6}", tokens[2].parse::<f64>().unwrap() + 480.0);
                    tokens.join(" ")
                } else {
                    line.to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let fixture = fixture(&broken);
        let file = RacelogicFile::open(fixture.path()).unwrap();
        assert!(file.source_lap_metadata().is_none());
        assert!(file
            .diagnostics()
            .iter()
            .any(|d| d.code == "vbo.gate_laps_skipped_gps_unreliable"));
    }

    #[test]
    fn a_long_time_gap_does_not_define_a_gate_crossing() {
        let gate = ["Start +4864.368500 +1751.261250 +4864.368500 +1751.262250"];
        let values = vec![vec![1751.26, 1751.27], vec![4864.3685; 2]];
        let mut diagnostics = Vec::new();
        let laps = gate_laps(
            &gate,
            &["lat", "long"],
            &values,
            &[0, 60_000_000_000],
            61_000_000_000,
            &mut diagnostics,
        );
        assert!(
            laps.is_none(),
            "two fixes a minute apart cannot time a crossing"
        );
    }

    #[test]
    fn gate_marks_are_parsed_and_widened_perpendicular_to_travel() {
        let marks = parse_gate(&[
            "Split +1.0 +2.0 +3.0 +4.0 \u{ac} S1",
            "Start +4864.368500 +1751.261250 +4864.367640 +1751.262290 \u{ac} Start / Finish",
        ])
        .unwrap();
        assert_eq!(marks[0], (4864.3685, 1751.26125));
        let gate = gate_metres(marks, marks[0]).unwrap();
        // 50 m wide, centred on the first mark, perpendicular to the
        // direction from the first mark to the second.
        let width = ((gate[1].0 - gate[0].0).powi(2) + (gate[1].1 - gate[0].1).powi(2)).sqrt();
        assert!((width - 2.0 * GATE_HALF_WIDTH_M).abs() < 1e-9);
        let centre = ((gate[0].0 + gate[1].0) / 2.0, (gate[0].1 + gate[1].1) / 2.0);
        assert!(centre.0.abs() < 1e-9 && centre.1.abs() < 1e-9);
        let travel = local_metres(marks[1], marks[0]);
        let along = (gate[1].0 - gate[0].0, gate[1].1 - gate[0].1);
        assert!((travel.0 * along.0 + travel.1 * along.1).abs() < 1e-9);
        // Identical marks give no direction and therefore no gate.
        assert!(parse_gate(&["Start +1.0 +2.0 +1.0 +2.0 \u{ac} x"]).is_none());
    }

    #[test]
    fn custom_channels_read_their_declared_units_in_order() {
        // The trailing non-builtin columns declare their units in `[channel
        // units]`, one entry per custom channel, aligned with the first custom
        // column at index `BUILTIN_NAMES.len()`. The first custom channel must
        // take units[0], not units[1].
        let builtin = BUILTIN_SHORT.join(" ");
        let builtin_header = BUILTIN_NAMES.join("\n");
        let fixture = fixture(&format!(
            "[header]\n{builtin_header}\ncustomA\ncustomB\n\
             [channel units]\ncustom-unit-a\ncustom-unit-b\n\
             [column names]\n{builtin} customA customB\n\
             [data]\n\
             120000.0 1 2 3 4 5 6 7 8 9 10 11 12 13\n\
             120001.0 1 2 3 4 5 6 7 8 9 10 11 12 13\n"
        ));
        let file = RacelogicFile::open(fixture.path()).unwrap();
        assert_eq!(file.channels.len(), 14);
        // The first builtin column keeps its spec-fixed unit (or none); the
        // custom channels must carry exactly their declared units in order.
        assert_eq!(file.channels[12].unit, "custom-unit-a");
        assert_eq!(file.channels[12].unit_source, UnitSource::Declared);
        assert_eq!(file.channels[13].unit, "custom-unit-b");
        assert_eq!(file.channels[13].unit_source, UnitSource::Declared);
        assert!(file.diagnostics().is_empty(), "{:?}", file.diagnostics());
    }

    #[test]
    fn surplus_leading_channel_unit_anchors_the_list_at_its_end() {
        // VBVDHD2 loggers write one more `[channel units]` entry than there
        // are custom columns: a leading "s" for `avisynctime`. Front-aligning
        // shifted every custom unit by one (Vehicle_Speed became "%"). The
        // last custom column must take the last unit, and the surplus entry
        // lands on the otherwise-unitless trailing builtin.
        let builtin = BUILTIN_SHORT.join(" ");
        let builtin_header = BUILTIN_NAMES.join("\n");
        let fixture = fixture(&format!(
            "[header]\n{builtin_header}\nEngine_Speed\nBrake_Pressure_Front\n\
             Throttle_Pedal\nVehicle_Speed\nGear\n\
             [channel units]\ns\nRPM\nbar\n%\nkmh\n(null)\n\
             [column names]\n{builtin} Engine_Speed Brake_Pressure_Front \
             Throttle_Pedal Vehicle_Speed Gear\n\
             [data]\n\
             120000.0 1 2 3 4 5 6 7 8 9 10 11 6000 40 80 250 5\n\
             120001.0 1 2 3 4 5 6 7 8 9 10 11 6000 40 80 250 5\n"
        ));
        let file = RacelogicFile::open(fixture.path()).unwrap();
        assert_eq!(file.channels.len(), 17);
        let unit_of = |name: &str| {
            file.channels
                .iter()
                .find(|channel| channel.name == name)
                .map(|channel| (channel.unit.as_str(), channel.unit_source))
                .unwrap()
        };
        assert_eq!(unit_of("avisynctime"), ("s", UnitSource::Declared));
        assert_eq!(unit_of("Engine_Speed"), ("RPM", UnitSource::Declared));
        assert_eq!(
            unit_of("Brake_Pressure_Front"),
            ("bar", UnitSource::Declared)
        );
        assert_eq!(unit_of("Throttle_Pedal"), ("%", UnitSource::Declared));
        assert_eq!(unit_of("Vehicle_Speed"), ("kmh", UnitSource::Declared));
        assert_eq!(unit_of("Gear"), ("", UnitSource::Unknown));
        assert!(file
            .diagnostics()
            .iter()
            .any(|d| d.code == "vbo.channel_units_count_mismatch"));
    }

    #[test]
    fn rejects_missing_data_and_time_sections() {
        let no_data = fixture("[header]\ntime\n");
        assert!(matches!(
            RacelogicFile::open(no_data.path()),
            Err(RacelogicError::Invalid { .. })
        ));
        let no_time = fixture("[column names]\nspeed\n[data]\n1\n2\n");
        assert!(matches!(
            RacelogicFile::open(no_time.path()),
            Err(RacelogicError::Invalid { .. })
        ));
    }

    #[test]
    fn clean_vbo_reports_no_diagnostics() {
        let file = RacelogicFile::from_bytes(
            "fixture.vbo",
            b"[column names]\ntime velocity\n[data]\n120000.0 10\n120000.5 20\n".to_vec(),
        )
        .unwrap();
        assert!(
            file.diagnostics().is_empty(),
            "unexpected diagnostics: {:?}",
            file.diagnostics()
        );
    }

    #[test]
    fn warns_on_unparsable_numeric_token() {
        let file = RacelogicFile::from_bytes(
            "fixture.vbo",
            b"[column names]\ntime velocity\n[data]\n120000.0 abc\n120000.5 20\n".to_vec(),
        )
        .unwrap();
        assert!(file
            .diagnostics()
            .iter()
            .any(|d| d.code == "vbo.value_unparsable" && d.channel.as_deref() == Some("velocity")));
    }

    #[test]
    fn warns_on_sample_period_defaulted() {
        // Both rows share the same timestamp so no positive delta exists and
        // there is no tsample column — the 100 ms default must be flagged.
        let file = RacelogicFile::from_bytes(
            "fixture.vbo",
            b"[column names]\ntime velocity\n[data]\n120000.0 10\n120000.0 20\n".to_vec(),
        )
        .unwrap();
        assert!(file
            .diagnostics()
            .iter()
            .any(|d| d.code == "vbo.sample_period_defaulted"));
    }

    #[test]
    fn warns_on_missing_column_names() {
        let file = RacelogicFile::from_bytes(
            "fixture.vbo",
            b"[header]\ntime\nvelocity\n[data]\n120000.0 10\n120000.5 20\n".to_vec(),
        )
        .unwrap();
        assert!(file
            .diagnostics()
            .iter()
            .any(|d| d.code == "vbo.column_names_missing"));
    }

    #[test]
    fn info_on_midnight_rollover_correction() {
        let file = RacelogicFile::from_bytes(
            "fixture.vbo",
            b"[column names]\ntime Gear\n[data]\n235959.5 3\n000000.0 4\n000000.5 4\n".to_vec(),
        )
        .unwrap();
        assert!(file
            .diagnostics()
            .iter()
            .any(|d| d.code == "vbo.time_rollover_corrected"));
    }

    #[test]
    fn vbo_time_of_day_plus_header_date_is_a_utc_instant() {
        // 26 Jan 2025 00:00:00 UTC = 1737849600 s; 12:00:00 UTC time of day.
        let fixture = fixture("File created on 26/01/2025 @ 12:23:57\n[column names]\ntime velocity\n[data]\n120000.00 10\n120000.50 20\n");
        let source = RacelogicFile::open(fixture.path()).unwrap();
        assert_eq!(
            source.utc_start_ns(),
            Some((1_737_849_600 + 12 * 3600) * 1_000_000_000)
        );
        assert_eq!(source.identity().date, "26/01/2025");
        assert_eq!(source.identity().time, "12:23:57");
    }

    #[test]
    fn vbo_without_a_header_date_has_no_utc_start() {
        let fixture =
            fixture("[column names]\ntime velocity\n[data]\n120000.00 10\n120000.50 20\n");
        let source = RacelogicFile::open(fixture.path()).unwrap();
        assert_eq!(source.utc_start_ns(), None);
        assert_eq!(header_date_days("31/13/2006"), None);
        assert_eq!(header_date_days("01/01/1970"), None); // before any VBOX
        assert_eq!(header_date_days("01/01/1980"), Some(3_652));
        assert_eq!(header_date_days("26/01/2025"), Some(20_114));
    }
}
