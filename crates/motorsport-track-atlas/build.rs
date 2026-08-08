use serde_json::Value;
use std::{env, fs, path::PathBuf};

fn q(value: &str) -> String {
    format!("{:?}", value)
}

fn main() {
    println!("cargo:rerun-if-changed=data/tracks.jsonl");
    println!("cargo:rerun-if-changed=data/track-atlas-revision.txt");
    let input = fs::read_to_string("data/tracks.jsonl").expect("read track-atlas tracks.jsonl");
    let revision = fs::read_to_string("data/track-atlas-revision.txt")
        .expect("read track-atlas revision")
        .trim()
        .to_owned();
    let mut tracks = String::new();
    for line in input.lines().filter(|line| !line.trim().is_empty()) {
        let track: Value = serde_json::from_str(line).expect("parse track-atlas JSONL");
        let slug = track["slug"].as_str().expect("track slug");
        let name = track["name"].as_str().expect("track name");
        let lat = track["location"]["lat"].as_f64().expect("track latitude");
        let lon = track["location"]["lon"].as_f64().expect("track longitude");
        let country = track["country"].as_str().unwrap_or("");
        let layouts = track["layouts"].as_array().expect("track layouts");
        let mut layout_code = String::new();
        for layout in layouts {
            let id = layout["id"].as_str().expect("layout id");
            let layout_name = layout["name"].as_str().expect("layout name");
            let length = layout["length_m"].as_f64();
            let direction = layout["direction"].as_str();
            let centerline = layout["geometry"]["centerline"].as_str().unwrap_or("");
            let centerline_geojson = serde_json::to_string(
                layout
                    .get("centerline_geojson")
                    .expect("embedded centerline geometry"),
            )
            .unwrap();
            let points = layout["point_layers"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let ranges = layout["range_layers"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let points_json = serde_json::to_string(&points).unwrap();
            let ranges_json = serde_json::to_string(&ranges).unwrap();
            layout_code.push_str(&format!(
                "Layout {{ id: {}, name: {}, length_m: {}, direction: {}, centerline: {}, centerline_geojson: {}, point_layers_json: {}, range_layers_json: {} }},",
                q(id),
                q(layout_name),
                length.map_or("None".into(), |value| format!("Some({value:?})")),
                direction.map_or("None".into(), |value| format!("Some({})", q(value))),
                q(centerline),
                q(&centerline_geojson),
                q(&points_json),
                q(&ranges_json)
            ));
        }
        tracks.push_str(&format!(
            "Track {{ slug: {}, name: {}, country: {}, latitude: {lat}, longitude: {lon}, layouts: &[{layout_code}] }},",
            q(slug), q(name), q(country)
        ));
    }
    let output = format!(
        "pub const TRACK_ATLAS_REVISION: &str = {};\npub static TRACKS: &[Track] = &[{}];\n",
        q(&revision),
        tracks
    );
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("track_atlas.rs");
    fs::write(out, output).expect("write generated track atlas");
}
