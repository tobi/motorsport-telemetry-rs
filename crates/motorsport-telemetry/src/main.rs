use motorsport_telemetry::motorsport_telemetry_core::{FileMetadata, TelemetrySource};
use motorsport_telemetry::{open, TelemetryFile};
use racelogic_telemetry::RacelogicFile;
use serde_json::json;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use telemetry_format::needs_update;

const USAGE: &str = "Usage: motorsport-telemetry [--json] <file>";
const SUSPICIOUS_CLOCK_AGE_DAYS: i64 = 365 * 2;

#[derive(Debug)]
struct Inspection {
    file: String,
    format: String,
    format_version: Option<u16>,
    format_needs_update: Option<bool>,
    driver_ids: Vec<i64>,
    laps: usize,
    complete_laps: usize,
    fastest_lap_ns: Option<u64>,
    fastest_lap_number: Option<i64>,
    video_included: bool,
    video_filenames: Vec<String>,
    video_file_indices: Vec<u32>,
    video_presentation_offset_ns: Option<i128>,
    session_key: Option<String>,
    car_type: Option<String>,
    car_number: Option<String>,
    car_class: Option<String>,
    track_gps: Option<(f64, f64)>,
    track_name: Option<String>,
    layout: Option<String>,
    track_length_m: Option<f64>,
    event_date: Option<String>,
    event_date_source: Option<String>,
    event_date_warning: Option<String>,
}

