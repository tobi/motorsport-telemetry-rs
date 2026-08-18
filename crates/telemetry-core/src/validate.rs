//! Format-neutral plausibility checks over a loaded [`TelemetrySource`].
//!
//! # Why this exists
//!
//! A reader can only report what it knows it guessed. It cannot notice that its
//! guess produced a car travelling at 1.5e308 m/s. That judgement needs physics,
//! not parsing, so it lives here and runs against any source.
//!
//! These checks caught a real defect: a Pi/Cosworth log whose sample type code
//! sat at an unexpected record offset decoded every channel as `float64`,
//! yielding speeds of 1.5e308 m/s and a summed sample footprint of 294% of the
//! file size. Both are impossible, and both are detectable without knowing
//! anything about PDS.
//!
//! # What is and is not an error
//!
//! Every finding here is a [`Severity::Warning`] at most. An implausible value
//! is evidence of a decode problem, but this module cannot prove which layer is
//! at fault, and a genuinely strange session (a crash, a sensor failure) must
//! still load. Callers decide whether to reject.
//!
//! # Bands
//!
//! Bands are deliberately generous: they exist to catch decode corruption by
//! orders of magnitude, not to police motorsport plausibility. A band must never
//! flag data a working sensor could legitimately produce.

use crate::diag::{Diagnostic, Diagnostics, Severity};
use crate::units::{lookup, Dimension};
use crate::TelemetrySource;

/// Inclusive range a working sensor could plausibly report, in the dimension's
/// SI base unit.
///
/// `None` means the dimension has no defensible bound: counters, codes, and
/// bitfields can legitimately hold any magnitude.
fn plausible_band(dimension: Dimension) -> Option<(f64, f64)> {
    Some(match dimension {
        // 500 m/s covers any land vehicle with a wide margin.
        Dimension::Speed => (-500.0, 500.0),
        // Track positions plus lifetime vehicle odometers (up to 1 million km).
        Dimension::Length => (-1.0e9, 1.0e9),
        // ~100 g. Impacts reach 60 g; nothing survives ten times that.
        Dimension::Acceleration => (-1000.0, 1000.0),
        // Unwrapped heading integrations stay far inside this.
        Dimension::Angle => (-1.0e5, 1.0e5),
        // 30000 rad/s is ~286000 rpm, past any turbo or driveline sensor.
        Dimension::AngularVelocity => (-30000.0, 30000.0),
        Dimension::AngularAcceleration => (-1.0e6, 1.0e6),
        // 0..10 kbar absolute; brake lines peak near 200 bar.
        Dimension::Pressure => (-1.0e6, 1.0e9),
        // 0 K to well past exhaust gas temperature.
        Dimension::Temperature => (0.0, 5000.0),
        // Includes Unix-epoch clocks as well as session-relative seconds.
        Dimension::Time => (-1.0e12, 1.0e12),
        Dimension::Frequency => (-1.0e9, 1.0e9),
        Dimension::Force => (-1.0e7, 1.0e7),
        Dimension::Torque => (-1.0e6, 1.0e6),
        Dimension::Energy => (-1.0e12, 1.0e12),
        Dimension::Power => (-1.0e9, 1.0e9),
        // Includes ignition coil and hybrid bus potentials.
        Dimension::Voltage => (-1.0e5, 1.0e5),
        Dimension::Current => (-1.0e5, 1.0e5),
        Dimension::Resistance => (-1.0e9, 1.0e9),
        Dimension::Mass => (-1.0e6, 1.0e6),
        Dimension::Volume => (-1.0e4, 1.0e4),
        Dimension::VolumetricFlow => (-1.0e4, 1.0e4),
        Dimension::MassFlow => (-1.0e4, 1.0e4),
        // A ratio channel may be stored as a fraction or a percent.
        Dimension::Ratio => (-1.0e6, 1.0e6),
        // Decibel-like scales are already compressed; nothing real is past this.
        Dimension::Logarithmic => (-1.0e4, 1.0e4),
        // Counts, codes, and markers carry no physical bound.
        Dimension::Count | Dimension::Marker => return None,
    })
}

/// Magnitude beyond which any channel, dimensioned or not, is implausible.
///
/// Reinterpreting narrow integers or `float32` as `float64` produces values
/// around 1e300. No sensor, counter, or bitfield reaches 1e15.
const ABSURD_MAGNITUDE: f64 = 1.0e15;

