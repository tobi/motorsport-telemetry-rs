//! FlatBuffers catalog stored as `metadata.fb`.

use crate::zip::ZipError;
use flatbuffers::{FlatBufferBuilder, WIPOffset};
use motorsport_telemetry_core::{
    lookup_unit, normalize_unit, AbsoluteTimeRange, Channel, Chunk, DriverStint, FileMetadata,
    LapMetadata, SampleType, SourceIdentity, Span, UnitSource, VideoFileRef,
};

const V: fn(u16) -> flatbuffers::VOffsetT = |field| 4 + field * 2;

/// Catalog format written by this crate.
///
/// `1` was the original catalog. `2` adds `video_frames.bin` and
/// `presentation_offset_ns`. `3` stores `first_video_frame` on each lap and
/// the presentation offset on each video handle. `4` requires `utc_start_ns`
/// (Unix epoch at file `t = 0`) and IANA `timezone`. `5` stores spans
/// (string-annotated intervals) and per-channel visibility so the native
/// catalog matches JSONL channel capabilities. Bump this when the on-disk
/// layout changes and add a step in `migrate.rs`.
/// [`crate::NativeRecording::open`] rewrites writable older files.
pub const FORMAT_VERSION: u16 = 5;

/// Parsed FlatBuffers catalog from `metadata.fb`.
#[derive(Debug, Clone)]
pub struct Catalog {
    pub format_version: u16,
    pub identity: SourceIdentity,
    pub laps: Vec<LapMetadata>,
    pub valid_laps: u32,
    pub channels: Vec<CatalogChannel>,
    pub source_format: String,
    pub source_path: String,
    pub schema_hash: u64,
    pub duration_ns: u64,
    pub sample_count: u64,
    pub channel_count: u32,
    pub sampled_channel_count: u32,
    pub session_hint: String,
    pub comment: String,
    pub clock: Option<AbsoluteTimeRange>,
    /// Unix-epoch nanoseconds (UTC) at file `t = 0`.
    pub utc_start_ns: Option<u64>,
    /// IANA timezone of the venue. Empty when unknown.
    pub timezone: String,
    pub driver_stints: Vec<DriverStint>,
    pub videos: Vec<VideoFileRef>,
    pub presentation_offset_ns: Option<i128>,
    /// Interval annotations. Same model as JSONL `k:"s"` records.
    pub spans: Vec<Span>,
}

#[derive(Debug, Clone)]
pub struct CatalogChannel {
    pub id: u32,
    pub name: String,
    pub member: String,
    pub time_member: String,
    pub unit_raw: String,
    pub unit_canonical: String,
    pub unit_source: UnitSource,
    pub dimension: u8,
    pub sample_type: SampleType,
    pub scale: f64,
    pub bias: f64,
    pub uses_step: bool,
    pub sample_count: u64,
    pub duration_ns: u64,
    pub kind: u8,
    pub chunks: Vec<Chunk>,
    /// Default visibility. Absent on v1–v4 catalogs (treated as visible).
    pub visible: bool,
}

