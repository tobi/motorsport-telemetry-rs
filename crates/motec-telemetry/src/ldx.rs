//! MoTeC LDX sidecar generation.
//!
//! LDX is XML metadata beside an LD recording.  The LD remains self-contained,
//! but i2 uses the sidecar for beacon/lap markers and session details.

use crate::write::MotecMetadata;
use crate::{invalid, MotecError};
use motorsport_telemetry_core::{LapMetadata, SourceLapMetadata, TelemetrySource};
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

const MIN_MARKER_SPACING_NS: u64 = 5_000_000_000;

/// Lap details read from a companion MoTeC LDX file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LdxMetadata {
    /// Beacon marker times relative to the LD recording.
    pub marker_times_ns: Vec<u64>,
    /// Total laps declared in the sidecar.
    pub total_laps: Option<usize>,
    /// Fastest lap number declared in the sidecar.
    pub fastest_lap: Option<i64>,
    /// Fastest lap duration declared in the sidecar.
    pub fastest_lap_time_ns: Option<u64>,
}

impl LdxMetadata {
    pub(crate) fn has_lap_data(&self) -> bool {
        !self.marker_times_ns.is_empty()
    }

    /// Converts sidecar beacons and reported timing into shared lap metadata.
    pub fn to_source_lap_metadata(&self, recording_duration_ns: u64) -> SourceLapMetadata {
        let markers = self
            .marker_times_ns
            .iter()
            .copied()
            .filter(|time| *time <= recording_duration_ns)
            .collect::<Vec<_>>();
        let lap_count = self
            .total_laps
            .unwrap_or_else(|| markers.len().saturating_add(1))
            .min(markers.len().saturating_add(1));
        let mut laps = Vec::with_capacity(lap_count);
        for number in 1..=lap_count {
            let start_ns = if number == 1 {
                0
            } else if let Some(start) = markers.get(number - 2) {
                *start
            } else {
                break;
            };
            let (end_ns, complete) = if let Some(end) = markers.get(number - 1) {
                (*end, number > 1)
            } else if number == markers.len().saturating_add(1) {
                (recording_duration_ns, false)
            } else {
                break;
            };
            if end_ns > start_ns {
                laps.push(LapMetadata {
                    number: number as i64,
                    start_ns,
                    end_ns,
                    duration_ns: end_ns - start_ns,
                    complete,
                    first_video_frame: None,
                });
            }
        }

        let fastest_lap =
            self.fastest_lap
                .zip(self.fastest_lap_time_ns)
                .and_then(|(number, duration_ns)| {
                    let marker_index = usize::try_from(number.checked_sub(1)?).ok()?;
                    let end_ns = *markers.get(marker_index)?;
                    let start_ns = end_ns.checked_sub(duration_ns)?;
                    Some(LapMetadata {
                        number,
                        start_ns,
                        end_ns,
                        duration_ns,
                        complete: true,
                        first_video_frame: None,
                    })
                });
        if let Some(fastest) = &fastest_lap {
            if let Some(lap) = laps.iter_mut().find(|lap| lap.number == fastest.number) {
                *lap = fastest.clone();
            } else {
                laps.push(fastest.clone());
                laps.sort_by_key(|lap| lap.start_ns);
            }
        }
        SourceLapMetadata { laps, fastest_lap }
    }
}

fn attributes(element: &BytesStart<'_>, path: &str) -> Result<Vec<(Vec<u8>, String)>, MotecError> {
    element
        .attributes()
        .map(|attribute| {
            let attribute = attribute
                .map_err(|error| invalid(path, format!("invalid LDX attribute: {error}")))?;
            // LDX files commonly declare a Windows-1252 locale without an XML
            // encoding. The lap fields parsed here are ASCII, while mapping
            // other bytes directly keeps unrelated localized details benign.
            let value = attribute.value.iter().copied().map(char::from).collect();
            Ok((attribute.key.as_ref().to_vec(), value))
        })
        .collect()
}

fn lap_time_ns(value: &str) -> Option<u64> {
    let (minutes, seconds) = value.rsplit_once(':')?;
    let minutes = minutes.parse::<u64>().ok()?;
    let seconds = seconds.parse::<f64>().ok()?;
    if !seconds.is_finite() || !(0.0..60.0).contains(&seconds) {
        return None;
    }
    minutes
        .checked_mul(60_000_000_000)?
        .checked_add((seconds * 1e9).round() as u64)
}

