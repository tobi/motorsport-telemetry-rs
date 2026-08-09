#![doc = include_str!("../README.md")]
#![deny(missing_docs)]

use aim_telemetry::AimFile;
use cosworth_telemetry::CosworthFile;
use motec_telemetry::MotecFile;
use motorsport_telemetry_core::{
    group_sessions, read_source_metadata, Channel, FileMetadata, SessionMetadata, SourceIdentity,
    TelemetrySource, VideoReference,
};
use motorsport_track_atlas::{match_track, TrackMatch};
use racelogic_telemetry::RacelogicFile;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use thiserror::Error;

pub use motorsport_telemetry_core;
pub use motorsport_track_atlas;

/// Errors returned while selecting or opening a supported telemetry format.
#[derive(Debug, Error)]
pub enum TelemetryError {
    /// The path does not have a supported telemetry extension.
    #[error("unsupported telemetry file {0}")]
    Unsupported(String),
    /// The AiM MP4 parser rejected the input.
    #[error(transparent)]
    Aim(#[from] aim_telemetry::AimError),
    /// The Pi/Cosworth PDS parser rejected the input.
    #[error(transparent)]
    Cosworth(#[from] cosworth_telemetry::CosworthError),
    /// The MoTeC LD parser rejected the input.
    #[error(transparent)]
    Motec(#[from] motec_telemetry::MotecError),
    /// The Racelogic VBOX parser rejected the input.
    #[error(transparent)]
    Racelogic(#[from] racelogic_telemetry::RacelogicError),
}

/// An opened telemetry file backed by one of the supported format readers.
///
/// This enum implements [`TelemetrySource`], so callers can inspect channels
/// and samples without matching on the source format.
#[derive(Debug)]
pub enum TelemetryFile {
    /// AiM `aimd` telemetry embedded in an MP4 recording.
    Aim(AimFile),
    /// Pi/Cosworth PDS telemetry.
    Cosworth(CosworthFile),
    /// MoTeC LD telemetry.
    Motec(MotecFile),
    /// Racelogic VBOX VBO telemetry.
    Racelogic(RacelogicFile),
}

/// Opens a native telemetry file using its case-insensitive extension.
///
/// Supported extensions are `.mp4`, `.pds`, `.ld`, and `.vbo`. This function
/// selects a parser by extension; the selected parser still validates the file
/// contents.
pub fn open(path: impl AsRef<Path>) -> Result<TelemetryFile, TelemetryError> {
    let path = path.as_ref();
    match path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "mp4" => Ok(TelemetryFile::Aim(AimFile::open(path)?)),
        "pds" => Ok(TelemetryFile::Cosworth(CosworthFile::open(path)?)),
        "ld" => Ok(TelemetryFile::Motec(MotecFile::open(path)?)),
        "vbo" => Ok(TelemetryFile::Racelogic(RacelogicFile::open(path)?)),
        _ => Err(TelemetryError::Unsupported(path.display().to_string())),
    }
}

macro_rules! delegate {
    ($self:expr, $source:ident => $body:expr) => {
        match $self {
            TelemetryFile::Aim($source) => $body,
            TelemetryFile::Cosworth($source) => $body,
            TelemetryFile::Motec($source) => $body,
            TelemetryFile::Racelogic($source) => $body,
        }
    };
}
impl TelemetrySource for TelemetryFile {
    fn path(&self) -> &str {
        delegate!(self, source => source.path())
    }

    fn format(&self) -> &'static str {
        delegate!(self, source => source.format())
    }

    fn channels(&self) -> &[Channel] {
        delegate!(self, source => source.channels())
    }

    fn decode(&self, channel_index: usize, chunk_index: usize, local_index: u64) -> f64 {
        delegate!(self, source => source.decode(channel_index, chunk_index, local_index))
    }

    fn sample_time_ns(&self, channel_index: usize, chunk_index: usize, local_index: u64) -> u64 {
        delegate!(self, source => source.sample_time_ns(channel_index, chunk_index, local_index))
    }

    fn sample_at(&self, channel_index: usize, time_ns: u64, linear: bool) -> Option<f64> {
        delegate!(self, source => source.sample_at(channel_index, time_ns, linear))
    }

    fn absolute_time_range(&self) -> Option<motorsport_telemetry_core::AbsoluteTimeRange> {
        delegate!(self, source => source.absolute_time_range())
    }

    fn identity(&self) -> SourceIdentity {
        delegate!(self, source => source.identity())
    }

    fn source_lap_metadata(&self) -> Option<motorsport_telemetry_core::SourceLapMetadata> {
        delegate!(self, source => source.source_lap_metadata())
    }

    fn video_frame_count(&self) -> Option<u64> {
        delegate!(self, source => source.video_frame_count())
    }

    fn video_frame_at(&self, time_ns: u64) -> Option<u64> {
        delegate!(self, source => source.video_frame_at(time_ns))
    }

    fn video_presentation_offset_ns(&self) -> Option<i128> {
        delegate!(self, source => source.video_presentation_offset_ns())
    }

    fn video_presentation_time_ns(&self, time_ns: u64) -> Option<u64> {
        delegate!(self, source => source.video_presentation_time_ns(time_ns))
    }
}

/// Channel indexes selected for the facade's format-neutral signal roles.
///
/// A missing role is `None`. Indexes refer to [`TelemetrySource::channels`]
/// for the same file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SignalRoles {
    /// Vehicle or ground speed channel.
    pub speed: Option<usize>,
    /// Driver throttle channel.
    pub throttle: Option<usize>,
    /// Driver brake channel.
    pub brake: Option<usize>,
    /// Distance or progress within the current lap.
    pub lap_distance: Option<usize>,
    /// Current lap counter.
    pub lap_number: Option<usize>,
    /// WGS84 latitude channel.
    pub latitude: Option<usize>,
    /// WGS84 longitude channel.
    pub longitude: Option<usize>,
}

/// Format-neutral values sampled at one file-relative timestamp.
///
/// Values remain `None` when no suitable source channel exists or its unit
/// cannot be converted safely.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NormalizedSample {
    /// Speed in metres per second.
    pub speed_mps: Option<f64>,
    /// Throttle position in the inclusive range `0.0..=1.0`.
    pub throttle_fraction: Option<f64>,
    /// Brake position in the inclusive range `0.0..=1.0`.
    pub brake_fraction: Option<f64>,
    /// Source lap number rounded to an integer.
    pub lap_number: Option<i64>,
    /// Progress through the current lap in the range `0.0..=1.0`.
    pub lap_progress: Option<f64>,
    /// WGS84 latitude in degrees.
    pub latitude_deg: Option<f64>,
    /// WGS84 longitude in degrees.
    pub longitude_deg: Option<f64>,
}

