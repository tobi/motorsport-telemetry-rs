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
use thiserror::Error;

pub use motorsport_telemetry_core;
pub use motorsport_track_atlas;

#[derive(Debug, Error)]
pub enum TelemetryError {
    #[error("unsupported telemetry file {0}")]
    Unsupported(String),
    #[error(transparent)]
    Aim(#[from] aim_telemetry::AimError),
    #[error(transparent)]
    Cosworth(#[from] cosworth_telemetry::CosworthError),
    #[error(transparent)]
    Motec(#[from] motec_telemetry::MotecError),
    #[error(transparent)]
    Racelogic(#[from] racelogic_telemetry::RacelogicError),
}

#[derive(Debug)]
pub enum TelemetryFile {
    Aim(AimFile),
    Cosworth(CosworthFile),
    Motec(MotecFile),
    Racelogic(RacelogicFile),
}

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

    fn video_frame_count(&self) -> Option<u64> {
        delegate!(self, source => source.video_frame_count())
    }

    fn video_frame_at(&self, time_ns: u64) -> Option<u64> {
        delegate!(self, source => source.video_frame_at(time_ns))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SignalRoles {
    pub speed: Option<usize>,
    pub throttle: Option<usize>,
    pub brake: Option<usize>,
    pub lap_distance: Option<usize>,
    pub lap_number: Option<usize>,
    pub latitude: Option<usize>,
    pub longitude: Option<usize>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct NormalizedSample {
    pub speed_mps: Option<f64>,
    pub throttle_fraction: Option<f64>,
    pub brake_fraction: Option<f64>,
    pub lap_number: Option<i64>,
    pub lap_progress: Option<f64>,
    pub latitude_deg: Option<f64>,
    pub longitude_deg: Option<f64>,
}

impl TelemetryFile {
    pub fn metadata(&self) -> FileMetadata {
        read_source_metadata(self)
    }

    pub fn signal_roles(&self) -> SignalRoles {
        infer_roles(self.channels())
    }

    pub fn normalized_sample(
        &self,
        time_ns: u64,
        roles: &SignalRoles,
        track: Option<&TrackContext>,
    ) -> NormalizedSample {
        let value = |index: Option<usize>, linear| {
            index.and_then(|index| self.sample_at(index, time_ns, linear))
        };
        let speed_mps = roles.speed.and_then(|index| {
            let raw = value(Some(index), true)?;
            normalize_speed(raw, &self.channels()[index].unit)
        });
        let throttle_fraction = roles.throttle.and_then(|index| {
            normalize_fraction(value(Some(index), true)?, &self.channels()[index].unit)
        });
        let brake_fraction = roles.brake.and_then(|index| {
            normalize_fraction(value(Some(index), true)?, &self.channels()[index].unit)
        });
        let latitude_deg = roles.latitude.and_then(|index| {
            normalize_coordinate(value(Some(index), true)?, &self.channels()[index].unit)
        });
        let longitude_deg = roles.longitude.and_then(|index| {
            normalize_coordinate(value(Some(index), true)?, &self.channels()[index].unit)
        });
        let lap_number = value(roles.lap_number, false)
            .filter(|value| value.is_finite())
            .map(|value| value.round() as i64);
        let lap_progress = roles
            .lap_distance
            .and_then(|index| {
                let raw = value(Some(index), true)?;
                normalize_lap_distance(raw, &self.channels()[index].unit, track)
            })
            .or_else(|| {
                latitude_deg
                    .zip(longitude_deg)
                    .and_then(|(lat, lon)| track.and_then(|track| track.progress(lat, lon)))
            })
            .or_else(|| {
                self.metadata()
                    .laps
                    .iter()
                    .find(|lap| time_ns >= lap.start_ns && time_ns < lap.end_ns)
                    .filter(|lap| lap.duration_ns > 0)
                    .map(|lap| time_ns.saturating_sub(lap.start_ns) as f64 / lap.duration_ns as f64)
            });
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

#[derive(Debug, Clone)]
pub struct TrackContext {
    pub matched: TrackMatch,
    centerline: Vec<[f64; 2]>,
    cumulative_m: Vec<f64>,
    total_m: f64,
}

impl TrackContext {
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

#[derive(Debug)]
pub struct TelemetrySession {
    pub files: Vec<TelemetryFile>,
    pub file_metadata: Vec<FileMetadata>,
    pub metadata: SessionMetadata,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionPosition {
    pub session_time_ns: u64,
    pub file_index: usize,
    pub source_path: PathBuf,
    pub file_time_ns: u64,
    pub video: VideoReference,
    pub driver_id: Option<i64>,
    pub lap_number: Option<i64>,
}

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
            .position(|channel| channel.sample_count > 0 && normalized(&channel.name) == *wanted)
    })
}

fn semantic_value(file: &TelemetryFile, time_ns: u64, names: &[&str]) -> Option<i64> {
    find(file.channels(), names)
        .and_then(|channel| file.sample_at(channel, time_ns, false))
        .filter(|value| value.is_finite())
        .map(|value| value.round() as i64)
}

fn normalized(name: &str) -> String {
    name.chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
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
