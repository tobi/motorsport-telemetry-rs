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
        // Track positions and odometers, frequencies, power, and resistance.
        Dimension::Length | Dimension::Frequency | Dimension::Power | Dimension::Resistance => {
            (-1.0e9, 1.0e9)
        }
        // ~100 g. Impacts reach 60 g; nothing survives ten times that.
        Dimension::Acceleration => (-1000.0, 1000.0),
        // Unwrapped headings, ignition coil and hybrid bus potentials/currents.
        Dimension::Angle | Dimension::Voltage | Dimension::Current => (-1.0e5, 1.0e5),
        // 30000 rad/s is ~286000 rpm, past any turbo or driveline sensor.
        Dimension::AngularVelocity => (-30000.0, 30000.0),
        // Ratios may be fractions or percentages; none of these reaches 1e6.
        Dimension::AngularAcceleration | Dimension::Torque | Dimension::Mass | Dimension::Ratio => {
            (-1.0e6, 1.0e6)
        }
        // 0..10 kbar absolute; brake lines peak near 200 bar.
        Dimension::Pressure => (-1.0e6, 1.0e9),
        // 0 K to well past exhaust gas temperature.
        Dimension::Temperature => (0.0, 5000.0),
        // Includes Unix-epoch clocks as well as session-relative seconds.
        Dimension::Time | Dimension::Energy => (-1.0e12, 1.0e12),
        Dimension::Force => (-1.0e7, 1.0e7),
        // Flow and volume in SI units; decibel-like scales are compressed.
        Dimension::Volume
        | Dimension::VolumetricFlow
        | Dimension::MassFlow
        | Dimension::Logarithmic => (-1.0e4, 1.0e4),
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
    /// Whether to compare the recovered laps against the car's motion
    /// against motion estimates. Costs at most 4096 speed lookups per
    /// checked interval plus lap recovery; on by default.
    pub check_laps: bool,
}