impl TelemetryFile {
    /// Derives the format-neutral metadata summary for this file.
    pub fn metadata(&self) -> FileMetadata {
        read_source_metadata(self)
    }

    /// Infers normalized roles from known source channel names.
    ///
    /// Role inference never assigns or guesses units; unit validation happens
    /// when values are normalized.
    pub fn signal_roles(&self) -> SignalRoles {
        infer_roles(self.channels())
    }

    /// Builds a reusable normalization context.
    ///
    /// Signal roles and track matching are resolved once. Lap metadata remains
    /// lazy and, if needed as a fallback, is computed once for the lifetime of
    /// the context rather than once per sample.
    pub fn normalizer(&self) -> TelemetryNormalizer<'_> {
        TelemetryNormalizer::new(self, self.signal_roles(), self.match_track())
    }

    /// Matches sampled GPS positions to the nearest track within 50 km.
    ///
    /// Returns `None` when suitable GPS channels or valid units are absent, no
    /// track is close enough, or the matched centerline cannot be decoded.
    pub fn match_track(&self) -> Option<TrackContext> {
        let roles = self.signal_roles();
        let (lat_index, lon_index) = roles.latitude.zip(roles.longitude)?;
        let duration = self.channels()[lat_index]
            .duration_ns
            .min(self.channels()[lon_index].duration_ns);
        let mut lat_sum = 0.0;
        let mut lon_sum = 0.0;
        let mut count = 0usize;
        for sample in 0..32u64 {
            let time = duration.saturating_mul(sample) / 32;
            let lat = normalize_coordinate(
                self.sample_at(lat_index, time, true)?,
                &self.channels()[lat_index].unit,
            )?;
            let lon = normalize_coordinate(
                self.sample_at(lon_index, time, true)?,
                &self.channels()[lon_index].unit,
            )?;
            if lat.is_finite() && lon.is_finite() && (lat != 0.0 || lon != 0.0) {
                lat_sum += lat;
                lon_sum += lon;
                count += 1;
            }
        }
        let matched = match_track(lat_sum / count as f64, lon_sum / count as f64, 50_000.0)?;
        TrackContext::new(matched).ok()
    }
}