impl Catalog {
    pub fn to_file_metadata(&self) -> FileMetadata {
        let driver_ids = self
            .driver_stints
            .iter()
            .map(|stint| stint.driver_id)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        let fastest_lap = self
            .laps
            .iter()
            .filter(|lap| lap.complete)
            .min_by_key(|lap| lap.duration_ns)
            .cloned();
        let session_key = (!self.session_hint.is_empty())
            .then(|| format!("{}:{:016x}", self.session_hint, self.schema_hash));
        FileMetadata {
            path: self.source_path.clone(),
            format: self.source_format.clone(),
            format_version: Some(self.format_version),
            channel_count: self.channel_count as usize,
            sampled_channel_count: self.sampled_channel_count as usize,
            sample_count: self.sample_count,
            duration_ns: self.duration_ns,
            schema_hash: self.schema_hash,
            session_key,
            absolute_clock: self.clock.as_ref().map(|clock| clock.clock.clone()),
            absolute_start_ns: self.clock.as_ref().map(|clock| clock.start_ns),
            absolute_end_ns: self.clock.as_ref().map(|clock| clock.end_ns),
            clock_offset_ns: self.clock.as_ref().map(|clock| i128::from(clock.start_ns)),
            utc_start_ns: self.utc_start_ns,
            timezone: self.timezone.clone(),
            identity: self.identity.clone(),
            driver_ids,
            driver_stints: self.driver_stints.clone(),
            laps: self.laps.clone(),
            valid_laps: self.valid_laps,
            fastest_lap,
            video_frame_count: self
                .videos
                .first()
                .and_then(|video| (video.frame_count > 0).then_some(video.frame_count)),
            video_presentation_offset_ns: self.presentation_offset_ns,
            videos: {
                let offset = self.presentation_offset_ns;
                self.videos
                    .iter()
                    .cloned()
                    .map(|mut video| {
                        if video.presentation_offset_ns.is_none() {
                            video.presentation_offset_ns = offset;
                        }
                        video
                    })
                    .collect()
            },
        }
    }
}

pub fn encode(catalog: &Catalog) -> Result<Vec<u8>, ZipError> {
    let mut builder = FlatBufferBuilder::new();
    let identity = {
        let driver = builder.create_string(&catalog.identity.driver);
        let vehicle = builder.create_string(&catalog.identity.vehicle);
        let venue = builder.create_string(&catalog.identity.venue);
        let event = builder.create_string(&catalog.identity.event);
        let session = builder.create_string(&catalog.identity.session);
        let date = builder.create_string(&catalog.identity.date);
        let time = builder.create_string(&catalog.identity.time);
        let start = builder.start_table();
        builder.push_slot_always::<WIPOffset<_>>(V(0), driver);
        builder.push_slot_always::<WIPOffset<_>>(V(1), vehicle);
        builder.push_slot_always::<WIPOffset<_>>(V(2), venue);
        builder.push_slot_always::<WIPOffset<_>>(V(3), event);
        builder.push_slot_always::<WIPOffset<_>>(V(4), session);
        builder.push_slot_always::<WIPOffset<_>>(V(5), date);
        builder.push_slot_always::<WIPOffset<_>>(V(6), time);
        builder.end_table(start)
    };

    let laps = builder.create_vector(&pack_laps(&catalog.laps, catalog.format_version));
    let channels = builder.create_vector(&pack_channels(&catalog.channels));
    let stints = builder.create_vector(&pack_stints(&catalog.driver_stints));
    let videos = builder.create_vector(&pack_videos(&catalog.videos, catalog.format_version));
    let visibility = (catalog.format_version >= 5)
        .then(|| builder.create_vector(&pack_visibility(&catalog.channels)));
    let spans =
        (catalog.format_version >= 5).then(|| builder.create_vector(&pack_spans(&catalog.spans)));

    let source_format = builder.create_string(&catalog.source_format);
    let source_path = builder.create_string(&catalog.source_path);
    let comment = builder.create_string(&catalog.comment);
    let session_hint = builder.create_string(&catalog.session_hint);
    let clock_name = catalog
        .clock
        .as_ref()
        .map(|clock| builder.create_string(&clock.clock));
    let timezone = (catalog.format_version >= 4 && !catalog.timezone.is_empty())
        .then(|| builder.create_string(&catalog.timezone));

    let start = builder.start_table();
    builder.push_slot(V(0), catalog.format_version, 0);
    builder.push_slot_always::<WIPOffset<_>>(V(1), identity);
    builder.push_slot_always::<WIPOffset<_>>(V(3), laps);
    builder.push_slot_always::<WIPOffset<_>>(V(4), channels);
    builder.push_slot_always::<WIPOffset<_>>(V(6), source_format);
    builder.push_slot_always::<WIPOffset<_>>(V(7), source_path);
    builder.push_slot(V(8), catalog.schema_hash, 0);
    builder.push_slot(V(9), catalog.duration_ns, 0);
    builder.push_slot(V(10), catalog.sample_count, 0);
    builder.push_slot(V(11), catalog.channel_count, 0);
    builder.push_slot(V(12), catalog.sampled_channel_count, 0);
    builder.push_slot(V(13), catalog.valid_laps, 0);
    builder.push_slot_always::<WIPOffset<_>>(V(14), comment);
    builder.push_slot_always::<WIPOffset<_>>(V(15), session_hint);
    builder.push_slot_always::<WIPOffset<_>>(V(16), stints);
    builder.push_slot_always::<WIPOffset<_>>(V(20), videos);
    if let (Some(clock), Some(name)) = (catalog.clock.as_ref(), clock_name) {
        builder.push_slot_always::<WIPOffset<_>>(V(17), name);
        builder.push_slot(V(18), clock.start_ns, 0);
        builder.push_slot(V(19), clock.end_ns, 0);
    }
    if let Some(offset) = catalog.presentation_offset_ns {
        builder.push_slot(V(21), 1u32, 0);
        builder.push_slot(V(22), i64::try_from(offset).unwrap_or(i64::MAX), 0);
    }
    if catalog.format_version >= 4 {
        if let Some(utc) = catalog.utc_start_ns {
            builder.push_slot(V(23), 1u32, 0);
            builder.push_slot(V(24), utc, 0);
        }
        if let Some(timezone) = timezone {
            builder.push_slot_always::<WIPOffset<_>>(V(25), timezone);
        }
    }
    if let Some(visibility) = visibility {
        builder.push_slot_always::<WIPOffset<_>>(V(26), visibility);
    }
    if let Some(spans) = spans {
        builder.push_slot_always::<WIPOffset<_>>(V(27), spans);
    }
    let root = builder.end_table(start);
    builder.finish(root, None);
    Ok(builder.finished_data().to_vec())
}

