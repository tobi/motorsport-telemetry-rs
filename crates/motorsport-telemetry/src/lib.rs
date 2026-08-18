#![doc = include_str!("../README.md")]
#![deny(missing_docs)]

use aim_telemetry::AimFile;
use cosworth_telemetry::CosworthFile;
use motec_telemetry::MotecFile;
use motorsport_telemetry_core::names;
use motorsport_telemetry_core::{
    group_sessions, implies_decode_fault, validate_source_with, Channel, Diagnostics, FileMetadata,
    SessionMetadata, TelemetrySource, ValidateOptions, VideoReference,
};
use motorsport_track_atlas::{match_track, TrackMatch};
use racelogic_telemetry::RacelogicFile;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use telemetry_format::{is_jsonl_path, is_jsonl_zstd_path, JsonlRecording, NativeRecording};
use thiserror::Error;

pub use motorsport_telemetry_core;
pub use motorsport_track_atlas;
/// Current `.telemetry` catalog version written by this crate.
pub use telemetry_format::FORMAT_VERSION;
/// Current Motorsport Telemetry JSONL (MTJ) document version.
pub use telemetry_format::JSONL_VERSION;
/// Default zstd level for compressed MTJ documents.
pub use telemetry_format::JSONL_ZSTD_LEVEL;

/// An opened telemetry file backed by one of the supported format readers.
///
/// The concrete reader is boxed behind the shared [`TelemetrySource`] trait, so
/// callers can inspect channels and samples without matching on the source
/// format. The blanket `impl TelemetrySource for Box<T>` forwards every method.
pub type TelemetryFile = Box<dyn TelemetrySource>;

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
    /// The native `.telemetry` parser rejected the input.
    #[error(transparent)]
    Telemetry(#[from] telemetry_format::TelemetryFormatError),
}

/// Opens a native telemetry file using its case-insensitive extension.
///
/// Supported extensions are `.mp4`, `.pds`, `.ld`, `.vbo`, `.telemetry`,
/// `.telemetry.jsonl`, `.jsonl`, `.mtj`, `.telemetry.ext.jsonl`, and those
/// names with a `.zstd` or `.zst` suffix.
/// This function selects a parser by extension; the selected parser still
/// validates the file contents. Opening a writable `.telemetry` file older
/// than [`FORMAT_VERSION`] rewrites it in place.
pub fn open(path: impl AsRef<Path>) -> Result<TelemetryFile, TelemetryError> {
    let path = path.as_ref();
    if is_jsonl_path(path) {
        return Ok(Box::new(JsonlRecording::open(path)?));
    }
    match extension(path).as_str() {
        "mp4" => Ok(Box::new(AimFile::open(path)?)),
        "pds" => Ok(Box::new(CosworthFile::open(path)?)),
        "ld" => Ok(Box::new(MotecFile::open(path)?)),
        "vbo" => Ok(Box::new(RacelogicFile::open(path)?)),
        "telemetry" => Ok(Box::new(NativeRecording::open(path)?)),
        _ => Err(TelemetryError::Unsupported(path.display().to_string())),
    }
}

/// Opens a telemetry file for fast metadata and lap-filmstrip construction.
///
/// The returned source preserves every signal needed to derive
/// [`FileMetadata::laps`] while formats with expensive bulk data may retain
/// only representative samples for unrelated channels. Use [`open`] when
/// complete signal arrays or exact video-frame indexing are required.
pub fn open_metadata(path: impl AsRef<Path>) -> Result<TelemetryFile, TelemetryError> {
    let path = path.as_ref();
    if is_jsonl_path(path) {
        return Ok(Box::new(JsonlRecording::open(path)?));
    }
    match extension(path).as_str() {
        "mp4" => Ok(Box::new(AimFile::open_index(path)?)),
        "pds" => Ok(Box::new(CosworthFile::open(path)?)),
        "ld" => Ok(Box::new(MotecFile::open(path)?)),
        "vbo" => Ok(Box::new(RacelogicFile::open_metadata(path)?)),
        "telemetry" => Ok(Box::new(NativeRecording::open(path)?)),
        _ => Err(TelemetryError::Unsupported(path.display().to_string())),
    }
}

