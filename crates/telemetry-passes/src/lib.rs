#![doc = include_str!("../README.md")]
#![deny(missing_docs)]

use motorsport_telemetry_core::{Channel, SampleType, TelemetrySource};

mod gps_clean;
mod gps_quality;
mod source;
mod speed_distance;

pub use gps_clean::GpsClean;
pub use gps_quality::GpsQuality;
pub use source::PassedSource;
pub use speed_distance::SpeedDistance;

/// Whether a pass can run against a given source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Applicability {
    /// Every precondition holds; `derive` will produce output channels.
    Ready,
    /// A precondition failed. The pass is not employed for this source.
    Skipped {
        /// User-facing sentence naming the missing requirement.
        reason: String,
    },
}

/// One derived channel produced by a pass, before it is attached to a
/// [`PassedSource`].
///
/// A derived channel mirrors an existing channel: it copies that channel's
/// chunk layout and sample times exactly and supplies one new value per
/// mirrored sample. Passes therefore never invent a timeline.
#[derive(Debug, Clone)]
pub struct DerivedChannel {
    /// New channel name. Must not collide with an existing channel.
    pub name: String,
    /// Unit string for the derived values; empty for dimensionless flags.
    pub unit: String,
    /// Scalar representation of `data`.
    pub sample_type: SampleType,
    /// Index of the source channel whose chunk layout and sample times this
    /// channel copies.
    pub mirrors: usize,
    /// Packed little-endian samples; exactly one value per mirrored sample.
    pub data: Vec<u8>,
}

impl DerivedChannel {
    /// Packs `values` as an `f64` channel mirroring `mirrors`.
    pub fn f64(name: &str, unit: &str, mirrors: usize, values: &[f64]) -> Self {
        let mut data = Vec::with_capacity(values.len() * 8);
        for value in values {
            data.extend_from_slice(&value.to_le_bytes());
        }
        Self {
            name: name.to_owned(),
            unit: unit.to_owned(),
            sample_type: SampleType::F64,
            mirrors,
            data,
        }
    }

    /// Packs `values` as an `f32` channel mirroring `mirrors`.
    pub fn f32(name: &str, unit: &str, mirrors: usize, values: &[f32]) -> Self {
        let mut data = Vec::with_capacity(values.len() * 4);
        for value in values {
            data.extend_from_slice(&value.to_le_bytes());
        }
        Self {
            name: name.to_owned(),
            unit: unit.to_owned(),
            sample_type: SampleType::F32,
            mirrors,
            data,
        }
    }

    /// Packs `values` as a `u8` channel mirroring `mirrors`.
    pub fn u8(name: &str, unit: &str, mirrors: usize, values: Vec<u8>) -> Self {
        Self {
            name: name.to_owned(),
            unit: unit.to_owned(),
            sample_type: SampleType::U8,
            mirrors,
            data: values,
        }
    }
}

/// Everything a successful `derive` returns.
#[derive(Debug, Clone, Default)]
pub struct PassOutput {
    /// Parameter values the pass actually used, as `key`/`value` strings.
    /// Sorted by key before being recorded.
    pub params: Vec<(String, String)>,
    /// Names of the source channels the pass read.
    pub inputs: Vec<String>,
    /// Derived channels to append.
    pub channels: Vec<DerivedChannel>,
}

/// A named, versioned, lossless processing pass.
///
/// Passes only append channels; they never mutate source data. The version
/// is bumped whenever the pass would produce different outputs for
/// identical inputs, so a recorded `name@version` pins the exact
/// derivation.
pub trait TelemetryPass: Send + Sync {
    /// Stable dotted identifier, e.g. `gps.clean`.
    fn name(&self) -> &'static str;
    /// Algorithm revision. Bump when outputs change for identical inputs.
    fn version(&self) -> u32;
    /// One-sentence summary of what the pass derives.
    fn description(&self) -> &'static str;
    /// Prose statement of what has to be true of a source for this pass to
    /// be employed. `check` is the machine-checked form of the same rules.
    fn requirements(&self) -> &'static str;
    /// Inspects `source` and reports whether the preconditions hold.
    fn check(&self, source: &dyn TelemetrySource) -> Applicability;
    /// Computes the derived channels. Only called after `check` returned
    /// [`Applicability::Ready`].
    fn derive(&self, source: &dyn TelemetrySource) -> Result<PassOutput, PassError>;
    /// `name@version`, the label recorded in provenance and shown in UIs.
    fn label(&self) -> String {
        format!("{}@{}", self.name(), self.version())
    }
}

