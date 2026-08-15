#![doc = include_str!("../README.md")]
#![deny(missing_docs)]

/// One racing layout at a facility.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layout {
    /// Stable layout identifier within the track.
    pub id: &'static str,
    /// Human-readable layout name.
    pub name: &'static str,
    /// Official lap length in metres, when known.
    pub length_m: Option<f64>,
    /// Declared driving direction, when known.
    pub direction: Option<&'static str>,
    /// Original compact centerline representation from track-atlas.
    pub centerline: &'static str,
    /// Centerline as an embedded GeoJSON feature collection.
    pub centerline_geojson: &'static str,
    /// JSON array containing point layers such as corners and start/finish.
    pub point_layers_json: &'static str,
    /// JSON array containing range layers such as sectors and complexes.
    pub range_layers_json: &'static str,
}

/// A racing facility and all of its known layouts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Track {
    /// Stable track-atlas slug.
    pub slug: &'static str,
    /// Human-readable facility name.
    pub name: &'static str,
    /// Alternate names from the upstream dataset.
    pub aka: &'static [&'static str],
    /// ISO 3166-1 alpha-2 country code when available.
    pub country: &'static str,
    /// IANA timezone of the facility, e.g. `America/New_York`.
    pub timezone: &'static str,
    /// Facility reference latitude in WGS84 degrees.
    pub latitude: f64,
    /// Facility reference longitude in WGS84 degrees.
    pub longitude: f64,
    /// Known configurations for this facility.
    pub layouts: &'static [Layout],
}

/// The nearest facility match and its default layout.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackMatch {
    /// Matched racing facility.
    pub track: &'static Track,
    /// Facility's default (first) layout.
    pub layout: &'static Layout,
    /// Great-circle distance from the query point to the facility reference.
    pub distance_m: f64,
}

include!(concat!(env!("OUT_DIR"), "/track_atlas.rs"));

/// Returns the complete embedded track catalog.
pub fn tracks() -> &'static [Track] {
    TRACKS
}

/// Finds a facility by its exact track-atlas slug.
pub fn find_track(slug: &str) -> Option<&'static Track> {
    TRACKS.iter().find(|track| track.slug == slug)
}

/// IANA timezone for a venue name, slug, or alias.
///
/// Matching is conservative: exact name/slug/aka (ignoring case and
/// punctuation) or a full-token prefix (`Sebring` → `Sebring International
/// Raceway`). Returns `None` rather than guessing.
pub fn timezone_for_venue(venue: &str) -> Option<&'static str> {
    find_track_for_venue(venue)
        .and_then(|track| (!track.timezone.is_empty()).then_some(track.timezone))
}

/// Facility match for a venue string. See [`timezone_for_venue`].
pub fn find_track_for_venue(venue: &str) -> Option<&'static Track> {
    let needle = normalize_name(venue);
    if needle.is_empty() {
        return None;
    }
    TRACKS.iter().find(|track| {
        names_match(&needle, track.name)
            || names_match(&needle, track.slug)
            || track.aka.iter().any(|aka| names_match(&needle, aka))
    })
}

fn normalize_name(value: &str) -> String {
    let mut out = String::new();
    let mut space = false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            if space && !out.is_empty() {
                out.push(' ');
            }
            space = false;
            out.extend(ch.to_lowercase());
        } else if ch == '-' || ch == '_' || ch.is_whitespace() {
            space = true;
        }
    }
    out
}

fn names_match(venue: &str, candidate: &str) -> bool {
    let candidate = normalize_name(candidate);
    venue == candidate
        || candidate.starts_with(&format!("{venue} "))
        || venue.starts_with(&format!("{candidate} "))
}

/// Finds the nearest facility within `max_distance_m` of a WGS84 point.
///
/// The returned layout is the facility's first layout. Returns `None` if no
/// facility is close enough or the matched facility has no layouts.
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

    #[test]
    fn timezone_lookup_matches_venue_names() {
        assert_eq!(timezone_for_venue("Sebring"), Some("America/New_York"));
        assert_eq!(timezone_for_venue("Road America"), Some("America/Chicago"));
        assert_eq!(timezone_for_venue("unknown circuit"), None);
        assert_eq!(timezone_for_venue(""), None);
    }
}