/// Quickly returns all format-neutral lap intervals needed by a session or
/// video filmstrip.
///
/// This is the stable public lap-summary API. Parsers may obtain the result
/// from native lap packets, sidecars, counters, or timer resets; callers do not
/// need to inspect the optional [`TelemetrySource::source_lap_metadata`] hook.
pub fn read_lap_metadata(
    path: impl AsRef<Path>,
) -> Result<Vec<motorsport_telemetry_core::LapMetadata>, TelemetryError> {
    if is_telemetry(path.as_ref()) {
        return Ok(telemetry_format::read_laps(path)?);
    }
    if is_jsonl_path(path.as_ref()) {
        return Ok(JsonlRecording::open(path)?.metadata().laps);
    }
    Ok(open_metadata(path)?.metadata().laps)
}

/// Returns the number of complete flying laps.
///
/// For `.telemetry` this is a header scalar and does not scan samples.
pub fn read_valid_laps(path: impl AsRef<Path>) -> Result<u32, TelemetryError> {
    if is_telemetry(path.as_ref()) {
        return Ok(telemetry_format::read_valid_laps(path)?);
    }
    if is_jsonl_path(path.as_ref()) {
        return Ok(JsonlRecording::open(path)?.metadata().valid_laps);
    }
    Ok(open_metadata(path)?.metadata().valid_laps)
}

/// Format-neutral file summary.
///
/// For `.telemetry` this reads only `metadata.fb`.
pub fn read_metadata(
    path: impl AsRef<Path>,
) -> Result<motorsport_telemetry_core::FileMetadata, TelemetryError> {
    if is_telemetry(path.as_ref()) {
        return Ok(telemetry_format::read_metadata(path)?);
    }
    if is_jsonl_path(path.as_ref()) {
        return Ok(JsonlRecording::open(path)?.metadata());
    }
    Ok(open_metadata(path)?.metadata())
}

/// Catalog format version from `metadata.fb`. Header-only for `.telemetry`.
pub fn read_format_version(path: impl AsRef<Path>) -> Result<u16, TelemetryError> {
    if !is_telemetry(path.as_ref()) {
        return Err(TelemetryError::Unsupported(
            path.as_ref().display().to_string(),
        ));
    }
    Ok(telemetry_format::read_format_version(path)?)
}

/// True when a `.telemetry` file is older than [`FORMAT_VERSION`] and should be rewritten.
pub fn telemetry_needs_update(path: impl AsRef<Path>) -> Result<bool, TelemetryError> {
    if !is_telemetry(path.as_ref()) {
        return Err(TelemetryError::Unsupported(
            path.as_ref().display().to_string(),
        ));
    }
    Ok(telemetry_format::file_needs_update(path)?)
}

fn extension(path: &Path) -> String {
    path.extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

fn is_telemetry(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("telemetry"))
}

/// Kind of file verified by [`verify`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyKind {
    /// Native `.telemetry` STORE zip.
    Native,
    /// MTJ JSONL recording.
    Mtj,
    /// MTX JSONL sidecar.
    Mtx,
}

