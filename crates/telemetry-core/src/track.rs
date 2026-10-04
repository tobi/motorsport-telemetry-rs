//! Local atlas geometry checks. No derived progress channels or GPS smoothing.
use crate::{names, SampleTimes, TelemetrySource};

/// A local planar copy of an atlas centerline and its pit landmarks.
#[derive(Debug)]
pub struct TrackGeometry {
    origin: (f64, f64),
    points: Vec<(f64, f64)>,
    lengths: Vec<f64>,
    pit: Option<(f64, f64)>,
}

/// Nearest centerline position; progress is used only for local validation.
#[derive(Debug, Clone, Copy)]
pub struct TrackPosition {
    /// Distance from the atlas centerline, in metres.
    pub distance_m: f64,
    /// Signed distance from the nearest segment, positive to its left.
    /// Local evidence only; this is not a derived channel.
    pub signed_distance_m: f64,
    /// Fraction of centerline length from its first point.
    pub progress: f64,
    /// Alongside the atlas pit-entry to pit-exit sector (not proof of pit lane).
    pub pit_sector: bool,
}

impl TrackGeometry {
    /// Loads embedded atlas geometry; returns `None` for an incomplete layout.
    pub fn new(layout: &motorsport_track_atlas::Layout) -> Option<Self> {
        let geo: serde_json::Value = serde_json::from_str(layout.centerline_geojson).ok()?;
        let line = geo["features"]
            .as_array()?
            .iter()
            .find(|f| f["properties"]["role"] == "outline")?["geometry"]["coordinates"]
            .as_array()?;
        let first = line.first()?.as_array()?;
        let origin = (first.get(1)?.as_f64()?, first.first()?.as_f64()?);
        let points: Option<Vec<_>> = line
            .iter()
            .map(|v| Some(project(v.get(1)?.as_f64()?, v.get(0)?.as_f64()?, origin)))
            .collect();
        let points = points?;
        let mut lengths = vec![0.0];
        for pair in points.windows(2) {
            lengths.push(lengths.last()? + (pair[1].0 - pair[0].0).hypot(pair[1].1 - pair[0].1));
        }
        if lengths.last().copied().unwrap_or(0.0) < 100.0 {
            return None;
        }
        let layers: serde_json::Value = serde_json::from_str(layout.point_layers_json).ok()?;
        let marker = |id: &str| -> Option<f64> {
            layers
                .as_array()?
                .iter()
                .flat_map(|l| l["items"].as_array().into_iter().flatten())
                .find(|p| p["id"] == id)?["marker"]
                .as_f64()
        };
        let pit = marker("pit_entry").zip(marker("pit_exit"));
        Some(Self {
            origin,
            points,
            lengths,
            pit,
        })
    }

    /// Projects one position onto the nearest atlas segment, for validation.
    pub fn locate(&self, latitude: f64, longitude: f64) -> Option<TrackPosition> {
        if !latitude.is_finite() || !longitude.is_finite() {
            return None;
        }
        let point = project(latitude, longitude, self.origin);
        let mut best = (f64::INFINITY, 0.0, 0.0);
        for (i, pair) in self.points.windows(2).enumerate() {
            let delta = (pair[1].0 - pair[0].0, pair[1].1 - pair[0].1);
            let squared = delta.0 * delta.0 + delta.1 * delta.1;
            if squared == 0.0 {
                continue;
            }
            let u = (((point.0 - pair[0].0) * delta.0 + (point.1 - pair[0].1) * delta.1) / squared)
                .clamp(0.0, 1.0);
            let distance =
                (point.0 - pair[0].0 - u * delta.0).hypot(point.1 - pair[0].1 - u * delta.1);
            if distance < best.0 {
                best = (
                    distance,
                    self.lengths[i] + u * squared.sqrt(),
                    (delta.0 * (point.1 - pair[0].1) - delta.1 * (point.0 - pair[0].0))
                        / squared.sqrt(),
                );
            }
        }
        let progress = best.1 / self.lengths.last()?;
        let pit_sector = self.pit.is_some_and(|(a, b)| {
            if a <= b {
                (a..=b).contains(&progress)
            } else {
                progress >= a || progress <= b
            }
        });
        Some(TrackPosition {
            distance_m: best.0,
            signed_distance_m: best.2,
            progress,
            pit_sector,
        })
    }

    pub(crate) fn pit_markers(&self) -> Option<(f64, f64)> {
        self.pit
    }
}

fn project(lat: f64, lon: f64, origin: (f64, f64)) -> (f64, f64) {
    (
        (lon - origin.1) * origin.0.to_radians().cos() * 111_195.0,
        (lat - origin.0) * 111_195.0,
    )
}

/// Native, recent GPS position with a valid fix and bounded reported error.
/// Missing fix status is unavailable evidence; carried-back coordinates
/// without receiver status cannot prove a physical crossing or pit location.
#[allow(
    clippy::float_cmp,
    reason = "Receiver fix type is an exact integer code"
)]
pub fn trusted_position(source: &dyn TelemetrySource, time: u64) -> Option<(f64, f64)> {
    let lat = names::find(source.channels(), &["gpslatitude", "latitude"])?;
    let lon = names::find(source.channels(), &["gpslongitude", "longitude"])?;
    let fix = names::find(source.channels(), &["gpsfixtype"])?;
    let accuracy = names::find(source.channels(), &["gpspositionaccuracy"])?;
    for index in [lat, lon, fix, accuracy] {
        if let SampleTimes::Explicit(times) = source.sample_times(index) {
            let i = times.partition_point(|t| *t <= time).checked_sub(1)?;
            if time - times[i] > 250_000_000 {
                return None;
            }
        }
    }
    let quality = source.sample_at(fix, time, false)?;
    let error = source.sample_at(accuracy, time, false)?;
    let a = crate::convert(
        source.sample_at(lat, time, false)?,
        &source.channels()[lat].unit,
        "deg",
    )
    .ok()?;
    let b = crate::convert(
        source.sample_at(lon, time, false)?,
        &source.channels()[lon].unit,
        "deg",
    )
    .ok()?;
    (quality == 3.0
        && error.is_finite()
        && (0.0..=8.0).contains(&error)
        && a.is_finite()
        && b.is_finite()
        && (-90.0..=90.0).contains(&a)
        && (-180.0..=180.0).contains(&b))
    .then_some((a, b))
}

pub(crate) fn on_circuit(source: &dyn TelemetrySource, time: u64) -> bool {
    let Some((lat, lon)) = trusted_position(source, time) else {
        return false;
    };
    let Some(track) = motorsport_track_atlas::match_track(lat, lon, 10_000.0) else {
        return false;
    };
    TrackGeometry::new(track.layout)
        .and_then(|g| g.locate(lat, lon))
        .is_some_and(|p| p.distance_m <= 20.0 && !p.pit_sector)
}
