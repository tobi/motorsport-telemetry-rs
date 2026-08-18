//! FlatBuffers catalog stored as `metadata.fb`.

use crate::zip::ZipError;
use flatbuffers::{FlatBufferBuilder, WIPOffset};
use motorsport_telemetry_core::{
    lookup_unit, normalize_unit, AbsoluteTimeRange, AppliedPass, Channel, ChannelDisplay,
    ChannelLabel, ChannelPlot, Chunk, DriverStint, FileMetadata, LapMetadata, SampleType,
    SourceIdentity, Span, SpanMetaValue, UnitSource, VideoFileRef, TIMESPAN_MS_MAX,
};

const V: fn(u16) -> flatbuffers::VOffsetT = |field| 4 + field * 2;

/// Catalog format written by this crate.
///
/// `1` was the original catalog. `2` adds `video_frames.bin` and
/// `presentation_offset_ns`. `3` stores `first_video_frame` on each lap and
/// the presentation offset on each video handle. `4` requires `utc_start_ns`
/// (Unix epoch at file `t = 0`) and IANA `timezone`. `5` stores spans
/// (string-annotated intervals) and per-channel visibility so the native
/// catalog matches JSONL channel capabilities. `6` adds sparse per-channel
/// comment labels. `7` adds plot class, display scale, and rounding. `8`
/// stores typed span meta (`timespan_ms` as u32le). `9` records the
/// provenance of applied processing passes (name, version, params, inputs,
/// outputs) and preserves the original `source_format`/`source_path` across
/// rewrites. `10` adds signed `int8` (`SampleType::I8`, sample-type code 0)
/// so PDS TPMS RSSI and similar 1-byte signed channels round-trip without
/// being misread as `f32`. No schema fields or zip members change; v1–v9
/// writers never emitted code 0, so older catalogs migrate as a no-op.
/// Bump this when the on-disk layout changes and add a step in `migrate.rs`.
/// [`crate::NativeRecording::open`] rewrites writable older files.
pub const FORMAT_VERSION: u16 = 10;

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
    /// Processing passes applied to this recording, in application order.
    ///
    /// Every pass is lossless: it only appended the channels it names in
    /// [`AppliedPass::outputs`]. Empty on raw conversions and on catalogs
    /// older than v9.
    pub passes: Vec<AppliedPass>,
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
    /// Sparse comments on this channel. Empty when none. Trace channels only.
    pub labels: Vec<ChannelLabel>,
    /// Plot class, optional scale, and rounding. Default is a time-series trace.
    pub display: ChannelDisplay,
}

