//! Unit-aware, read-only plausibility audit: one JSON object per recording.
//!
//! `cargo run --release --example probe -- PATH... > report.jsonl`
//! Add `--preview` for a bounded trace suitable for plotting. Use
//! `--files-from LIST` for a newline-delimited manifest to avoid another
//! recursive NAS walk. Flags are review candidates, not proof of reader bugs.
//! Existing native `.telemetry` files are excluded from directory walks because
//! opening an older native catalog can migrate it in place.
use motorsport_telemetry::{
    motorsport_telemetry_core::{can_convert, convert, motion::summarize_motion, TelemetrySource},
    open, SourceExt,
};
use serde_json::{json, Value};
use std::{
    io::{self, Write},
    path::{Path, PathBuf},
};

const SAMPLE_BUDGET: u64 = 200_000;

fn walk(path: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    if path.is_dir() {
        let mut entries = std::fs::read_dir(path)?.collect::<io::Result<Vec<_>>>()?;
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            if !entry.file_type()?.is_symlink() {
                walk(&entry.path(), out)?;
            }
        }
    } else if path.extension().is_some_and(|s| {
        ["pds", "ld", "vbo", "mp4"]
            .iter()
            .any(|e| s.eq_ignore_ascii_case(e))
    }) {
        out.push(path.to_path_buf());
    }
    Ok(())
}

#[derive(Default, Debug)]
struct Stats {
    n: u64,
    finite: u64,
    min: Option<f64>,
    max: Option<f64>,
    changes: u64,
}
impl Stats {
    fn json(&self) -> Value {
        json!({"n":self.n, "finite":self.finite, "min":self.min, "max":self.max,
            "changes":self.changes})
    }
}

fn stats(
    source: &dyn TelemetrySource,
    channel: usize,
    transform: impl Fn(f64) -> Option<f64>,
) -> Stats {
    let ch = &source.channels()[channel];
    let step = ch.sample_count.div_ceil(SAMPLE_BUDGET).max(1);
    let mut result = Stats::default();
    let mut previous = None;
    let mut global = 0;
    for (ci, chunk) in ch.chunks.iter().enumerate() {
        // Keep the stride channel-global, not one new budget per chunk.
        for local in (0..chunk.sample_count).step_by(usize::try_from(step).unwrap_or(usize::MAX)) {
            let local = local + (step - global % step) % step;
            if local >= chunk.sample_count {
                break;
            }
            result.n += 1;
            let raw = source.decode(channel, ci, local);
            // Core conversion clamps infinities; reject them BEFORE converting.
            let Some(v) = raw
                .is_finite()
                .then(|| transform(raw))
                .flatten()
                .filter(|v| v.is_finite())
            else {
                previous = None;
                continue;
            };
            result.finite += 1;
            result.min = Some(result.min.map_or(v, |old| old.min(v)));
            result.max = Some(result.max.map_or(v, |old| old.max(v)));
            if previous.is_some_and(|old| old != v) {
                result.changes += 1;
            }
            previous = Some(v);
        }
        global += chunk.sample_count;
    }
    result
}

fn converted_stats(source: &dyn TelemetrySource, channel: usize, target: &str) -> Stats {
    stats(source, channel, |v| {
        convert(v, &source.channels()[channel].unit, target).ok()
    })
}

fn coordinate_deg(v: f64, unit: &str, longitude: bool) -> Option<f64> {
    match unit.trim().to_ascii_lowercase().as_str() {
        // VBO's `min` means angular minutes, not minutes of time. Longitude
        // is west-positive, as in the facade's normalization contract.
        "min" | "arcmin" | "arcminute" => Some(v / 60.0 * if longitude { -1.0 } else { 1.0 }),
        _ => convert(v, unit, "deg").ok(),
    }
}