/// Parses a MoTeC LDX sidecar without reading the companion LD payload.
pub fn parse_motec_ldx_bytes(
    path: impl Into<String>,
    data: &[u8],
) -> Result<LdxMetadata, MotecError> {
    let path = path.into();
    let mut reader = Reader::from_reader(data);
    reader.config_mut().trim_text(true);
    let mut metadata = LdxMetadata::default();
    loop {
        let event = reader
            .read_event()
            .map_err(|error| invalid(&path, format!("invalid LDX XML: {error}")))?;
        let element = match event {
            Event::Start(element) | Event::Empty(element) => element,
            Event::Eof => break,
            _ => continue,
        };
        let name = element.name();
        if name.as_ref() == b"Marker" {
            let attributes = attributes(&element, &path)?;
            let class = attributes
                .iter()
                .find(|(key, _)| key == b"ClassName")
                .map(|(_, value)| value.as_str());
            let time = attributes
                .iter()
                .find(|(key, _)| key == b"Time")
                .and_then(|(_, value)| value.parse::<f64>().ok());
            if class == Some("BCN") {
                if let Some(time_us) = time.filter(|time| time.is_finite() && *time >= 0.0) {
                    metadata
                        .marker_times_ns
                        .push((time_us * 1_000.0).round() as u64);
                }
            }
        } else if name.as_ref() == b"String" {
            let attributes = attributes(&element, &path)?;
            let id = attributes
                .iter()
                .find(|(key, _)| key == b"Id")
                .map(|(_, value)| value.as_str());
            let value = attributes
                .iter()
                .find(|(key, _)| key == b"Value")
                .map(|(_, value)| value.as_str());
            match (id, value) {
                (Some("Total Laps"), Some(value)) => {
                    metadata.total_laps = value.parse::<usize>().ok().filter(|laps| *laps > 0);
                }
                (Some("Fastest Lap"), Some(value)) => {
                    metadata.fastest_lap = value.parse::<i64>().ok().filter(|lap| *lap > 0);
                }
                (Some("Fastest Time"), Some(value)) => {
                    metadata.fastest_lap_time_ns = lap_time_ns(value);
                }
                _ => {}
            }
        }
    }
    metadata.marker_times_ns.sort_unstable();
    metadata.marker_times_ns.dedup();
    Ok(metadata)
}

/// Lap beacon times inferred from one source channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LapMarkers {
    /// Beacon times relative to the beginning of the recording.
    pub times_ns: Vec<u64>,
    /// Exact name of the channel used to infer the markers.
    pub source_channel: String,
}

fn normalized_eq(value: &str, wanted: &str) -> bool {
    value
        .bytes()
        .filter(u8::is_ascii_alphanumeric)
        .map(|byte| byte.to_ascii_lowercase())
        .eq(wanted.bytes())
}

fn channel_index(source: &dyn TelemetrySource, wanted: &str) -> Option<usize> {
    source
        .channels()
        .iter()
        .position(|channel| normalized_eq(&channel.name, wanted) && channel.sample_count > 0)
}

fn transitions(source: &dyn TelemetrySource, index: usize, rising_pulse: bool) -> Vec<u64> {
    let channel = &source.channels()[index];
    let mut previous: Option<f64> = None;
    let mut high_water: Option<i64> = None;
    let mut markers = Vec::new();
    for (chunk_index, chunk) in channel.chunks.iter().enumerate() {
        for local_index in 0..chunk.sample_count {
            let value = source.decode(index, chunk_index, local_index);
            let changed = if rising_pulse {
                previous.is_some_and(|before| {
                    value.is_finite() && before.is_finite() && value > 0.0 && before <= 0.0
                })
            } else if value.is_finite() {
                let counter = value.round() as i64;
                let changed = counter >= 0 && high_water.is_some_and(|before| counter > before);
                if counter >= 0 && high_water.is_none_or(|before| counter > before) {
                    high_water = Some(counter);
                }
                changed
            } else {
                false
            };
            if changed {
                let time = source.sample_time_ns(index, chunk_index, local_index);
                if time > 0
                    && markers
                        .last()
                        .is_none_or(|last| time.saturating_sub(*last) >= MIN_MARKER_SPACING_NS)
                {
                    markers.push(time);
                }
            }
            previous = Some(value);
        }
    }
    markers
}