/// Errors from applying passes.
#[derive(Debug, thiserror::Error)]
pub enum PassError {
    /// The source records this pass at a different version. Strip the
    /// derived channels back to the raw conversion, then re-apply.
    #[error(
        "pass {name} was recorded at version {recorded} but this build \
         implements version {current}; strip derived channels first \
         (telemetry-convert --strip-passes) and re-apply"
    )]
    VersionConflict {
        /// Pass name.
        name: String,
        /// Version recorded in the source's provenance.
        recorded: u32,
        /// Version implemented by this build.
        current: u32,
    },
    /// A derived channel's data length does not match its mirror's sample
    /// count.
    #[error(
        "pass {pass} produced {actual} bytes for channel {channel:?} but its \
         mirror requires {expected}"
    )]
    OutputShape {
        /// Pass label.
        pass: String,
        /// Derived channel name.
        channel: String,
        /// Required byte length (`mirror sample count * sample width`).
        expected: usize,
        /// Actual byte length produced.
        actual: usize,
    },
    /// A derived channel mirrors a channel index that does not exist.
    #[error("pass {pass} mirrors channel index {mirrors} but the source has {channel_count}")]
    BadMirror {
        /// Pass label.
        pass: String,
        /// Requested mirror index.
        mirrors: usize,
        /// Number of channels in the source.
        channel_count: usize,
    },
    /// `derive` was called on a source that does not satisfy the pass's
    /// preconditions.
    #[error("pass {pass} precondition failed: {reason}")]
    Precondition {
        /// Pass label.
        pass: String,
        /// What was missing.
        reason: String,
    },
}

/// Outcome of one pass against one source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PassOutcome {
    /// The pass ran and appended these channels.
    Applied {
        /// Names of the appended channels.
        outputs: Vec<String>,
    },
    /// The pass was not employed.
    Skipped {
        /// User-facing reason.
        reason: String,
    },
}

/// What happened when a pass was offered a source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PassReport {
    /// Pass name.
    pub name: String,
    /// Pass version.
    pub version: u32,
    /// Whether it was applied or skipped, and why.
    pub outcome: PassOutcome,
}

impl PassReport {
    /// `name@version`.
    pub fn label(&self) -> String {
        format!("{}@{}", self.name, self.version)
    }
}

/// A pass that is designed and documented but not yet implemented.
///
/// Listed so tooling can show the full strategy ladder — including which
/// strategies exist in principle — next to the ones actually employed.
#[derive(Debug, Clone, Copy)]
pub struct PlannedPass {
    /// Stable dotted identifier the implementation will use.
    pub name: &'static str,
    /// What the pass will derive.
    pub summary: &'static str,
    /// What has to be true of a source for the pass to be employed.
    pub requirements: &'static str,
}

/// Passes designed in `docs/WHY_POSITIONING_IS_HARD.md` but not yet
/// implemented. Order is the intended position in the ladder after the
/// implemented registry.
pub const PLANNED: &[PlannedPass] = &[
    PlannedPass {
        name: "progress.project",
        summary: "Projects cleaned GPS onto the venue centerline spline, \
                  yielding an arc-length lap-progress channel with sigma.",
        requirements: "gps.clean outputs present; a surveyed centerline \
                       spline for the venue; a confident venue match from \
                       the track atlas.",
    },
    PlannedPass {
        name: "progress.fuse",
        summary: "Fuses the wheel-speed odometer with GPS-anchored progress \
                  through a monotonic filter, so progress is smooth at \
                  odometer rate, pinned to GPS truth, and never runs \
                  backwards except through reverse/spin gates.",
        requirements: "Distance Odometer present, plus either \
                       progress.project output or per-lap start/finish \
                       anchors (beacons).",
    },
    PlannedPass {
        name: "landmark.damper",
        summary: "Detects repeatable bump signatures in damper-position \
                  channels and records them as landmark spans, so no-GPS \
                  sources can be aligned on the same physical bump just \
                  before a corner — exactly where turn-in alignment \
                  matters most.",
        requirements: "Damper position channels sampled at 100 Hz or \
                       faster; a reference lap to correlate against.",
    },
    PlannedPass {
        name: "progress.time",
        summary: "Last-resort progress from lap-relative elapsed time \
                  alone, honestly labeled with a large sigma.",
        requirements: "Lap start/end boundaries; nothing else. Employed \
                       only when every better strategy was skipped.",
    },
];

/// The implemented pass ladder, in application order.
pub fn registry() -> Vec<Box<dyn TelemetryPass>> {
    vec![
        Box::new(GpsQuality),
        Box::new(GpsClean),
        Box::new(SpeedDistance),
    ]
}

/// Applies the full [`registry`] to `source`.
///
/// Returns the accumulated view (base channels plus everything derived)
/// and one report per pass saying whether it was employed and why not
/// otherwise. Passes already recorded on the source at the same version
/// are skipped; a version mismatch is an error rather than a silent
/// re-derivation.
pub fn apply_registry(
    source: &dyn TelemetrySource,
) -> Result<(PassedSource<'_>, Vec<PassReport>), PassError> {
    apply_passes(source, &registry())
}