/// Reusable state for high-throughput normalized sampling.
#[derive(Debug)]
pub struct TelemetryNormalizer<'a> {
    source: &'a TelemetryFile,
    roles: SignalRoles,
    track: Option<TrackContext>,
    laps: OnceLock<Vec<motorsport_telemetry_core::LapMetadata>>,
}

impl<'a> TelemetryNormalizer<'a> {
    /// Creates a normalizer with caller-selected signal roles and track.
    pub fn new(source: &'a TelemetryFile, roles: SignalRoles, track: Option<TrackContext>) -> Self {
        Self {
            source,
            roles,
            track,
            laps: OnceLock::new(),
        }
    }

    /// Returns the channel roles used by this normalizer.
    pub fn roles(&self) -> &SignalRoles {
        &self.roles
    }

    /// Returns the matched track context, when available.
    pub fn track(&self) -> Option<&TrackContext> {
        self.track.as_ref()
    }

    /// Returns the normalized values at a file-relative timestamp.
    pub fn sample(&self, time_ns: u64) -> NormalizedSample {
        normalize_sample(
            self.source,
            time_ns,
            &self.roles,
            self.track.as_ref(),
            || {
                let laps = self.laps.get_or_init(|| self.source.metadata().laps);
                lap_progress_from_metadata(laps, time_ns)
            },
        )
    }
}

fn normalize_sample(
    source: &TelemetryFile,
    time_ns: u64,
    roles: &SignalRoles,
    track: Option<&TrackContext>,
    lap_fallback: impl FnOnce() -> Option<f64>,
) -> NormalizedSample {
    let value = |index: Option<usize>, linear| {
        index
            .and_then(|index| source.sample_at(index, time_ns, linear))
            .filter(|value| value.is_finite())
    };
    let speed_mps = roles.speed.and_then(|index| {
        let raw = value(Some(index), true)?;
        normalize_speed(raw, &source.channels()[index].unit)
    });
    let throttle_fraction = roles.throttle.and_then(|index| {
        normalize_fraction(value(Some(index), true)?, &source.channels()[index].unit)
    });
    let brake_fraction = roles.brake.and_then(|index| {
        normalize_fraction(value(Some(index), true)?, &source.channels()[index].unit)
    });
    let latitude_deg = roles.latitude.and_then(|index| {
        normalize_coordinate(value(Some(index), true)?, &source.channels()[index].unit)
    });
    let longitude_deg = roles.longitude.and_then(|index| {
        normalize_coordinate(value(Some(index), true)?, &source.channels()[index].unit)
    });
    let lap_number = value(roles.lap_number, false)
        .filter(|value| value.is_finite())
        .map(|value| value.round() as i64);
    let lap_progress = roles
        .lap_distance
        .and_then(|index| {
            let raw = value(Some(index), true)?;
            normalize_lap_distance(raw, &source.channels()[index].unit, track)
        })
        .or_else(|| {
            latitude_deg
                .zip(longitude_deg)
                .and_then(|(lat, lon)| track.and_then(|track| track.progress(lat, lon)))
        })
        .or_else(lap_fallback);
    NormalizedSample {
        speed_mps,
        throttle_fraction,
        brake_fraction,
        lap_number,
        lap_progress,
        latitude_deg,
        longitude_deg,
    }
}