/// Recover lap beacons from the strongest channel available.
///
/// Dedicated trigger pulses have better time resolution than the low-rate lap
/// counter, so they are preferred.  Counter channels are conservative
/// fallbacks.  Channel names are exact after punctuation/case normalization to
/// avoid interpreting an unrelated switch as a beacon.
pub fn infer_lap_markers(source: &dyn TelemetrySource) -> Option<LapMarkers> {
    for wanted in ["lapbeacontrig", "laptrigger", "blaptrig", "fiabeacon"] {
        if let Some(index) = channel_index(source, wanted) {
            let times_ns = transitions(source, index, true);
            if !times_ns.is_empty() {
                return Some(LapMarkers {
                    times_ns,
                    source_channel: source.channels()[index].name.clone(),
                });
            }
        }
    }
    for wanted in [
        "lapnumber",
        "lapnum",
        "lapcount",
        "lapcounter",
        "currentlap",
        "lap",
        "beaconeventcount",
        "beaconcount",
        "lapbeaconcount",
    ] {
        if let Some(index) = channel_index(source, wanted) {
            let times_ns = transitions(source, index, false);
            if !times_ns.is_empty() {
                return Some(LapMarkers {
                    times_ns,
                    source_channel: source.channels()[index].name.clone(),
                });
            }
        }
    }
    None
}