/// Structured outcome of verifying a native or JSONL telemetry file.
///
/// Returned by [`verify`]; the CLI formats it into its one-line report.
#[derive(Debug)]
pub struct VerifyReport {
    /// Container kind that was verified.
    pub kind: VerifyKind,
    /// JSONL document version (MTJ/MTX only).
    pub jsonl_version: Option<u16>,
    /// Native catalog format version (`.telemetry` only).
    pub format_version: Option<u16>,
    /// True when the document was zstd-compressed on disk.
    pub compressed: bool,
    /// Decoded channel count.
    pub channels: usize,
    /// Lap count (native catalog or MTJ recording).
    pub laps: usize,
    /// Span count (JSONL only; 0 for native).
    pub spans: usize,
    /// Unix-epoch nanoseconds at `t = 0`, when stamped.
    pub utc_start_ns: Option<u64>,
    /// JSONL lattice quantum in ns (0 for native).
    pub quantum_ns: u64,
    /// MTX sidecar group count (0 outside MTX).
    pub sidecar_groups: usize,
    /// True when a native catalog is older than [`FORMAT_VERSION`].
    pub needs_update: bool,
    /// Reader diagnostics plus plausibility findings.
    pub diagnostics: Diagnostics,
}

/// Errors returned by [`verify`].
#[derive(Debug, Error)]
pub enum VerifyError {
    /// The path is not a `.telemetry` or JSONL document.
    #[error("verify accepts .telemetry, .telemetry.jsonl, and .zstd (not vendor source files)")]
    Unsupported,
    /// The file could not be opened or parsed.
    #[error(transparent)]
    Format(#[from] telemetry_format::TelemetryFormatError),
    /// A channel was decoded at the wrong sample width; the file is unusable.
    #[error("decode fault: at least one channel was decoded at the wrong sample width")]
    DecodeFault(Diagnostics),
}

/// Verifies a native `.telemetry` or MTJ/MTX JSONL document.
///
/// Opens the file without rewriting an older catalog, decodes one sample from
/// every channel, and runs the reader diagnostics plus the format-neutral
/// plausibility validator. A proven decode-layout fault returns
/// [`VerifyError::DecodeFault`]; ordinary warnings stay inside
/// [`VerifyReport::diagnostics`]. Vendor source files (`.pds`, `.ld`, `.mp4`,
/// `.vbo`) are rejected with [`VerifyError::Unsupported`].
pub fn verify(path: impl AsRef<Path>) -> Result<VerifyReport, VerifyError> {
    let path = path.as_ref();
    if is_jsonl_path(path) {
        verify_jsonl(path)
    } else if is_telemetry(path) {
        verify_native(path)
    } else {
        Err(VerifyError::Unsupported)
    }
}

fn verify_native(path: &Path) -> Result<VerifyReport, VerifyError> {
    let opened = NativeRecording::open_unchanged(path)?;
    let metadata = opened.metadata();
    probe_samples(&opened);
    let diagnostics = combine_diagnostics(&opened, fs::metadata(path).ok().map(|meta| meta.len()));
    if implies_decode_fault(&diagnostics) {
        return Err(VerifyError::DecodeFault(diagnostics));
    }
    Ok(VerifyReport {
        kind: VerifyKind::Native,
        format_version: metadata.format_version,
        jsonl_version: None,
        compressed: false,
        channels: metadata.channel_count,
        laps: metadata.laps.len(),
        spans: opened.spans().len(),
        utc_start_ns: metadata.utc_start_ns,
        quantum_ns: 0,
        sidecar_groups: 0,
        needs_update: telemetry_format::needs_update(metadata.format_version.unwrap_or(0)),
        diagnostics,
    })
}

fn verify_jsonl(path: &Path) -> Result<VerifyReport, VerifyError> {
    let opened = JsonlRecording::open(path)?;
    probe_samples(&opened);
    // JSONL is text: a sample is many bytes of text, not `byte_width`, so the
    // file length bears no relation to the decoded footprint and the footprint
    // check is skipped.
    let diagnostics = combine_diagnostics(&opened, None);
    if implies_decode_fault(&diagnostics) {
        return Err(VerifyError::DecodeFault(diagnostics));
    }
    let extension = opened.is_extension();
    Ok(VerifyReport {
        kind: if extension {
            VerifyKind::Mtx
        } else {
            VerifyKind::Mtj
        },
        format_version: None,
        jsonl_version: Some(if extension {
            telemetry_format::JSONL_EXT_VERSION
        } else {
            telemetry_format::JSONL_VERSION
        }),
        compressed: is_jsonl_zstd_path(path) || starts_with_zstd(path),
        channels: opened.channels().len(),
        laps: opened.metadata().laps.len(),
        spans: opened.spans().len(),
        utc_start_ns: opened.utc_start_ns(),
        quantum_ns: opened.quantum_ns(),
        sidecar_groups: opened.sidecar_groups().len(),
        needs_update: false,
        diagnostics,
    })
}

/// Decodes one sample from every non-empty channel so a malformed payload is
/// surfaced as a reader diagnostic instead of a latent later failure.
fn probe_samples(source: &dyn TelemetrySource) {
    for (index, channel) in source.channels().iter().enumerate() {
        if channel.sample_count == 0 || channel.chunks.is_empty() {
            continue;
        }
        let _ = source.decode(index, 0, 0);
    }
}

/// Combines a source's reader diagnostics with the plausibility validator's
/// findings.
///
/// `file_len` is the backing file's byte length for binary formats whose
/// decoded samples correspond one-to-one to packed file bytes (native
/// `.telemetry`); `None` for text formats such as JSONL, where the file
/// length bears no relation to the decoded footprint and the footprint
/// check would falsely fire on compact text.
fn combine_diagnostics(source: &dyn TelemetrySource, file_len: Option<u64>) -> Diagnostics {
    let mut combined = Diagnostics::new();
    combined.extend(source.diagnostics().iter().cloned());
    let options = ValidateOptions {
        file_len,
        ..ValidateOptions::default()
    };
    combined.append(validate_source_with(source, options));
    combined
}

fn starts_with_zstd(path: &Path) -> bool {
    let mut magic = [0u8; 4];
    fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut magic))
        .is_ok()
        && magic == [0x28, 0xB5, 0x2F, 0xFD]
}