fn main() {
    match arguments(std::env::args_os().skip(1)) {
        Ok(Command::Help) => println!("{USAGE}"),
        Ok(Command::Version) => println!("motorsport-telemetry {}", env!("CARGO_PKG_VERSION")),
        Ok(Command::Inspect { path, json }) => match inspect(&path) {
            Ok(inspection) if json => print_json(&inspection),
            Ok(inspection) => print_human(&inspection),
            Err(error) => {
                eprintln!("motorsport-telemetry: {error}");
                std::process::exit(1);
            }
        },
        Err(message) => {
            eprintln!("motorsport-telemetry: {message}\n{USAGE}");
            std::process::exit(2);
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Help,
    Version,
    Inspect { path: PathBuf, json: bool },
}

fn arguments(args: impl IntoIterator<Item = OsString>) -> Result<Command, String> {
    let mut path = None;
    let mut json = false;
    let mut positional_only = false;
    for argument in args {
        if !positional_only && argument == "--" {
            positional_only = true;
        } else if !positional_only && (argument == "-h" || argument == "--help") {
            return Ok(Command::Help);
        } else if !positional_only && (argument == "-V" || argument == "--version") {
            return Ok(Command::Version);
        } else if !positional_only && argument == "--json" {
            json = true;
        } else if !positional_only && argument.to_string_lossy().starts_with('-') {
            return Err(format!("unknown option {}", argument.to_string_lossy()));
        } else if path.replace(PathBuf::from(&argument)).is_some() {
            return Err("expected exactly one telemetry file".into());
        }
    }
    path.map(|path| Command::Inspect { path, json })
        .ok_or_else(|| "missing telemetry file".into())
}

fn inspect(path: &Path) -> Result<Inspection, motorsport_telemetry::TelemetryError> {
    let file = open_for_inspection(path)?;
    let metadata = file.metadata();
    let gps_candidates = average_gps_candidates(&file);
    let matched_gps = gps_candidates.iter().find_map(|&(latitude, longitude)| {
        motorsport_telemetry::motorsport_track_atlas::match_track(latitude, longitude, 50_000.0)
            .map(|matched| ((latitude, longitude), matched))
    });
    let track_gps = matched_gps
        .map(|(gps, _)| gps)
        .or_else(|| gps_candidates.first().copied());
    let matched = matched_gps.map(|(_, matched)| matched);
    let (video_included, video_file_indices) = video_info(&file, &metadata);
    let video_filenames = if metadata.video_frame_count.is_some() {
        path.file_name()
            .map(|name| vec![name.to_string_lossy().into_owned()])
            .unwrap_or_default()
    } else if video_included {
        nearby_video_filenames(path)
    } else {
        Vec::new()
    };
    let identity = &metadata.identity;
    let car_type = nonempty(&identity.vehicle).or_else(|| {
        first_semantic_value(
            &file,
            &["cartype", "vehicletype", "vehiclemodel", "carmodel"],
        )
    });
    let car_number = first_semantic_value(
        &file,
        &[
            "carnumber",
            "vehiclenumber",
            "racenumber",
            "competitionnumber",
        ],
    );
    let car_class = first_semantic_value(
        &file,
        &["carclass", "vehicleclass", "classid", "competitionclass"],
    );
    let event_date = event_date(path, &metadata);

    Ok(Inspection {
        file: path.to_string_lossy().into_owned(),
        format: metadata.format.clone(),
        format_version: metadata.format_version,
        format_needs_update: metadata.format_version.map(needs_update),
        driver_ids: metadata.driver_ids.clone(),
        laps: metadata.laps.len(),
        complete_laps: metadata.laps.iter().filter(|lap| lap.complete).count(),
        fastest_lap_ns: metadata.fastest_lap.as_ref().map(|lap| lap.duration_ns),
        fastest_lap_number: metadata.fastest_lap.as_ref().map(|lap| lap.number),
        video_included,
        video_filenames,
        video_file_indices,
        video_presentation_offset_ns: metadata.video_presentation_offset_ns,
        session_key: metadata.session_key.clone(),
        car_type,
        car_number,
        car_class,
        track_gps,
        track_name: matched
            .map(|matched| matched.track.name.to_owned())
            .or_else(|| nonempty(&identity.venue)),
        layout: matched.map(|matched| matched.layout.name.to_owned()),
        track_length_m: matched.and_then(|matched| matched.layout.length_m),
        event_date: event_date.selected.map(|date| date.to_string()),
        event_date_source: event_date.source,
        event_date_warning: event_date.warning,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CivilDate {
    year: i32,
    month: u32,
    day: u32,
}

impl std::fmt::Display for CivilDate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{:04}-{:02}-{:02}",
            self.year, self.month, self.day
        )
    }
}

#[derive(Debug)]
struct EventDate {
    selected: Option<CivilDate>,
    source: Option<String>,
    warning: Option<String>,
}

fn event_date(path: &Path, metadata: &FileMetadata) -> EventDate {
    let telemetry = telemetry_date(metadata);
    let created = std::fs::metadata(path)
        .and_then(|metadata| metadata.created())
        .ok()
        .and_then(date_from_system_time);
    select_event_date(telemetry, created)
}

fn telemetry_date(metadata: &FileMetadata) -> Option<(CivilDate, String)> {
    let clock_date = metadata
        .absolute_clock
        .as_deref()
        .filter(|clock| *clock != "time_of_day")
        .and(metadata.absolute_start_ns)
        .and_then(date_from_unix_ns)
        .filter(plausible_date)
        .map(|date| {
            let clock = metadata.absolute_clock.as_deref().unwrap_or("absolute");
            (date, format!("{clock}_clock"))
        });
    clock_date.or_else(|| {
        parse_source_date(&metadata.identity.date)
            .filter(plausible_date)
            .map(|date| (date, "embedded_date".into()))
    })
}

fn select_event_date(
    telemetry: Option<(CivilDate, String)>,
    created: Option<CivilDate>,
) -> EventDate {
    if let (Some((telemetry_date, telemetry_source)), Some(created_date)) = (&telemetry, created) {
        let age_days = days_from_civil(created_date) - days_from_civil(*telemetry_date);
        let future_days = -age_days;
        if age_days >= SUSPICIOUS_CLOCK_AGE_DAYS {
            return EventDate {
                selected: Some(created_date),
                source: Some("file_created_at".into()),
                warning: Some(format!(
                    "rejected {telemetry_source} date {telemetry_date}: {age_days} days older than file creation"
                )),
            };
        }
        if future_days > 7 {
            return EventDate {
                selected: Some(created_date),
                source: Some("file_created_at".into()),
                warning: Some(format!(
                    "rejected {telemetry_source} date {telemetry_date}: {future_days} days newer than file creation"
                )),
            };
        }
    }
    if let Some((date, source)) = telemetry {
        EventDate {
            selected: Some(date),
            source: Some(source),
            warning: None,
        }
    } else {
        EventDate {
            selected: created,
            source: created.map(|_| "file_created_at".into()),
            warning: None,
        }
    }
}

fn plausible_date(date: &CivilDate) -> bool {
    (1980..=2200).contains(&date.year)
}

fn parse_source_date(value: &str) -> Option<CivilDate> {
    let separator = if value.contains('/') { '/' } else { '-' };
    let parts = value
        .split(separator)
        .map(str::trim)
        .map(str::parse::<i32>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    let [first, second, third] = parts.as_slice() else {
        return None;
    };
    let (year, month, day) = if *first >= 1000 {
        (
            *first,
            u32::try_from(*second).ok()?,
            u32::try_from(*third).ok()?,
        )
    } else {
        (
            *third,
            u32::try_from(*second).ok()?,
            u32::try_from(*first).ok()?,
        )
    };
    valid_civil_date(CivilDate { year, month, day })
}

fn valid_civil_date(date: CivilDate) -> Option<CivilDate> {
    let leap = date.year % 4 == 0 && (date.year % 100 != 0 || date.year % 400 == 0);
    let days = match date.month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    (date.day > 0 && date.day <= days).then_some(date)
}

fn date_from_system_time(time: SystemTime) -> Option<CivilDate> {
    let seconds = time.duration_since(UNIX_EPOCH).ok()?.as_secs();
    date_from_unix_seconds(seconds)
}

fn date_from_unix_ns(timestamp_ns: u64) -> Option<CivilDate> {
    date_from_unix_seconds(timestamp_ns / 1_000_000_000)
}

fn date_from_unix_seconds(seconds: u64) -> Option<CivilDate> {
    let days = i64::try_from(seconds / 86_400).ok()?;
    civil_from_days(days)
}

fn days_from_civil(date: CivilDate) -> i64 {
    let year = i64::from(date.year) - i64::from(date.month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month = i64::from(date.month) + if date.month > 2 { -3 } else { 9 };
    let day_of_year = (153 * month + 2) / 5 + i64::from(date.day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn civil_from_days(days: i64) -> Option<CivilDate> {
    let shifted = days.checked_add(719_468)?;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    valid_civil_date(CivilDate {
        year: i32::try_from(year).ok()?,
        month: u32::try_from(month).ok()?,
        day: u32::try_from(day).ok()?,
    })
}

fn open_for_inspection(path: &Path) -> Result<TelemetryFile, motorsport_telemetry::TelemetryError> {
    if path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("vbo"))
    {
        Ok(TelemetryFile::Racelogic(RacelogicFile::open_metadata(
            path,
        )?))
    } else {
        open(path)
    }
}

fn nonempty(value: &str) -> Option<String> {
    (!value.trim().is_empty()).then(|| value.trim().to_owned())
}

fn normalized_eq(value: &str, wanted: &str) -> bool {
    value
        .bytes()
        .filter(u8::is_ascii_alphanumeric)
        .map(|byte| byte.to_ascii_lowercase())
        .eq(wanted.bytes())
}

fn first_semantic_value(file: &TelemetryFile, names: &[&str]) -> Option<String> {
    let index = file.channels().iter().position(|channel| {
        channel.sample_count > 0
            && names
                .iter()
                .any(|wanted| normalized_eq(&channel.name, wanted))
    })?;
    let value = file.decode(index, 0, 0);
    if !value.is_finite() {
        return None;
    }
    Some(if (value - value.round()).abs() < 1e-9 {
        format!("{value:.0}")
    } else {
        value.to_string()
    })
}

fn coordinate(value: f64, unit: &str) -> Option<f64> {
    match unit.trim().to_ascii_lowercase().as_str() {
        "deg" | "degree" | "degrees" | "°" => Some(value),
        "rad" | "radian" | "radians" => Some(value.to_degrees()),
        _ => None,
    }
}

fn packed_coordinate(value: f64, maximum_degrees: f64, reverse_sign: bool) -> Option<f64> {
    let absolute = value.abs();
    let degrees = (absolute / 100.0).floor();
    let minutes = absolute - degrees * 100.0;
    if !value.is_finite() || degrees > maximum_degrees || minutes >= 60.0 {
        return None;
    }
    let sign = if value.is_sign_negative() { -1.0 } else { 1.0 };
    Some((degrees + minutes / 60.0) * sign * if reverse_sign { -1.0 } else { 1.0 })
}

fn average_gps_candidates(file: &TelemetryFile) -> Vec<(f64, f64)> {
    let roles = file.signal_roles();
    let Some((latitude_index, longitude_index)) = roles.latitude.zip(roles.longitude) else {
        return Vec::new();
    };
    let duration_ns = file.channels()[latitude_index]
        .duration_ns
        .min(file.channels()[longitude_index].duration_ns);
    let mut latitude_sum = 0.0;
    let mut longitude_sum = 0.0;
    let mut count = 0u32;
    for sample in 0..32u64 {
        let time_ns = duration_ns.saturating_mul(sample) / 32;
        let latitude = file.sample_at(latitude_index, time_ns, true);
        let longitude = file.sample_at(longitude_index, time_ns, true);
        if let Some((latitude, longitude)) = latitude.zip(longitude) {
            if latitude.is_finite()
                && longitude.is_finite()
                && (latitude != 0.0 || longitude != 0.0)
            {
                latitude_sum += latitude;
                longitude_sum += longitude;
                count += 1;
            }
        }
    }
    if count == 0 {
        return Vec::new();
    }
    let raw = (
        latitude_sum / f64::from(count),
        longitude_sum / f64::from(count),
    );
    let latitude_unit = file.channels()[latitude_index]
        .unit
        .trim()
        .to_ascii_lowercase();
    let longitude_unit = file.channels()[longitude_index]
        .unit
        .trim()
        .to_ascii_lowercase();
    let minutes = matches!(latitude_unit.as_str(), "min" | "arcmin" | "arcminute")
        && matches!(longitude_unit.as_str(), "min" | "arcmin" | "arcminute");
    let mut candidates = Vec::new();
    if minutes {
        if let Some(packed) =
            packed_coordinate(raw.0, 90.0, false).zip(packed_coordinate(raw.1, 180.0, true))
        {
            candidates.push(packed);
        } else {
            let continuous = (raw.0 / 60.0, -raw.1 / 60.0);
            if valid_gps(continuous) {
                candidates.push(continuous);
            }
            // Some conversion tools export VBOX columns as decimal degrees
            // while retaining the native column names. Keep that as a
            // conservative fallback only when packed coordinates are invalid.
            if valid_gps(raw) {
                candidates.push(raw);
            }
        }
    } else if let Some(converted) = coordinate(raw.0, &latitude_unit)
        .zip(coordinate(raw.1, &longitude_unit))
        .filter(|candidate| valid_gps(*candidate))
    {
        candidates.push(converted);
    }
    candidates.dedup_by(|left, right| {
        (left.0 - right.0).abs() < 1e-10 && (left.1 - right.1).abs() < 1e-10
    });
    candidates
}

fn valid_gps((latitude, longitude): (f64, f64)) -> bool {
    latitude.is_finite()
        && longitude.is_finite()
        && latitude.abs() <= 90.0
        && longitude.abs() <= 180.0
        && (latitude != 0.0 || longitude != 0.0)
}

fn channel_values(file: &TelemetryFile, names: &[&str]) -> Vec<f64> {
    let Some(index) = file.channels().iter().position(|channel| {
        names
            .iter()
            .any(|wanted| normalized_eq(&channel.name, wanted))
    }) else {
        return Vec::new();
    };
    let mut values = Vec::new();
    for (chunk_index, chunk) in file.channels()[index].chunks.iter().enumerate() {
        values.extend(
            (0..chunk.sample_count).map(|local_index| file.decode(index, chunk_index, local_index)),
        );
    }
    values
}

fn video_info(file: &TelemetryFile, metadata: &FileMetadata) -> (bool, Vec<u32>) {
    if metadata.video_frame_count.is_some() {
        return (true, Vec::new());
    }
    let indices = channel_values(file, &["avifileindex"])
        .into_iter()
        .filter(|value| value.is_finite() && *value >= 0.0)
        .map(|value| value.round() as u32)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let sync_present = channel_values(file, &["avisynctime", "avitime"])
        .into_iter()
        .any(|value| value.is_finite() && value.abs() > f64::EPSILON);
    let linked = sync_present || indices.iter().any(|index| *index > 0);
    (linked, if linked { indices } else { Vec::new() })
}

fn nearby_video_filenames(path: &Path) -> Vec<String> {
    let Some(parent) = path.parent() else {
        return Vec::new();
    };
    let source_stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let mut files = parent
        .read_dir()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let candidate = entry.path();
            let extension = candidate
                .extension()?
                .to_string_lossy()
                .to_ascii_lowercase();
            if !matches!(extension.as_str(), "avi" | "mp4" | "mov" | "mkv") {
                return None;
            }
            let stem = candidate
                .file_stem()?
                .to_string_lossy()
                .to_ascii_lowercase();
            (stem == source_stem || stem.starts_with(&format!("{source_stem}_"))).then(|| {
                candidate
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
        })
        .collect::<Vec<_>>();
    files.sort();
    files
}

fn print_human(inspection: &Inspection) {
    println!("file: {}", inspection.file);
    println!("format: {}", inspection.format);
    if let Some(version) = inspection.format_version {
        println!("format_version: {version}");
        println!(
            "format_needs_update: {}",
            inspection.format_needs_update.unwrap_or(false)
        );
    }
    println!(
        "event_date: {}",
        inspection.event_date.as_deref().unwrap_or("unknown")
    );
    println!(
        "event_date_source: {}",
        inspection.event_date_source.as_deref().unwrap_or("unknown")
    );
    if let Some(warning) = &inspection.event_date_warning {
        println!("event_date_warning: {warning}");
    }
    println!("driver_id: {}", display_ids(&inspection.driver_ids));
    println!("laps: {}", inspection.laps);
    println!("complete_laps: {}", inspection.complete_laps);
    println!(
        "fastest_lap: {}",
        inspection
            .fastest_lap_ns
            .map(format_duration)
            .unwrap_or_else(|| "unknown".into())
    );
    println!(
        "fastest_lap_number: {}",
        display(inspection.fastest_lap_number)
    );
    println!("video_included: {}", inspection.video_included);
    println!(
        "video_filenames: {}",
        if inspection.video_included {
            display_strings(&inspection.video_filenames)
        } else {
            "none".into()
        }
    );
    if !inspection.video_file_indices.is_empty() {
        println!(
            "video_file_indices: {}",
            inspection
                .video_file_indices
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    println!(
        "video_presentation_offset_ns: {}",
        display(inspection.video_presentation_offset_ns)
    );
    println!("part_of_larger_session: unknown (single-file inspection)");
    println!(
        "session_key: {}",
        inspection.session_key.as_deref().unwrap_or("unknown")
    );
    println!(
        "car_type: {}",
        inspection.car_type.as_deref().unwrap_or("unknown")
    );
    println!(
        "car_number: {}",
        inspection.car_number.as_deref().unwrap_or("unknown")
    );
    println!(
        "car_class: {}",
        inspection.car_class.as_deref().unwrap_or("unknown")
    );
    println!(
        "track_gps: {}",
        inspection.track_gps.map_or_else(
            || "unknown".into(),
            |(latitude, longitude)| format!("{latitude:.6}, {longitude:.6}")
        )
    );
    println!(
        "track_name: {}",
        inspection.track_name.as_deref().unwrap_or("unknown")
    );
    println!(
        "layout: {}",
        inspection.layout.as_deref().unwrap_or("unknown")
    );
    println!(
        "track_length: {}",
        inspection
            .track_length_m
            .map_or_else(|| "unknown".into(), |length| format!("{length:.0} m"))
    );
}

fn print_json(inspection: &Inspection) {
    let video_filenames = inspection
        .video_included
        .then_some(&inspection.video_filenames)
        .filter(|filenames| !filenames.is_empty());
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "file": inspection.file,
            "format": inspection.format,
            "format_version": inspection.format_version,
            "format_needs_update": inspection.format_needs_update,
            "event_date": inspection.event_date,
            "event_date_source": inspection.event_date_source,
            "event_date_warning": inspection.event_date_warning,
            "driver_id": inspection.driver_ids.first(),
            "driver_ids": inspection.driver_ids,
            "laps": inspection.laps,
            "complete_laps": inspection.complete_laps,
            "fastest_lap_ns": inspection.fastest_lap_ns,
            "fastest_lap": inspection.fastest_lap_ns.map(format_duration),
            "fastest_lap_number": inspection.fastest_lap_number,
            "video_included": inspection.video_included,
            "video_filenames": video_filenames,
            "video_file_indices": inspection.video_file_indices,
            "video_presentation_offset_ns": inspection.video_presentation_offset_ns,
            "part_of_larger_session": null,
            "session_key": inspection.session_key,
            "car_type": inspection.car_type,
            "car_number": inspection.car_number,
            "car_class": inspection.car_class,
            "track_gps": inspection.track_gps.map(|(latitude, longitude)| json!({
                "latitude": latitude,
                "longitude": longitude,
            })),
            "track_name": inspection.track_name,
            "layout": inspection.layout,
            "track_length_m": inspection.track_length_m,
        }))
        .expect("inspection JSON is serializable")
    );
}

fn display_ids(values: &[i64]) -> String {
    if values.is_empty() {
        "unknown".into()
    } else {
        values
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn display_strings(values: &[String]) -> String {
    if values.is_empty() {
        "unknown".into()
    } else {
        values.join(", ")
    }
}

fn display<T: std::fmt::Display>(value: Option<T>) -> String {
    value.map_or_else(|| "unknown".into(), |value| value.to_string())
}

fn format_duration(duration_ns: u64) -> String {
    let milliseconds = duration_ns / 1_000_000;
    let minutes = milliseconds / 60_000;
    let seconds = milliseconds / 1_000 % 60;
    let fraction = milliseconds % 1_000;
    format!("{minutes}:{seconds:02}.{fraction:03}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cli_arguments() {
        assert_eq!(
            arguments(["--json".into(), "run.ld".into()]),
            Ok(Command::Inspect {
                path: PathBuf::from("run.ld"),
                json: true,
            })
        );
        assert!(arguments(Vec::<OsString>::new()).is_err());
        assert!(arguments(["one.ld".into(), "two.ld".into()]).is_err());
    }

    #[test]
    fn formats_lap_time() {
        assert_eq!(format_duration(83_456_789_000), "1:23.456");
    }

    #[test]
    fn decodes_vbox_packed_coordinates_before_other_conventions() {
        let latitude = packed_coordinate(3119.09973, 90.0, false).unwrap();
        let longitude = packed_coordinate(58.49277, 180.0, true).unwrap();
        assert!((latitude - 31.318_328_833_333_335).abs() < 1e-12);
        assert!((longitude - -0.974_879_5).abs() < 1e-12);
        assert_eq!(packed_coordinate(3190.0, 90.0, false), None);
    }

    #[test]
    fn converts_unix_days_and_source_dates() {
        let date = CivilDate {
            year: 2026,
            month: 8,
            day: 8,
        };
        assert_eq!(civil_from_days(days_from_civil(date)), Some(date));
        assert_eq!(parse_source_date("08/08/2026"), Some(date));
        assert_eq!(parse_source_date("2026-08-08"), Some(date));
    }

    #[test]
    fn rejects_a_stale_telemetry_clock_in_favor_of_creation_date() {
        let telemetry = CivilDate {
            year: 2022,
            month: 7,
            day: 1,
        };
        let created = CivilDate {
            year: 2026,
            month: 8,
            day: 8,
        };
        let selected = select_event_date(Some((telemetry, "gps_clock".into())), Some(created));
        assert_eq!(selected.selected, Some(created));
        assert_eq!(selected.source.as_deref(), Some("file_created_at"));
        assert!(selected.warning.unwrap().contains("rejected gps_clock"));
    }
}
