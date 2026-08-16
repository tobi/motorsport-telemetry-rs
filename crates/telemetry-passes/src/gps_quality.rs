//! `gps.quality` — grade every GPS fix and derive a position sigma.

use crate::{
    collect_samples, degrees_precondition, find_channel, Applicability, DerivedChannel, PassError,
    PassOutput, TelemetryPass,
};
use motorsport_telemetry_core::TelemetrySource;

/// Latitude channel names, in priority order (normalized).
pub(crate) const LATITUDE: &[&str] = &["gpslatitude", "latitude", "gpslat", "lat"];
/// Longitude channel names, in priority order (normalized).
pub(crate) const LONGITUDE: &[&str] = &["gpslongitude", "longitude", "gpslon", "lon", "long"];
const SATELLITES: &[&str] = &[
    "gpssatellites",
    "satellites",
    "gpsnumsat",
    "numsats",
    "sats",
];
// AiM-style fix codes: 0 none, 1 dead reckoning, 2 = 2D, 3 = 3D. VBOX
// "solution type" uses different codes and is deliberately not matched.
const FIX_TYPE: &[&str] = &["gpsfixtype", "fixtype", "gpsfix"];
const ACCURACY: &[&str] = &[
    "gpspositionaccuracy",
    "gpsposaccuracy",
    "positionaccuracy",
    "gpsaccuracy",
];
const DOP: &[&str] = &["gpsdop", "gpshdop", "hdop"];

/// Fix codes below this are not a position solution (AiM: 2 = 2D fix).
const MIN_FIX: f64 = 2.0;
/// Minimum satellites for a usable solution when no fix-type channel exists.
const MIN_SATELLITES: f64 = 4.0;
/// AiM writes ~4294967.29 m into accuracy when there is no fix.
const SENTINEL_M: f64 = 4_000_000.0;
/// User-equivalent range error: sigma ≈ DOP × UERE when only DOP is known.
const UERE_M: f64 = 5.0;
/// Sigma when the fix is valid but no accuracy or DOP channel exists.
const DEFAULT_SIGMA_M: f64 = 15.0;

/// Grades every GPS fix (`GPS Fix Valid`) and derives a per-fix position
/// uncertainty in meters (`GPS Position Sigma`).
///
/// This is the gatekeeper for every GPS-based strategy downstream: it turns
/// "the camera wrote coordinates" into "these samples are a position
/// solution, this good". Sources whose GPS never locked — SmartyCam
/// sessions recorded before the receiver acquired — come out with every
/// sample invalid, which is the correct, honest answer.
#[derive(Debug, Clone, Copy, Default)]
pub struct GpsQuality;

impl TelemetryPass for GpsQuality {
    fn name(&self) -> &'static str {
        "gps.quality"
    }

    fn version(&self) -> u32 {
        1
    }

    fn description(&self) -> &'static str {
        "Grades every GPS fix and derives a per-fix position sigma in meters"
    }

    fn requirements(&self) -> &'static str {
        "GPS latitude and longitude channels with samples, in decimal \
         degrees. Fix type, satellite count, position accuracy, and DOP \
         channels are used when present; with none of them, validity falls \
         back to coordinate sanity alone."
    }

    fn check(&self, source: &dyn TelemetrySource) -> Applicability {
        let channels = source.channels();
        let (Some(latitude), Some(longitude)) = (
            find_channel(channels, LATITUDE),
            find_channel(channels, LONGITUDE),
        ) else {
            return Applicability::Skipped {
                reason: "no GPS coordinate channels present".to_owned(),
            };
        };
        if channels[latitude].sample_count == 0 || channels[longitude].sample_count == 0 {
            return Applicability::Skipped {
                reason: "GPS coordinate channels are empty".to_owned(),
            };
        }
        match degrees_precondition(source, latitude) {
            Ok(()) => Applicability::Ready,
            Err(reason) => Applicability::Skipped { reason },
        }
    }

    fn derive(&self, source: &dyn TelemetrySource) -> Result<PassOutput, PassError> {
        let channels = source.channels();
        let (Some(latitude), Some(longitude)) = (
            find_channel(channels, LATITUDE),
            find_channel(channels, LONGITUDE),
        ) else {
            return Err(PassError::Precondition {
                pass: self.label(),
                reason: "no GPS coordinate channels present".to_owned(),
            });
        };
        let fix_type = find_channel(channels, FIX_TYPE);
        let satellites = find_channel(channels, SATELLITES);
        let accuracy = find_channel(channels, ACCURACY);
        let dop = find_channel(channels, DOP);

        let samples = collect_samples(source, latitude);
        let mut valid = Vec::with_capacity(samples.len());
        let mut sigma = Vec::with_capacity(samples.len());
        for (time_ns, lat) in samples {
            let lon = source
                .sample_at(longitude, time_ns, false)
                .unwrap_or(f64::NAN);
            let fix = fix_type.and_then(|index| source.sample_at(index, time_ns, false));
            let sats = satellites.and_then(|index| source.sample_at(index, time_ns, false));
            let acc = accuracy.and_then(|index| source.sample_at(index, time_ns, false));
            let hdop = dop.and_then(|index| source.sample_at(index, time_ns, false));

            let coordinates_sane = lat.is_finite()
                && lon.is_finite()
                && lat.abs() <= 90.0
                && lon.abs() <= 180.0
                && !(lat.abs() < 1e-7 && lon.abs() < 1e-7);
            let solution_ok = match (fix, sats) {
                (Some(fix), _) => fix.is_finite() && fix >= MIN_FIX,
                (None, Some(sats)) => sats.is_finite() && sats >= MIN_SATELLITES,
                (None, None) => true,
            };
            let accuracy_ok = acc.is_none_or(|acc| !acc.is_finite() || acc < SENTINEL_M);
            let is_valid = coordinates_sane && solution_ok && accuracy_ok;

            valid.push(u8::from(is_valid));
            sigma.push(if !is_valid {
                f32::NAN
            } else if let Some(acc) = acc.filter(|acc| acc.is_finite() && *acc > 0.0) {
                acc as f32
            } else if let Some(hdop) =
                hdop.filter(|dop| dop.is_finite() && *dop > 0.0 && *dop < 99.0)
            {
                (hdop * UERE_M) as f32
            } else {
                DEFAULT_SIGMA_M as f32
            });
        }

        let mut inputs = vec![
            channels[latitude].name.clone(),
            channels[longitude].name.clone(),
        ];
        for index in [fix_type, satellites, accuracy, dop].into_iter().flatten() {
            inputs.push(channels[index].name.clone());
        }
        Ok(PassOutput {
            params: vec![
                ("default_sigma_m".to_owned(), "15".to_owned()),
                ("min_fix".to_owned(), "2".to_owned()),
                ("min_satellites".to_owned(), "4".to_owned()),
                ("sentinel_m".to_owned(), "4000000".to_owned()),
                ("uere_m".to_owned(), "5".to_owned()),
            ],
            inputs,
            channels: vec![
                DerivedChannel::u8("GPS Fix Valid", "", latitude, valid),
                DerivedChannel::f32("GPS Position Sigma", "m", latitude, &sigma),
            ],
        })
    }
}