fn lap_progress_from_metadata(
    laps: &[motorsport_telemetry_core::LapMetadata],
    time_ns: u64,
) -> Option<f64> {
    laps.iter()
        .find(|lap| time_ns >= lap.start_ns && time_ns < lap.end_ns)
        .filter(|lap| lap.duration_ns > 0)
        .map(|lap| time_ns.saturating_sub(lap.start_ns) as f64 / lap.duration_ns as f64)
}

/// A matched track plus precomputed centerline distances for GPS projection.
#[derive(Debug, Clone)]
pub struct TrackContext {
    /// The selected facility and layout from the offline track atlas.
    pub matched: TrackMatch,
    centerline: Vec<[f64; 2]>,
    cumulative_m: Vec<f64>,
    total_m: f64,
}

impl TrackContext {
    /// Builds projection state from a track-atlas match.
    ///
    /// The error indicates that the layout's embedded centerline is not valid
    /// GeoJSON. An empty or one-point centerline constructs successfully but
    /// cannot produce progress values.
    pub fn new(matched: TrackMatch) -> Result<Self, serde_json::Error> {
        let value: serde_json::Value = serde_json::from_str(matched.layout.centerline_geojson)?;
        let coordinates = &value["features"][0]["geometry"]["coordinates"];
        let centerline = coordinates
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|point| {
                let point = point.as_array()?;
                Some([point.first()?.as_f64()?, point.get(1)?.as_f64()?])
            })
            .collect::<Vec<_>>();
        let mut cumulative_m = Vec::with_capacity(centerline.len());
        cumulative_m.push(0.0);
        for pair in centerline.windows(2) {
            let distance = haversine_m(pair[0][1], pair[0][0], pair[1][1], pair[1][0]);
            cumulative_m.push(cumulative_m.last().copied().unwrap_or(0.0) + distance);
        }
        let total_m = cumulative_m.last().copied().unwrap_or(0.0);
        Ok(Self {
            matched,
            centerline,
            cumulative_m,
            total_m,
        })
    }

    /// Projects a WGS84 point onto the centerline and returns lap progress.
    ///
    /// Progress is clamped to `0.0..=1.0`; `None` means that the layout has no
    /// usable centerline.
    pub fn progress(&self, latitude: f64, longitude: f64) -> Option<f64> {
        if self.centerline.len() < 2 || self.total_m <= 0.0 {
            return None;
        }
        self.centerline
            .windows(2)
            .enumerate()
            .map(|(index, segment)| {
                let (fraction, distance) =
                    project_segment(latitude, longitude, segment[0], segment[1]);
                let progress_m = self.cumulative_m[index]
                    + fraction * (self.cumulative_m[index + 1] - self.cumulative_m[index]);
                (distance, progress_m / self.total_m)
            })
            .min_by(|left, right| left.0.total_cmp(&right.0))
            .map(|(_, progress)| progress.clamp(0.0, 1.0))
    }
}

/// Files grouped into one session using internal clocks and identity.
#[derive(Debug)]
pub struct TelemetrySession {
    /// Open files in session order.
    pub files: Vec<TelemetryFile>,
    /// Per-file summaries in the same order as [`Self::files`].
    pub file_metadata: Vec<FileMetadata>,
    /// Metadata merged across the session.
    pub metadata: SessionMetadata,
}

