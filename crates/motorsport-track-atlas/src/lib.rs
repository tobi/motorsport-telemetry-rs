#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layout {
    pub id: &'static str,
    pub name: &'static str,
    pub length_m: Option<f64>,
    pub direction: Option<&'static str>,
    pub centerline: &'static str,
    pub centerline_geojson: &'static str,
    pub point_layers_json: &'static str,
    pub range_layers_json: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Track {
    pub slug: &'static str,
    pub name: &'static str,
    pub country: &'static str,
    pub latitude: f64,
    pub longitude: f64,
    pub layouts: &'static [Layout],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackMatch {
    pub track: &'static Track,
    pub layout: &'static Layout,
    pub distance_m: f64,
}

include!(concat!(env!("OUT_DIR"), "/track_atlas.rs"));

pub fn tracks() -> &'static [Track] {
    TRACKS
}

pub fn find_track(slug: &str) -> Option<&'static Track> {
    TRACKS.iter().find(|track| track.slug == slug)
}

pub fn match_track(latitude: f64, longitude: f64, max_distance_m: f64) -> Option<TrackMatch> {
    TRACKS
        .iter()
        .filter_map(|track| {
            let distance = haversine_m(latitude, longitude, track.latitude, track.longitude);
            (distance <= max_distance_m).then_some((track, distance))
        })
        .min_by(|left, right| left.1.total_cmp(&right.1))
        .and_then(|(track, distance_m)| {
            track.layouts.first().map(|layout| TrackMatch {
                track,
                layout,
                distance_m,
            })
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_road_america_and_exposes_layers() {
        let matched = match_track(43.7978, -87.9899, 20_000.0).unwrap();
        assert_eq!(matched.track.slug, "road-america");
        assert_eq!(matched.layout.length_m, Some(6514.0));
        assert!(matched.layout.point_layers_json.contains("corners"));
        assert!(matched.layout.range_layers_json.contains("timing_sectors"));
    }
}
