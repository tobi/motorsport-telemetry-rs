#![doc = include_str!("../README.md")]
#![deny(missing_docs)]

use motorsport_telemetry_core::{
    chunk_bytes as core_chunk_bytes, sample_bytes as core_sample_bytes, Channel, Chunk, Diagnostic,
    SampleType, SourceLapMetadata, Storage, TelemetrySource, UnitSource,
};
#[cfg(not(target_os = "emscripten"))]
use std::path::Path;
use thiserror::Error;

const MAGIC: u32 = 0x40;
pub(crate) const CHANNEL_META_SIZE: usize = 124;
const MIN_FILE_SIZE: usize = 0x1a0;
const MAX_CHANNELS: usize = 4096;

pub mod ldx;
pub mod write;
pub use ldx::{
    infer_lap_markers, parse_motec_ldx_bytes, write_motec_ldx_bytes, LapMarkers, LdxMetadata,
};
pub use write::{
    motec_sidecar_path, write_motec, write_motec_bytes, write_motec_sidecar, MotecMetadata,
    MotecWriteError,
};

#[cfg(not(target_os = "emscripten"))]
/// Opens an LD file and derives its format-neutral metadata summary.
pub fn read_metadata(
    path: impl AsRef<Path>,
) -> Result<motorsport_telemetry_core::FileMetadata, MotecError> {
    MotecFile::open(path).map(|file| motorsport_telemetry_core::read_source_metadata(&file))
}

/// Derives format-neutral metadata from an owned LD byte buffer.
pub fn read_metadata_from_bytes(
    path: impl Into<String>,
    data: Vec<u8>,
) -> Result<motorsport_telemetry_core::FileMetadata, MotecError> {
    MotecFile::from_bytes(path, data)
        .map(|file| motorsport_telemetry_core::read_source_metadata(&file))
}

/// Derives format-neutral metadata from owned LD bytes and an LDX sidecar.
pub fn read_metadata_from_bytes_with_ldx(
    path: impl Into<String>,
    data: Vec<u8>,
    ldx_data: &[u8],
) -> Result<motorsport_telemetry_core::FileMetadata, MotecError> {
    MotecFile::from_bytes_with_ldx(path, data, ldx_data)
        .map(|file| motorsport_telemetry_core::read_source_metadata(&file))
}

/// Errors returned while opening or parsing MoTeC LD telemetry.
#[derive(Debug, Error)]
pub enum MotecError {
    /// The LD file could not be opened or memory-mapped.
    #[error("I/O error for {path}: {source}")]
    Io {
        /// Path that was being opened.
        path: String,
        /// Underlying filesystem error.
        source: std::io::Error,
    },
    /// The LD structure is malformed or unsupported.
    #[error("invalid MoTeC LD file {path}: {message}")]
    Invalid {
        /// Path or caller-supplied input name.
        path: String,
        /// Specific validation failure.
        message: String,
    },
}

#[derive(Debug, Clone)]
struct Encoding {
    datatype_a: u16,
    width: usize,
    factor: f64,
    offset: f64,
}

/// An opened MoTeC LD telemetry source and its embedded session identity.
#[derive(Debug)]
pub struct MotecFile {
    /// Source path or caller-supplied name.
    pub path: String,
    /// Driver name stored in the LD header.
    pub driver: String,
    /// Vehicle name stored in the LD header.
    pub vehicle: String,
    /// Venue name stored in the LD header.
    pub venue: String,
    /// Recording date as stored by the source.
    pub date: String,
    /// Recording time as stored by the source.
    pub time: String,
    /// Event name stored in the LD metadata.
    pub event: String,
    /// Session name stored in the LD metadata.
    pub session: String,
    /// Recording comment stored in the LD metadata.
    pub comment: String,
    /// Source-exact telemetry channel metadata.
    pub channels: Vec<Channel>,
    /// Parsed companion LDX metadata, when a valid sidecar was available.
    pub ldx: Option<Box<LdxMetadata>>,
    encodings: Vec<Encoding>,
    data: Storage,
    /// Recovery diagnostics collected during parse, surfaced via
    /// [`TelemetrySource::diagnostics`].
    pub diagnostics: Vec<Diagnostic>,
}