/// Reads only the catalog format version from a catalog buffer.
pub fn decode_format_version(bytes: &[u8]) -> Result<u16, ZipError> {
    Ok(root_table(bytes)?.u16_field(0))
}

/// True when `version` is older than [`FORMAT_VERSION`] and should be rewritten.
pub fn needs_update(version: u16) -> bool {
    version < FORMAT_VERSION
}

/// Reads only `valid_laps` from a catalog buffer.
pub fn decode_valid_laps(bytes: &[u8]) -> Result<u32, ZipError> {
    Ok(root_table(bytes)?.u32(13))
}

/// Reads only the lap list from a catalog buffer. Does not unpack channels.
pub fn decode_laps(bytes: &[u8]) -> Result<Vec<LapMetadata>, ZipError> {
    let table = root_table(bytes)?;
    Ok(unpack_laps(&table.u8s(3), table.u16_field(0)))
}

pub fn decode(bytes: &[u8]) -> Result<Catalog, ZipError> {
    let table = root_table(bytes)?;
    let identity = table
        .table(1)
        .map(|identity| SourceIdentity {
            driver: identity.string(0).unwrap_or_default(),
            vehicle: identity.string(1).unwrap_or_default(),
            venue: identity.string(2).unwrap_or_default(),
            event: identity.string(3).unwrap_or_default(),
            session: identity.string(4).unwrap_or_default(),
            date: identity.string(5).unwrap_or_default(),
            time: identity.string(6).unwrap_or_default(),
        })
        .unwrap_or_default();
    let format_version = table.u16_field(0);
    let laps = unpack_laps(&table.u8s(3), format_version);
    let mut channels = unpack_channels(&table.u8s(4));
    if format_version >= 5 {
        apply_visibility(&mut channels, &table.u8s(26));
    }
    let driver_stints = unpack_stints(&table.u8s(16));
    let clock = table.string(17).and_then(|name| {
        if name.is_empty() {
            None
        } else {
            Some(AbsoluteTimeRange {
                clock: name,
                start_ns: table.u64(18),
                end_ns: table.u64(19),
                session_hint: table.string(15).unwrap_or_default(),
            })
        }
    });
    Ok(Catalog {
        format_version,
        identity,
        laps,
        valid_laps: table.u32(13),
        channels,
        source_format: table.string(6).unwrap_or_default(),
        source_path: table.string(7).unwrap_or_default(),
        schema_hash: table.u64(8),
        duration_ns: table.u64(9),
        sample_count: table.u64(10),
        channel_count: table.u32(11),
        sampled_channel_count: table.u32(12),
        session_hint: table.string(15).unwrap_or_default(),
        comment: table.string(14).unwrap_or_default(),
        clock,
        utc_start_ns: (table.u32(23) != 0).then(|| table.u64(24)),
        timezone: table.string(25).unwrap_or_default(),
        driver_stints,
        videos: unpack_videos(&table.u8s(20), format_version),
        presentation_offset_ns: (table.u32(21) != 0).then(|| i128::from(table.i64_field(22))),
        spans: if format_version >= 5 {
            unpack_spans(&table.u8s(27))
        } else {
            Vec::new()
        },
    })
}