fn xml(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

fn lap_summary(markers: &[u64]) -> (usize, Option<(f64, usize)>) {
    let total_laps = markers.len().saturating_add(1);
    let fastest = markers
        .windows(2)
        .enumerate()
        .map(|(index, pair)| ((pair[1] - pair[0]) as f64 / 1e9, index + 2))
        .filter(|(seconds, _)| *seconds > 0.0)
        .min_by(|left, right| left.0.total_cmp(&right.0));
    (total_laps, fastest)
}

fn lap_time(seconds: f64) -> String {
    let minutes = (seconds / 60.0).floor() as u64;
    format!("{minutes}:{:06.3}", seconds - minutes as f64 * 60.0)
}

fn detail(out: &mut String, id: &str, value: &str) {
    out.push_str(&format!(
        "   <String Id=\"{}\" Value=\"{}\"/>\n",
        xml(id),
        xml(value)
    ));
}

/// Build the companion LDX XML.  Unknown details are left empty rather than
/// guessed.  When no reliable beacon channel exists, the sidecar still carries
/// the supplied session identity but does not invent lap boundaries.
pub fn write_motec_ldx_bytes(source: &dyn TelemetrySource, metadata: &MotecMetadata) -> Vec<u8> {
    let markers = infer_lap_markers(source);
    let mut out = String::from(
        "<?xml version=\"1.0\"?>\n<LDXFile Locale=\"English_United States.1252\" DefaultLocale=\"C\" Version=\"1.6\">\n <Layers>\n  <Layer>\n",
    );

    if let Some(markers) = &markers {
        out.push_str("   <MarkerBlock>\n    <MarkerGroup Name=\"Beacons\" Index=\"3\">\n");
        for (index, time_ns) in markers.times_ns.iter().enumerate() {
            // LDX beacon time is expressed in microseconds.
            let time_us = *time_ns as f64 / 1_000.0;
            out.push_str(&format!(
                "     <Marker Version=\"100\" ClassName=\"BCN\" Name=\"Manual.{}\" Flags=\"77\" Time=\"{time_us:.17e}\"/>\n",
                index + 1
            ));
        }
        out.push_str("    </MarkerGroup>\n   </MarkerBlock>\n");
    }

    out.push_str("  </Layer>\n  <Details>\n");
    detail(&mut out, "Event", &metadata.event);
    detail(&mut out, "Venue", &metadata.venue);
    detail(&mut out, "Driver", &metadata.driver);
    detail(&mut out, "Team", &metadata.team);
    detail(&mut out, "Vehicle Id", &metadata.vehicle);
    detail(&mut out, "Vehicle Number", &metadata.vehicle_number);
    detail(&mut out, "Session", &metadata.session);
    detail(&mut out, "Short Comment", &metadata.short_comment);
    detail(&mut out, "Long Comment", &metadata.event_comment);
    out.push_str(&format!(
        "   <DateTime Id=\"Log Date\" Value=\"{}\"/>\n   <DateTime Id=\"Log Time\" Value=\"{}\"/>\n",
        xml(&metadata.date),
        xml(&metadata.time)
    ));
    if let Some(markers) = &markers {
        let (total_laps, fastest) = lap_summary(&markers.times_ns);
        detail(&mut out, "Total Laps", &total_laps.to_string());
        if let Some((seconds, lap)) = fastest {
            detail(&mut out, "Fastest Time", &lap_time(seconds));
            detail(&mut out, "Fastest Lap", &lap.to_string());
        }
        detail(&mut out, "Beacon Source", &markers.source_channel);
    } else {
        detail(&mut out, "Total Laps", "");
        detail(&mut out, "Fastest Time", "");
        detail(&mut out, "Fastest Lap", "");
    }
    out.push_str("  </Details>\n </Layers>\n</LDXFile>\n");
    out.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use motorsport_telemetry_core::{Channel, Chunk, SampleType, UnitSource};

    struct Source {
        channel: Channel,
        values: Vec<f64>,
    }

    impl TelemetrySource for Source {
        fn path(&self) -> &str {
            "laps.pds"
        }
        fn format(&self) -> &'static str {
            "pds"
        }
        fn channels(&self) -> &[Channel] {
            std::slice::from_ref(&self.channel)
        }
        fn decode(&self, _: usize, _: usize, local_index: u64) -> f64 {
            self.values[local_index as usize]
        }
    }

    fn source(name: &str, values: Vec<f64>) -> Source {
        let count = values.len() as u64;
        Source {
            channel: Channel {
                id: 1,
                name: name.into(),
                unit: String::new(),
                unit_source: UnitSource::Unknown,
                sample_type: SampleType::U8,
                chunks: vec![Chunk {
                    sample_period_ns: 1_000_000_000,
                    sample_count: count,
                    data_ptr: 0,
                    sample_base: 0,
                    time_base_ns: 0,
                }],
                sample_count: count,
                duration_ns: count * 1_000_000_000,
            },
            values,
        }
    }

    #[test]
    fn trigger_edges_become_beacons_and_summary() {
        let input = source(
            "lap_beacon_trig",
            vec![
                0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0,
            ],
        );
        let metadata = MotecMetadata {
            driver: "A&B".into(),
            ..Default::default()
        };
        let text = String::from_utf8(write_motec_ldx_bytes(&input, &metadata)).unwrap();
        assert!(text.contains("Name=\"Manual.1\"") && text.contains("Name=\"Manual.3\""));
        assert!(text.contains("Id=\"Total Laps\" Value=\"4\""));
        assert!(text.contains("Id=\"Fastest Time\" Value=\"0:06.000\""));
        assert!(text.contains("Value=\"A&amp;B\""));
    }

    #[test]
    fn lap_counter_is_a_fallback_and_resets_are_ignored() {
        let input = source(
            "Lap Count",
            vec![
                1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 2.0, 2.0, 2.0, 2.0, 2.0, 2.0, 3.0, 0.0, 0.0, 0.0,
                0.0, 0.0, 0.0, 1.0,
            ],
        );
        let markers = infer_lap_markers(&input).unwrap();
        assert_eq!(markers.times_ns, vec![6_000_000_000, 12_000_000_000]);
    }

    #[test]
    fn no_signal_writes_metadata_without_inventing_markers() {
        let input = source("Speed", vec![1.0, 2.0]);
        let text =
            String::from_utf8(write_motec_ldx_bytes(&input, &MotecMetadata::default())).unwrap();
        assert!(!text.contains("<Marker "));
        assert!(text.contains("Id=\"Total Laps\" Value=\"\""));
    }

    #[test]
    fn parses_beacons_and_reconstructs_explicit_fastest_lap() {
        let xml = br#"<?xml version="1.0"?>
<LDXFile><Layers><Layer><MarkerBlock><MarkerGroup Name="Beacons">
<Marker Time="1.5e+08" ClassName="BCN"/>
</MarkerGroup></MarkerBlock></Layer><Details>
<String Value="2" Id="Total Laps"/>
<String Id="Fastest Time" Value="1:40.000"/>
<String Id="Fastest Lap" Value="1"/>
</Details></Layers></LDXFile>"#;
        let parsed = parse_motec_ldx_bytes("synthetic.ldx", xml).unwrap();
        assert_eq!(parsed.marker_times_ns, [150_000_000_000]);
        assert_eq!(parsed.total_laps, Some(2));
        assert_eq!(parsed.fastest_lap_time_ns, Some(100_000_000_000));

        let source = parsed.to_source_lap_metadata(260_000_000_000);
        assert_eq!(source.laps.len(), 2);
        assert_eq!(source.laps[0].start_ns, 50_000_000_000);
        assert_eq!(source.laps[0].end_ns, 150_000_000_000);
        assert!(source.laps[0].complete);
        assert!(!source.laps[1].complete);
        assert_eq!(source.fastest_lap, Some(source.laps[0].clone()));
    }

    #[test]
    fn rejects_malformed_xml() {
        assert!(parse_motec_ldx_bytes("broken.ldx", b"<LDXFile><Marker").is_err());
    }
}