/// How many samples per channel to inspect.
///
/// Validation runs on every load, so it must stay cheap on a 40 MB, 1400
/// channel log. Corruption of the kind this catches is dense: when a channel is
/// decoded at the wrong width, a stride sample finds it immediately.
const SAMPLES_PER_CHANNEL: u64 = 256;

/// Settings for [`validate_source_with`].
#[derive(Debug, Clone, Copy)]
pub struct ValidateOptions {
    /// Samples inspected per channel.
    pub samples_per_channel: u64,
    /// Byte length of a source whose channels map one-to-one onto packed
    /// sample payloads.
    ///
    /// Supplying it enables the footprint check: the sum of every channel's
    /// `sample_count * byte_width` cannot exceed the bytes that exist. Use
    /// `None` for text formats and packet formats that expand one stored value
    /// into several decoded channels; their decoded footprint can legitimately
    /// exceed the source byte length.
    pub file_len: Option<u64>,
}

impl Default for ValidateOptions {
    fn default() -> Self {
        Self {
            samples_per_channel: SAMPLES_PER_CHANNEL,
            file_len: None,
        }
    }
}

/// Runs the default plausibility checks over `source`.
pub fn validate_source(source: &dyn TelemetrySource) -> Diagnostics {
    validate_source_with(source, ValidateOptions::default())
}

/// Runs the plausibility checks over `source` with explicit options.
pub fn validate_source_with(source: &dyn TelemetrySource, options: ValidateOptions) -> Diagnostics {
    let mut diagnostics = Diagnostics::new();
    let mut absurd_diagnostics = Vec::new();
    let mut active_channels = 0usize;
    check_footprint(source, options.file_len, &mut diagnostics);
    for (index, channel) in source.channels().iter().enumerate() {
        check_chunks(channel, &mut diagnostics);
        if channel.sample_count > 0 && options.samples_per_channel > 0 {
            active_channels += 1;
        }
        check_values(
            source,
            index,
            options.samples_per_channel,
            &mut diagnostics,
            &mut absurd_diagnostics,
        );
    }
    let absurd_channels = absurd_diagnostics.len();
    if absurd_channels >= 4 && absurd_channels.saturating_mul(20) >= active_channels {
        diagnostics.warning(
            "value.widespread_absurd_magnitude",
            format!(
                "{absurd_channels} of {active_channels} sampled channels contain values above \
                 1e15; the source is probably decoded with the wrong sample layout"
            ),
        );
    }
    // Put the summary before the channel details. On a completely misdecoded
    // 1400-channel log this keeps the decisive finding inside Diagnostics::CAP.
    diagnostics.extend(absurd_diagnostics);
    diagnostics
}

/// Flags a decoded footprint larger than the file that supposedly holds it.
///
/// This is the single strongest signal of a wrong sample width, because it needs
/// no knowledge of what the channel measures.
fn check_footprint(
    source: &dyn TelemetrySource,
    file_len: Option<u64>,
    diagnostics: &mut Diagnostics,
) {
    let Some(file_len) = file_len.filter(|len| *len > 0) else {
        return;
    };
    let footprint: u64 = source
        .channels()
        .iter()
        .map(|channel| {
            channel
                .sample_count
                .saturating_mul(channel.sample_type.byte_width() as u64)
        })
        .fold(0, u64::saturating_add);
    if footprint > file_len {
        diagnostics.warning(
            "layout.footprint_exceeds_file",
            format!(
                "channels claim {footprint} sample bytes but the file holds {file_len} \
                 ({:.0}%); at least one channel's sample width is wrong",
                footprint as f64 * 100.0 / file_len as f64
            ),
        );
    }
}

/// Flags chunk tables that cannot describe a real timeline.
fn check_chunks(channel: &crate::Channel, diagnostics: &mut Diagnostics) {
    let mut previous_end = 0u64;
    for (index, chunk) in channel.chunks.iter().enumerate() {
        if chunk.sample_period_ns == 0 {
            diagnostics.push(
                Diagnostic::warning(
                    "layout.zero_sample_period",
                    format!("chunk {index} has a zero sample period; its timing is unusable"),
                )
                .with_channel(&channel.name),
            );
        }
        if chunk.time_base_ns < previous_end {
            diagnostics.push(
                Diagnostic::warning(
                    "layout.chunk_time_overlap",
                    format!(
                        "chunk {index} starts at {} ns, before the previous chunk ended at \
                         {previous_end} ns",
                        chunk.time_base_ns
                    ),
                )
                .with_channel(&channel.name),
            );
        }
        previous_end = chunk
            .time_base_ns
            .saturating_add(chunk.sample_count.saturating_mul(chunk.sample_period_ns));
    }
}