pub fn unit_fields(channel: &Channel) -> (String, u8) {
    let canonical = normalize_unit(&channel.unit).unwrap_or("").to_owned();
    let dimension = lookup_unit(&channel.unit).map_or(0, |unit| dimension_code(unit.dimension));
    (canonical, dimension)
}

fn unit_source_code(source: UnitSource) -> u8 {
    match source {
        UnitSource::Unknown => 0,
        UnitSource::Declared => 1,
        UnitSource::SpecDefault => 2,
    }
}

fn unit_source_from(code: u8) -> UnitSource {
    match code {
        1 => UnitSource::Declared,
        2 => UnitSource::SpecDefault,
        _ => UnitSource::Unknown,
    }
}

fn sample_type_from(code: u8) -> SampleType {
    match code {
        1 => SampleType::U8,
        2 => SampleType::I16,
        3 => SampleType::U16,
        4 => SampleType::I32,
        5 => SampleType::U32,
        7 => SampleType::F64,
        _ => SampleType::F32,
    }
}

fn dimension_code(dimension: motorsport_telemetry_core::Dimension) -> u8 {
    use motorsport_telemetry_core::Dimension::*;
    match dimension {
        Length => 1,
        Speed => 2,
        Acceleration => 3,
        Angle => 4,
        AngularVelocity => 5,
        AngularAcceleration => 6,
        Pressure => 7,
        Temperature => 8,
        Time => 9,
        Frequency => 10,
        Force => 11,
        Torque => 12,
        Energy => 13,
        Power => 14,
        Voltage => 15,
        Current => 16,
        Resistance => 17,
        Mass => 18,
        Volume => 19,
        VolumetricFlow => 20,
        MassFlow => 21,
        Ratio => 22,
        Count => 23,
        Marker => 24,
        Logarithmic => 25,
    }
}

struct Table<'a> {
    buf: &'a [u8],
    loc: usize,
}

impl<'a> Table<'a> {
    fn slot(&self, field: u16) -> Option<usize> {
        let vtable_rel = i32::from_le_bytes(self.buf[self.loc..self.loc + 4].try_into().ok()?);
        let vtable = self.loc.checked_sub(vtable_rel as usize)?;
        let vsize = u16::from_le_bytes(self.buf.get(vtable..vtable + 2)?.try_into().ok()?) as usize;
        let off = 4 + field as usize * 2;
        if off + 2 > vsize {
            return None;
        }
        let rel = u16::from_le_bytes(self.buf[vtable + off..vtable + off + 2].try_into().ok()?);
        (rel != 0).then_some(self.loc + rel as usize)
    }

