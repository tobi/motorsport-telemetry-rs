#![doc = include_str!("../README.md")]
#![deny(missing_docs)]

use aim_telemetry::AimFile;
use cosworth_telemetry::CosworthFile;
use motec_telemetry::MotecFile;
use motorsport_telemetry_core::names;
use motorsport_telemetry_core::{
    group_sessions, implies_decode_fault, validate_source_with, Channel, Diagnostics, FileMetadata,
    LapKind, SessionMetadata, TelemetrySource, ValidateOptions, VideoReference,
};
use motorsport_track_atlas::{match_track, TrackMatch};
use racelogic_telemetry::RacelogicFile;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use telemetry_format::{
    is_jsonl_path, is_jsonl_zstd_path, sniff_container, Container, JsonlRecording, NativeRecording,
};
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

/// Opens a telemetry file using its case-insensitive extension.
///
/// Supported extensions are `.mp4`, `.pds`, `.ld`, `.vbo`, `.telemetry`,
/// `.telemetry.jsonl`, `.jsonl`, `.mtj`, `.telemetry.ext.jsonl`, and those
/// names with a `.zstd` or `.zst` suffix.
/// This function selects a parser by extension; the selected parser still
/// validates the file contents. A `.telemetry` file is dispatched by its
/// container (zstd MTJ, or the legacy native zip); a writable legacy zip
/// older than [`FORMAT_VERSION`] is rewritten in place.
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
        "telemetry" => Ok(telemetry_format::open_telemetry(path)?),
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
        "telemetry" => Ok(telemetry_format::open_telemetry(path)?),
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
        // O(header): MTJ reads its first two lines; a legacy zip maps the
        // catalog and probes speed for classification.
        return Ok(telemetry_format::read_laps(path)?);
    }
    if is_jsonl_path(path.as_ref()) {
        return Ok(telemetry_format::read_laps(path)?);
    }
    Ok(open_metadata(path)?.metadata().laps)
}

/// Returns the number of flying laps.
///
/// For a `.telemetry` this is an O(header) read (see [`read_metadata`]).
pub fn read_valid_laps(path: impl AsRef<Path>) -> Result<u32, TelemetryError> {
    if is_telemetry(path.as_ref()) {
        return Ok(telemetry_format::read_valid_laps(path)?);
    }
    if is_jsonl_path(path.as_ref()) {
        return Ok(telemetry_format::read_valid_laps(path)?);
    }
    Ok(open_metadata(path)?.metadata().valid_laps)
}

/// Format-neutral file summary with the stint model resolved.
///
/// For a `.telemetry` this is O(header): a zstd-MTJ document is decoded only
/// through its header and laps lines; a legacy zip maps the catalog and probes
/// the speed channel for classification. Vendor files are opened.
pub fn read_metadata(
    path: impl AsRef<Path>,
) -> Result<motorsport_telemetry_core::FileMetadata, TelemetryError> {
    if is_telemetry(path.as_ref()) {
        return Ok(telemetry_format::read_metadata(path)?);
    }
    if is_jsonl_path(path.as_ref()) {
        return Ok(telemetry_format::read_metadata(path)?);
    }
    Ok(open_metadata(path)?.metadata())
}

/// Catalog format version from `metadata.fb` of a **legacy zip** `.telemetry`.
/// Header-only. A zstd-MTJ `.telemetry` has no catalog version and errors.
pub fn read_format_version(path: impl AsRef<Path>) -> Result<u16, TelemetryError> {
    if !is_telemetry(path.as_ref()) {
        return Err(TelemetryError::Unsupported(
            path.as_ref().display().to_string(),
        ));
    }
    Ok(telemetry_format::read_format_version(path)?)
}

/// True when a legacy-zip `.telemetry` is older than [`FORMAT_VERSION`] and
/// should be rewritten. Always `false` for the zstd-MTJ container.
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
    /// Legacy native `.telemetry` STORE zip.
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