/// The source, video, driver, and lap state at one session timestamp.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionPosition {
    /// Requested session-relative timestamp in nanoseconds.
    pub session_time_ns: u64,
    /// Index into [`TelemetrySession::files`].
    pub file_index: usize,
    /// Path reported by the selected telemetry source.
    pub source_path: PathBuf,
    /// Corresponding file-relative timestamp in nanoseconds.
    pub file_time_ns: u64,
    /// Video linkage reported at the file timestamp.
    pub video: VideoReference,
    /// Internal driver identifier, when the format exposes one.
    pub driver_id: Option<i64>,
    /// Current lap number, when a supported channel exists.
    pub lap_number: Option<i64>,
}

/// Opens files and groups compatible adjacent recordings into sessions.
///
/// `max_gap_ns` is the largest allowed gap between consecutive files with the
/// same internal session key. Inputs lacking compatible absolute clocks and
/// session keys are not joined. A failure to open any input returns an error
/// and no partial session list.
pub fn open_sessions<I, P>(
    paths: I,
    max_gap_ns: u64,
) -> Result<Vec<TelemetrySession>, TelemetryError>
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    let opened = paths.into_iter().map(open).collect::<Result<Vec<_>, _>>()?;
    let metadata = opened
        .iter()
        .map(TelemetryFile::metadata)
        .collect::<Vec<_>>();
    let grouped = group_sessions(&metadata, max_gap_ns);
    let mut files = opened.into_iter().map(Some).collect::<Vec<_>>();
    Ok(grouped
        .into_iter()
        .map(|session| {
            let selected_files = session
                .files
                .iter()
                .map(|index| files[*index].take().expect("session file used once"))
                .collect::<Vec<_>>();
            let selected_metadata = session
                .files
                .iter()
                .map(|index| metadata[*index].clone())
                .collect();
            TelemetrySession {
                files: selected_files,
                file_metadata: selected_metadata,
                metadata: session,
            }
        })
        .collect())
}

impl TelemetrySession {
    /// Resolves a session-relative timestamp to its containing source file.
    ///
    /// Returns `None` for gaps, out-of-range timestamps, or sessions without a
    /// usable absolute clock.
    pub fn position(&self, session_time_ns: u64) -> Option<SessionPosition> {
        let base = self.metadata.absolute_start_ns?;
        for (index, metadata) in self.file_metadata.iter().enumerate() {
            let offset = u64::try_from(metadata.clock_offset_ns? - i128::from(base)).ok()?;
            if session_time_ns < offset
                || session_time_ns >= offset.saturating_add(metadata.duration_ns)
            {
                continue;
            }
            let file_time_ns = session_time_ns - offset;
            let file = &self.files[index];
            let roles = file.signal_roles();
            let driver_id =
                semantic_value(file, file_time_ns, &["driverid", "driver", "driverindex"]);
            let lap_number = roles
                .lap_number
                .and_then(|channel| file.sample_at(channel, file_time_ns, false))
                .map(|value| value.round() as i64);
            return Some(SessionPosition {
                session_time_ns,
                file_index: index,
                source_path: PathBuf::from(file.path()),
                file_time_ns,
                video: file.video_reference_at(file_time_ns),
                driver_id,
                lap_number,
            });
        }
        None
    }
}

fn infer_roles(channels: &[Channel]) -> SignalRoles {
    SignalRoles {
        speed: find(
            channels,
            &[
                "groundspeed",
                "speedref",
                "corrspeed",
                "vehiclespeed",
                "gpsspeed",
                "speed",
                "velocitykmh",
            ],
        ),
        throttle: find(
            channels,
            &[
                "throttlepos",
                "driverthrottlepos",
                "throttlepedal",
                "pedalpos",
                "throttle",
            ],
        ),
        brake: find(
            channels,
            &[
                "brakepedalpos",
                "driverbrakepressure",
                "brakepressure",
                "pbrakefront",
                "brake",
            ],
        ),
        lap_distance: find(
            channels,
            &[
                "lapdistancecorrected",
                "lapdistance",
                "lapdist",
                "lapdistpct",
                "lapprogression",
                "lapprogress",
                "lapprogresspct",
                "linelapdistancel",
                "distance",
            ],
        ),
        lap_number: find(
            channels,
            &[
                "lapnumber",
                "lapnum",
                "lapcount",
                "lapcounter",
                "currentlap",
                "lap",
            ],
        ),
        latitude: find(channels, &["gpslatitude", "latitude", "gpslat", "lat"]),
        longitude: find(
            channels,
            &["gpslongitude", "longitude", "gpslon", "lon", "long"],
        ),
    }
}