fn invalid(path: &str, message: impl Into<String>) -> MotecError {
    MotecError::Invalid {
        path: path.into(),
        message: message.into(),
    }
}

fn u16le(data: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        data.get(offset..offset + 2)?.try_into().ok()?,
    ))
}
fn i16le(data: &[u8], offset: usize) -> Option<i16> {
    Some(i16::from_le_bytes(
        data.get(offset..offset + 2)?.try_into().ok()?,
    ))
}
fn u32le(data: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        data.get(offset..offset + 4)?.try_into().ok()?,
    ))
}
fn text(data: &[u8], offset: usize, length: usize) -> String {
    data.get(offset..offset.saturating_add(length).min(data.len()))
        .unwrap_or_default()
        .iter()
        .take_while(|&&byte| byte != 0)
        .map(|&byte| char::from(byte))
        .collect::<String>()
        .trim()
        .to_owned()
}

fn parse_datetime_ns(date: &str, time: &str) -> Option<u64> {
    let mut date_parts = date.split('/').map(str::parse::<i64>);
    let day = date_parts.next()?.ok()?;
    let month = date_parts.next()?.ok()?;
    let year = date_parts.next()?.ok()?;
    let mut time_parts = time.split(':').map(str::parse::<i64>);
    let hour = time_parts.next()?.ok()?;
    let minute = time_parts.next()?.ok()?;
    let second = time_parts.next()?.ok()?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=60).contains(&second)
    {
        return None;
    }
    let adjusted_year = year - i64::from(month <= 2);
    let era = adjusted_year.div_euclid(400);
    let year_of_era = adjusted_year - era * 400;
    let adjusted_month = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * adjusted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days_since_epoch = era * 146_097 + day_of_era - 719_468;
    let seconds = days_since_epoch
        .checked_mul(86_400)?
        .checked_add(hour * 3_600 + minute * 60 + second)?;
    u64::try_from(seconds).ok()?.checked_mul(1_000_000_000)
}

impl MotecFile {
    #[cfg(not(target_os = "emscripten"))]
    /// Memory-maps and parses a local MoTeC LD file.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, MotecError> {
        let path = path.as_ref();
        let display = path.to_string_lossy().into_owned();
        let data = Storage::open(path).map_err(|source| MotecError::Io {
            path: display.clone(),
            source,
        })?;
        let mut parsed = Self::parse(display, data)?;
        let sidecar = motec_sidecar_path(path);
        match std::fs::read(&sidecar) {
            Ok(bytes) => match parse_motec_ldx_bytes(sidecar.to_string_lossy(), &bytes) {
                Ok(ldx) => {
                    parsed.diagnostics.extend(ldx.diagnostics.iter().cloned());
                    parsed.ldx = Some(Box::new(ldx));
                }
                Err(error) => {
                    parsed.diagnostics.push(Diagnostic::warning(
                        "ld.sidecar_unreadable",
                        format!(
                            "companion LDX at {} existed but could not be parsed: {error}",
                            sidecar.display()
                        ),
                    ));
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                parsed.diagnostics.push(Diagnostic::warning(
                    "ld.sidecar_unreadable",
                    format!(
                        "companion LDX at {} existed but could not be read: {error}",
                        sidecar.display()
                    ),
                ));
            }
        }
        Ok(parsed)
    }

    /// Parses MoTeC telemetry from an owned LD byte buffer.
    pub fn from_bytes(path: impl Into<String>, data: Vec<u8>) -> Result<Self, MotecError> {
        Self::parse(path.into(), Storage::from_vec(data))
    }

    /// Parses an LD byte buffer together with its companion LDX sidecar.
    pub fn from_bytes_with_ldx(
        path: impl Into<String>,
        data: Vec<u8>,
        ldx_data: &[u8],
    ) -> Result<Self, MotecError> {
        let path = path.into();
        let ldx = parse_motec_ldx_bytes(format!("{path}x"), ldx_data)?;
        let mut parsed = Self::parse(path, Storage::from_vec(data))?;
        parsed.diagnostics.extend(ldx.diagnostics.iter().cloned());
        parsed.ldx = Some(Box::new(ldx));
        Ok(parsed)
    }