/// Format-neutral extensions implemented for every [`TelemetrySource`],
/// including boxed sources and pass-wrapped sources.
///
/// Import this trait to use [`SourceExt::signal_roles`],
/// [`SourceExt::normalizer`], [`SourceExt::match_track`], and
/// [`SourceExt::validate`] on any source value.
pub trait SourceExt: TelemetrySource {
    /// Infers normalized roles from known source channel names.
    ///
    /// Role inference never assigns or guesses units; unit validation happens
    /// when values are normalized.
    fn signal_roles(&self) -> SignalRoles {
        infer_roles(self.channels())
    }

    /// Builds a reusable normalization context.
    ///
    /// Signal roles and track matching are resolved once. Lap metadata remains
    /// lazy and, if needed as a fallback, is computed once for the lifetime of
    /// the context rather than once per sample.
    fn normalizer(&self) -> TelemetryNormalizer<'_>
    where
        Self: Sized,
    {
        TelemetryNormalizer::new(self, self.signal_roles(), self.match_track())
    }

    /// Matches sampled GPS positions to the nearest track within 50 km.
    ///
    /// Returns `None` when suitable GPS channels or valid units are absent, no
    /// sample produces a finite non-origin fix, no track is close enough, or
    /// the matched centerline cannot be decoded.
    fn match_track(&self) -> Option<TrackContext> {
        let roles = self.signal_roles();
        let (lat_index, lon_index) = roles.latitude.zip(roles.longitude)?;
        let channels = self.channels();
        let duration = channels[lat_index]
            .duration_ns
            .min(channels[lon_index].duration_ns);
        let mut lat_sum = 0.0;
        let mut lon_sum = 0.0;
        let mut count = 0usize;
        for sample in 0..32u64 {
            let time = duration.saturating_mul(sample) / 32;
            if let Some((lat, lon)) = self
                .sample_at(lat_index, time, true)
                .zip(self.sample_at(lon_index, time, true))
            {
                if lat.is_finite() && lon.is_finite() && (lat != 0.0 || lon != 0.0) {
                    lat_sum += lat;
                    lon_sum += lon;
                    count += 1;
                }
            }
        }
        if count == 0 {
            return None;
        }
        let raw = (lat_sum / count as f64, lon_sum / count as f64);
        let lat_unit = channels[lat_index].unit.trim().to_ascii_lowercase();
        let lon_unit = channels[lon_index].unit.trim().to_ascii_lowercase();
        let minutes = matches!(lat_unit.as_str(), "min" | "arcmin" | "arcminute")
            && matches!(lon_unit.as_str(), "min" | "arcmin" | "arcminute");
        let mut candidates = Vec::new();
        if minutes {
            if let Some(packed) =
                packed_coordinate(raw.0, 90.0, false).zip(packed_coordinate(raw.1, 180.0, true))
            {
                candidates.push(packed);
            } else {
                let continuous = (raw.0 / 60.0, -raw.1 / 60.0);
                if valid_gps(continuous) {
                    candidates.push(continuous);
                }
                // Some conversion tools export VBOX columns as decimal degrees
                // while retaining the native column names. Keep that as a
                // conservative fallback only when packed coordinates are invalid.
                if valid_gps(raw) {
                    candidates.push(raw);
                }
            }
        } else if let Some(converted) = coordinate(raw.0, &lat_unit)
            .zip(coordinate(raw.1, &lon_unit))
            .filter(|candidate| valid_gps(*candidate))
        {
            candidates.push(converted);
        }
        for (latitude, longitude) in candidates {
            if let Some(matched) = match_track(latitude, longitude, 50_000.0) {
                if let Ok(context) = TrackContext::new(matched, (latitude, longitude)) {
                    return Some(context);
                }
            }
        }
        None
    }