/// Stride-samples one channel and flags non-finite or implausible values.
fn check_values(
    source: &dyn TelemetrySource,
    index: usize,
    budget: u64,
    diagnostics: &mut Diagnostics,
    absurd_diagnostics: &mut Vec<Diagnostic>,
) {
    let channel = &source.channels()[index];
    if channel.sample_count == 0 || budget == 0 {
        return;
    }
    let band = lookup(&channel.unit).and_then(|def| plausible_band(def.dimension));
    let mut nonfinite = 0u64;
    let mut absurd = 0u64;
    let mut out_of_band = 0u64;
    let mut seen = 0u64;
    let mut extreme = 0.0f64;
    let mut first_nonfinite_ns = None;
    let mut first_absurd_ns = None;
    let mut first_out_of_band_ns = None;

    let stride = (channel.sample_count / budget).max(1);
    for (chunk_index, chunk) in channel.chunks.iter().enumerate() {
        let mut local = 0u64;
        while local < chunk.sample_count {
            let value = source.decode(index, chunk_index, local);
            let time_ns = source.sample_time_ns(index, chunk_index, local);
            seen += 1;
            if !value.is_finite() {
                nonfinite += 1;
                first_nonfinite_ns.get_or_insert(time_ns);
            } else {
                if value.abs() > extreme.abs() {
                    extreme = value;
                }
                if value.abs() > ABSURD_MAGNITUDE {
                    absurd += 1;
                    first_absurd_ns.get_or_insert(time_ns);
                } else if let Some((low, high)) = band {
                    if value < low || value > high {
                        out_of_band += 1;
                        first_out_of_band_ns.get_or_insert(time_ns);
                    }
                }
            }
            let next = local.saturating_add(stride);
            if next == local {
                break;
            }
            local = next;
        }
    }
    if seen == 0 {
        return;
    }
    if nonfinite > 0 {
        diagnostics.push(
            Diagnostic::warning(
                "value.not_finite",
                format!(
                    "{nonfinite} of {seen} inspected samples are NaN or infinite \
                     (first observed at {} ns)",
                    first_nonfinite_ns.unwrap_or(0)
                ),
            )
            .with_channel(&channel.name),
        );
    }
    if absurd > 0 {
        absurd_diagnostics.push(
            Diagnostic::warning(
                "value.absurd_magnitude",
                format!(
                    "{absurd} of {seen} inspected samples exceed 1e15 (extreme {extreme:.3e}, \
                     first observed at {} ns); the file may contain corrupt samples or this \
                     channel may use the wrong sample layout",
                    first_absurd_ns.unwrap_or(0)
                ),
            )
            .with_channel(&channel.name),
        );
    }
    if out_of_band > 0 {
        let unit = &channel.unit;
        diagnostics.push(
            Diagnostic::warning(
                "value.out_of_range",
                format!(
                    "{out_of_band} of {seen} inspected samples fall outside the plausible \
                     range for {unit} (extreme {extreme:.3}, first observed at {} ns)",
                    first_out_of_band_ns.unwrap_or(0)
                ),
            )
            .with_channel(&channel.name),
        );
    }
}