    fn u16_field(&self, field: u16) -> u16 {
        self.slot(field)
            .and_then(|at| self.buf.get(at..at + 2))
            .and_then(|bytes| bytes.try_into().ok())
            .map(u16::from_le_bytes)
            .unwrap_or(0)
    }
    fn u32(&self, field: u16) -> u32 {
        self.slot(field)
            .and_then(|at| self.buf.get(at..at + 4))
            .and_then(|bytes| bytes.try_into().ok())
            .map(u32::from_le_bytes)
            .unwrap_or(0)
    }
    fn i64_field(&self, field: u16) -> i64 {
        self.slot(field)
            .and_then(|at| self.buf.get(at..at + 8))
            .and_then(|bytes| bytes.try_into().ok())
            .map(i64::from_le_bytes)
            .unwrap_or(0)
    }
    fn u64(&self, field: u16) -> u64 {
        self.slot(field)
            .and_then(|at| self.buf.get(at..at + 8))
            .and_then(|bytes| bytes.try_into().ok())
            .map(u64::from_le_bytes)
            .unwrap_or(0)
    }
    fn string(&self, field: u16) -> Option<String> {
        let at = self.indirect(field)?;
        let len = u32::from_le_bytes(self.buf.get(at..at + 4)?.try_into().ok()?) as usize;
        let bytes = self.buf.get(at + 4..at + 4 + len)?;
        Some(std::str::from_utf8(bytes).ok()?.to_owned())
    }
    fn table(&self, field: u16) -> Option<Table<'a>> {
        Some(Table {
            buf: self.buf,
            loc: self.indirect(field)?,
        })
    }
    fn u8s(&self, field: u16) -> Vec<u8> {
        let Some(at) = self.indirect(field) else {
            return Vec::new();
        };
        let Some(len) = self
            .buf
            .get(at..at + 4)
            .and_then(|bytes| bytes.try_into().ok())
            .map(u32::from_le_bytes)
        else {
            return Vec::new();
        };
        self.buf
            .get(at + 4..at + 4 + len as usize)
            .unwrap_or_default()
            .to_vec()
    }

    fn indirect(&self, field: u16) -> Option<usize> {
        let at = self.slot(field)?;
        let rel = u32::from_le_bytes(self.buf.get(at..at + 4)?.try_into().ok()?) as usize;
        Some(at + rel)
    }
}

fn pack_videos(videos: &[VideoFileRef], format_version: u16) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(videos.len() as u32).to_le_bytes());
    for video in videos {
        pack_string(&mut out, &video.filename);
        out.extend_from_slice(&video.index.to_le_bytes());
        match video.blake3 {
            Some(hash) => {
                out.push(1);
                out.extend_from_slice(&hash);
            }
            None => out.push(0),
        }
        out.extend_from_slice(&video.frame_count.to_le_bytes());
        if format_version >= 3 {
            match video.presentation_offset_ns {
                Some(offset) => {
                    out.push(1);
                    out.extend_from_slice(&i64::try_from(offset).unwrap_or(i64::MAX).to_le_bytes());
                }
                None => out.push(0),
            }
        }
    }
    out
}

fn unpack_videos(bytes: &[u8], format_version: u16) -> Vec<VideoFileRef> {
    if bytes.len() < 4 {
        return Vec::new();
    }
    let count = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
    let mut cursor = 4;
    let mut videos = Vec::with_capacity(count);
    for _ in 0..count {
        let filename = unpack_string(bytes, &mut cursor);
        if cursor + 4 + 1 > bytes.len() {
            break;
        }
        let index = u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().unwrap());
        cursor += 4;
        let hashed = bytes[cursor];
        cursor += 1;
        let blake3 = if hashed != 0 {
            if cursor + 32 > bytes.len() {
                break;
            }
            let mut hash = [0u8; 32];
            hash.copy_from_slice(&bytes[cursor..cursor + 32]);
            cursor += 32;
            Some(hash)
        } else {
            None
        };
        if cursor + 8 > bytes.len() {
            break;
        }
        let frame_count = u64::from_le_bytes(bytes[cursor..cursor + 8].try_into().unwrap());
        cursor += 8;
        let presentation_offset_ns = if format_version >= 3 {
            if cursor >= bytes.len() {
                break;
            }
            let present = bytes[cursor];
            cursor += 1;
            if present != 0 {
                if cursor + 8 > bytes.len() {
                    break;
                }
                let offset = i64::from_le_bytes(bytes[cursor..cursor + 8].try_into().unwrap());
                cursor += 8;
                Some(i128::from(offset))
            } else {
                None
            }
        } else {
            None
        };
        videos.push(VideoFileRef {
            filename,
            index,
            blake3,
            frame_count,
            presentation_offset_ns,
        });
    }
    videos
}