/// Verifies a `.telemetry` (either container) or MTJ/MTX JSONL document.
///
/// A `.telemetry` path is dispatched by content: a zstd frame or `{` is an
/// MTJ document, `PK` is the legacy native zip.
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
        match sniff_container(path)? {
            Container::NativeZip => verify_native(path),
            Container::JsonlZstd | Container::Jsonl => verify_jsonl(path),
            Container::Unknown => Err(VerifyError::Format(
                telemetry_format::TelemetryFormatError::Invalid(format!(
                    "{}: not a .telemetry file (neither a zstd MTJ frame nor a native zip)",
                    path.display()
                )),
            )),
        }
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
        TelemetryNormalizer::new(self, self.resolved_roles(), self.match_track())
    }

    /// [`Self::signal_roles`] with the speed role settled the way the
    /// normalizer will read it: the highest-priority speed channel whose unit
    /// is declared **or provable from its value range** (see [`RoleUnits`]).
    ///
    /// Name-only inference prefers any channel with a declared unit, which on
    /// an AiM dash picks the 25 Hz `GPS Speed` (m/s, with dropouts) over the
    /// dash's own unitless wheel speed. Once the range proves that channel is
    /// km/h it is the better source and outranks GPS by priority.
    fn resolved_roles(&self) -> SignalRoles
    where
        Self: Sized,
    {
        let mut roles = self.signal_roles();
        let channels = self.channels();
        for name in SPEED_NAMES {
            let Some(index) = channels
                .iter()
                .position(|channel| channel.sample_count > 0 && names::eq(&channel.name, name))
            else {
                continue;
            };
            let unit = channels[index].unit.trim();
            let resolvable = if unit.is_empty() {
                channel_extent(self, index).is_some_and(|(_, max)| max > 130.0)
            } else {
                motorsport_telemetry_core::can_convert(unit, "m/s")
            };
            if resolvable {
                roles.speed = Some(index);
                break;
            }
        }
        roles
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
    /// Driver brake pedal *position* channel (a fraction or percent).
    pub brake: Option<usize>,
    /// Brake line pressure channel (a pressure unit), when the source logs
    /// hydraulic pressure instead of or in addition to pedal travel.
    pub brake_pressure: Option<usize>,
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

/// Format-neutral values sampled at one file-relative timestamp — the
/// "blessed channels" every client can rely on regardless of logger.
///
/// Units are fixed per field (m/s, 0–1, deg, rpm, bar, s). A field is `Some`
/// only when the source declares a convertible unit, or — for a channel that
/// declares none — when its value range leaves exactly one physical reading
/// (see [`RoleUnits`]). Otherwise it stays `None`; nothing is guessed. The
/// full contract, per-format availability and inference rules are in the
/// crate README under "The client contract".
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NormalizedSample {
    /// Speed in metres per second.
    pub speed_mps: Option<f64>,
    /// Throttle pedal in the inclusive range `0.0..=1.0`.
    pub throttle_fraction: Option<f64>,
    /// Brake pedal in the inclusive range `0.0..=1.0`. Pedal position only;
    /// a source that logs pressure alone leaves this `None` and fills
    /// [`Self::brake_pressure_bar`].
    pub brake_fraction: Option<f64>,
    /// Front (or total) brake line pressure in bar, when the source logs it.
    pub brake_pressure_bar: Option<f64>,
    /// Clutch pedal in the inclusive range `0.0..=1.0`.
    pub clutch_fraction: Option<f64>,
    /// Steering wheel angle in degrees (positive as the source reports).
    pub steering_deg: Option<f64>,
    /// Selected gear as an integer.
    pub gear: Option<i64>,
    /// Engine speed in revolutions per minute.
    pub rpm: Option<f64>,
    /// Virtual session lap number: 1-based, monotonic across stints, from
    /// the recording's classified laps (`FileMetadata::laps[].number`). When
    /// the recording has no laps this falls back to the source counter.
    pub lap_number: Option<i64>,
    /// The vendor counter as logged (`Lap_Number`, `Lap Count`, …), rounded.
    /// This is a *stint* lap counter on most dashes: it restarts after a pit
    /// stop or logger reset. Use `lap_number` to identify a lap.
    pub stint_lap_number: Option<i64>,
    /// 1-based stint index of the lap containing this sample.
    pub stint: Option<u32>,
    /// Normalised role of the containing lap (flying, out, in, out-in, pit).
    pub lap_kind: Option<LapKind>,
    /// Human label of the containing lap: `S1 out`, `S2 L3`, `S1 pit L5`.
    pub lap_label: Option<String>,
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
    units: OnceLock<RoleUnits>,
}

/// The unit each role is read in: the channel's declared unit, or — for a
/// channel that declares none — the unit its value range proves.
///
/// Declared units always win. Inference runs only on unitless channels
/// (AiM `aimd` CAN echoes, VBOX CAN columns, stripped exports) and only where
/// the physical range leaves one reading: a pedal that reaches 99 is percent,
/// a steering trace spanning 300 is degrees not radians, an engine speed
/// topping 7981 is rpm not rad/s, a lap timer counting to 331460 is
/// milliseconds. Where the range is ambiguous the role stays unresolved and
/// the sample field is `None`. Ranges come from at most 4096 probes per
/// channel, taken once per normalizer.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RoleUnits {
    /// Unit `speed_mps` converts from.
    pub speed: Option<String>,
    /// Unit `throttle_fraction` converts from (`%`, `ratio`, or an angle for
    /// Cosworth pedals, see [`NormalizedSample::throttle_fraction`]).
    pub throttle: Option<String>,
    /// Unit `brake_fraction` converts from.
    pub brake: Option<String>,
    /// Unit `brake_pressure_bar` converts from.
    pub brake_pressure: Option<String>,
    /// Unit `clutch_fraction` converts from.
    pub clutch: Option<String>,
    /// Unit `steering_deg` converts from.
    pub steering: Option<String>,
    /// Unit `rpm` converts from.
    pub rpm: Option<String>,
    /// Unit `lap_time_s` converts from.
    pub lap_time: Option<String>,
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
            units: OnceLock::new(),
        }
    }

    /// The unit each role is read in, declared or inferred (see [`RoleUnits`]).
    pub fn units(&self) -> &RoleUnits {
        self.units
            .get_or_init(|| resolve_role_units(self.source, &self.roles))
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
                lap_at(laps, time_ns)
            },
            self.clock.get_or_init(|| file_clock(self.source)).as_ref(),
            self.units(),
        )
    }
}