/// Returns whether `diagnostics` contains a finding that implies a decode fault.
///
/// One absurd channel can be a failed sensor or a corrupt packet. It only
/// implies a layout defect when the corruption is widespread, or when the
/// claimed sample footprint is physically larger than the source file.
pub fn implies_decode_fault(diagnostics: &Diagnostics) -> bool {
    diagnostics.items().iter().any(|item| {
        item.severity >= Severity::Warning
            && matches!(
                item.code,
                "layout.footprint_exceeds_file" | "value.widespread_absurd_magnitude"
            )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Channel, Chunk, SampleType, UnitSource};

    struct Fake {
        channels: Vec<Channel>,
        values: Vec<Vec<f64>>,
    }

    impl TelemetrySource for Fake {
        fn path(&self) -> &str {
            "fake"
        }
        fn format(&self) -> &'static str {
            "fake"
        }
        fn channels(&self) -> &[Channel] {
            &self.channels
        }
        fn decode(&self, channel_index: usize, _chunk_index: usize, local_index: u64) -> f64 {
            self.values[channel_index][local_index as usize]
        }
    }

    fn source(unit: &str, sample_type: SampleType, values: Vec<f64>) -> Fake {
        Fake {
            channels: vec![Channel {
                id: 1,
                name: "Speed".into(),
                unit: unit.into(),
                unit_source: UnitSource::SpecDefault,
                sample_type,
                chunks: vec![Chunk {
                    sample_period_ns: 20_000_000,
                    sample_count: values.len() as u64,
                    data_ptr: 0,
                    sample_base: 0,
                    time_base_ns: 0,
                }],
                sample_count: values.len() as u64,
                duration_ns: values.len() as u64 * 20_000_000,
            }],
            values: vec![values],
        }
    }

    #[test]
    fn plausible_speed_reports_nothing() {
        let diagnostics = validate_source(&source("m/s", SampleType::F32, vec![0.0, 42.0, 83.6]));
        assert!(diagnostics.is_empty(), "{diagnostics}");
    }

    #[test]
    fn float64_misread_speed_is_flagged_as_absurd() {
        let diagnostics =
            validate_source(&source("m/s", SampleType::F64, vec![0.0, 1.5e308, 3.0e200]));
        let found = diagnostics.find("value.absurd_magnitude").expect("absurd");
        assert_eq!(found.channel.as_deref(), Some("Speed"));
        assert!(
            !implies_decode_fault(&diagnostics),
            "one corrupt sensor is not proof of a layout defect"
        );
    }

    #[test]
    fn widespread_absurd_values_imply_a_decode_fault() {
        let mut fake = source("m/s", SampleType::F64, vec![1.5e308]);
        let template = fake.channels[0].clone();
        fake.channels = (0..5)
            .map(|index| {
                let mut channel = template.clone();
                channel.id = index;
                channel.name = format!("Broken {index}");
                channel
            })
            .collect();
        fake.values = vec![vec![1.5e308]; 5];
        let diagnostics = validate_source(&fake);
        assert!(
            diagnostics
                .find("value.widespread_absurd_magnitude")
                .is_some(),
            "{diagnostics}"
        );
        assert!(implies_decode_fault(&diagnostics));
    }

    #[test]
    fn speed_past_the_band_is_out_of_range_not_absurd() {
        let diagnostics = validate_source(&source("m/s", SampleType::F32, vec![0.0, 900.0]));
        assert!(
            diagnostics.find("value.out_of_range").is_some(),
            "{diagnostics}"
        );
        assert!(diagnostics.find("value.absurd_magnitude").is_none());
    }

    #[test]
    fn counts_have_no_band_so_large_indices_pass() {
        let diagnostics = validate_source(&source("", SampleType::U32, vec![0.0, 4.0e9]));
        assert!(diagnostics.is_empty(), "{diagnostics}");
    }

    #[test]
    fn non_finite_samples_are_reported() {
        let diagnostics = validate_source(&source("m/s", SampleType::F32, vec![f64::NAN, 1.0]));
        assert!(
            diagnostics.find("value.not_finite").is_some(),
            "{diagnostics}"
        );
    }

    #[test]
    fn footprint_larger_than_file_is_flagged() {
        let fake = source("m/s", SampleType::F64, vec![1.0; 64]);
        let options = ValidateOptions {
            file_len: Some(16),
            ..ValidateOptions::default()
        };
        let diagnostics = validate_source_with(&fake, options);
        assert!(
            diagnostics.find("layout.footprint_exceeds_file").is_some(),
            "{diagnostics}"
        );
        assert!(implies_decode_fault(&diagnostics));
    }

    #[test]
    fn zero_sample_period_is_flagged() {
        let mut fake = source("m/s", SampleType::F32, vec![1.0, 2.0]);
        fake.channels[0].chunks[0].sample_period_ns = 0;
        let diagnostics = validate_source(&fake);
        assert!(
            diagnostics.find("layout.zero_sample_period").is_some(),
            "{diagnostics}"
        );
    }
}