impl Catalog {
    /// Format-neutral summary. `path` is the `.telemetry` file itself;
    /// the catalog's `source_*` fields describe what it was converted from.
    pub fn to_file_metadata(&self, path: &str) -> FileMetadata {
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
            path: path.to_owned(),
            format: self.source_format.clone(),
            source_format: self.source_format.clone(),
            source_path: self.source_path.clone(),
            format_version: Some(self.format_version),
            passes: self.passes.clone(),
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
    let labels = (catalog.format_version >= 6)
        .then(|| builder.create_vector(&pack_labels(&catalog.channels)));
    let display = (catalog.format_version >= 7)
        .then(|| builder.create_vector(&pack_display(&catalog.channels)));
    let passes = (catalog.format_version >= 9 && !catalog.passes.is_empty())
        .then(|| builder.create_vector(&pack_passes(&catalog.passes)));

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
    if let Some(labels) = labels {
        builder.push_slot_always::<WIPOffset<_>>(V(28), labels);
    }
    if let Some(display) = display {
        builder.push_slot_always::<WIPOffset<_>>(V(29), display);
    }
    if let Some(passes) = passes {
        builder.push_slot_always::<WIPOffset<_>>(V(30), passes);
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
    if format_version >= 6 {
        apply_labels(&mut channels, &table.u8s(28));
    }
    if format_version >= 7 {
        apply_display(&mut channels, &table.u8s(29));
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
            unpack_spans(&table.u8s(27), format_version)
        } else {
            Vec::new()
        },
        passes: if format_version >= 9 {
            unpack_passes(&table.u8s(30))
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
        0 => SampleType::I8,
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

/// Reads a little-endian `u16` from `bytes` at `at`, or `None` if out of range.
fn le_u16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}
/// Reads a little-endian `u32` from `bytes` at `at`, or `None` if out of range.
fn le_u32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}
/// Reads a little-endian `i32` from `bytes` at `at`, or `None` if out of range.
fn le_i32(bytes: &[u8], at: usize) -> Option<i32> {
    Some(i32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}
/// Reads a little-endian `i64` from `bytes` at `at`, or `None` if out of range.
fn le_i64(bytes: &[u8], at: usize) -> Option<i64> {
    Some(i64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}
/// Reads a little-endian `u64` from `bytes` at `at`, or `None` if out of range.
fn le_u64(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}
/// Reads a little-endian `f64` from `bytes` at `at`, or `None` if out of range.
fn le_f64(bytes: &[u8], at: usize) -> Option<f64> {
    Some(f64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}
/// Reads one byte from `bytes` at `at`, or `None` if out of range.
fn byte_at(bytes: &[u8], at: usize) -> Option<u8> {
    bytes.get(at).copied()
}

struct Table<'a> {
    buf: &'a [u8],
    loc: usize,
}

impl<'a> Table<'a> {
    fn slot(&self, field: u16) -> Option<usize> {
        let vtable_rel = le_i32(self.buf, self.loc)?;
        let vtable = self.loc.checked_sub(vtable_rel as usize)?;
        let vsize = le_u16(self.buf, vtable)? as usize;
        let off = 4 + field as usize * 2;
        if off + 2 > vsize {
            return None;
        }
        let rel = le_u16(self.buf, vtable.checked_add(off)?)?;
        (rel != 0).then_some(self.loc.checked_add(rel as usize)?)
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
        let len = le_u32(self.buf, at)? as usize;
        let start = at.checked_add(4)?;
        let end = start.checked_add(len)?;
        let bytes = self.buf.get(start..end)?;
        Some(std::str::from_utf8(bytes).ok()?.to_owned())
    }
    fn table(&self, field: u16) -> Option<Table<'a>> {
        let loc = self.indirect(field)?;
        let end = loc.checked_add(4)?;
        if end > self.buf.len() {
            return None;
        }
        Some(Table { buf: self.buf, loc })
    }
    fn u8s(&self, field: u16) -> Vec<u8> {
        let Some(at) = self.indirect(field) else {
            return Vec::new();
        };
        let Some(len) = le_u32(self.buf, at) else {
            return Vec::new();
        };
        let start = match at.checked_add(4) {
            Some(s) => s,
            None => return Vec::new(),
        };
        let end = match start.checked_add(len as usize) {
            Some(e) => e,
            None => return Vec::new(),
        };
        self.buf.get(start..end).unwrap_or_default().to_vec()
    }

    fn indirect(&self, field: u16) -> Option<usize> {
        let at = self.slot(field)?;
        let rel = le_u32(self.buf, at)? as usize;
        at.checked_add(rel)
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
    let Some(count) = le_u32(bytes, 0) else {
        return Vec::new();
    };
    let mut cursor = 4;
    // Each video entry is at least 17 bytes (filename len + index + hashed
    // flag + frame count), so a valid count never exceeds the remaining bytes
    // divided by 17. Bounding the count prevents a mutated u32::MAX from
    // driving an unbounded allocation or iteration; the read loop also
    // bounds-breaks on missing bytes.
    let count = (count as usize).min(bytes.len().saturating_sub(cursor) / 17);
    let mut videos = Vec::with_capacity(count);
    for _ in 0..count {
        let filename = unpack_string(bytes, &mut cursor);
        let Some(index) = le_u32(bytes, cursor) else {
            break;
        };
        cursor += 4;
        let Some(hashed) = byte_at(bytes, cursor) else {
            break;
        };
        cursor += 1;
        let blake3 = if hashed != 0 {
            let Some(bytes_slice) = bytes.get(cursor..cursor + 32) else {
                break;
            };
            let mut hash = [0u8; 32];
            hash.copy_from_slice(bytes_slice);
            cursor += 32;
            Some(hash)
        } else {
            None
        };
        let Some(frame_count) = le_u64(bytes, cursor) else {
            break;
        };
        cursor += 8;
        let presentation_offset_ns = if format_version >= 3 {
            let Some(present) = byte_at(bytes, cursor) else {
                break;
            };
            cursor += 1;
            if present != 0 {
                let Some(offset) = le_i64(bytes, cursor) else {
                    break;
                };
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
    let Some(count) = le_u32(bytes, 0) else {
        return Vec::new();
    };
    let mut cursor = 4;
    // Each lap entry is at least 33 bytes (number + start + end + duration +
    // complete flag), so a valid count never exceeds the remaining bytes
    // divided by 33. Bounds the allocation and iteration against a mutated
    // u32::MAX; the read loop also bounds-breaks on missing bytes.
    let count = (count as usize).min(bytes.len().saturating_sub(cursor) / 33);
    let mut laps = Vec::with_capacity(count);
    for _ in 0..count {
        let Some(number) = le_i64(bytes, cursor) else {
            break;
        };
        let Some(start_ns) = le_u64(bytes, cursor + 8) else {
            break;
        };
        let Some(end_ns) = le_u64(bytes, cursor + 16) else {
            break;
        };
        let Some(duration_ns) = le_u64(bytes, cursor + 24) else {
            break;
        };
        let Some(complete_byte) = byte_at(bytes, cursor + 32) else {
            break;
        };
        let complete = complete_byte != 0;
        cursor += 33;
        let first_video_frame = if format_version >= 3 {
            let Some(present) = byte_at(bytes, cursor) else {
                break;
            };
            cursor += 1;
            if present != 0 {
                let Some(frame) = le_u64(bytes, cursor) else {
                    break;
                };
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
    let Some(count) = le_u32(bytes, 0) else {
        return Vec::new();
    };
    let mut cursor = 4;
    // Each stint is 24 bytes, so a valid count never exceeds the remaining
    // bytes divided by 24. Bounding the count prevents a mutated u32::MAX
    // from driving an unbounded allocation or iteration; the read loop also
    // bounds-breaks on missing bytes.
    let count = (count as usize).min(bytes.len().saturating_sub(cursor) / 24);
    let mut stints = Vec::with_capacity(count);
    for _ in 0..count {
        let Some(driver_id) = le_i64(bytes, cursor) else {
            break;
        };
        let Some(start_ns) = le_u64(bytes, cursor + 8) else {
            break;
        };
        let Some(end_ns) = le_u64(bytes, cursor + 16) else {
            break;
        };
        stints.push(DriverStint {
            driver_id,
            start_ns,
            end_ns,
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
    let Some(len) = le_u32(bytes, *cursor) else {
        return String::new();
    };
    *cursor += 4;
    let end = cursor
        .checked_add(len as usize)
        .unwrap_or(bytes.len())
        .min(bytes.len());
    let value = String::from_utf8_lossy(bytes.get(*cursor..end).unwrap_or_default()).into_owned();
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
    let Some(count) = le_u32(bytes, 0) else {
        return Vec::new();
    };
    let mut cursor = 4;
    // Each channel entry is at least 65 bytes (id + five string lengths +
    // five flag bytes + scale + bias + sample_count + duration + chunk_count),
    // so a valid count never exceeds the remaining bytes divided by 65. Bounds
    // allocation and iteration against a mutated u32::MAX; the read loop also
    // bounds-breaks on missing bytes.
    let count = (count as usize).min(bytes.len().saturating_sub(cursor) / 65);
    let mut channels = Vec::with_capacity(count);
    for _ in 0..count {
        let Some(id) = le_u32(bytes, cursor) else {
            break;
        };
        cursor += 4;
        let name = unpack_string(bytes, &mut cursor);
        let member = unpack_string(bytes, &mut cursor);
        let time_member = unpack_string(bytes, &mut cursor);
        let unit_raw = unpack_string(bytes, &mut cursor);
        let unit_canonical = unpack_string(bytes, &mut cursor);
        let Some(unit_source_byte) = byte_at(bytes, cursor) else {
            break;
        };
        let Some(dimension_byte) = byte_at(bytes, cursor + 1) else {
            break;
        };
        let Some(sample_type_byte) = byte_at(bytes, cursor + 2) else {
            break;
        };
        let Some(uses_step_byte) = byte_at(bytes, cursor + 3) else {
            break;
        };
        let Some(kind_byte) = byte_at(bytes, cursor + 4) else {
            break;
        };
        let unit_source = unit_source_from(unit_source_byte);
        let dimension = dimension_byte;
        let sample_type = sample_type_from(sample_type_byte);
        let uses_step = uses_step_byte != 0;
        let kind = kind_byte;
        cursor += 5;
        let Some(scale) = le_f64(bytes, cursor) else {
            break;
        };
        cursor += 8;
        let Some(bias) = le_f64(bytes, cursor) else {
            break;
        };
        cursor += 8;
        let Some(sample_count) = le_u64(bytes, cursor) else {
            break;
        };
        cursor += 8;
        let Some(duration_ns) = le_u64(bytes, cursor) else {
            break;
        };
        cursor += 8;
        let Some(chunk_count) = le_u32(bytes, cursor) else {
            break;
        };
        cursor += 4;
        // Each chunk is 32 bytes; bound the count by the remaining bytes so a
        // mutated u32::MAX cannot drive unbounded allocation or iteration.
        let chunk_count = (chunk_count as usize).min(bytes.len().saturating_sub(cursor) / 32);
        let mut chunks = Vec::with_capacity(chunk_count);
        for _ in 0..chunk_count {
            let Some(sample_period_ns) = le_u64(bytes, cursor) else {
                break;
            };
            let Some(sample_count_chunk) = le_u64(bytes, cursor + 8) else {
                break;
            };
            let Some(sample_base) = le_u64(bytes, cursor + 16) else {
                break;
            };
            let Some(time_base_ns) = le_u64(bytes, cursor + 24) else {
                break;
            };
            chunks.push(Chunk {
                sample_period_ns,
                sample_count: sample_count_chunk,
                data_ptr: 0,
                sample_base,
                time_base_ns,
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
            labels: Vec::new(),
            display: ChannelDisplay::trace(),
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

fn pack_labels(channels: &[CatalogChannel]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(channels.len() as u32).to_le_bytes());
    for channel in channels {
        out.extend_from_slice(&(channel.labels.len() as u32).to_le_bytes());
        for label in &channel.labels {
            out.extend_from_slice(&label.time_ns.to_le_bytes());
            pack_string(&mut out, &label.text);
        }
    }
    out
}

fn apply_labels(channels: &mut [CatalogChannel], bytes: &[u8]) {
    let Some(count) = le_u32(bytes, 0) else {
        return;
    };
    let count = count as usize;
    let mut cursor = 4;
    for channel in channels.iter_mut().take(count) {
        let Some(n) = le_u32(bytes, cursor) else {
            break;
        };
        cursor += 4;
        // Each label is at least 12 bytes (time_ns + text length), so a valid
        // count never exceeds the remaining bytes divided by 12. Bounds the
        // allocation and iteration against a mutated u32::MAX.
        let n = (n as usize).min(bytes.len().saturating_sub(cursor) / 12);
        let mut labels = Vec::with_capacity(n);
        for _ in 0..n {
            let Some(time_ns) = le_u64(bytes, cursor) else {
                break;
            };
            cursor += 8;
            let text = unpack_string(bytes, &mut cursor);
            labels.push(ChannelLabel { time_ns, text });
        }
        channel.labels = labels;
    }
}

fn plot_code(plot: ChannelPlot) -> u8 {
    match plot {
        ChannelPlot::Trace => 0,
        ChannelPlot::Gauge => 1,
        ChannelPlot::Compass => 2,
    }
}

fn plot_from(code: u8) -> ChannelPlot {
    match code {
        1 => ChannelPlot::Gauge,
        2 => ChannelPlot::Compass,
        _ => ChannelPlot::Trace,
    }
}

fn pack_display(channels: &[CatalogChannel]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(channels.len() as u32).to_le_bytes());
    for channel in channels {
        let display = &channel.display;
        out.push(plot_code(display.plot));
        let mut flags = 0u8;
        if display.scale_min.is_some() {
            flags |= 1;
        }
        if display.scale_max.is_some() {
            flags |= 2;
        }
        if display.decimals.is_some() {
            flags |= 4;
        }
        if !display.format.is_empty() {
            flags |= 8;
        }
        out.push(flags);
        if let Some(min) = display.scale_min {
            out.extend_from_slice(&min.to_le_bytes());
        }
        if let Some(max) = display.scale_max {
            out.extend_from_slice(&max.to_le_bytes());
        }
        if let Some(decimals) = display.decimals {
            out.push(decimals);
        }
        if !display.format.is_empty() {
            pack_string(&mut out, &display.format);
        }
    }
    out
}

fn apply_display(channels: &mut [CatalogChannel], bytes: &[u8]) {
    let Some(count) = le_u32(bytes, 0) else {
        return;
    };
    let count = count as usize;
    let mut cursor = 4;
    for channel in channels.iter_mut().take(count) {
        let Some(plot_byte) = byte_at(bytes, cursor) else {
            break;
        };
        let Some(flags) = byte_at(bytes, cursor + 1) else {
            break;
        };
        let plot = plot_from(plot_byte);
        cursor += 2;
        let mut display = ChannelDisplay {
            plot,
            ..ChannelDisplay::trace()
        };
        if flags & 1 != 0 {
            let Some(min) = le_f64(bytes, cursor) else {
                break;
            };
            display.scale_min = Some(min);
            cursor += 8;
        }
        if flags & 2 != 0 {
            let Some(max) = le_f64(bytes, cursor) else {
                break;
            };
            display.scale_max = Some(max);
            cursor += 8;
        }
        if flags & 4 != 0 {
            let Some(decimals) = byte_at(bytes, cursor) else {
                break;
            };
            display.decimals = Some(decimals);
            cursor += 1;
        }
        if flags & 8 != 0 {
            display.format = unpack_string(bytes, &mut cursor);
        }
        channel.display = display;
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
            match value {
                SpanMetaValue::Text(text) => {
                    out.push(0);
                    pack_string(&mut out, text);
                }
                SpanMetaValue::TimeMs(ms) => {
                    out.push(1);
                    out.extend_from_slice(&ms.to_le_bytes());
                }
            }
        }
    }
    out
}

fn unpack_spans(bytes: &[u8], format_version: u16) -> Vec<Span> {
    let Some(count) = le_u32(bytes, 0) else {
        return Vec::new();
    };
    let mut cursor = 4;
    // Each span is at least 33 bytes (name length + start + end + visible +
    // color + title + subtitle lengths + meta_count), so a valid count never
    // exceeds the remaining bytes divided by 33. Bounds allocation and
    // iteration against a mutated u32::MAX; the read loop also bounds-breaks.
    let count = (count as usize).min(bytes.len().saturating_sub(cursor) / 33);
    let mut spans = Vec::with_capacity(count);
    for _ in 0..count {
        let name = unpack_string(bytes, &mut cursor);
        let Some(start_ns) = le_u64(bytes, cursor) else {
            break;
        };
        let Some(end_ns) = le_u64(bytes, cursor + 8) else {
            break;
        };
        let Some(visible_byte) = byte_at(bytes, cursor + 16) else {
            break;
        };
        let visible = visible_byte != 0;
        cursor += 17;
        let color = unpack_string(bytes, &mut cursor);
        let title = unpack_string(bytes, &mut cursor);
        let subtitle = unpack_string(bytes, &mut cursor);
        let Some(meta_count) = le_u32(bytes, cursor) else {
            break;
        };
        cursor += 4;
        // Each meta entry is at least 8 bytes (key length + value length), so
        // a valid count never exceeds the remaining bytes divided by 8.
        let meta_count = (meta_count as usize).min(bytes.len().saturating_sub(cursor) / 8);
        let mut meta = Vec::with_capacity(meta_count);
        for _ in 0..meta_count {
            let key = unpack_string(bytes, &mut cursor);
            let value = if format_version >= 8 {
                let Some(kind) = byte_at(bytes, cursor) else {
                    break;
                };
                cursor += 1;
                match kind {
                    1 => {
                        let Some(ms) = le_u32(bytes, cursor) else {
                            break;
                        };
                        cursor += 4;
                        SpanMetaValue::TimeMs(ms.min(TIMESPAN_MS_MAX))
                    }
                    _ => SpanMetaValue::from_stored_text(unpack_string(bytes, &mut cursor)),
                }
            } else {
                SpanMetaValue::from_stored_text(unpack_string(bytes, &mut cursor))
            };
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

fn pack_passes(passes: &[AppliedPass]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(passes.len() as u32).to_le_bytes());
    for pass in passes {
        pack_string(&mut out, &pass.name);
        out.extend_from_slice(&pass.version.to_le_bytes());
        out.extend_from_slice(&(pass.params.len() as u32).to_le_bytes());
        for (key, value) in &pass.params {
            pack_string(&mut out, key);
            pack_string(&mut out, value);
        }
        out.extend_from_slice(&(pass.inputs.len() as u32).to_le_bytes());
        for input in &pass.inputs {
            pack_string(&mut out, input);
        }
        out.extend_from_slice(&(pass.outputs.len() as u32).to_le_bytes());
        for output in &pass.outputs {
            pack_string(&mut out, output);
        }
    }
    out
}

fn unpack_passes(bytes: &[u8]) -> Vec<AppliedPass> {
    fn count(bytes: &[u8], cursor: &mut usize) -> Option<usize> {
        let at = *cursor;
        let value = le_u32(bytes, at)? as usize;
        *cursor = at.checked_add(4)?;
        Some(value)
    }
    /// Bounds an untrusted element count by the bytes remaining at `cursor`
    /// divided by the minimum encoded size of one element. A valid file never
    /// has more elements than `remaining / min_record`, so this never truncates
    /// real data but prevents a mutated u32::MAX from driving unbounded
    /// allocation or iteration. The params/inputs/outputs loops call only
    /// `unpack_string`, which returns empty without advancing once the buffer
    /// is exhausted, so without this bound they could spin billions of times.
    fn bounded(count: usize, bytes: &[u8], cursor: usize, min_record: usize) -> usize {
        count.min(bytes.len().saturating_sub(cursor) / min_record)
    }
    let mut cursor = 0;
    let Some(pass_count) = count(bytes, &mut cursor) else {
        return Vec::new();
    };
    // Each pass is at least 20 bytes (name + version + three count fields).
    let pass_count = bounded(pass_count, bytes, cursor, 20);
    let mut passes = Vec::with_capacity(pass_count);
    for _ in 0..pass_count {
        let name = unpack_string(bytes, &mut cursor);
        let Some(version) = count(bytes, &mut cursor) else {
            break;
        };
        let Some(param_count) = count(bytes, &mut cursor) else {
            break;
        };
        // Each param is two strings, at least 8 bytes total.
        let param_count = bounded(param_count, bytes, cursor, 8);
        let mut params = Vec::with_capacity(param_count);
        for _ in 0..param_count {
            let key = unpack_string(bytes, &mut cursor);
            let value = unpack_string(bytes, &mut cursor);
            params.push((key, value));
        }
        let Some(input_count) = count(bytes, &mut cursor) else {
            break;
        };
        // Each input is one string, at least 4 bytes.
        let input_count = bounded(input_count, bytes, cursor, 4);
        let mut inputs = Vec::with_capacity(input_count);
        for _ in 0..input_count {
            inputs.push(unpack_string(bytes, &mut cursor));
        }
        let Some(output_count) = count(bytes, &mut cursor) else {
            break;
        };
        // Each output is one string, at least 4 bytes.
        let output_count = bounded(output_count, bytes, cursor, 4);
        let mut outputs = Vec::with_capacity(output_count);
        for _ in 0..output_count {
            outputs.push(unpack_string(bytes, &mut cursor));
        }
        passes.push(AppliedPass {
            name,
            version: version as u32,
            params,
            inputs,
            outputs,
        });
    }
    passes
}

fn root_table(bytes: &[u8]) -> Result<Table<'_>, ZipError> {
    if bytes.len() < 8 {
        return Err(ZipError("catalog is too small".into()));
    }
    let root = le_u32(bytes, 0)
        .ok_or_else(|| ZipError("catalog root offset is unreadable".into()))?
        as usize;
    let end = root
        .checked_add(4)
        .ok_or_else(|| ZipError("catalog root offset overflows usize".into()))?;
    if end > bytes.len() {
        return Err(ZipError("catalog root is out of range".into()));
    }
    Ok(Table {
        buf: bytes,
        loc: root,
    })
}
