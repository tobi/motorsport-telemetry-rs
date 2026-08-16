//! `speed.distance` — integrated odometer with an honest uncertainty.

use crate::{
    collect_samples, find_channel, Applicability, DerivedChannel, PassError, PassOutput,
    TelemetryPass,
};
use motorsport_telemetry_core::{convert, TelemetrySource};

/// Speed channels in preference order (normalized). Chassis-derived speed
/// integrates better than GPS speed — it never drops out under bridges or
/// trees — so GPS speed is the last resort.
const SPEED: &[&str] = &[
    "groundspeed",
    "speedref",
    "corrspeed",
    "vehiclespeed",
    "wheelspeed",
    "speed",
    "gpsspeed",
];

/// Odometer drift as a fraction of distance traveled (tire growth, slip,
/// rolling-circumference error).
const DRIFT_RATE: f64 = 0.005;
/// Sample gaps longer than this are integrated but flagged as uncertain.
const MAX_GAP_S: f64 = 1.0;
/// Fraction of gap distance added to the uncertainty.
const GAP_UNCERTAINTY: f64 = 0.5;

/// Derives `Distance Odometer` (trapezoidal integral of the best speed
/// channel, meters) and `Distance Odometer Sigma` (accumulated drift plus
/// gap uncertainty, meters).
///
/// The odometer is monotone by construction — negative speeds clamp to
/// zero — which makes it the backbone for lap progress between GPS
/// anchors, and the only positioning signal for no-GPS sources.
#[derive(Debug, Clone, Copy, Default)]
pub struct SpeedDistance;

impl SpeedDistance {
    fn speed_channel(&self, source: &dyn TelemetrySource) -> Result<usize, String> {
        let channels = source.channels();
        let Some(index) = find_channel(channels, SPEED) else {
            return Err("no speed channel present (looked for ground, reference, \
                 corrected, vehicle, wheel, and GPS speed)"
                .to_owned());
        };
        let channel = &channels[index];
        if channel.sample_count == 0 {
            return Err(format!("speed channel {:?} has no samples", channel.name));
        }
        if channel.unit.is_empty() {
            return Err(format!(
                "speed channel {:?} has no declared unit; refusing to guess \
                 a scale for integration",
                channel.name
            ));
        }
        if !motorsport_telemetry_core::can_convert(&channel.unit, "m/s") {
            return Err(format!(
                "speed channel {:?} unit {:?} is not convertible to m/s",
                channel.name, channel.unit
            ));
        }
        Ok(index)
    }
}

impl TelemetryPass for SpeedDistance {
    fn name(&self) -> &'static str {
        "speed.distance"
    }

    fn version(&self) -> u32 {
        1
    }

    fn description(&self) -> &'static str {
        "Integrates the best speed channel into a monotone odometer with \
         an accumulated uncertainty"
    }

    fn requirements(&self) -> &'static str {
        "A speed channel (ground, reference, corrected, vehicle, wheel, or \
         GPS speed) with samples and a declared unit convertible to m/s."
    }

    fn check(&self, source: &dyn TelemetrySource) -> Applicability {
        match self.speed_channel(source) {
            Ok(_) => Applicability::Ready,
            Err(reason) => Applicability::Skipped { reason },
        }
    }

    fn derive(&self, source: &dyn TelemetrySource) -> Result<PassOutput, PassError> {
        let speed_index = self
            .speed_channel(source)
            .map_err(|reason| PassError::Precondition {
                pass: self.label(),
                reason,
            })?;
        let channel = &source.channels()[speed_index];
        let unit = channel.unit.clone();

        let samples = collect_samples(source, speed_index);
        let mut odometer = Vec::with_capacity(samples.len());
        let mut sigma = Vec::with_capacity(samples.len());
        let mut distance_m = 0.0f64;
        let mut gap_sigma_m = 0.0f64;
        let mut previous: Option<(u64, f64)> = None;
        for (time_ns, raw) in samples {
            let mut speed = convert(raw, &unit, "m/s").unwrap_or(f64::NAN);
            if !speed.is_finite() || speed < 0.0 {
                // Hold zero-or-last through dropouts; reverse driving still
                // moves the car forward along nothing, so clamp to zero
                // rather than winding the odometer backwards.
                speed = previous.map_or(0.0, |(_, last)| last.max(0.0));
            }
            if let Some((last_ns, last_speed)) = previous {
                let dt = (time_ns.saturating_sub(last_ns)) as f64 / 1e9;
                let segment = 0.5 * (last_speed + speed) * dt;
                distance_m += segment;
                if dt > MAX_GAP_S {
                    gap_sigma_m += GAP_UNCERTAINTY * segment;
                }
            }
            previous = Some((time_ns, speed));
            odometer.push(distance_m);
            sigma.push((DRIFT_RATE * distance_m + gap_sigma_m) as f32);
        }

        Ok(PassOutput {
            params: vec![
                ("drift_rate".to_owned(), "0.005".to_owned()),
                ("gap_uncertainty".to_owned(), "0.5".to_owned()),
                ("max_gap_s".to_owned(), "1".to_owned()),
            ],
            inputs: vec![channel.name.clone()],
            channels: vec![
                DerivedChannel::f64("Distance Odometer", "m", speed_index, &odometer),
                DerivedChannel::f32("Distance Odometer Sigma", "m", speed_index, &sigma),
            ],
        })
    }
}