fn report(source: &dyn TelemetrySource) -> Value {
    let md = source.metadata();
    let roles = source.signal_roles();
    let channels = source.channels();
    let duration_ns = md.duration_ns;
    let mut flags = Vec::<String>::new();
    let role_name = |index: Option<usize>| index.map(|i| channels[i].name.as_str());
    let mut result = json!({
        "file":source.path(), "format":source.format(), "duration_ns":duration_ns,
        "duration_s":duration_ns as f64 / 1e9, "channels":channels.len(),
        "empty_channels":channels.iter().filter(|c| c.sample_count == 0).count(),
        "roles":{
            "speed":role_name(roles.speed), "throttle":role_name(roles.throttle),
            "brake":role_name(roles.brake), "steering":role_name(roles.steering),
            "rpm":role_name(roles.rpm), "gear":role_name(roles.gear),
            "lat":role_name(roles.latitude), "lon":role_name(roles.longitude),
            "lap_number":role_name(roles.lap_number), "lap_distance":role_name(roles.lap_distance)
        },
        "lap_count":md.laps.len(), "complete_laps":md.valid_laps,
        "laps":md.laps.iter().map(|l| json!({"n":l.number, "start_ns":l.start_ns,
            "end_ns":l.end_ns, "duration_ns":l.duration_ns, "complete":l.complete})).collect::<Vec<_>>()
    });
    for (name, index) in [
        ("speed", roles.speed),
        ("throttle", roles.throttle),
        ("brake", roles.brake),
        ("steering", roles.steering),
        ("rpm", roles.rpm),
    ] {
        let Some(index) = index else {
            flags.push(format!("no_{name}_role"));
            continue;
        };
        let channel = &channels[index];
        let raw = stats(source, index, Some);
        let mut field = raw.json();
        field["unit"] = json!(channel.unit);
        field["name"] = json!(channel.name);
        if raw.finite == 0 {
            flags.push(format!("{name}_no_finite_samples"));
        } else if raw.finite > 1 && raw.changes == 0 {
            flags.push(format!("{name}_constant"));
        }
        // Compare physical ranges only AFTER conversion. Pressure is not a
        // pedal fraction, and an angle-valued PPS has no known full scale.
        let targets: &[(&str, f64, f64)] = match name {
            "speed" => &[("km/h", -5.0, 450.0)],
            "throttle" => &[("%", -5.0, 105.0)],
            "brake" => &[("%", -5.0, 105.0), ("bar", -5.0, 400.0)],
            "steering" => &[("deg", -1000.0, 1000.0)],
            "rpm" => &[("rpm", -1.0, 20_000.0)],
            _ => &[],
        };
        if let Some(&(unit, low, high)) = targets
            .iter()
            .find(|(u, _, _)| can_convert(&channel.unit, u))
        {
            let normalized = converted_stats(source, index, unit);
            field["normalized"] = normalized.json();
            field["normalized"]["unit"] = json!(unit);
            if normalized.min.is_some_and(|v| v < low) || normalized.max.is_some_and(|v| v > high) {
                flags.push(format!("{name}_out_of_range"));
            }
        } else {
            flags.push(format!("{name}_normalization_unavailable"));
        }
        result[name] = field;
    }
    let motion = roles
        .speed
        .map(|i| summarize_motion(source, i, 0, duration_ns))
        .unwrap_or_default();
    result["motion"] = json!({"observed_s_estimate":motion.observed_ns as f64 / 1e9,
        "moving_s_estimate":motion.moving_ns as f64 / 1e9,
        "top_kmh_inspected":motion.top_speed_mps.map(|v| v * 3.6)});
    if md.laps.is_empty() {
        flags.push(
            if motion.moving_ns > 180_000_000_000 {
                "no_laps_but_moving"
            } else {
                "no_laps"
            }
            .into(),
        );
    } else if md.valid_laps == 0 && motion.moving_ns > 400_000_000_000 {
        flags.push("no_complete_laps_but_moving".into());
    }
    for lap in &md.laps {
        if lap.end_ns <= lap.start_ns {
            flags.push("nonpositive_lap_duration".into());
        }
        if lap.end_ns > duration_ns {
            flags.push("lap_beyond_duration".into());
        }
        if !lap.complete {
            continue;
        }
        if lap.duration_ns < 40_000_000_000 {
            flags.push(format!("short_lap_{}", lap.number));
        }
        if lap.duration_ns > 600_000_000_000 {
            flags.push(format!("long_lap_{}", lap.number));
        }
        if let Some(speed) = roles.speed {
            let motion = summarize_motion(source, speed, lap.start_ns, lap.end_ns);
            if motion.observed_ns as f64 >= lap.duration_ns as f64 * 0.8
                && motion.top_speed_mps.is_some_and(|v| v < 40.0 / 3.6)
            {
                flags.push(format!("stationary_lap_{}", lap.number));
            }
        }
    }
    if md.laps.windows(2).any(|w| w[1].start_ns < w[0].end_ns) {
        flags.push("laps_overlap".into());
    }
    if md.laps.windows(2).any(|w| w[1].number <= w[0].number) {
        flags.push("lap_numbers_not_increasing".into());
    }
    if let (Some(lat), Some(lon)) = (roles.latitude, roles.longitude) {
        let a = stats(source, lat, |v| {
            coordinate_deg(v, &channels[lat].unit, false)
        });
        let b = stats(source, lon, |v| {
            coordinate_deg(v, &channels[lon].unit, true)
        });
        result["gps"] = json!({"unit":"deg", "lat":a.json(), "lon":b.json()});
        match (a.min, a.max, b.min, b.max) {
            (Some(amin), Some(amax), Some(bmin), Some(bmax)) => {
                if amin < -90.0 || amax > 90.0 || bmin < -180.0 || bmax > 180.0 {
                    flags.push("gps_out_of_range".into());
                } else if amax - amin > 1.0 || bmax - bmin > 1.0 {
                    flags.push("gps_spread_too_large".into());
                } else if amax == 0.0 && amin == 0.0 && bmin == 0.0 && bmax == 0.0 {
                    flags.push("gps_zero".into());
                }
            }
            _ => flags.push("gps_normalized_data_unavailable".into()),
        }
    } else {
        flags.push("no_gps_role".into());
    }
    result["diagnostics"] = json!(source.validate().items().iter().map(|d| json!({
        "severity":format!("{:?}", d.severity), "code":d.code, "channel":d.channel, "message":d.message
    })).collect::<Vec<_>>());
    result["flags"] = json!(flags);
    result
}