    /// Runs the format-neutral plausibility validator over this open source and
    /// returns its findings combined with the reader's own diagnostics.
    ///
    /// Reader diagnostics come first, in the order the reader encountered
    /// them; validator findings follow, in the order the validator produces
    /// them. The validator is given the byte length of the backing file read
    /// from filesystem metadata, which is what enables the
    /// `layout.footprint_exceeds_file` check.
    ///
    /// That footprint check compares the sum of every channel's claimed
    /// sample bytes to the file length, so it is only meaningful for binary
    /// formats where decoded samples correspond one-to-one to packed file
    /// bytes: Pi/Cosworth PDS, MoTeC LD, and native `.telemetry`. VBO and
    /// JSONL are text (a sample is many bytes of text, not `byte_width`), and
    /// AiM `aimd` expands one GPS packet into many channels, so their file
    /// length bears no relation to the decoded footprint. For those formats
    /// `file_len` is left `None` and the footprint check is skipped, as if
    /// [`validate_source`](motorsport_telemetry_core::validate::validate_source)
    /// had been called directly.
    fn validate(&self) -> Diagnostics {
        let mut combined = Diagnostics::new();
        combined.extend(self.diagnostics().iter().cloned());
        let mut options = ValidateOptions::default();
        if matches!(self.format(), "pds" | "motec" | "telemetry") {
            options.file_len = fs::metadata(self.path()).ok().map(|meta| meta.len());
        }
        combined.append(validate_source_with(&self, options));
        combined
    }
}

impl<T: TelemetrySource + ?Sized> SourceExt for T {}

