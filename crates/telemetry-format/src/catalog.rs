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

    let laps = builder.create_vector(&pack_laps(&catalog.laps, catalog.format_version)?);
    let channels = builder.create_vector(&pack_channels(&catalog.channels)?);
    let stints = builder.create_vector(&pack_stints(&catalog.driver_stints)?);
    let videos = builder.create_vector(&pack_videos(&catalog.videos, catalog.format_version)?);
    let visibility = (catalog.format_version >= 5)
        .then(|| builder.create_vector(&pack_visibility(&catalog.channels)));
    let spans = if catalog.format_version >= 5 {
        Some(builder.create_vector(&pack_spans(&catalog.spans)?))
    } else {
        None
    };
    let labels = if catalog.format_version >= 6 {
        Some(builder.create_vector(&pack_labels(&catalog.channels)?))
    } else {
        None
    };
    let display = if catalog.format_version >= 7 {
        Some(builder.create_vector(&pack_display(&catalog.channels)?))
    } else {
        None
    };
    let passes = if catalog.format_version >= 9 && !catalog.passes.is_empty() {
        Some(builder.create_vector(&pack_passes(&catalog.passes)?))
    } else {
        None
    };

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
    unpack_laps(&table.u8s(3), table.u16_field(0))
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
    let laps = unpack_laps(&table.u8s(3), format_version)?;
    let mut channels = unpack_channels(&table.u8s(4))?;
    if format_version >= 5 {
        apply_visibility(&mut channels, &table.u8s(26))?;
    }
    if format_version >= 6 {
        apply_labels(&mut channels, &table.u8s(28))?;
    }
    if format_version >= 7 {
        apply_display(&mut channels, &table.u8s(29))?;
    }
    let driver_stints = unpack_stints(&table.u8s(16))?;
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
    let videos = unpack_videos(&table.u8s(20), format_version)?;
    let spans = if format_version >= 5 {
        unpack_spans(&table.u8s(27), format_version)?
    } else {
        Vec::new()
    };
    let passes_bytes = table.u8s(30);
    let passes = if format_version >= 9 && !passes_bytes.is_empty() {
        unpack_passes(&passes_bytes)?
    } else {
        Vec::new()
    };
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
        videos,
        presentation_offset_ns: (table.u32(21) != 0).then(|| i128::from(table.i64_field(22))),
        spans,
        passes,
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