/// Applies an explicit list of passes to `source`, in order.
///
/// Each pass sees the channels derived by the passes before it, so later
/// passes can consume earlier outputs (e.g. `gps.clean` reads
/// `GPS Fix Valid` from `gps.quality`).
pub fn apply_passes<'a>(
    source: &'a dyn TelemetrySource,
    passes: &[Box<dyn TelemetryPass>],
) -> Result<(PassedSource<'a>, Vec<PassReport>), PassError> {
    let mut passed = PassedSource::new(source);
    let mut reports = Vec::with_capacity(passes.len());
    for pass in passes {
        let mut report = PassReport {
            name: pass.name().to_owned(),
            version: pass.version(),
            outcome: PassOutcome::Skipped {
                reason: String::new(),
            },
        };
        if let Some(recorded) = passed
            .applied_passes()
            .iter()
            .find(|applied| applied.name == pass.name())
        {
            if recorded.version != pass.version() {
                return Err(PassError::VersionConflict {
                    name: pass.name().to_owned(),
                    recorded: recorded.version,
                    current: pass.version(),
                });
            }
            report.outcome = PassOutcome::Skipped {
                reason: "already applied (recorded in source)".to_owned(),
            };
            reports.push(report);
            continue;
        }
        match pass.check(&passed) {
            Applicability::Skipped { reason } => {
                report.outcome = PassOutcome::Skipped { reason };
                reports.push(report);
                continue;
            }
            Applicability::Ready => {}
        }
        let output = pass.derive(&passed)?;
        if let Some(collision) = output.channels.iter().find(|derived| {
            passed
                .channels()
                .iter()
                .any(|channel| channel.name == derived.name)
        }) {
            report.outcome = PassOutcome::Skipped {
                reason: format!(
                    "output channel {:?} already present in source",
                    collision.name
                ),
            };
            reports.push(report);
            continue;
        }
        let outputs = passed.push(pass.name(), pass.version(), output)?;
        report.outcome = PassOutcome::Applied { outputs };
        reports.push(report);
    }
    Ok((passed, reports))
}

/// Lowercase alphanumeric projection used for channel-name matching,
/// mirroring the facade crate's `normalized_eq`.
fn normalized(name: &str) -> String {
    name.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|character| character.to_ascii_lowercase())
        .collect()
}

/// Finds the first channel whose normalized name matches any of `names`,
/// in `names` priority order.
pub(crate) fn find_channel(channels: &[Channel], names: &[&str]) -> Option<usize> {
    names.iter().find_map(|want| {
        channels
            .iter()
            .position(|channel| normalized(&channel.name) == *want)
    })
}

/// Checks that a latitude channel is plausibly decimal degrees.
///
/// VBOX sources store coordinates in arc-minutes; grading or cleaning those
/// as degrees would silently produce garbage, so GPS passes skip with an
/// explicit "decode-level normalization required" reason instead. Unit
/// strings are checked first, then the values themselves on a thin probe,
/// because unit strings lie or go missing.
pub(crate) fn degrees_precondition(
    source: &dyn TelemetrySource,
    latitude: usize,
) -> Result<(), String> {
    let channel = &source.channels()[latitude];
    let unit = channel.unit.to_ascii_lowercase();
    if matches!(unit.as_str(), "min" | "arcmin" | "arcminute" | "arcminutes") {
        return Err(format!(
            "GPS coordinates are stored in {unit}; decode-level \
             normalization to decimal degrees is required first"
        ));
    }
    let mut magnitudes = Vec::with_capacity(512);
    let stride = (channel.sample_count / 512).max(1);
    for (chunk_index, chunk) in channel.chunks.iter().enumerate() {
        let mut local = 0;
        while local < chunk.sample_count {
            let value = source.decode(latitude, chunk_index, local);
            if value.is_finite() && value != 0.0 {
                magnitudes.push(value.abs());
            }
            local += stride;
        }
    }
    if magnitudes.is_empty() {
        return Ok(());
    }
    magnitudes.sort_by(f64::total_cmp);
    let median = magnitudes[magnitudes.len() / 2];
    if median > 90.0 {
        return Err(format!(
            "GPS coordinate values are not decimal degrees \
             (median |latitude| is {median:.1}); decode-level \
             normalization is required first"
        ));
    }
    Ok(())
}

/// Collects `(file-relative time ns, decoded value)` for every native
/// sample of one channel, in time order.
pub(crate) fn collect_samples(
    source: &dyn TelemetrySource,
    channel_index: usize,
) -> Vec<(u64, f64)> {
    let channel = &source.channels()[channel_index];
    let mut samples = Vec::with_capacity(channel.sample_count as usize);
    for (chunk_index, chunk) in channel.chunks.iter().enumerate() {
        for local in 0..chunk.sample_count {
            samples.push((
                source.sample_time_ns(channel_index, chunk_index, local),
                source.decode(channel_index, chunk_index, local),
            ));
        }
    }
    samples
}