/// Bounded value range of a channel: at most 4096 evenly spaced probes.
fn channel_extent(source: &dyn TelemetrySource, index: usize) -> Option<(f64, f64)> {
    let channel = source.channels().get(index)?;
    if channel.sample_count == 0 || channel.duration_ns == 0 {
        return None;
    }
    let start = channel.chunks.first().map_or(0, |chunk| chunk.time_base_ns);
    let span = channel.duration_ns.saturating_sub(start);
    let probes = channel.sample_count.clamp(1, 4096);
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    for probe in 0..probes {
        let at = start + (u128::from(span) * u128::from(probe) / u128::from(probes)) as u64;
        if let Some(value) = source
            .sample_at(index, at, false)
            .filter(|value| value.is_finite())
        {
            min = min.min(value);
            max = max.max(value);
        }
    }
    (min <= max).then_some((min, max))
}

fn resolve_role_units(source: &dyn TelemetrySource, roles: &SignalRoles) -> RoleUnits {
    let declared = |index: usize| {
        let unit = source.channels()[index].unit.trim();
        (!unit.is_empty()).then(|| unit.to_owned())
    };
    let extent = |index: usize| channel_extent(source, index);
    let resolve = |index: Option<usize>, infer: &dyn Fn((f64, f64)) -> Option<&'static str>| {
        let index = index?;
        declared(index).or_else(|| infer(extent(index)?).map(str::to_owned))
    };
    let pedal = |(min, max): (f64, f64)| -> Option<&'static str> {
        if min < -0.05 {
            None
        } else if max <= 1.05 && max > 0.0 {
            Some("ratio")
        } else if max <= 105.0 && max > 1.05 {
            Some("%")
        } else {
            None
        }
    };
    RoleUnits {
        // A unitless speed above 130 cannot be m/s (468 km/h); below that
        // km/h and m/s overlap and nothing is inferred.
        speed: resolve(roles.speed, &|(_, max)| (max > 130.0).then_some("km/h")),
        throttle: resolve(roles.throttle, &pedal),
        brake: resolve(roles.brake, &pedal),
        // Pressure ranges overlap between bar and psi; never inferred.
        brake_pressure: resolve(roles.brake_pressure, &|_| None),
        clutch: resolve(roles.clutch, &pedal),
        // A trace exceeding 2π in magnitude cannot be radians of steering.
        steering: resolve(roles.steering, &|(min, max)| {
            (max.abs().max(min.abs()) > std::f64::consts::TAU).then_some("deg")
        }),
        // Engine speed above 3000 is rpm; rad/s tops out near 1000 (9550 rpm).
        rpm: resolve(roles.rpm, &|(_, max)| (max > 3000.0).then_some("rpm")),
        // Mirrors the lap recovery rule for unitless dash timers.
        lap_time: resolve(roles.lap_time, &|(_, max)| {
            Some(if max > 1000.0 { "ms" } else { "s" })
        }),
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
    lap_lookup: impl FnOnce() -> Option<motorsport_telemetry_core::LapMetadata>,
    clock: Option<&(i128, String)>,
    units: &RoleUnits,
) -> NormalizedSample {
    let value = |index: Option<usize>, linear| {
        index
            .and_then(|index| source.sample_at(index, time_ns, linear))
            .filter(|value| value.is_finite())
    };
    fn unit(unit: &Option<String>) -> &str {
        unit.as_deref().unwrap_or("")
    }
    let speed_mps = roles.speed.and_then(|index| {
        let raw = value(Some(index), true)?;
        normalize_speed(raw, unit(&units.speed))
    });
    let throttle_fraction = roles
        .throttle
        .and_then(|index| normalize_fraction(value(Some(index), true)?, unit(&units.throttle)));
    let brake_fraction = roles
        .brake
        .and_then(|index| normalize_fraction(value(Some(index), true)?, unit(&units.brake)));
    let brake_pressure_bar = roles.brake_pressure.and_then(|index| {
        let raw = value(Some(index), true)?;
        motorsport_telemetry_core::convert(raw, unit(&units.brake_pressure), "bar").ok()
    });
    let clutch_fraction = roles
        .clutch
        .and_then(|index| normalize_fraction(value(Some(index), true)?, unit(&units.clutch)));
    let steering_deg = roles
        .steering
        .and_then(|index| normalize_angle_deg(value(Some(index), true)?, unit(&units.steering)));
    let gear = value(roles.gear, false).map(|value| value.round() as i64);
    let rpm = roles
        .rpm
        .and_then(|index| normalize_rpm(value(Some(index), false)?, unit(&units.rpm)));
    let latitude_deg = roles.latitude.and_then(|index| {
        normalize_coordinate(value(Some(index), true)?, &source.channels()[index].unit)
    });
    let longitude_deg = roles.longitude.and_then(|index| {
        normalize_longitude(value(Some(index), true)?, &source.channels()[index].unit)
    });
    let lap = lap_lookup();
    let stint_lap_number = value(roles.lap_number, false)
        .map(|value| value.round() as i64)
        .or_else(|| lap.as_ref().map(|lap| lap.stint_lap));
    let lap_number = lap.as_ref().map(|lap| lap.number).or(stint_lap_number);
    let stint = lap.as_ref().map(|lap| lap.stint);
    let lap_kind = lap.as_ref().map(|lap| lap.kind);
    let lap_label = lap
        .as_ref()
        .map(motorsport_telemetry_core::LapMetadata::label);
    // The dash's running lap timer when it has one; otherwise time since the
    // classified lap's start, which is the same quantity to the resolution
    // of the lap boundary.
    let lap_time_s = roles
        .lap_time
        .and_then(|index| normalize_duration_s(value(Some(index), true)?, unit(&units.lap_time)))
        .or_else(|| {
            lap.as_ref()
                .map(|lap| time_ns.saturating_sub(lap.start_ns) as f64 / 1e9)
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
        .or_else(|| {
            lap.as_ref()
                .filter(|lap| lap.duration_ns > 0)
                .map(|lap| time_ns.saturating_sub(lap.start_ns) as f64 / lap.duration_ns as f64)
        });
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
        brake_pressure_bar,
        clutch_fraction,
        steering_deg,
        gear,
        rpm,
        lap_number,
        stint_lap_number,
        stint,
        lap_kind,
        lap_label,
        lap_progress,
        lap_time_s,
        latitude_deg,
        longitude_deg,
        time_of_day_ns,
        absolute_time_ns,
    }
}

/// The classified lap containing `time_ns`, if any.
fn lap_at(
    laps: &[motorsport_telemetry_core::LapMetadata],
    time_ns: u64,
) -> Option<motorsport_telemetry_core::LapMetadata> {
    laps.iter()
        .find(|lap| time_ns >= lap.start_ns && time_ns < lap.end_ns)
        .cloned()
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

/// [`names::find`] restricted to channels that actually carry samples.
///
/// Cosworth PDS exports list every logger channel, including ones that were
/// configured but never logged (`vehRefSpeed` with zero samples next to a
/// populated `Speed_Wspd_App`). A role bound to an empty channel is useless,
/// so priority order only applies among populated channels.
fn find_sampled(channels: &[Channel], wanted: &[&str]) -> Option<usize> {
    for name in wanted {
        if let Some(index) = channels
            .iter()
            .position(|channel| channel.sample_count > 0 && names::eq(&channel.name, name))
        {
            return Some(index);
        }
    }
    None
}

/// [`find_sampled`] that prefers a candidate with a convertible speed unit.
///
/// A speed without a unit cannot be normalised, so a unitless CAN echo
/// (AiM `Speed_Wspd_App` with no unit string) must not outrank a lower-priority
/// channel that does say what it measures (`GPS Speed` in m/s). Priority order
/// still decides among the unit-bearing candidates; only when none has a unit
/// does the plain priority winner stand.
fn find_sampled_with_unit(channels: &[Channel], wanted: &[&str]) -> Option<usize> {
    for name in wanted {
        if let Some(index) = channels.iter().position(|channel| {
            channel.sample_count > 0
                && motorsport_telemetry_core::can_convert(&channel.unit, "m/s")
                && names::eq(&channel.name, name)
        }) {
            return Some(index);
        }
    }
    find_sampled(channels, wanted)
}

const SPEED_NAMES: &[&str] = &[
    "groundspeed",
    "speedref",
    "corrspeed",
    "vehiclespeed",
    "speedwspdapp",
    "vehrefspeed",
    "vcar",
    "gpsspeed",
    "speed",
    "velocitykmh",
];

fn infer_roles(channels: &[Channel]) -> SignalRoles {
    SignalRoles {
        speed: find_sampled_with_unit(channels, SPEED_NAMES),
        // Driver demand first (pedal position), throttle-plate position last:
        // Cosworth names the pedal `PPS` and the plate `TPS`.
        throttle: find_sampled(
            channels,
            &[
                "driverthrottlepos",
                "throttlepedal",
                "pedalpos",
                "pps",
                "throttlepos",
                "throttle",
                "tps",
            ],
        ),
        brake: find_sampled(
            channels,
            &[
                "brakepedalpos",
                "brakepedal",
                "brakepos",
                "brakepedalposition",
                "brake",
            ],
        ),
        brake_pressure: find_sampled(
            channels,
            &[
                "driverbrakepressure",
                "brakepressurefront",
                "brakepressuref",
                "pbrakefront",
                "pfbrake",
                "pbrakef",
                "brakepressure",
                "brakepress",
            ],
        ),
        clutch: find_sampled(
            channels,
            &["clutchpos", "clutchpedal", "clutchpedalpos", "clutch"],
        ),
        steering: find_sampled(
            channels,
            &[
                "steeringangle",
                "steerangle",
                "steeringpos",
                "handwheelangle",
                "swangle",
                "steeringwheelangle",
                "steering",
                "steer",
                "steer001",
            ],
        ),
        gear: find_sampled(channels, &["gearpos", "selectedgear", "ngear", "gear"]),
        rpm: find_sampled(
            channels,
            &[
                "enginerpm",
                "engspeed",
                "enginespeed",
                "rpm",
                "nmot",
                "nengine",
            ],
        ),
        lap_distance: find_sampled(
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
        lap_number: find_sampled(
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
        lap_time: find_sampled(
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
        latitude: find_sampled(
            channels,
            &[
                "gpslatitudeclean",
                "gpslatitude",
                "latitude",
                "gpslat",
                "lat",
            ],
        ),
        longitude: find_sampled(
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
        // Cosworth PDS stores pedal position as an *angle* whose value in
        // degrees is the percent (`PPS` 0.035..1.763 rad = 2..101 deg): the
        // quantity code is angle, the meaning is pedal travel.
        "rad" | "radian" | "radians" => Some((value.to_degrees() / 100.0).clamp(0.0, 1.0)),
        "deg" | "degree" | "degrees" | "°" => Some((value / 100.0).clamp(0.0, 1.0)),
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
    use motorsport_telemetry_core::{Chunk, SampleType, UnitSource};

    fn channel(name: &str, unit: &str, sample_count: u64) -> Channel {
        Channel {
            id: 0,
            name: name.into(),
            unit: unit.into(),
            unit_source: UnitSource::Declared,
            sample_type: SampleType::F32,
            chunks: (sample_count > 0)
                .then_some(Chunk {
                    sample_period_ns: 20_000_000,
                    sample_count,
                    data_ptr: 0,
                    sample_base: 0,
                    time_base_ns: 0,
                })
                .into_iter()
                .collect(),
            sample_count,
            duration_ns: sample_count * 20_000_000,
        }
    }

    #[test]
    fn roles_recognise_cosworth_oreca_names_and_skip_empty_channels() {
        // Channel set of a real ORECA 07 Cosworth PDS export: the preferred
        // `vehRefSpeed` exists but was never logged, `PPS` is the pedal and
        // `TPS` the throttle plate, `P_F_BRAKE` is front line pressure.
        let channels = [
            channel("vehRefSpeed", "m/s", 0),
            channel("Speed_Wspd_App", "m/s", 100),
            channel("fl_speed", "m/s", 100),
            channel("TPS", "rad", 100),
            channel("PPS", "rad", 100),
            channel("P_R_BRAKE", "Pa", 100),
            channel("P_F_BRAKE", "Pa", 100),
            channel("STEER", "rad", 100),
            channel("RPM", "rad/s", 100),
            channel("gear_pos", "", 100),
            channel("Lap Number", "", 100),
        ];
        let roles = infer_roles(&channels);
        let name = |index: Option<usize>| index.map(|index| channels[index].name.as_str());
        assert_eq!(name(roles.speed), Some("Speed_Wspd_App"));
        assert_eq!(name(roles.throttle), Some("PPS"));
        assert_eq!(name(roles.brake), None, "pressure is not a pedal position");
        assert_eq!(name(roles.brake_pressure), Some("P_F_BRAKE"));
        assert_eq!(name(roles.steering), Some("STEER"));
        assert_eq!(name(roles.rpm), Some("RPM"));
        assert_eq!(name(roles.gear), Some("gear_pos"));
        assert_eq!(name(roles.lap_number), Some("Lap Number"));
    }

    #[test]
    fn speed_role_prefers_a_candidate_with_a_declared_unit() {
        // AiM SmartyCam aimd: the CAN echo of wheel speed has no unit string,
        // the GPS speed does. Only the latter can be normalised.
        let channels = [
            channel("Speed_Wspd_App", "", 100),
            channel("GPS Speed", "m/s", 100),
            channel("STEER_001", "", 100),
        ];
        let roles = infer_roles(&channels);
        assert_eq!(roles.speed, Some(1));
        assert_eq!(roles.steering, Some(2));
        // With no unit anywhere, priority order still decides.
        let channels = [
            channel("GPS Speed", "", 100),
            channel("Speed_Wspd_App", "", 100),
        ];
        assert_eq!(infer_roles(&channels).speed, Some(1));
    }

    #[test]
    fn role_selection_uses_compatible_units_and_the_driver_pedal() {
        let channels = [
            channel("Vehicle Speed", "%", 100),
            channel("GPS Speed", "m/s", 100),
            channel("Throttle Pos", "ratio", 100),
            channel("Driver Throttle Pos", "ratio", 100),
            channel("P_F_BRAKE", "bar", 100),
            channel("Brake Pos", "ratio", 100),
        ];
        let roles = infer_roles(&channels);
        assert_eq!(roles.speed, Some(1), "wrong dimension is not usable speed");
        assert_eq!(roles.throttle, Some(3), "driver demand, not throttle plate");
        assert_eq!(roles.brake, Some(5), "pedal fraction before line pressure");
    }

    #[test]
    fn roles_recognise_vbox_can_channel_names() {
        let channels = [
            channel("velocity kmh", "km/h", 100),
            channel("Engine_Speed", "RPM", 100),
            channel("Brake_Pressure_Front", "bar", 100),
            channel("Throttle_Pedal", "%", 100),
            channel("Vehicle_Speed", "kmh", 100),
            channel("Steering_Angle", "", 100),
        ];
        let roles = infer_roles(&channels);
        let name = |index: Option<usize>| index.map(|index| channels[index].name.as_str());
        assert_eq!(name(roles.speed), Some("Vehicle_Speed"));
        assert_eq!(name(roles.rpm), Some("Engine_Speed"));
        assert_eq!(name(roles.brake), None);
        assert_eq!(name(roles.brake_pressure), Some("Brake_Pressure_Front"));
        assert_eq!(name(roles.throttle), Some("Throttle_Pedal"));
        assert_eq!(name(roles.steering), Some("Steering_Angle"));
    }

    #[test]
    fn decodes_vbox_packed_coordinates_before_other_conventions() {
        let latitude = packed_coordinate(3119.09973, 90.0, false).unwrap();
        let longitude = packed_coordinate(58.49277, 180.0, true).unwrap();
        assert!((latitude - 31.318_328_833_333_335).abs() < 1e-12);
        assert!((longitude - -0.974_879_5).abs() < 1e-12);
        assert_eq!(packed_coordinate(3190.0, 90.0, false), None);
    }
}
