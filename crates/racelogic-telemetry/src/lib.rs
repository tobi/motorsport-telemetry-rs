#![doc = include_str!("../README.md")]
#![deny(missing_docs)]

use memmap2::Mmap;
use motorsport_telemetry_core::{
    Channel, Chunk, SampleType, TelemetrySource, UnitSource, VideoFileRef,
};
use std::fs::File;
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
    values: Vec<Vec<f64>>,
    absolute_start_ns: u64,
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
            if let Some((date, time)) = created.split_once(" at ") {
                result.created_date = date.trim().to_owned();
                result.created_time = time.trim().to_owned();
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
            _ => {}
        }
    }
    result
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
        name.to_ascii_lowercase()
            .replace([' ', '_', '-'], "")
            .as_str(),
        "time"
            | "tsample"
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
        let file = File::open(path).map_err(|source| RacelogicError::Io {
            path: display.clone(),
            source,
        })?;
        // SAFETY: the read-only mapping remains valid for this parse and
        // callers must not truncate or rewrite the file concurrently.
        let mapping = unsafe { Mmap::map(&file) }.map_err(|source| RacelogicError::Io {
            path: display.clone(),
            source,
        })?;
        Self::from_slice_mode(display, &mapping, metadata_only)
    }

    /// Parses VBO telemetry from an owned byte buffer.
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
        let text: &str = match std::str::from_utf8(bytes) {
            Ok(text) => text,
            Err(_) => {
                fallback = bytes
                    .iter()
                    .map(|&byte| char::from(byte))
                    .collect::<String>();
                &fallback
            }
        };
        let parsed = sections(text);
        if parsed.data.is_empty() {
            return Err(invalid(&display, "missing or empty [data] section"));
        }
        let short_names: Vec<&str> = if parsed.column_names.is_empty() {
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
        for line in &parsed.data {
            if line.split_whitespace().nth(1).is_none() {
                continue;
            }
            rows += 1;
            let mut tokens = line.split_whitespace();
            for (column, output) in values.iter_mut().enumerate() {
                if !selected[column] {
                    tokens.next();
                    continue;
                }
                output.push(
                    tokens
                        .next()
                        .and_then(|token| token.parse().ok())
                        .unwrap_or(f64::NAN),
                );
            }
        }
        if rows == 0 {
            return Err(invalid(&display, "no valid data rows"));
        }
        let time_column = short_names
            .iter()
            .position(|name| name.eq_ignore_ascii_case("time"))
            .ok_or_else(|| invalid(&display, "no time column"))?;
        let first = time_seconds(values[time_column][0]);
        let mut time_ns = Vec::with_capacity(rows);
        for value in &mut values[time_column] {
            let mut seconds = time_seconds(*value);
            if seconds < first - 43200.0 {
                seconds += 86400.0;
            }
            *value = seconds - first;
            time_ns.push((*value * 1e9).round().max(0.0) as u64);
        }
        let sample_period = short_names
            .iter()
            .position(|name| name.eq_ignore_ascii_case("tsample"))
            .and_then(|index| values[index].iter().copied().find(|value| *value > 0.0))
            .map(|seconds| (seconds * 1e9).round() as u64)
            .or_else(|| {
                time_ns
                    .windows(2)
                    .map(|pair| pair[1].saturating_sub(pair[0]))
                    .find(|delta| *delta > 0)
            })
            .unwrap_or(100_000_000);
        let duration = time_ns
            .last()
            .copied()
            .unwrap_or(0)
            .saturating_add(sample_period);

        let mut channels = Vec::with_capacity(count);
        for index in 0..count {
            let name: String = match parsed.header.get(index) {
                Some(declared) => (*declared).to_owned(),
                None if index < BUILTIN_NAMES.len() => BUILTIN_NAMES[index].to_owned(),
                None => short_names[index].to_owned(),
            };
            let custom_unit_index = index.saturating_sub(BUILTIN_NAMES.len());
            // Builtin VBOX columns have units fixed by the format spec; the
            // trailing custom columns declare theirs in [channel units].
            let (unit, unit_source) = if index < BUILTIN_NAMES.len() {
                let builtin = builtin_unit(short_names[index]);
                if builtin.is_empty() {
                    (String::new(), UnitSource::Unknown)
                } else {
                    (builtin.to_owned(), UnitSource::SpecDefault)
                }
            } else {
                match parsed
                    .units
                    .get(custom_unit_index)
                    .filter(|unit| **unit != "(null)" && !unit.is_empty())
                {
                    Some(declared) => ((*declared).to_owned(), UnitSource::Declared),
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
        let videos = discover_videos(&display, &parsed.avi, &short_names, &values);
        Ok(Self {
            path: display,
            channels,
            time_ns,
            date: parsed.created_date,
            recording_time: parsed.created_time,
            videos,
            values,
            absolute_start_ns: (first * 1e9).round().max(0.0) as u64,
        })
    }
}

fn discover_videos(
    path: &str,
    avi: &[&str],
    short_names: &[&str],
    values: &[Vec<f64>],
) -> Vec<VideoFileRef> {
    let prefix = avi.first().copied().unwrap_or("");
    let ext = avi
        .get(1)
        .copied()
        .unwrap_or("avi")
        .trim_start_matches('.')
        .to_ascii_lowercase();
    let parent = Path::new(path).parent();
    let mut indices = std::collections::BTreeSet::new();
    if let Some(column) = short_names
        .iter()
        .position(|name| name.eq_ignore_ascii_case("avifileindex"))
    {
        for value in &values[column] {
            if value.is_finite() && *value >= 0.0 {
                indices.insert(value.round() as u32);
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
            let present = parent
                .map(|dir| dir.join(&filename).is_file())
                .unwrap_or(false);
            let _ = present;
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
    fn decode(&self, channel_index: usize, _chunk_index: usize, local_index: u64) -> f64 {
        self.values[channel_index][local_index as usize]
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
    fn sample_time_ns(&self, _channel_index: usize, _chunk_index: usize, local_index: u64) -> u64 {
        self.time_ns[local_index as usize]
    }
    fn sample_at(&self, channel_index: usize, time_ns: u64, linear: bool) -> Option<f64> {
        if time_ns >= self.channels[channel_index].duration_ns {
            return None;
        }
        let upper = self.time_ns.partition_point(|time| *time <= time_ns);
        let lower = upper.saturating_sub(1).min(self.time_ns.len() - 1);
        let a = self.values[channel_index][lower];
        if !linear
            || self.channels[channel_index].uses_step_interpolation()
            || upper >= self.time_ns.len()
        {
            return Some(a);
        }
        let interval = self.time_ns[upper].saturating_sub(self.time_ns[lower]);
        if interval == 0 {
            return Some(a);
        }
        let fraction = time_ns.saturating_sub(self.time_ns[lower]) as f64 / interval as f64;
        Some(a + (self.values[channel_index][upper] - a) * fraction)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

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
}