fn find(channels: &[Channel], names: &[&str]) -> Option<usize> {
    names.iter().find_map(|wanted| {
        channels
            .iter()
            .position(|channel| channel.sample_count > 0 && normalized_eq(&channel.name, wanted))
    })
}

fn semantic_value(file: &TelemetryFile, time_ns: u64, names: &[&str]) -> Option<i64> {
    find(file.channels(), names)
        .and_then(|channel| file.sample_at(channel, time_ns, false))
        .filter(|value| value.is_finite())
        .map(|value| value.round() as i64)
}

fn normalized_eq(value: &str, wanted: &str) -> bool {
    value
        .bytes()
        .filter(u8::is_ascii_alphanumeric)
        .map(|byte| byte.to_ascii_lowercase())
        .eq(wanted.bytes())
}

fn normalize_speed(value: f64, unit: &str) -> Option<f64> {
    if unit.is_empty() {
        return None;
    }
    motorsport_telemetry_core::convert(value, unit, "m/s").ok()
}

fn normalize_fraction(value: f64, unit: &str) -> Option<f64> {
    match unit.trim().to_ascii_lowercase().as_str() {
        "%" | "percent" => Some((value / 100.0).clamp(0.0, 1.0)),
        "ratio" | "fraction" => Some(value.clamp(0.0, 1.0)),
        _ => None,
    }
}

fn normalize_coordinate(value: f64, unit: &str) -> Option<f64> {
    match unit.trim().to_ascii_lowercase().as_str() {
        "deg" | "degree" | "degrees" | "°" => Some(value),
        "rad" | "radian" | "radians" => Some(value.to_degrees()),
        "min" | "arcmin" | "arcminute" => Some(value / 60.0),
        _ => None,
    }
}

fn normalize_lap_distance(value: f64, unit: &str, track: Option<&TrackContext>) -> Option<f64> {
    match unit.trim().to_ascii_lowercase().as_str() {
        "%" | "percent" => Some((value / 100.0).rem_euclid(1.0)),
        "ratio" | "fraction" => Some(value.rem_euclid(1.0)),
        "m" | "meter" | "metre" => track
            .and_then(|track| track.matched.layout.length_m)
            .filter(|length| *length > 0.0)
            .map(|length| value.rem_euclid(length) / length),
        _ => None,
    }
}

fn haversine_m(a_lat: f64, a_lon: f64, b_lat: f64, b_lon: f64) -> f64 {
    let radius = 6_371_000.0;
    let lat1 = a_lat.to_radians();
    let lat2 = b_lat.to_radians();
    let dlat = lat2 - lat1;
    let dlon = (b_lon - a_lon).to_radians();
    let h = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
    2.0 * radius * h.sqrt().asin()
}

fn project_segment(latitude: f64, longitude: f64, a: [f64; 2], b: [f64; 2]) -> (f64, f64) {
    let mean_lat = latitude.to_radians();
    let scale_x = mean_lat.cos() * 111_320.0;
    let scale_y = 110_540.0;
    let ax = (a[0] - longitude) * scale_x;
    let ay = (a[1] - latitude) * scale_y;
    let bx = (b[0] - longitude) * scale_x;
    let by = (b[1] - latitude) * scale_y;
    let dx = bx - ax;
    let dy = by - ay;
    let length_sq = dx * dx + dy * dy;
    let t = if length_sq == 0.0 {
        0.0
    } else {
        (-(ax * dx + ay * dy) / length_sq).clamp(0.0, 1.0)
    };
    let px = ax + t * dx;
    let py = ay + t * dy;
    (t, px.hypot(py))
}