fn pack_laps(laps: &[LapMetadata], format_version: u16) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(laps.len() as u32).to_le_bytes());
    for lap in laps {
        out.extend_from_slice(&lap.number.to_le_bytes());
        out.extend_from_slice(&lap.start_ns.to_le_bytes());
        out.extend_from_slice(&lap.end_ns.to_le_bytes());
        out.extend_from_slice(&lap.duration_ns.to_le_bytes());
        out.push(u8::from(lap.complete));
        if format_version >= 3 {
            match lap.first_video_frame {
                Some(frame) => {
                    out.push(1);
                    out.extend_from_slice(&frame.to_le_bytes());
                }
                None => out.push(0),
            }
        }
    }
    out
}

fn unpack_laps(bytes: &[u8], format_version: u16) -> Vec<LapMetadata> {
    if bytes.len() < 4 {
        return Vec::new();
    }
    let count = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
    let mut cursor = 4;
    let mut laps = Vec::with_capacity(count);
    for _ in 0..count {
        if cursor + 33 > bytes.len() {
            break;
        }
        let number = i64::from_le_bytes(bytes[cursor..cursor + 8].try_into().unwrap());
        let start_ns = u64::from_le_bytes(bytes[cursor + 8..cursor + 16].try_into().unwrap());
        let end_ns = u64::from_le_bytes(bytes[cursor + 16..cursor + 24].try_into().unwrap());
        let duration_ns = u64::from_le_bytes(bytes[cursor + 24..cursor + 32].try_into().unwrap());
        let complete = bytes[cursor + 32] != 0;
        cursor += 33;
        let first_video_frame = if format_version >= 3 {
            if cursor >= bytes.len() {
                break;
            }
            let present = bytes[cursor];
            cursor += 1;
            if present != 0 {
                if cursor + 8 > bytes.len() {
                    break;
                }
                let frame = u64::from_le_bytes(bytes[cursor..cursor + 8].try_into().unwrap());
                cursor += 8;
                Some(frame)
            } else {
                None
            }
        } else {
            None
        };
        laps.push(LapMetadata {
            number,
            start_ns,
            end_ns,
            duration_ns,
            complete,
            first_video_frame,
        });
    }
    laps
}

fn pack_stints(stints: &[DriverStint]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(stints.len() as u32).to_le_bytes());
    for stint in stints {
        out.extend_from_slice(&stint.driver_id.to_le_bytes());
        out.extend_from_slice(&stint.start_ns.to_le_bytes());
        out.extend_from_slice(&stint.end_ns.to_le_bytes());
    }
    out
}

fn unpack_stints(bytes: &[u8]) -> Vec<DriverStint> {
    if bytes.len() < 4 {
        return Vec::new();
    }
    let count = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
    let mut cursor = 4;
    let mut stints = Vec::with_capacity(count);
    for _ in 0..count {
        if cursor + 24 > bytes.len() {
            break;
        }
        stints.push(DriverStint {
            driver_id: i64::from_le_bytes(bytes[cursor..cursor + 8].try_into().unwrap()),
            start_ns: u64::from_le_bytes(bytes[cursor + 8..cursor + 16].try_into().unwrap()),
            end_ns: u64::from_le_bytes(bytes[cursor + 16..cursor + 24].try_into().unwrap()),
        });
        cursor += 24;
    }
    stints
}