/// Channel indexes selected for the facade's format-neutral signal roles.
///
/// A missing role is `None`. Indexes refer to [`TelemetrySource::channels`]
/// for the same file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SignalRoles {
    /// Vehicle or ground speed channel.
    pub speed: Option<usize>,
    /// Driver throttle pedal channel.
    pub throttle: Option<usize>,
    /// Driver brake pedal channel.
    pub brake: Option<usize>,
    /// Driver clutch pedal channel.
    pub clutch: Option<usize>,
    /// Handwheel / steering-angle channel.
    pub steering: Option<usize>,
    /// Selected gear channel.
    pub gear: Option<usize>,
    /// Engine speed channel.
    pub rpm: Option<usize>,
    /// Distance or progress within the current lap.
    pub lap_distance: Option<usize>,
    /// Current lap counter.
    pub lap_number: Option<usize>,
    /// Running or current lap-time channel.
    pub lap_time: Option<usize>,
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
    /// Throttle pedal in the inclusive range `0.0..=1.0`.
    pub throttle_fraction: Option<f64>,
    /// Brake pedal in the inclusive range `0.0..=1.0`.
    pub brake_fraction: Option<f64>,
    /// Clutch pedal in the inclusive range `0.0..=1.0`.
    pub clutch_fraction: Option<f64>,
    /// Steering wheel angle in degrees (positive as the source reports).
    pub steering_deg: Option<f64>,
    /// Selected gear as an integer.
    pub gear: Option<i64>,
    /// Engine speed in revolutions per minute.
    pub rpm: Option<f64>,
    /// Source lap number rounded to an integer.
    pub lap_number: Option<i64>,
    /// Progress through the current lap in the range `0.0..=1.0`.
    pub lap_progress: Option<f64>,
    /// Current lap time in seconds.
    pub lap_time_s: Option<f64>,
    /// WGS84 latitude in degrees.
    pub latitude_deg: Option<f64>,
    /// WGS84 longitude in degrees.
    pub longitude_deg: Option<f64>,
    /// Time of day in nanoseconds since local midnight, when a clock exists.
    pub time_of_day_ns: Option<u64>,
    /// Absolute clock nanoseconds (`file_relative + clock_offset`), when known.
    pub absolute_time_ns: Option<u64>,
}

/// Reusable state for high-throughput normalized sampling.
pub struct TelemetryNormalizer<'a> {
    source: &'a dyn TelemetrySource,
    roles: SignalRoles,
    track: Option<TrackContext>,
    laps: OnceLock<Vec<motorsport_telemetry_core::LapMetadata>>,
    clock: OnceLock<Option<(i128, String)>>,
}

impl std::fmt::Debug for TelemetryNormalizer<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelemetryNormalizer")
            .field("source", &self.source.path())
            .field("roles", &self.roles)
            .field("track", &self.track)
            .finish_non_exhaustive()
    }
}

impl<'a> TelemetryNormalizer<'a> {
    /// Creates a normalizer with caller-selected signal roles and track.
    pub fn new(
        source: &'a dyn TelemetrySource,
        roles: SignalRoles,
        track: Option<TrackContext>,
    ) -> Self {
        Self {
            source,
            roles,
            track,
            laps: OnceLock::new(),
            clock: OnceLock::new(),
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
                let laps = self
                    .laps
                    .get_or_init(|| self.source.metadata().laps.clone());
                lap_progress_from_metadata(laps, time_ns)
            },
            self.clock.get_or_init(|| file_clock(self.source)).as_ref(),
        )
    }
}

fn file_clock(source: &dyn TelemetrySource) -> Option<(i128, String)> {
    if let Some(range) = source.absolute_time_range() {
        return Some((i128::from(range.start_ns), range.clock));
    }
    let metadata = source.metadata();
    Some((metadata.clock_offset_ns?, metadata.absolute_clock?))
}