fn pack_videos(videos: &[VideoFileRef], format_version: u16) -> Result<Vec<u8>, ZipError> {
    let mut out = Vec::new();
    pack_count(&mut out, videos.len())?;
    for video in videos {
        pack_string(&mut out, &video.filename)?;
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
    Ok(out)
}

fn unpack_videos(bytes: &[u8], format_version: u16) -> Result<Vec<VideoFileRef>, ZipError> {
    let count = le_u32(bytes, 0).ok_or_else(|| ZipError("truncated video count".into()))? as usize;
    let mut cursor = 4usize;
    // Capacity is bounded for allocation safety; the loop iterates the
    // declared count and errors on the first short read rather than silently
    // truncating.
    let mut videos = Vec::with_capacity(count.min(bytes.len().saturating_sub(cursor) / 17));
    for _ in 0..count {
        let filename = unpack_string(bytes, &mut cursor)?;
        let index =
            le_u32(bytes, cursor).ok_or_else(|| ZipError("truncated video index".into()))?;
        cursor = cursor
            .checked_add(4)
            .ok_or_else(|| ZipError("video cursor overflow".into()))?;
        let hashed =
            byte_at(bytes, cursor).ok_or_else(|| ZipError("truncated video hash flag".into()))?;
        cursor = cursor
            .checked_add(1)
            .ok_or_else(|| ZipError("video cursor overflow".into()))?;
        let blake3 = if hashed != 0 {
            let bytes_slice = bytes
                .get(cursor..cursor + 32)
                .ok_or_else(|| ZipError("truncated video blake3".into()))?;
            let mut hash = [0u8; 32];
            hash.copy_from_slice(bytes_slice);
            cursor = cursor
                .checked_add(32)
                .ok_or_else(|| ZipError("video cursor overflow".into()))?;
            Some(hash)
        } else {
            None
        };
        let frame_count =
            le_u64(bytes, cursor).ok_or_else(|| ZipError("truncated video frame count".into()))?;
        cursor = cursor
            .checked_add(8)
            .ok_or_else(|| ZipError("video cursor overflow".into()))?;
        let presentation_offset_ns = if format_version >= 3 {
            let present = byte_at(bytes, cursor)
                .ok_or_else(|| ZipError("truncated video offset flag".into()))?;
            cursor = cursor
                .checked_add(1)
                .ok_or_else(|| ZipError("video cursor overflow".into()))?;
            if present != 0 {
                let offset = le_i64(bytes, cursor)
                    .ok_or_else(|| ZipError("truncated video offset".into()))?;
                cursor = cursor
                    .checked_add(8)
                    .ok_or_else(|| ZipError("video cursor overflow".into()))?;
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
    Ok(videos)
}

fn pack_laps(laps: &[LapMetadata], format_version: u16) -> Result<Vec<u8>, ZipError> {
    let mut out = Vec::new();
    pack_count(&mut out, laps.len())?;
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
    Ok(out)
}

fn unpack_laps(bytes: &[u8], format_version: u16) -> Result<Vec<LapMetadata>, ZipError> {
    let count = le_u32(bytes, 0).ok_or_else(|| ZipError("truncated lap count".into()))? as usize;
    let mut cursor = 4usize;
    // Each lap entry is at least 33 bytes; the capacity is bounded for
    // allocation safety, but the loop iterates the declared count and errors
    // on the first short read instead of silently truncating.
    let mut laps = Vec::with_capacity(count.min(bytes.len().saturating_sub(cursor) / 33));
    for _ in 0..count {
        let number =
            le_i64(bytes, cursor).ok_or_else(|| ZipError("truncated lap number".into()))?;
        let start_ns =
            le_u64(bytes, cursor + 8).ok_or_else(|| ZipError("truncated lap start".into()))?;
        let end_ns =
            le_u64(bytes, cursor + 16).ok_or_else(|| ZipError("truncated lap end".into()))?;
        let duration_ns =
            le_u64(bytes, cursor + 24).ok_or_else(|| ZipError("truncated lap duration".into()))?;
        let complete_byte =
            byte_at(bytes, cursor + 32).ok_or_else(|| ZipError("truncated lap complete".into()))?;
        let complete = complete_byte != 0;
        cursor = cursor
            .checked_add(33)
            .ok_or_else(|| ZipError("lap cursor overflow".into()))?;
        let first_video_frame = if format_version >= 3 {
            let present = byte_at(bytes, cursor)
                .ok_or_else(|| ZipError("truncated lap video flag".into()))?;
            cursor = cursor
                .checked_add(1)
                .ok_or_else(|| ZipError("lap cursor overflow".into()))?;
            if present != 0 {
                let frame = le_u64(bytes, cursor)
                    .ok_or_else(|| ZipError("truncated lap video frame".into()))?;
                cursor = cursor
                    .checked_add(8)
                    .ok_or_else(|| ZipError("lap cursor overflow".into()))?;
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
    Ok(laps)
}

fn pack_stints(stints: &[DriverStint]) -> Result<Vec<u8>, ZipError> {
    let mut out = Vec::new();
    pack_count(&mut out, stints.len())?;
    for stint in stints {
        out.extend_from_slice(&stint.driver_id.to_le_bytes());
        out.extend_from_slice(&stint.start_ns.to_le_bytes());
        out.extend_from_slice(&stint.end_ns.to_le_bytes());
    }
    Ok(out)
}

fn unpack_stints(bytes: &[u8]) -> Result<Vec<DriverStint>, ZipError> {
    let count = le_u32(bytes, 0).ok_or_else(|| ZipError("truncated stint count".into()))? as usize;
    let mut cursor = 4usize;
    // Each stint is 24 bytes; capacity is bounded for allocation safety, but
    // the loop iterates the declared count and errors on a short read.
    let mut stints = Vec::with_capacity(count.min(bytes.len().saturating_sub(cursor) / 24));
    for _ in 0..count {
        let driver_id =
            le_i64(bytes, cursor).ok_or_else(|| ZipError("truncated stint driver".into()))?;
        let start_ns =
            le_u64(bytes, cursor + 8).ok_or_else(|| ZipError("truncated stint start".into()))?;
        let end_ns =
            le_u64(bytes, cursor + 16).ok_or_else(|| ZipError("truncated stint end".into()))?;
        stints.push(DriverStint {
            driver_id,
            start_ns,
            end_ns,
        });
        cursor = cursor
            .checked_add(24)
            .ok_or_else(|| ZipError("stint cursor overflow".into()))?;
    }
    Ok(stints)
}

fn pack_count(out: &mut Vec<u8>, count: usize) -> Result<(), ZipError> {
    out.extend_from_slice(
        &u32::try_from(count)
            .map_err(|_| ZipError("catalog has too many entries for u32".into()))?
            .to_le_bytes(),
    );
    Ok(())
}

fn pack_string(out: &mut Vec<u8>, value: &str) -> Result<(), ZipError> {
    let bytes = value.as_bytes();
    out.extend_from_slice(
        &u32::try_from(bytes.len())
            .map_err(|_| ZipError("catalog string too long for u32".into()))?
            .to_le_bytes(),
    );
    out.extend_from_slice(bytes);
    Ok(())
}

fn unpack_string(bytes: &[u8], cursor: &mut usize) -> Result<String, ZipError> {
    let len = le_u32(bytes, *cursor).ok_or_else(|| ZipError("truncated string length".into()))?;
    *cursor = cursor
        .checked_add(4)
        .ok_or_else(|| ZipError("string length cursor overflow".into()))?;
    let len_us =
        usize::try_from(len).map_err(|_| ZipError("string length overflows usize".into()))?;
    let end = cursor
        .checked_add(len_us)
        .ok_or_else(|| ZipError("string length overflows cursor".into()))?;
    let slice = bytes
        .get(*cursor..end)
        .ok_or_else(|| ZipError("truncated string body".into()))?;
    let value = std::str::from_utf8(slice)
        .map_err(|_| ZipError("catalog string is not valid UTF-8".into()))?
        .to_owned();
    *cursor = end;
    Ok(value)
}

fn pack_channels(channels: &[CatalogChannel]) -> Result<Vec<u8>, ZipError> {
    let mut out = Vec::new();
    pack_count(&mut out, channels.len())?;
    for channel in channels {
        out.extend_from_slice(&channel.id.to_le_bytes());
        pack_string(&mut out, &channel.name)?;
        pack_string(&mut out, &channel.member)?;
        pack_string(&mut out, &channel.time_member)?;
        pack_string(&mut out, &channel.unit_raw)?;
        pack_string(&mut out, &channel.unit_canonical)?;
        out.push(unit_source_code(channel.unit_source));
        out.push(channel.dimension);
        out.push(channel.sample_type.code() as u8);
        out.push(u8::from(channel.uses_step));
        out.push(channel.kind);
        out.extend_from_slice(&channel.scale.to_le_bytes());
        out.extend_from_slice(&channel.bias.to_le_bytes());
        out.extend_from_slice(&channel.sample_count.to_le_bytes());
        out.extend_from_slice(&channel.duration_ns.to_le_bytes());
        pack_count(&mut out, channel.chunks.len())?;
        for chunk in &channel.chunks {
            out.extend_from_slice(&chunk.sample_period_ns.to_le_bytes());
            out.extend_from_slice(&chunk.sample_count.to_le_bytes());
            out.extend_from_slice(&chunk.sample_base.to_le_bytes());
            out.extend_from_slice(&chunk.time_base_ns.to_le_bytes());
        }
    }
    Ok(out)
}

fn unpack_channels(bytes: &[u8]) -> Result<Vec<CatalogChannel>, ZipError> {
    let count =
        le_u32(bytes, 0).ok_or_else(|| ZipError("truncated channel count".into()))? as usize;
    let mut cursor = 4usize;
    // Each channel entry is at least 65 bytes; capacity is bounded for
    // allocation safety, but the loop iterates the declared count and errors
    // on the first short read instead of silently truncating.
    let mut channels = Vec::with_capacity(count.min(bytes.len().saturating_sub(cursor) / 65));
    for _ in 0..count {
        let id = le_u32(bytes, cursor).ok_or_else(|| ZipError("truncated channel id".into()))?;
        cursor = cursor
            .checked_add(4)
            .ok_or_else(|| ZipError("channel cursor overflow".into()))?;
        let name = unpack_string(bytes, &mut cursor)?;
        let member = unpack_string(bytes, &mut cursor)?;
        let time_member = unpack_string(bytes, &mut cursor)?;
        let unit_raw = unpack_string(bytes, &mut cursor)?;
        let unit_canonical = unpack_string(bytes, &mut cursor)?;
        let unit_source_byte =
            byte_at(bytes, cursor).ok_or_else(|| ZipError("truncated channel flags".into()))?;
        let dimension_byte =
            byte_at(bytes, cursor + 1).ok_or_else(|| ZipError("truncated channel flags".into()))?;
        let sample_type_byte =
            byte_at(bytes, cursor + 2).ok_or_else(|| ZipError("truncated channel flags".into()))?;
        let uses_step_byte =
            byte_at(bytes, cursor + 3).ok_or_else(|| ZipError("truncated channel flags".into()))?;
        let kind_byte =
            byte_at(bytes, cursor + 4).ok_or_else(|| ZipError("truncated channel flags".into()))?;
        let unit_source = unit_source_from(unit_source_byte);
        let dimension = dimension_byte;
        let sample_type = sample_type_from(sample_type_byte);
        let uses_step = uses_step_byte != 0;
        let kind = kind_byte;
        cursor = cursor
            .checked_add(5)
            .ok_or_else(|| ZipError("channel cursor overflow".into()))?;
        let scale =
            le_f64(bytes, cursor).ok_or_else(|| ZipError("truncated channel scale".into()))?;
        cursor = cursor
            .checked_add(8)
            .ok_or_else(|| ZipError("channel cursor overflow".into()))?;
        let bias =
            le_f64(bytes, cursor).ok_or_else(|| ZipError("truncated channel bias".into()))?;
        cursor = cursor
            .checked_add(8)
            .ok_or_else(|| ZipError("channel cursor overflow".into()))?;
        let sample_count = le_u64(bytes, cursor)
            .ok_or_else(|| ZipError("truncated channel sample count".into()))?;
        cursor = cursor
            .checked_add(8)
            .ok_or_else(|| ZipError("channel cursor overflow".into()))?;
        let duration_ns =
            le_u64(bytes, cursor).ok_or_else(|| ZipError("truncated channel duration".into()))?;
        cursor = cursor
            .checked_add(8)
            .ok_or_else(|| ZipError("channel cursor overflow".into()))?;
        let chunk_count =
            le_u32(bytes, cursor).ok_or_else(|| ZipError("truncated chunk count".into()))? as usize;
        cursor = cursor
            .checked_add(4)
            .ok_or_else(|| ZipError("channel cursor overflow".into()))?;
        // Each chunk is 32 bytes; capacity is bounded for allocation safety,
        // but the loop iterates the declared count and errors on a short read.
        let mut chunks =
            Vec::with_capacity(chunk_count.min(bytes.len().saturating_sub(cursor) / 32));
        for _ in 0..chunk_count {
            let sample_period_ns =
                le_u64(bytes, cursor).ok_or_else(|| ZipError("truncated chunk period".into()))?;
            let sample_count_chunk = le_u64(bytes, cursor + 8)
                .ok_or_else(|| ZipError("truncated chunk count".into()))?;
            let sample_base = le_u64(bytes, cursor + 16)
                .ok_or_else(|| ZipError("truncated chunk base".into()))?;
            let time_base_ns = le_u64(bytes, cursor + 24)
                .ok_or_else(|| ZipError("truncated chunk time base".into()))?;
            chunks.push(Chunk {
                sample_period_ns,
                sample_count: sample_count_chunk,
                data_ptr: 0,
                sample_base,
                time_base_ns,
            });
            cursor = cursor
                .checked_add(32)
                .ok_or_else(|| ZipError("chunk cursor overflow".into()))?;
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
    Ok(channels)
}

fn pack_visibility(channels: &[CatalogChannel]) -> Vec<u8> {
    channels
        .iter()
        .map(|channel| u8::from(channel.visible))
        .collect()
}

fn apply_visibility(channels: &mut [CatalogChannel], bytes: &[u8]) -> Result<(), ZipError> {
    if bytes.len() < channels.len() {
        return Err(ZipError("truncated visibility vector".into()));
    }
    for (channel, flag) in channels.iter_mut().zip(bytes) {
        channel.visible = *flag != 0;
    }
    Ok(())
}

fn pack_labels(channels: &[CatalogChannel]) -> Result<Vec<u8>, ZipError> {
    let mut out = Vec::new();
    pack_count(&mut out, channels.len())?;
    for channel in channels {
        pack_count(&mut out, channel.labels.len())?;
        for label in &channel.labels {
            out.extend_from_slice(&label.time_ns.to_le_bytes());
            pack_string(&mut out, &label.text)?;
        }
    }
    Ok(out)
}

fn apply_labels(channels: &mut [CatalogChannel], bytes: &[u8]) -> Result<(), ZipError> {
    let count = le_u32(bytes, 0).ok_or_else(|| ZipError("truncated label count".into()))? as usize;
    let mut cursor = 4usize;
    for channel in channels.iter_mut().take(count) {
        let n = le_u32(bytes, cursor)
            .ok_or_else(|| ZipError("truncated label vector count".into()))?
            as usize;
        cursor = cursor
            .checked_add(4)
            .ok_or_else(|| ZipError("label cursor overflow".into()))?;
        // Each label is at least 12 bytes; capacity is bounded for allocation
        // safety, but the loop iterates the declared count and errors on a
        // short read.
        let mut labels = Vec::with_capacity(n.min(bytes.len().saturating_sub(cursor) / 12));
        for _ in 0..n {
            let time_ns =
                le_u64(bytes, cursor).ok_or_else(|| ZipError("truncated label time".into()))?;
            cursor = cursor
                .checked_add(8)
                .ok_or_else(|| ZipError("label cursor overflow".into()))?;
            let text = unpack_string(bytes, &mut cursor)?;
            labels.push(ChannelLabel { time_ns, text });
        }
        channel.labels = labels;
    }
    Ok(())
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

fn pack_display(channels: &[CatalogChannel]) -> Result<Vec<u8>, ZipError> {
    let mut out = Vec::new();
    pack_count(&mut out, channels.len())?;
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
            pack_string(&mut out, &display.format)?;
        }
    }
    Ok(out)
}

fn apply_display(channels: &mut [CatalogChannel], bytes: &[u8]) -> Result<(), ZipError> {
    let count =
        le_u32(bytes, 0).ok_or_else(|| ZipError("truncated display count".into()))? as usize;
    let mut cursor = 4usize;
    for channel in channels.iter_mut().take(count) {
        let plot_byte =
            byte_at(bytes, cursor).ok_or_else(|| ZipError("truncated display plot".into()))?;
        let flags =
            byte_at(bytes, cursor + 1).ok_or_else(|| ZipError("truncated display flags".into()))?;
        let plot = plot_from(plot_byte);
        cursor = cursor
            .checked_add(2)
            .ok_or_else(|| ZipError("display cursor overflow".into()))?;
        let mut display = ChannelDisplay {
            plot,
            ..ChannelDisplay::trace()
        };
        if flags & 1 != 0 {
            let min = le_f64(bytes, cursor)
                .ok_or_else(|| ZipError("truncated display scale min".into()))?;
            display.scale_min = Some(min);
            cursor = cursor
                .checked_add(8)
                .ok_or_else(|| ZipError("display cursor overflow".into()))?;
        }
        if flags & 2 != 0 {
            let max = le_f64(bytes, cursor)
                .ok_or_else(|| ZipError("truncated display scale max".into()))?;
            display.scale_max = Some(max);
            cursor = cursor
                .checked_add(8)
                .ok_or_else(|| ZipError("display cursor overflow".into()))?;
        }
        if flags & 4 != 0 {
            let decimals = byte_at(bytes, cursor)
                .ok_or_else(|| ZipError("truncated display decimals".into()))?;
            display.decimals = Some(decimals);
            cursor = cursor
                .checked_add(1)
                .ok_or_else(|| ZipError("display cursor overflow".into()))?;
        }
        if flags & 8 != 0 {
            display.format = unpack_string(bytes, &mut cursor)?;
        }
        channel.display = display;
    }
    Ok(())
}

fn pack_spans(spans: &[Span]) -> Result<Vec<u8>, ZipError> {
    let mut out = Vec::new();
    pack_count(&mut out, spans.len())?;
    for span in spans {
        pack_string(&mut out, &span.name)?;
        out.extend_from_slice(&span.start_ns.to_le_bytes());
        out.extend_from_slice(&span.end_ns.to_le_bytes());
        out.push(u8::from(span.visible));
        pack_string(&mut out, &span.color)?;
        pack_string(&mut out, &span.primary.title)?;
        pack_string(&mut out, &span.primary.subtitle)?;
        pack_count(&mut out, span.meta.len())?;
        for (key, value) in &span.meta {
            pack_string(&mut out, key)?;
            match value {
                SpanMetaValue::Text(text) => {
                    out.push(0);
                    pack_string(&mut out, text)?;
                }
                SpanMetaValue::TimeMs(ms) => {
                    out.push(1);
                    out.extend_from_slice(&ms.to_le_bytes());
                }
            }
        }
    }
    Ok(out)
}

fn unpack_spans(bytes: &[u8], format_version: u16) -> Result<Vec<Span>, ZipError> {
    let count = le_u32(bytes, 0).ok_or_else(|| ZipError("truncated span count".into()))? as usize;
    let mut cursor = 4usize;
    // Each span is at least 33 bytes; capacity is bounded for allocation
    // safety, but the loop iterates the declared count and errors on the
    // first short read instead of silently truncating.
    let mut spans = Vec::with_capacity(count.min(bytes.len().saturating_sub(cursor) / 33));
    for _ in 0..count {
        let name = unpack_string(bytes, &mut cursor)?;
        let start_ns =
            le_u64(bytes, cursor).ok_or_else(|| ZipError("truncated span start".into()))?;
        let end_ns =
            le_u64(bytes, cursor + 8).ok_or_else(|| ZipError("truncated span end".into()))?;
        let visible_byte =
            byte_at(bytes, cursor + 16).ok_or_else(|| ZipError("truncated span visible".into()))?;
        let visible = visible_byte != 0;
        cursor = cursor
            .checked_add(17)
            .ok_or_else(|| ZipError("span cursor overflow".into()))?;
        let color = unpack_string(bytes, &mut cursor)?;
        let title = unpack_string(bytes, &mut cursor)?;
        let subtitle = unpack_string(bytes, &mut cursor)?;
        let meta_count = le_u32(bytes, cursor)
            .ok_or_else(|| ZipError("truncated span meta count".into()))?
            as usize;
        cursor = cursor
            .checked_add(4)
            .ok_or_else(|| ZipError("span cursor overflow".into()))?;
        // Each meta entry is at least 8 bytes; capacity is bounded for
        // allocation safety, but the loop iterates the declared count and
        // errors on a short read.
        let mut meta = Vec::with_capacity(meta_count.min(bytes.len().saturating_sub(cursor) / 8));
        for _ in 0..meta_count {
            let key = unpack_string(bytes, &mut cursor)?;
            let value = if format_version >= 8 {
                let kind = byte_at(bytes, cursor)
                    .ok_or_else(|| ZipError("truncated span meta kind".into()))?;
                cursor = cursor
                    .checked_add(1)
                    .ok_or_else(|| ZipError("span cursor overflow".into()))?;
                match kind {
                    1 => {
                        let ms = le_u32(bytes, cursor)
                            .ok_or_else(|| ZipError("truncated span meta ms".into()))?;
                        cursor = cursor
                            .checked_add(4)
                            .ok_or_else(|| ZipError("span cursor overflow".into()))?;
                        SpanMetaValue::TimeMs(ms.min(TIMESPAN_MS_MAX))
                    }
                    _ => SpanMetaValue::from_stored_text(unpack_string(bytes, &mut cursor)?),
                }
            } else {
                SpanMetaValue::from_stored_text(unpack_string(bytes, &mut cursor)?)
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
    Ok(spans)
}

fn pack_passes(passes: &[AppliedPass]) -> Result<Vec<u8>, ZipError> {
    let mut out = Vec::new();
    pack_count(&mut out, passes.len())?;
    for pass in passes {
        pack_string(&mut out, &pass.name)?;
        out.extend_from_slice(&pass.version.to_le_bytes());
        pack_count(&mut out, pass.params.len())?;
        for (key, value) in &pass.params {
            pack_string(&mut out, key)?;
            pack_string(&mut out, value)?;
        }
        pack_count(&mut out, pass.inputs.len())?;
        for input in &pass.inputs {
            pack_string(&mut out, input)?;
        }
        pack_count(&mut out, pass.outputs.len())?;
        for output in &pass.outputs {
            pack_string(&mut out, output)?;
        }
    }
    Ok(out)
}

fn unpack_passes(bytes: &[u8]) -> Result<Vec<AppliedPass>, ZipError> {
    fn count(bytes: &[u8], cursor: &mut usize) -> Result<usize, ZipError> {
        let at = *cursor;
        let value =
            le_u32(bytes, at).ok_or_else(|| ZipError("truncated pass count".into()))? as usize;
        *cursor = at
            .checked_add(4)
            .ok_or_else(|| ZipError("pass cursor overflow".into()))?;
        Ok(value)
    }
    /// Bounds a `with_capacity` hint by the bytes remaining at `cursor`
    /// divided by the minimum encoded size of one element, for allocation
    /// safety only. The loops iterate the declared count and error on a short
    /// read, so this never silently truncates real data.
    fn bounded(cap: usize, bytes: &[u8], cursor: usize, min_record: usize) -> usize {
        cap.min(bytes.len().saturating_sub(cursor) / min_record)
    }
    let mut cursor = 0usize;
    let pass_count = count(bytes, &mut cursor)?;
    let mut passes = Vec::with_capacity(bounded(pass_count, bytes, cursor, 20));
    for _ in 0..pass_count {
        let name = unpack_string(bytes, &mut cursor)?;
        let version = count(bytes, &mut cursor)?;
        let param_count = count(bytes, &mut cursor)?;
        let mut params = Vec::with_capacity(bounded(param_count, bytes, cursor, 8));
        for _ in 0..param_count {
            let key = unpack_string(bytes, &mut cursor)?;
            let value = unpack_string(bytes, &mut cursor)?;
            params.push((key, value));
        }
        let input_count = count(bytes, &mut cursor)?;
        let mut inputs = Vec::with_capacity(bounded(input_count, bytes, cursor, 4));
        for _ in 0..input_count {
            inputs.push(unpack_string(bytes, &mut cursor)?);
        }
        let output_count = count(bytes, &mut cursor)?;
        let mut outputs = Vec::with_capacity(bounded(output_count, bytes, cursor, 4));
        for _ in 0..output_count {
            outputs.push(unpack_string(bytes, &mut cursor)?);
        }
        passes.push(AppliedPass {
            name,
            version: u32::try_from(version)
                .map_err(|_| ZipError("pass version overflows u32".into()))?,
            params,
            inputs,
            outputs,
        });
    }
    Ok(passes)
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

#[cfg(test)]
mod tests {
    use super::*;
    use motorsport_telemetry_core::{ChannelDisplay, Chunk, LapMetadata, UnitSource};

    fn channel_with_labels(count: usize) -> Vec<CatalogChannel> {
        (0..count)
            .map(|index| CatalogChannel {
                id: index as u32,
                name: format!("ch{index}"),
                member: format!("channels/{index:04}.bin"),
                time_member: String::new(),
                unit_raw: String::new(),
                unit_canonical: String::new(),
                unit_source: UnitSource::Unknown,
                dimension: 0,
                sample_type: SampleType::F32,
                scale: 1.0,
                bias: 0.0,
                uses_step: false,
                sample_count: 0,
                duration_ns: 0,
                kind: 0,
                visible: true,
                labels: Vec::new(),
                display: ChannelDisplay::trace(),
                chunks: vec![Chunk {
                    sample_period_ns: 1_000_000,
                    sample_count: 0,
                    data_ptr: 0,
                    sample_base: 0,
                    time_base_ns: 0,
                }],
            })
            .collect()
    }

    #[test]
    fn pack_rejects_too_many_entries() {
        // u32::MAX + 1 channels overflow the u32 entry count. The packer must
        // report an error rather than truncating the count.
        let channels = channel_with_labels(0);
        // Directly exercise the count helper with an unreachable usize: synthesize
        // via pack_channels by constructing a slice whose len cannot be u32. We
        // cannot allocate u32::MAX+1 entries, so assert the helper itself.
        let mut out = Vec::new();
        let err = pack_count(&mut out, u32::MAX as usize + 1).unwrap_err();
        assert!(err.0.contains("too many entries"));
        // A normal count packs fine.
        let mut out = Vec::new();
        pack_count(&mut out, channels.len()).unwrap();
        assert_eq!(out, (0u32).to_le_bytes());
    }

    #[test]
    fn unpack_laps_errors_on_truncated_body() {
        let bytes = 1u32.to_le_bytes();
        let err = unpack_laps(&bytes, FORMAT_VERSION).unwrap_err();
        assert!(err.0.contains("truncated"));
    }

    #[test]
    fn unpack_channels_errors_on_truncated_flags() {
        // channel count = 1, id present, then five empty strings, then no
        // flag bytes left.
        let mut bytes = 1u32.to_le_bytes().to_vec();
        bytes.extend_from_slice(&1u32.to_le_bytes()); // id
        for _ in 0..5 {
            bytes.extend_from_slice(&0u32.to_le_bytes()); // empty string lengths
        }
        let err = unpack_channels(&bytes).unwrap_err();
        assert!(err.0.contains("truncated"));
    }

    #[test]
    fn unpack_string_rejects_invalid_utf8() {
        // length 2, then two continuation bytes that are not valid UTF-8.
        let mut bytes = 2u32.to_le_bytes().to_vec();
        bytes.extend_from_slice(&[0xC3, 0x28]);
        let mut cursor = 0usize;
        let err = unpack_string(&bytes, &mut cursor).unwrap_err();
        assert!(err.0.contains("UTF-8"));
    }

    #[test]
    fn pack_unpack_laps_round_trip() {
        let laps = vec![LapMetadata {
            number: 1,
            start_ns: 0,
            end_ns: 40_000_000,
            duration_ns: 40_000_000,
            complete: true,
            first_video_frame: Some(3),
        }];
        let packed = pack_laps(&laps, FORMAT_VERSION).unwrap();
        let back = unpack_laps(&packed, FORMAT_VERSION).unwrap();
        assert_eq!(back, laps);
    }
}