fn pack_string(out: &mut Vec<u8>, value: &str) {
    let bytes = value.as_bytes();
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
}

fn unpack_string(bytes: &[u8], cursor: &mut usize) -> String {
    if *cursor + 4 > bytes.len() {
        return String::new();
    }
    let len = u32::from_le_bytes(bytes[*cursor..*cursor + 4].try_into().unwrap()) as usize;
    *cursor += 4;
    let end = (*cursor + len).min(bytes.len());
    let value = String::from_utf8_lossy(&bytes[*cursor..end]).into_owned();
    *cursor = end;
    value
}

fn pack_channels(channels: &[CatalogChannel]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(channels.len() as u32).to_le_bytes());
    for channel in channels {
        out.extend_from_slice(&channel.id.to_le_bytes());
        pack_string(&mut out, &channel.name);
        pack_string(&mut out, &channel.member);
        pack_string(&mut out, &channel.time_member);
        pack_string(&mut out, &channel.unit_raw);
        pack_string(&mut out, &channel.unit_canonical);
        out.push(unit_source_code(channel.unit_source));
        out.push(channel.dimension);
        out.push(channel.sample_type.code() as u8);
        out.push(u8::from(channel.uses_step));
        out.push(channel.kind);
        out.extend_from_slice(&channel.scale.to_le_bytes());
        out.extend_from_slice(&channel.bias.to_le_bytes());
        out.extend_from_slice(&channel.sample_count.to_le_bytes());
        out.extend_from_slice(&channel.duration_ns.to_le_bytes());
        out.extend_from_slice(&(channel.chunks.len() as u32).to_le_bytes());
        for chunk in &channel.chunks {
            out.extend_from_slice(&chunk.sample_period_ns.to_le_bytes());
            out.extend_from_slice(&chunk.sample_count.to_le_bytes());
            out.extend_from_slice(&chunk.sample_base.to_le_bytes());
            out.extend_from_slice(&chunk.time_base_ns.to_le_bytes());
        }
    }
    out
}

fn unpack_channels(bytes: &[u8]) -> Vec<CatalogChannel> {
    if bytes.len() < 4 {
        return Vec::new();
    }
    let count = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
    let mut cursor = 4;
    let mut channels = Vec::with_capacity(count);
    for _ in 0..count {
        if cursor + 4 > bytes.len() {
            break;
        }
        let id = u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().unwrap());
        cursor += 4;
        let name = unpack_string(bytes, &mut cursor);
        let member = unpack_string(bytes, &mut cursor);
        let time_member = unpack_string(bytes, &mut cursor);
        let unit_raw = unpack_string(bytes, &mut cursor);
        let unit_canonical = unpack_string(bytes, &mut cursor);
        if cursor + 5 + 8 + 8 + 8 + 8 + 4 > bytes.len() {
            break;
        }
        let unit_source = unit_source_from(bytes[cursor]);
        let dimension = bytes[cursor + 1];
        let sample_type = sample_type_from(bytes[cursor + 2]);
        let uses_step = bytes[cursor + 3] != 0;
        let kind = bytes[cursor + 4];
        cursor += 5;
        let scale = f64::from_le_bytes(bytes[cursor..cursor + 8].try_into().unwrap());
        cursor += 8;
        let bias = f64::from_le_bytes(bytes[cursor..cursor + 8].try_into().unwrap());
        cursor += 8;
        let sample_count = u64::from_le_bytes(bytes[cursor..cursor + 8].try_into().unwrap());
        cursor += 8;
        let duration_ns = u64::from_le_bytes(bytes[cursor..cursor + 8].try_into().unwrap());
        cursor += 8;
        let chunk_count =
            u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().unwrap()) as usize;
        cursor += 4;
        let mut chunks = Vec::with_capacity(chunk_count);
        for _ in 0..chunk_count {
            if cursor + 32 > bytes.len() {
                break;
            }
            chunks.push(Chunk {
                sample_period_ns: u64::from_le_bytes(bytes[cursor..cursor + 8].try_into().unwrap()),
                sample_count: u64::from_le_bytes(
                    bytes[cursor + 8..cursor + 16].try_into().unwrap(),
                ),
                data_ptr: 0,
                sample_base: u64::from_le_bytes(
                    bytes[cursor + 16..cursor + 24].try_into().unwrap(),
                ),
                time_base_ns: u64::from_le_bytes(
                    bytes[cursor + 24..cursor + 32].try_into().unwrap(),
                ),
            });
            cursor += 32;
        }
        channels.push(CatalogChannel {
            id,
            name,
            member,
            time_member,
            unit_raw,
            unit_canonical,
            unit_source,
            dimension,
            sample_type,
            scale,
            bias,
            uses_step,
            sample_count,
            duration_ns,
            kind,
            chunks,
            visible: true,
        });
    }
    channels
}

