//! `gps.clean` — masked decimal-degree coordinates safe to position from.

use crate::{
    collect_samples, degrees_precondition, find_channel, gps_quality, Applicability,
    DerivedChannel, PassError, PassOutput, TelemetryPass,
};
use motorsport_telemetry_core::TelemetrySource;

/// Motion faster than this between consecutive fixes is a teleport, not a
/// race car (150 m/s = 540 km/h).
const MAX_SPEED_MPS: f64 = 150.0;
/// After this many consecutive rejections, accept the new position as a
/// re-anchor: the receiver genuinely re-acquired somewhere else.
const REANCHOR_AFTER: u32 = 8;

/// Derives `GPS Latitude Clean` / `GPS Longitude Clean`: decimal degrees on
/// the latitude channel's exact timeline, with invalid fixes and teleports
/// masked to NaN instead of being smoothed over.
///
/// Downstream consumers get a hard guarantee: every non-NaN sample is a
/// plausible position of this car. No interpolation, no filtering — samples
/// are either passed through exactly or masked.
#[derive(Debug, Clone, Copy, Default)]
pub struct GpsClean;

impl TelemetryPass for GpsClean {
    fn name(&self) -> &'static str {
        "gps.clean"
    }

    fn version(&self) -> u32 {
        1
    }

    fn description(&self) -> &'static str {
        "Masks invalid fixes and teleports out of the GPS coordinates; \
         every remaining sample is a plausible position"
    }

    fn requirements(&self) -> &'static str {
        "GPS latitude and longitude channels in decimal degrees. Uses \
         gps.quality's GPS Fix Valid flags when present; consecutive fixes \
         implying motion faster than 150 m/s are masked as teleports."
    }

    fn check(&self, source: &dyn TelemetrySource) -> Applicability {
        let channels = source.channels();
        let (Some(latitude), Some(longitude)) = (
            find_channel(channels, gps_quality::LATITUDE),
            find_channel(channels, gps_quality::LONGITUDE),
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
            find_channel(channels, gps_quality::LATITUDE),
            find_channel(channels, gps_quality::LONGITUDE),
        ) else {
            return Err(PassError::Precondition {
                pass: self.label(),
                reason: "no GPS coordinate channels present".to_owned(),
            });
        };
        let fix_valid = find_channel(channels, &["gpsfixvalid"]);

        let samples = collect_samples(source, latitude);
        let mut clean_latitude = Vec::with_capacity(samples.len());
        let mut clean_longitude = Vec::with_capacity(samples.len());
        let mut last_accepted: Option<(u64, f64, f64)> = None;
        let mut rejected_streak = 0u32;
        for (time_ns, lat) in samples {
            let lon = source
                .sample_at(longitude, time_ns, true)
                .unwrap_or(f64::NAN);
            let flagged_valid = match fix_valid {
                Some(index) => source
                    .sample_at(index, time_ns, false)
                    .is_some_and(|flag| flag != 0.0),
                None => true,
            };
            let sane = lat.is_finite()
                && lon.is_finite()
                && lat.abs() <= 90.0
                && lon.abs() <= 180.0
                && !(lat.abs() < 1e-7 && lon.abs() < 1e-7);
            let mut accept = flagged_valid && sane;
            if accept {
                if let Some((last_ns, last_lat, last_lon)) = last_accepted {
                    let dt = (time_ns.saturating_sub(last_ns)) as f64 / 1e9;
                    if dt > 0.0
                        && equirectangular_m(last_lat, last_lon, lat, lon) / dt > MAX_SPEED_MPS
                    {
                        rejected_streak += 1;
                        // A long streak means the receiver truly re-acquired
                        // elsewhere; re-anchor instead of masking forever.
                        if rejected_streak <= REANCHOR_AFTER {
                            accept = false;
                        }
                    }
                }
            }
            if accept {
                rejected_streak = 0;
                last_accepted = Some((time_ns, lat, lon));
                clean_latitude.push(lat);
                clean_longitude.push(lon);
            } else {
                clean_latitude.push(f64::NAN);
                clean_longitude.push(f64::NAN);
            }
        }

        let mut inputs = vec![
            channels[latitude].name.clone(),
            channels[longitude].name.clone(),
        ];
        if let Some(index) = fix_valid {
            inputs.push(channels[index].name.clone());
        }
        Ok(PassOutput {
            params: vec![
                ("max_speed_mps".to_owned(), "150".to_owned()),
                ("reanchor_after".to_owned(), "8".to_owned()),
            ],
            inputs,
            channels: vec![
                DerivedChannel::f64("GPS Latitude Clean", "deg", latitude, &clean_latitude),
                DerivedChannel::f64("GPS Longitude Clean", "deg", latitude, &clean_longitude),
            ],
        })
    }
}

/// Fast small-area distance; exact enough to spot a 150 m/s teleport.
fn equirectangular_m(lat0: f64, lon0: f64, lat1: f64, lon1: f64) -> f64 {
    const EARTH_RADIUS_M: f64 = 6_371_000.0;
    let mean_latitude = ((lat0 + lat1) * 0.5).to_radians();
    let dx = (lon1 - lon0).to_radians() * mean_latitude.cos() * EARTH_RADIUS_M;
    let dy = (lat1 - lat0).to_radians() * EARTH_RADIUS_M;
    (dx * dx + dy * dy).sqrt()
}