/// Small visual-review trace. Pedals retain their declared physical meaning:
/// pressures become bar, ratios become percent, angles remain angles.
fn preview(source: &dyn TelemetrySource) -> Value {
    let roles = source.signal_roles();
    let indices = [roles.speed, roles.throttle, roles.brake, roles.lap_number];
    let units: Vec<Option<&str>> = indices
        .iter()
        .enumerate()
        .map(|(role, index)| {
            let channel = source.channels().get((*index)?)?;
            let targets: &[&str] = match role {
                0 => &["km/h"],
                1 => &["%"],
                2 => &["bar", "%"],
                _ => &[],
            };
            Some(
                targets
                    .iter()
                    .copied()
                    .find(|u| can_convert(&channel.unit, u))
                    .unwrap_or(&channel.unit),
            )
        })
        .collect();
    let duration = source
        .channels()
        .iter()
        .map(|c| c.duration_ns)
        .max()
        .unwrap_or(0);
    let step = duration.div_ceil(2000).max(1_000_000);
    let rows: Vec<Value> = (0..duration.div_ceil(step))
        .map(|bin| {
            let at = bin * step;
            let mut row = vec![json!(at as f64 / 1e9)];
            for (role, index) in indices.iter().enumerate() {
                let value = index.and_then(|index| {
                    let raw = source
                        .sample_at(index, at, false)
                        .filter(|v| v.is_finite())?;
                    let original = &source.channels()[index].unit;
                    let target = units[role]?;
                    if target == original {
                        Some(raw)
                    } else {
                        convert(raw, original, target).ok()
                    }
                });
                row.push(json!(value));
            }
            json!(row)
        })
        .collect();
    json!({"columns":["time_s", "speed", "throttle", "brake", "source_lap_counter"],
        "units":[Some("s"), units[0], units[1], units[2], units[3]], "samples":rows})
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut files = Vec::new();
    let mut args = std::env::args_os().skip(1);
    let mut include_preview = false;
    while let Some(arg) = args.next() {
        if arg == "--preview" {
            include_preview = true;
        } else if arg == "--files-from" {
            let path = args.next().ok_or("--files-from needs a manifest")?;
            files.extend(
                std::fs::read_to_string(path)?
                    .lines()
                    .filter(|p| !p.is_empty())
                    .map(PathBuf::from),
            );
        } else {
            walk(Path::new(&arg), &mut files)?;
        }
    }
    if files.is_empty() {
        return Err("no vendor recordings selected".into());
    }
    files.sort();
    files.dedup();
    let stdout = io::stdout();
    let mut out = stdout.lock();
    for path in files {
        // Refuse native paths even from a manifest: an audit must not migrate
        // the collection. The native writer has separate round-trip tests.
        let value = if path
            .extension()
            .is_some_and(|s| s.eq_ignore_ascii_case("telemetry"))
        {
            json!({"file":path, "error":"native migration excluded from read-only vendor audit"})
        } else {
            match open(&path) {
                Ok(source) => {
                    let mut value = report(source.as_ref());
                    if include_preview {
                        value["preview"] = preview(source.as_ref());
                    }
                    value
                }
                Err(error) => json!({"file":path,"error":error.to_string()}),
            }
        };
        if let Err(error) = writeln!(out, "{value}") {
            if error.kind() == io::ErrorKind::BrokenPipe {
                return Ok(());
            }
            return Err(error.into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use racelogic_telemetry::RacelogicFile;

    #[test]
    fn missing_speed_is_valid_json_and_not_stationary() {
        let source = RacelogicFile::from_bytes("nan.vbo", b"[header]\ntime\nvelocity kmh\n[column names]\ntime velocity\n[data]\n120000 NaN\n120001 NaN\n".to_vec()).unwrap();
        let value = report(&source);
        let text = value.to_string();
        assert!(serde_json::from_str::<Value>(&text).is_ok());
        assert_eq!(value["speed"]["max"], Value::Null);
        assert_eq!(value["motion"]["top_kmh_inspected"], Value::Null);
        assert!(value["flags"]
            .as_array()
            .unwrap()
            .contains(&json!("speed_no_finite_samples")));
        assert!(!value["flags"]
            .as_array()
            .unwrap()
            .contains(&json!("speed_constant")));
    }
    #[test]
    fn vbo_angular_minutes_are_not_degrees_or_minutes_of_time() {
        assert_eq!(coordinate_deg(1800.0, "min", false), Some(30.0));
        assert_eq!(coordinate_deg(4800.0, "min", true), Some(-80.0));
        assert_eq!(coordinate_deg(1.0, "unknown", false), None);
    }
    #[test]
    fn infinity_is_rejected_before_unit_conversion() {
        let source = RacelogicFile::from_bytes("inf.vbo", b"[header]\ntime\nvelocity kmh\n[column names]\ntime velocity\n[data]\n120000 inf\n120001 -inf\n".to_vec()).unwrap();
        let s = converted_stats(&source, 1, "km/h");
        assert_eq!(s.max, None);
        assert_eq!(s.finite, 0);
    }
}