fn pack_visibility(channels: &[CatalogChannel]) -> Vec<u8> {
    channels
        .iter()
        .map(|channel| u8::from(channel.visible))
        .collect()
}

fn apply_visibility(channels: &mut [CatalogChannel], bytes: &[u8]) {
    for (channel, flag) in channels.iter_mut().zip(bytes) {
        channel.visible = *flag != 0;
    }
}

fn pack_spans(spans: &[Span]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(spans.len() as u32).to_le_bytes());
    for span in spans {
        pack_string(&mut out, &span.name);
        out.extend_from_slice(&span.start_ns.to_le_bytes());
        out.extend_from_slice(&span.end_ns.to_le_bytes());
        out.push(u8::from(span.visible));
        pack_string(&mut out, &span.color);
        pack_string(&mut out, &span.primary.title);
        pack_string(&mut out, &span.primary.subtitle);
        out.extend_from_slice(&(span.meta.len() as u32).to_le_bytes());
        for (key, value) in &span.meta {
            pack_string(&mut out, key);
            pack_string(&mut out, value);
        }
    }
    out
}

fn unpack_spans(bytes: &[u8]) -> Vec<Span> {
    if bytes.len() < 4 {
        return Vec::new();
    }
    let count = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
    let mut cursor = 4;
    let mut spans = Vec::with_capacity(count);
    for _ in 0..count {
        let name = unpack_string(bytes, &mut cursor);
        if cursor + 16 + 1 > bytes.len() {
            break;
        }
        let start_ns = u64::from_le_bytes(bytes[cursor..cursor + 8].try_into().unwrap());
        let end_ns = u64::from_le_bytes(bytes[cursor + 8..cursor + 16].try_into().unwrap());
        let visible = bytes[cursor + 16] != 0;
        cursor += 17;
        let color = unpack_string(bytes, &mut cursor);
        let title = unpack_string(bytes, &mut cursor);
        let subtitle = unpack_string(bytes, &mut cursor);
        if cursor + 4 > bytes.len() {
            break;
        }
        let meta_count = u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().unwrap()) as usize;
        cursor += 4;
        let mut meta = Vec::with_capacity(meta_count);
        for _ in 0..meta_count {
            let key = unpack_string(bytes, &mut cursor);
            let value = unpack_string(bytes, &mut cursor);
            meta.push((key, value));
        }
        spans.push(Span {
            name,
            start_ns,
            end_ns,
            visible,
            color,
            primary: motorsport_telemetry_core::SpanPrimary { title, subtitle },
            meta,
        });
    }
    spans
}

fn root_table(bytes: &[u8]) -> Result<Table<'_>, ZipError> {
    if bytes.len() < 8 {
        return Err(ZipError("catalog is too small".into()));
    }
    let root = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
    if root + 4 > bytes.len() {
        return Err(ZipError("catalog root is out of range".into()));
    }
    Ok(Table {
        buf: bytes,
        loc: root,
    })
}