fn normalize_sample(
    source: &dyn TelemetrySource,
    time_ns: u64,
    roles: &SignalRoles,
    track: Option<&TrackContext>,
    lap_fallback: impl FnOnce() -> Option<f64>,
    clock: Option<&(i128, String)>,
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
    let clutch_fraction = roles.clutch.and_then(|index| {
        normalize_fraction(value(Some(index), true)?, &source.channels()[index].unit)
    });
    let steering_deg = roles.steering.and_then(|index| {
        normalize_angle_deg(value(Some(index), true)?, &source.channels()[index].unit)
    });
    let gear = value(roles.gear, false).map(|value| value.round() as i64);
    let rpm = roles.rpm.and_then(|index| {
        normalize_rpm(value(Some(index), false)?, &source.channels()[index].unit)
    });
    let latitude_deg = roles.latitude.and_then(|index| {
        normalize_coordinate(value(Some(index), true)?, &source.channels()[index].unit)
    });
    let longitude_deg = roles.longitude.and_then(|index| {
        normalize_longitude(value(Some(index), true)?, &source.channels()[index].unit)
    });
    let lap_number = value(roles.lap_number, false).map(|value| value.round() as i64);
    let lap_time_s = roles.lap_time.and_then(|index| {
        normalize_duration_s(value(Some(index), true)?, &source.channels()[index].unit)
    });
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
    let (absolute_time_ns, time_of_day_ns) = match clock {
        Some((offset, name)) => {
            let absolute = u64::try_from(i128::from(time_ns) + *offset).ok();
            let tod = if name == "time_of_day" {
                absolute
            } else {
                absolute.map(|value| value % 86_400_000_000_000)
            };
            (absolute, tod)
        }
        None => (None, None),
    };
    NormalizedSample {
        speed_mps,
        throttle_fraction,
        brake_fraction,
        clutch_fraction,
        steering_deg,
        gear,
        rpm,
        lap_number,
        lap_progress,
        lap_time_s,
        latitude_deg,
        longitude_deg,
        time_of_day_ns,
        absolute_time_ns,
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
    /// The WGS84 `(latitude, longitude)` query point that matched the track.
    pub gps: (f64, f64),
    centerline: Vec<[f64; 2]>,
    cumulative_m: Vec<f64>,
    total_m: f64,
}

impl TrackContext {
    /// Builds projection state from a track-atlas match and its query point.
    ///
    /// The error indicates that the layout's embedded centerline is not valid
    /// GeoJSON. An empty or one-point centerline constructs successfully but
    /// cannot produce progress values.
    pub fn new(matched: TrackMatch, gps: (f64, f64)) -> Result<Self, serde_json::Error> {
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
            gps,
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
pub struct TelemetrySession {
    /// Open files in session order.
    pub files: Vec<TelemetryFile>,
    /// Per-file summaries in the same order as [`Self::files`].
    pub file_metadata: Vec<FileMetadata>,
    /// Metadata merged across the session.
    pub metadata: SessionMetadata,
}

impl std::fmt::Debug for TelemetrySession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelemetrySession")
            .field("files", &self.files.len())
            .field("file_metadata", &self.file_metadata)
            .field("metadata", &self.metadata)
            .finish()
    }
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
        .map(|file| file.metadata())
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
            let driver_id = semantic_value(
                file.as_ref(),
                file_time_ns,
                &["driverid", "driver", "driverindex"],
            );
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
        speed: names::find(
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
        throttle: names::find(
            channels,
            &[
                "throttlepos",
                "driverthrottlepos",
                "throttlepedal",
                "pedalpos",
                "throttle",
            ],
        ),
        brake: names::find(
            channels,
            &[
                "brakepedalpos",
                "brakepedal",
                "driverbrakepressure",
                "brakepressure",
                "pbrakefront",
                "brake",
            ],
        ),
        clutch: names::find(
            channels,
            &["clutchpos", "clutchpedal", "clutchpedalpos", "clutch"],
        ),
        steering: names::find(
            channels,
            &[
                "steeringangle",
                "steerangle",
                "steeringpos",
                "handwheelangle",
                "swangle",
                "steering",
            ],
        ),
        gear: names::find(channels, &["gearpos", "selectedgear", "ngear", "gear"]),
        rpm: names::find(channels, &["enginerpm", "engspeed", "rpm", "nmot"]),
        lap_distance: names::find(
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
        lap_number: names::find(
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
        lap_time: names::find(
            channels,
            &[
                "currentlaptime",
                "lapcurrentlaptime",
                "laptimerunning",
                "laptime",
            ],
        ),
        // Pass-derived clean coordinates (gps.clean) are NaN-masked copies
        // of the raw fixes and always preferable when present.
        latitude: names::find(
            channels,
            &[
                "gpslatitudeclean",
                "gpslatitude",
                "latitude",
                "gpslat",
                "lat",
            ],
        ),
        longitude: names::find(
            channels,
            &[
                "gpslongitudeclean",
                "gpslongitude",
                "longitude",
                "gpslon",
                "lon",
                "long",
            ],
        ),
    }
}

fn semantic_value(source: &dyn TelemetrySource, time_ns: u64, names: &[&str]) -> Option<i64> {
    let index = names::find(source.channels(), names)?;
    let channel = &source.channels()[index];
    if channel.sample_count == 0 || channel.chunks.is_empty() {
        return None;
    }
    source
        .sample_at(index, time_ns, false)
        .filter(|value| value.is_finite())
        .map(|value| value.round() as i64)
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

/// Longitude form of [`normalize_coordinate`]: VBOX stores arc-minutes with
/// west positive, so the sign flips to the east-positive convention.
fn normalize_longitude(value: f64, unit: &str) -> Option<f64> {
    match unit.trim().to_ascii_lowercase().as_str() {
        "min" | "arcmin" | "arcminute" => Some(-value / 60.0),
        _ => normalize_coordinate(value, unit),
    }
}

fn coordinate(value: f64, unit: &str) -> Option<f64> {
    match unit.trim().to_ascii_lowercase().as_str() {
        "deg" | "degree" | "degrees" | "°" => Some(value),
        "rad" | "radian" | "radians" => Some(value.to_degrees()),
        _ => None,
    }
}

fn packed_coordinate(value: f64, maximum_degrees: f64, reverse_sign: bool) -> Option<f64> {
    let absolute = value.abs();
    let degrees = (absolute / 100.0).floor();
    let minutes = absolute - degrees * 100.0;
    if !value.is_finite() || degrees > maximum_degrees || minutes >= 60.0 {
        return None;
    }
    let sign = if value.is_sign_negative() { -1.0 } else { 1.0 };
    Some((degrees + minutes / 60.0) * sign * if reverse_sign { -1.0 } else { 1.0 })
}

fn valid_gps((latitude, longitude): (f64, f64)) -> bool {
    latitude.is_finite()
        && longitude.is_finite()
        && latitude.abs() <= 90.0
        && longitude.abs() <= 180.0
        && (latitude != 0.0 || longitude != 0.0)
}

fn normalize_angle_deg(value: f64, unit: &str) -> Option<f64> {
    if unit.is_empty() {
        return None;
    }
    motorsport_telemetry_core::convert(value, unit, "deg")
        .ok()
        .or_else(|| normalize_coordinate(value, unit))
}

fn normalize_rpm(value: f64, unit: &str) -> Option<f64> {
    if unit.is_empty() {
        return None;
    }
    motorsport_telemetry_core::convert(value, unit, "rpm")
        .ok()
        .or_else(|| motorsport_telemetry_core::convert(value, unit, "1/min").ok())
}

fn normalize_duration_s(value: f64, unit: &str) -> Option<f64> {
    if unit.is_empty() {
        return None;
    }
    motorsport_telemetry_core::convert(value, unit, "s").ok()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_vbox_packed_coordinates_before_other_conventions() {
        let latitude = packed_coordinate(3119.09973, 90.0, false).unwrap();
        let longitude = packed_coordinate(58.49277, 180.0, true).unwrap();
        assert!((latitude - 31.318_328_833_333_335).abs() < 1e-12);
        assert!((longitude - -0.974_879_5).abs() < 1e-12);
        assert_eq!(packed_coordinate(3190.0, 90.0, false), None);
    }
}