impl Default for ValidateOptions {
    fn default() -> Self {
        Self {
            samples_per_channel: SAMPLES_PER_CHANNEL,
            file_len: None,
            check_laps: true,
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
    if options.check_laps {
        check_laps(source, &mut diagnostics);
    }
    diagnostics
}

/// A file with this much running and no laps merits a missing-lap warning.
const MOVING_WITHOUT_LAPS_NS: u64 = 300_000_000_000;
/// Review threshold, not a universal maximum: slow laps, long circuits and
/// non-circuit recordings can legitimately exceed it.
const LONG_LAP_NS: u64 = 720_000_000_000;
/// Fraction of a long lap the car must be moving for it to count as
/// "running": a red flag or a garage stint is a long lap but not a missing
/// beacon.
const LONG_LAP_MOVING_FRACTION: f64 = 0.6;

/// Flags lap structures that contradict the car's motion.
///
/// The lap recovery in [`crate::laps`] is faithful to the counters and
/// timers it finds; when those never advance (no beacon configured at a
/// test, a receiver that missed every crossing) it reports one enormous lap
/// or none at all. Neither is a decode error, but both mean the lap data is
/// unusable, and a reader cannot know that — it takes a speed channel to see
/// the car was lapping the whole time.
fn check_laps(source: &dyn TelemetrySource, diagnostics: &mut Diagnostics) {
    let channels = source.channels();
    let Some(speed) = crate::laps::speed_channel(source) else {
        return;
    };
    let duration_ns = channels
        .iter()
        .map(|channel| channel.duration_ns)
        .max()
        .unwrap_or(0);
    if duration_ns == 0 {
        return;
    }
    let moving_between = |start_ns: u64, end_ns: u64| {
        let motion = crate::motion::summarize_motion(source, speed, start_ns, end_ns);
        (motion.moving_ns, motion.top_speed_mps.unwrap_or(0.0))
    };
    let laps = crate::read_source_metadata(source).laps;
    if laps.is_empty() {
        let (moving_ns, top_mps) = moving_between(0, duration_ns);
        if moving_ns >= MOVING_WITHOUT_LAPS_NS {
            diagnostics.warning(
                "laps.none_while_moving",
                format!(
                    "no laps were recovered, yet speed samples indicate about {:.0} s \
                     moving out of {:.0} s (up to {:.0} km/h); check for missing lap markers",
                    moving_ns as f64 / 1e9,
                    duration_ns as f64 / 1e9,
                    top_mps * 3.6
                ),
            );
        }
        return;
    }
    for lap in &laps {
        if lap.duration_ns < LONG_LAP_NS {
            continue;
        }
        let (moving_ns, top_mps) = moving_between(lap.start_ns, lap.end_ns);
        let fraction = moving_ns as f64 / lap.duration_ns as f64;
        if fraction >= LONG_LAP_MOVING_FRACTION {
            diagnostics.warning(
                "laps.long_lap_while_moving",
                format!(
                    "lap {} spans {}:{:02} with an estimated {:.0}% moving time (up to \
                     {:.0} km/h); review for missed crossings or a genuinely long lap",
                    lap.number,
                    lap.duration_ns / 60_000_000_000,
                    lap.duration_ns % 60_000_000_000 / 1_000_000_000,
                    fraction * 100.0,
                    top_mps * 3.6
                ),
            );
        }
    }
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
    let mut previous_last = 0u64;
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
        // A chunk ends at its last *sample*, not one period past it. Readers
        // that fit each run's period re-anchor the next chunk on the logger's
        // own stamp, which may legitimately fall inside the last modeled
        // interval by a sample's worth of jitter; only a chunk that begins at
        // or before the previous chunk's final sample describes an
        // impossible timeline.
        if chunk.time_base_ns <= previous_last && index > 0 {
            diagnostics.push(
                Diagnostic::warning(
                    "layout.chunk_time_overlap",
                    format!(
                        "chunk {index} starts at {} ns, at or before the previous chunk's \
                         last sample at {previous_last} ns",
                        chunk.time_base_ns
                    ),
                )
                .with_channel(&channel.name),
            );
        }
        previous_last = chunk.time_base_ns.saturating_add(
            chunk
                .sample_count
                .saturating_sub(1)
                .saturating_mul(chunk.sample_period_ns),
        );
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
    let band = lookup(&channel.unit)
        .and_then(|def| plausible_band(def.dimension).map(|range| (def, range)));
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
            if value.is_finite() {
                if value.abs() > extreme.abs() {
                    extreme = value;
                }
                if value.abs() > ABSURD_MAGNITUDE {
                    absurd += 1;
                    first_absurd_ns.get_or_insert(time_ns);
                } else if let Some((def, (low, high))) = band {
                    // Bands are SI/base-unit bounds. In particular, a
                    // negative Celsius reading is not negative kelvin.
                    let base = def.to_base(value);
                    if !base.is_finite() || base < low || base > high {
                        out_of_band += 1;
                        first_out_of_band_ns.get_or_insert(time_ns);
                    }
                }
            } else {
                nonfinite += 1;
                first_nonfinite_ns.get_or_insert(time_ns);
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
        fn path(&self) -> &'static str {
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

    /// A 1 Hz speed trace plus a 1 Hz lap counter of the same length.
    fn lapping(speed_mps: Vec<f64>, counter: Vec<f64>) -> Fake {
        let count = speed_mps.len() as u64;
        let channel = |id, name: &str, unit: &str| Channel {
            id,
            name: name.into(),
            unit: unit.into(),
            unit_source: UnitSource::Declared,
            sample_type: SampleType::F32,
            chunks: vec![Chunk {
                sample_period_ns: 1_000_000_000,
                sample_count: count,
                data_ptr: 0,
                sample_base: 0,
                time_base_ns: 0,
            }],
            sample_count: count,
            duration_ns: count * 1_000_000_000,
        };
        Fake {
            channels: vec![channel(1, "Speed", "m/s"), channel(2, "Lap Number", "")],
            values: vec![speed_mps, counter],
        }
    }

    #[test]
    fn running_for_twenty_minutes_with_a_frozen_lap_counter_is_flagged() {
        // 1300 s at 60 m/s, counter stuck at 1: one 21-minute "lap".
        let diagnostics = validate_source(&lapping(vec![60.0; 1300], vec![1.0; 1300]));
        let found = diagnostics
            .find("laps.long_lap_while_moving")
            .expect("long lap");
        assert!(found.message.contains("21:40"), "{}", found.message);
        assert!(!implies_decode_fault(&diagnostics));

        // Same counter, but the car sat in the garage: a long lap, not a
        // missing beacon.
        let mut parked = vec![0.0; 1300];
        parked[..300].fill(60.0);
        let diagnostics = validate_source(&lapping(parked, vec![1.0; 1300]));
        assert!(diagnostics.find("laps.long_lap_while_moving").is_none());

        // A counter that advances normally produces no finding.
        let counter: Vec<f64> = (0..1300).map(|t| f64::from(t / 100)).collect();
        let diagnostics = validate_source(&lapping(vec![60.0; 1300], counter));
        assert!(diagnostics.find("laps.long_lap_while_moving").is_none());
        assert!(diagnostics.find("laps.none_while_moving").is_none());
    }

    #[test]
    fn running_with_no_lap_information_at_all_is_flagged() {
        let mut fake = lapping(vec![60.0; 400], vec![0.0; 400]);
        fake.channels.truncate(1);
        fake.values.truncate(1);
        let diagnostics = validate_source(&fake);
        assert!(
            diagnostics.find("laps.none_while_moving").is_some(),
            "{diagnostics}"
        );
        // Short runs (a system check in the garage) are not worth a warning.
        let mut fake = lapping(vec![60.0; 200], vec![0.0; 200]);
        fake.channels.truncate(1);
        fake.values.truncate(1);
        assert!(validate_source(&fake)
            .find("laps.none_while_moving")
            .is_none());
    }

    #[test]
    fn physical_bands_are_checked_in_base_units_not_display_units() {
        // The real VBOX ambient sensor reports -40 °C (233.15 K), not
        // negative absolute temperature. Scale and offset both matter.
        for (unit, value) in [("°C", -40.0), ("°F", -40.0), ("km/h", 1500.0)] {
            let diagnostics = validate_source(&source(unit, SampleType::F64, vec![value]));
            assert!(
                diagnostics.find("value.out_of_range").is_none(),
                "{unit}: {diagnostics}"
            );
        }
        for (unit, value) in [("°C", -300.0), ("km/h", 3931.0)] {
            let diagnostics = validate_source(&source(unit, SampleType::F64, vec![value]));
            assert!(
                diagnostics.find("value.out_of_range").is_some(),
                "{unit}: {diagnostics}"
            );
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