    fn parse(display: String, data: Storage) -> Result<Self, MotecError> {
        if data.len() < MIN_FILE_SIZE {
            return Err(invalid(&display, "file is too small"));
        }
        let magic = u32le(&data, 0).unwrap_or(0);
        if magic != MAGIC {
            return Err(invalid(
                &display,
                format!("expected magic 0x40, got 0x{magic:x}"),
            ));
        }

        let mut channels = Vec::new();
        let mut encodings = Vec::new();
        let mut diagnostics = Vec::new();
        let mut address = u32le(&data, 0x08).unwrap_or(0) as usize;
        while address > 0
            && address + CHANNEL_META_SIZE <= data.len()
            && channels.len() < MAX_CHANNELS
        {
            let next = u32le(&data, address + 0x04).unwrap_or(0) as usize;
            let data_ptr = u32le(&data, address + 0x08).unwrap_or(0) as u64;
            let requested_count = u32le(&data, address + 0x0c).unwrap_or(0) as u64;
            let datatype_a = u16le(&data, address + 0x12).unwrap_or(0);
            let width = u16le(&data, address + 0x14).unwrap_or(0) as usize;
            let frequency = u16le(&data, address + 0x16).unwrap_or(0) as u64;
            let shift = i16le(&data, address + 0x18).unwrap_or(0);
            let mul = i16le(&data, address + 0x1a).unwrap_or(0);
            let scale = i16le(&data, address + 0x1c).unwrap_or(0);
            let decimal_places = i16le(&data, address + 0x1e).unwrap_or(0);
            let multiplier = if mul == 0 { 1.0 } else { f64::from(mul) };
            let divisor = if scale == 0 { 1.0 } else { f64::from(scale) };
            let encoding = Encoding {
                datatype_a,
                width,
                factor: multiplier / divisor * 10f64.powi(-i32::from(decimal_places)),
                offset: f64::from(shift) * multiplier,
            };
            let name = text(&data, address + 0x20, 32);
            // Per the LD layout the 124-byte channel block holds name at 0x20
            // (32 bytes), short name at 0x40 (8 bytes) and unit at 0x48 (12
            // bytes).
            let unit = text(&data, address + 0x48, 12);
            // LD channel blocks carry a real unit string; absent means unknown.
            let unit_source = if unit.is_empty() {
                UnitSource::Unknown
            } else {
                UnitSource::Declared
            };
            let sample_type = match (datatype_a, width) {
                // 0x08/8 is MoTeC's little-endian f64 (seen on GPS channels).
                (0x08, 8) => SampleType::F64,
                (0x07, 8) => SampleType::F64,
                (0x07, _) => SampleType::F32,
                (_, 2) => SampleType::I16,
                (_, 4) => SampleType::I32,
                _ => SampleType::F32,
            };
            let valid_width = matches!(width, 2 | 4 | 8);
            let decodable = sample_type.byte_width() == width;
            let count = if valid_width && decodable && data_ptr < data.len() as u64 {
                requested_count.min((data.len() as u64 - data_ptr) / width as u64)
            } else {
                0
            };
            let period_ns = 1_000_000_000u64.checked_div(frequency).unwrap_or(0);
            let chunks = if count > 0 && period_ns > 0 {
                vec![Chunk {
                    sample_period_ns: period_ns,
                    sample_count: count,
                    data_ptr,
                    sample_base: 0,
                    time_base_ns: 0,
                }]
            } else {
                Vec::new()
            };
            // Report recoveries that would otherwise be silent. Each message
            // names the concrete evidence (offset, channel, observed value).
            if !valid_width {
                diagnostics.push(
                    Diagnostic::warning(
                        "ld.invalid_width",
                        format!(
                            "channel at offset 0x{address:x} has width {width}; expected 2, 4, \
                             or 8; sample count forced to 0"
                        ),
                    )
                    .with_channel(&name),
                );
            }
            if frequency == 0 {
                diagnostics.push(
                    Diagnostic::warning(
                        "ld.zero_frequency",
                        format!(
                            "channel at offset 0x{address:x} has frequency 0; sample period is \
                             zero and no samples were produced"
                        ),
                    )
                    .with_channel(&name),
                );
            }
            if (mul == 0 || scale == 0) && !matches!(datatype_a, 0x07 | 0x08) {
                // Float channels (0x07/0x08) carry raw IEEE values; the
                // affine transform is never applied, so a zero
                // multiplier/divisor does not change engineering values.
                let fields = match (mul == 0, scale == 0) {
                    (true, true) => "multiplier and divisor",
                    (true, false) => "multiplier",
                    (false, true) => "divisor",
                    _ => unreachable!(),
                };
                diagnostics.push(
                    Diagnostic::warning(
                        "ld.zero_scale_factor",
                        format!(
                            "channel at offset 0x{address:x} has {fields} of zero; affine \
                             factor defaulted to 1.0, changing engineering values"
                        ),
                    )
                    .with_channel(&name),
                );
            }
            if valid_width && data_ptr < data.len() as u64 {
                let available = (data.len() as u64 - data_ptr) / width as u64;
                if requested_count > available {
                    diagnostics.push(
                        Diagnostic::warning(
                            "ld.sample_count_clamped",
                            format!(
                                "channel at offset 0x{address:x} requested {requested_count} \
                                 samples but only {available} fit before end of file; count \
                                 clamped"
                            ),
                        )
                        .with_channel(&name),
                    );
                }
            }
            if !matches!((datatype_a, width), (0x08, 8) | (0x07, _) | (_, 2) | (_, 4)) {
                diagnostics.push(
                    Diagnostic::warning(
                        "ld.unknown_datatype_width",
                        format!(
                            "channel at offset 0x{address:x} has datatype 0x{datatype_a:02x} \
                             with width {width}; no recognised encoding, falling back to F32"
                        ),
                    )
                    .with_channel(&name),
                );
            }
            if valid_width && !decodable {
                diagnostics.push(
                    Diagnostic::warning(
                        "ld.decode_unsupported_width",
                        format!(
                            "channel at offset 0x{address:x} has datatype 0x{datatype_a:02x} \
                             with width {width}; unsupported combination, sample count forced \
                             to 0"
                        ),
                    )
                    .with_channel(&name),
                );
            }
            channels.push(Channel {
                id: channels.len() as u32,
                name,
                unit,
                unit_source,
                sample_type,
                chunks,
                sample_count: count,
                duration_ns: count.saturating_mul(period_ns),
            });
            encodings.push(encoding);
            if next == 0 || next <= address {
                break;
            }
            address = next;
        }
        if channels.is_empty() {
            return Err(invalid(&display, "no channel metadata found"));
        }
        let event_ptr = u32le(&data, 0x24).unwrap_or(0) as usize;
        let (event, session, comment) = if event_ptr > 0 && event_ptr < data.len() {
            (
                text(&data, event_ptr, 64),
                text(&data, event_ptr.saturating_add(64), 64),
                text(&data, event_ptr.saturating_add(128), 1024),
            )
        } else {
            (String::new(), String::new(), String::new())
        };
        let date = text(&data, 0x5e, 16);
        let time = text(&data, 0x7e, 16);
        if (!date.is_empty() || !time.is_empty()) && parse_datetime_ns(&date, &time).is_none() {
            diagnostics.push(Diagnostic::info(
                "ld.datetime_unparsable",
                format!(
                    "recording date \"{date}\" time \"{time}\" could not be parsed; no absolute \
                     time range will be reported"
                ),
            ));
        }
        Ok(Self {
            path: display,
            driver: text(&data, 0x9e, 64),
            vehicle: text(&data, 0xde, 64),
            venue: text(&data, 0x15e, 64),
            date,
            time,
            event,
            session,
            comment,
            channels,
            ldx: None,
            encodings,
            data,
            diagnostics,
        })
    }
}

impl TelemetrySource for MotecFile {
    fn path(&self) -> &str {
        &self.path
    }
    fn format(&self) -> &'static str {
        "motec"
    }
    fn channels(&self) -> &[Channel] {
        &self.channels
    }
    fn absolute_time_range(&self) -> Option<motorsport_telemetry_core::AbsoluteTimeRange> {
        let start_ns = parse_datetime_ns(&self.date, &self.time)?;
        let duration_ns = self
            .channels
            .iter()
            .map(|channel| channel.duration_ns)
            .max()
            .unwrap_or(0);
        Some(motorsport_telemetry_core::AbsoluteTimeRange {
            clock: "utc".into(),
            start_ns,
            end_ns: start_ns.saturating_add(duration_ns),
            session_hint: format!(
                "motec:{}:{}:{}",
                self.date.to_ascii_lowercase(),
                self.vehicle.to_ascii_lowercase(),
                self.venue.to_ascii_lowercase()
            ),
        })
    }

    fn identity(&self) -> motorsport_telemetry_core::SourceIdentity {
        motorsport_telemetry_core::SourceIdentity {
            driver: self.driver.clone(),
            vehicle: self.vehicle.clone(),
            venue: self.venue.clone(),
            event: self.event.clone(),
            session: self.session.clone(),
            date: self.date.clone(),
            time: self.time.clone(),
        }
    }

    fn source_lap_metadata(&self) -> Option<SourceLapMetadata> {
        let duration_ns = self
            .channels
            .iter()
            .map(|channel| channel.duration_ns)
            .max()
            .unwrap_or(0);
        self.ldx
            .as_deref()
            .filter(|ldx| ldx.has_lap_data())
            .map(|ldx| ldx.to_source_lap_metadata(duration_ns))
            .filter(|metadata| !metadata.laps.is_empty())
    }

    fn chunk_bytes(&self, channel_index: usize, chunk_index: usize) -> Option<&[u8]> {
        let channel = self.channels.get(channel_index)?;
        let chunk = channel.chunks.get(chunk_index)?;
        let width = self.encodings.get(channel_index)?.width;
        core_chunk_bytes(&self.data, chunk, width)
    }

    fn sample_affine(&self, channel_index: usize) -> (f64, f64) {
        let Some(encoding) = self.encodings.get(channel_index) else {
            return (1.0, 0.0);
        };
        if encoding.datatype_a == 0x07 || encoding.datatype_a == 0x08 {
            (1.0, 0.0)
        } else {
            (encoding.factor, encoding.offset)
        }
    }

    fn decode(&self, channel_index: usize, _chunk_index: usize, local_index: u64) -> f64 {
        let channel = &self.channels[channel_index];
        let encoding = &self.encodings[channel_index];
        let Some(chunk) = channel.chunks.first() else {
            return f64::NAN;
        };
        let width = channel.sample_type.byte_width();
        let raw = core_sample_bytes(&self.data, chunk, local_index, width)
            .and_then(|bytes| channel.sample_type.decode_le(bytes))
            .unwrap_or(f64::NAN);
        // Float channels carry raw IEEE values; the scale/shift/mul transform
        // applies to the integer encodings only.
        if encoding.datatype_a == 0x07 || encoding.datatype_a == 0x08 {
            raw
        } else {
            raw.mul_add(encoding.factor, encoding.offset)
        }
    }

    fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn u16_at(data: &mut [u8], at: usize, value: u16) {
        data[at..at + 2].copy_from_slice(&value.to_le_bytes());
    }
    fn i16_at(data: &mut [u8], at: usize, value: i16) {
        data[at..at + 2].copy_from_slice(&value.to_le_bytes());
    }
    fn u32_at(data: &mut [u8], at: usize, value: u32) {
        data[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }
    fn fixture_bytes() -> Vec<u8> {
        let mut data = vec![0u8; 0x500];
        u32_at(&mut data, 0, MAGIC);
        u32_at(&mut data, 8, 0x200);
        data[0x9e..0xa2].copy_from_slice(b"Tobi");
        data[0xde..0xe5].copy_from_slice(b"Oreca07");
        data[0x15e..0x165].copy_from_slice(b"Mosport");
        let speed = 0x200;
        u32_at(&mut data, speed + 4, 0x27c);
        u32_at(&mut data, speed + 8, 0x380);
        u32_at(&mut data, speed + 0xc, 3);
        u16_at(&mut data, speed + 0x12, 0x07);
        u16_at(&mut data, speed + 0x14, 4);
        u16_at(&mut data, speed + 0x16, 2);
        data[speed + 0x20..speed + 0x25].copy_from_slice(b"Speed");
        data[speed + 0x48..speed + 0x4b].copy_from_slice(b"m/s");
        for (index, value) in [1.0_f32, 2.0, 3.0].into_iter().enumerate() {
            data[0x380 + index * 4..0x384 + index * 4].copy_from_slice(&value.to_le_bytes());
        }
        let brake = 0x27c;
        u32_at(&mut data, brake + 8, 0x3a0);
        u32_at(&mut data, brake + 0xc, 2);
        u16_at(&mut data, brake + 0x12, 0x03);
        u16_at(&mut data, brake + 0x14, 2);
        u16_at(&mut data, brake + 0x16, 1);
        i16_at(&mut data, brake + 0x1a, 1);
        i16_at(&mut data, brake + 0x1c, 1);
        i16_at(&mut data, brake + 0x1e, 1);
        data[brake + 0x20..brake + 0x29].copy_from_slice(b"P_F_BRAKE");
        data[brake + 0x48..brake + 0x4b].copy_from_slice(b"bar");
        i16_at(&mut data, 0x3a0, 423);
        i16_at(&mut data, 0x3a2, -10);
        data
    }

    fn fixture() -> tempfile::NamedTempFile {
        let data = fixture_bytes();
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&data).unwrap();
        file
    }

    #[test]
    fn decodes_float_and_scaled_integer_channels() {
        let fixture = fixture();
        let file = MotecFile::open(fixture.path()).unwrap();
        let in_memory =
            MotecFile::from_bytes("fixture.ld", std::fs::read(fixture.path()).unwrap()).unwrap();
        assert_eq!(in_memory.channels.len(), 2);
        let metadata = read_metadata(fixture.path()).unwrap();
        assert_eq!(metadata.channel_count, 2);
        assert_eq!(metadata.sample_count, 5);
        assert_eq!(
            (&file.driver, &file.vehicle, &file.venue),
            (&"Tobi".into(), &"Oreca07".into(), &"Mosport".into())
        );
        assert_eq!(file.decode(0, 0, 2), 3.0);
        assert!((file.decode(1, 0, 0) - 42.3).abs() < 1e-10);
        assert!((file.decode(1, 0, 1) + 1.0).abs() < 1e-10);
        assert_eq!(file.sample_at(0, 250_000_000, true), Some(1.5));
    }

    #[test]
    fn rejects_wrong_magic_and_truncated_files() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&vec![0u8; MIN_FILE_SIZE]).unwrap();
        assert!(matches!(
            MotecFile::open(file.path()),
            Err(MotecError::Invalid { .. })
        ));
    }

    #[test]
    fn byte_parser_accepts_an_ldx_sidecar() {
        let fixture = fixture();
        let ldx = br#"<LDXFile><Layers><Layer><MarkerBlock><MarkerGroup>
            <Marker ClassName="BCN" Time="7.5e+05"/>
            </MarkerGroup></MarkerBlock></Layer><Details>
            <String Id="Total Laps" Value="2"/>
            <String Id="Fastest Time" Value="0:00.250"/>
            <String Id="Fastest Lap" Value="1"/>
            </Details></Layers></LDXFile>"#;
        let file = MotecFile::from_bytes_with_ldx(
            "fixture.ld",
            std::fs::read(fixture.path()).unwrap(),
            ldx,
        )
        .unwrap();
        let metadata = file.metadata();
        assert_eq!(metadata.laps.len(), 2);
        assert_eq!(metadata.laps[0].start_ns, 500_000_000);
        assert_eq!(metadata.fastest_lap, Some(metadata.laps[0].clone()));
    }

    #[test]
    fn file_parser_discovers_the_companion_ldx() {
        let fixture = fixture();
        let directory = tempfile::tempdir().unwrap();
        let ld_path = directory.path().join("run.ld");
        std::fs::copy(fixture.path(), &ld_path).unwrap();
        std::fs::write(
            directory.path().join("run.ldx"),
            br#"<LDXFile><Marker ClassName="BCN" Time="5e+05"/></LDXFile>"#,
        )
        .unwrap();

        let file = MotecFile::open(&ld_path).unwrap();
        assert_eq!(file.ldx.unwrap().marker_times_ns, [500_000_000]);
    }

    #[test]
    fn clean_fixture_reports_no_diagnostics() {
        let file = MotecFile::from_bytes("fixture.ld", fixture_bytes()).unwrap();
        assert!(
            file.diagnostics().is_empty(),
            "unexpected diagnostics: {:?}",
            file.diagnostics()
        );
    }

    #[test]
    fn warns_on_zero_frequency_channel() {
        let mut data = fixture_bytes();
        // Speed channel starts at 0x200; frequency is at offset 0x16.
        u16_at(&mut data, 0x200 + 0x16, 0);
        let file = MotecFile::from_bytes("fixture.ld", data).unwrap();
        assert!(file
            .diagnostics()
            .iter()
            .any(|d| d.code == "ld.zero_frequency" && d.channel.as_deref() == Some("Speed")));
    }

    #[test]
    fn warns_on_zero_scale_factor() {
        let mut data = fixture_bytes();
        // Brake channel starts at 0x27c; scale (divisor) is at offset 0x1c.
        i16_at(&mut data, 0x27c + 0x1c, 0);
        let file = MotecFile::from_bytes("fixture.ld", data).unwrap();
        assert!(
            file.diagnostics()
                .iter()
                .any(|d| d.code == "ld.zero_scale_factor"
                    && d.channel.as_deref() == Some("P_F_BRAKE"))
        );
    }

    #[test]
    fn warns_on_invalid_width() {
        let mut data = fixture_bytes();
        // Speed channel starts at 0x200; width is at offset 0x14.  Set to 3,
        // which is not 2/4/8.
        u16_at(&mut data, 0x200 + 0x14, 3);
        let file = MotecFile::from_bytes("fixture.ld", data).unwrap();
        assert!(file
            .diagnostics()
            .iter()
            .any(|d| d.code == "ld.invalid_width" && d.channel.as_deref() == Some("Speed")));
    }

    #[test]
    fn unsupported_datatype_width_is_visibly_empty() {
        // Float type 0x07 with width 2 is a valid width but not a decodable
        // combination: the sample type would be F32 (4 bytes) but the data
        // is only 2 bytes wide. The channel must be exposed with
        // sample_count 0 and a diagnostic, not silently decode to 0.0.
        let mut data = fixture_bytes();
        // Speed channel starts at 0x200; datatype is at offset 0x12 (already
        // 0x07), width is at offset 0x14. Set width to 2.
        u16_at(&mut data, 0x200 + 0x14, 2);
        // Adjust data_ptr and count so they would normally produce samples
        // if the width were valid.
        u32_at(&mut data, 0x200 + 0x08, 0x380);
        u32_at(&mut data, 0x200 + 0x0c, 10);
        // Write some bytes at the data area so bounds checks would pass.
        for i in 0..20 {
            data[0x380 + i] = 0xff;
        }
        let file = MotecFile::from_bytes("fixture.ld", data).unwrap();
        let speed = file
            .channels()
            .iter()
            .find(|c| c.name == "Speed")
            .expect("Speed channel");
        assert_eq!(speed.sample_count, 0);
        assert!(speed.chunks.is_empty());
        assert!(
            file.diagnostics()
                .iter()
                .any(|d| d.code == "ld.decode_unsupported_width"
                    && d.channel.as_deref() == Some("Speed")),
            "expected ld.decode_unsupported_width diagnostic: {:?}",
            file.diagnostics()
        );
    }

    #[test]
    fn ldx_warns_on_unparsable_marker_time() {
        let ldx = br#"<LDXFile><Marker ClassName="BCN" Time="not-a-number"/></LDXFile>"#;
        let parsed = parse_motec_ldx_bytes("synthetic.ldx", ldx).unwrap();
        assert!(parsed
            .diagnostics
            .iter()
            .any(|d| d.code == "ldx.marker_time_unparsable"));
    }

    #[test]
    fn ldx_clean_sidecar_reports_no_diagnostics() {
        let ldx = br#"<LDXFile><Layers><Layer><MarkerBlock><MarkerGroup>
            <Marker ClassName="BCN" Time="7.5e+05"/>
            </MarkerGroup></MarkerBlock></Layer><Details>
            <String Id="Total Laps" Value="2"/>
            </Details></Layers></LDXFile>"#;
        let parsed = parse_motec_ldx_bytes("synthetic.ldx", ldx).unwrap();
        assert!(
            parsed.diagnostics.is_empty(),
            "unexpected diagnostics: {:?}",
            parsed.diagnostics
        );
    }
}
