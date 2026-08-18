#![doc = include_str!("../README.md")]
#![deny(missing_docs)]

#[cfg(not(target_os = "emscripten"))]
use memmap2::Mmap;
#[cfg(not(target_os = "emscripten"))]
use std::fs::File;
#[cfg(not(target_os = "emscripten"))]
use std::path::Path;
use thiserror::Error;

pub mod units;

use motorsport_telemetry_core::{
    Channel, Chunk, Diagnostic, Diagnostics, SampleType, TelemetrySource, UnitSource,
};
use units::DefLayout;

/// Duration of one native PDS clock tick in nanoseconds.
pub const TICK_NS: u64 = 100;

#[cfg(not(target_os = "emscripten"))]
/// Opens a PDS file and derives its format-neutral metadata summary.
pub fn read_metadata(
    path: impl AsRef<Path>,
) -> Result<motorsport_telemetry_core::FileMetadata, CosworthError> {
    CosworthFile::open(path).map(|file| motorsport_telemetry_core::read_source_metadata(&file))
}

/// Derives format-neutral metadata from an owned PDS byte buffer.
pub fn read_metadata_from_bytes(
    path: impl Into<String>,
    data: Vec<u8>,
) -> Result<motorsport_telemetry_core::FileMetadata, CosworthError> {
    CosworthFile::from_bytes(path, data)
        .map(|file| motorsport_telemetry_core::read_source_metadata(&file))
}
const MARKER: u64 = 0x7c72;

/// Errors returned while opening or parsing Pi/Cosworth PDS telemetry.
#[derive(Debug, Error)]
pub enum CosworthError {
    /// The PDS file could not be opened or memory-mapped.
    #[error("I/O error for {path}: {source}")]
    Io {
        /// Path that was being opened.
        path: String,
        /// Underlying filesystem error.
        source: std::io::Error,
    },
    /// The PDS structure is malformed or unsupported.
    #[error("invalid PDS file {path}: {message}")]
    Invalid {
        /// Path or caller-supplied input name.
        path: String,
        /// Specific validation failure.
        message: String,
    },
}

#[derive(Debug)]
enum Storage {
    #[cfg(not(target_os = "emscripten"))]
    Mapped(Mmap),
    Owned(Box<[u8]>),
}
impl std::ops::Deref for Storage {
    type Target = [u8];
    fn deref(&self) -> &Self::Target {
        match self {
            #[cfg(not(target_os = "emscripten"))]
            Self::Mapped(value) => value,
            Self::Owned(value) => value,
        }
    }
}

/// An opened Pi/Cosworth PDS telemetry source.
#[derive(Debug)]
pub struct CosworthFile {
    /// Source path or caller-supplied name.
    pub path: String,
    /// Source-exact telemetry channel metadata.
    pub channels: Vec<Channel>,
    /// Problems recovered from while parsing the source.
    pub diagnostics: Vec<Diagnostic>,
    data: Storage,
}

#[derive(Clone, Copy)]
struct DirEntry {
    offset: u64,
    count: u32,
    class_b: u32,
    next_count: u32,
}

#[derive(Clone, Copy)]
struct Layout {
    defs_offset: usize,
    defs_count: usize,
    chunk_offset: usize,
    next_offset: usize,
    chunk_count: usize,
}

#[derive(Clone)]
struct RawChannelDef<'a> {
    id: u32,
    name: String,
    unit: String,
    unit_source: UnitSource,
    record: &'a [u8],
}

#[derive(Clone)]
struct ChannelDef {
    id: u32,
    name: String,
    unit: String,
    unit_source: UnitSource,
    sample_type: SampleType,
}

#[derive(Clone, Copy)]
struct RawChunk {
    channel_id: u32,
    sample_period_ticks: u32,
    sample_count: u64,
    data_ptr: u64,
}

enum ChannelDispatch {
    Dense { first: u32, indexes: Box<[usize]> },
    Sparse(Box<[(u32, usize)]>),
}

impl ChannelDispatch {
    fn new(channels: &[Channel]) -> Self {
        let first = channels.iter().map(|channel| channel.id).min().unwrap_or(0);
        let last = channels
            .iter()
            .map(|channel| channel.id)
            .max()
            .unwrap_or(first);
        let span = u64::from(last) - u64::from(first) + 1;
        if span <= channels.len().saturating_mul(4) as u64 {
            let mut indexes = vec![usize::MAX; span as usize];
            for (index, channel) in channels.iter().enumerate() {
                indexes[(channel.id - first) as usize] = index;
            }
            Self::Dense {
                first,
                indexes: indexes.into_boxed_slice(),
            }
        } else {
            let mut indexes = channels
                .iter()
                .enumerate()
                .map(|(index, channel)| (channel.id, index))
                .collect::<Vec<_>>();
            indexes.sort_unstable_by_key(|entry| entry.0);
            Self::Sparse(indexes.into_boxed_slice())
        }
    }

    #[inline]
    fn get(&self, channel_id: u32) -> Option<usize> {
        match self {
            Self::Dense { first, indexes } => channel_id
                .checked_sub(*first)
                .and_then(|index| indexes.get(index as usize))
                .copied()
                .filter(|index| *index != usize::MAX),
            Self::Sparse(indexes) => indexes
                .binary_search_by_key(&channel_id, |entry| entry.0)
                .ok()
                .map(|index| indexes[index].1),
        }
    }
}

fn invalid(path: &str, message: impl Into<String>) -> CosworthError {
    CosworthError::Invalid {
        path: path.to_owned(),
        message: message.into(),
    }
}

fn u16le(data: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        data.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

fn u32le(data: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        data.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn u64le(data: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        data.get(offset..offset + 8)?.try_into().ok()?,
    ))
}

fn utf16le(data: &[u8], offset: usize, max_bytes: usize) -> String {
    let end = offset.saturating_add(max_bytes).min(data.len());
    let units = (offset..end.saturating_sub(1))
        .step_by(2)
        .map(|pos| u16le(data, pos).unwrap_or(0))
        .take_while(|code| *code != 0);
    let mut text = String::with_capacity(end.saturating_sub(offset) / 2);
    for decoded in char::decode_utf16(units) {
        text.push(decoded.unwrap_or(char::REPLACEMENT_CHARACTER));
    }
    if text.trim().len() == text.len() {
        text
    } else {
        text.trim().to_owned()
    }
}

fn entry_at(data: &[u8], base: usize) -> Option<DirEntry> {
    if base + 32 > data.len() {
        return None;
    }
    let lo = u32le(data, base)? as u64;
    let hi = u32le(data, base + 4)? as u64;
    Some(DirEntry {
        offset: lo | hi << 32,
        count: u32le(data, base + 8)?,
        class_b: u32le(data, base + 0x14)?,
        next_count: u32le(data, base + 0x18)?,
    })
}

fn read_entries_at(data: &[u8], start: usize) -> Vec<DirEntry> {
    (0..20)
        .filter_map(|index| entry_at(data, start + index * 32))
        .collect()
}

fn find_directory(data: &[u8]) -> Vec<DirEntry> {
    let mut best_start = 0x80;
    let mut best_score = i32::MIN;
    for start in [0x80, 0x78, 0x70, 0x68, 0x60, 0x58, 0x50, 0x48, 0x40] {
        let score = (0..20)
            .filter_map(|index| entry_at(data, start + index * 32))
            .map(|e| {
                if e.class_b <= 3 && e.offset > 0 && e.offset < data.len() as u64 {
                    2
                } else if e.class_b <= 3 {
                    1
                } else {
                    0
                }
            })
            .sum();
        if score > best_score {
            best_start = start;
            best_score = score;
        }
    }
    read_entries_at(data, best_start)
}

fn find_layout(
    entries: &[DirEntry],
    file_size: usize,
    path: &str,
) -> Result<Layout, CosworthError> {
    for window in entries.windows(3) {
        let defs = window[0];
        let chunks = window[1];
        let next = window[2];
        if !(defs.offset < chunks.offset
            && chunks.offset < next.offset
            && next.offset <= file_size as u64)
            || defs.class_b != 1
            || defs.count == 0
        {
            continue;
        }
        let span = (next.offset - chunks.offset) as usize;
        let plausible = |count: u32| -> Option<usize> {
            if count == 0 || span % count as usize != 0 {
                return None;
            }
            let width = span / count as usize;
            (48..=512).contains(&width).then_some(count as usize)
        };
        let chunk_count = plausible(defs.next_count).or_else(|| plausible(chunks.count));
        if let Some(chunk_count) = chunk_count {
            return Ok(Layout {
                defs_offset: defs.offset as usize,
                defs_count: defs.count as usize,
                chunk_offset: chunks.offset as usize,
                next_offset: next.offset as usize,
                chunk_count,
            });
        }
    }
    Err(invalid(path, "no valid definitions/chunk layout found"))
}

fn marker_defs<'a>(data: &'a [u8], layout: Layout) -> Vec<RawChannelDef<'a>> {
    let scan_end = layout
        .chunk_offset
        .min(layout.defs_offset.saturating_add(8192))
        .min(data.len());
    let marker_pos = (layout.defs_offset..scan_end.saturating_sub(7))
        .step_by(2)
        .find(|&pos| u64le(data, pos) == Some(MARKER));
    let Some(first) = marker_pos else {
        return Vec::new();
    };
    let probe_end = layout.chunk_offset.min(first + 1024).min(data.len());
    let record_size = ((first + 16)..probe_end.saturating_sub(7))
        .step_by(2)
        .find(|&pos| u64le(data, pos) == Some(MARKER))
        .map(|pos| pos - first)
        .unwrap_or(304);
    if record_size < 0xdc {
        return Vec::new();
    }

    // Pass 1: collect records so the unit and sample-type fields can both be
    // detected from the file. Their offsets vary with logger firmware.
    let mut raw: Vec<(u32, String, &[u8])> = Vec::new();
    let mut pos = first;
    while pos + 0xdc <= layout.chunk_offset.min(data.len()) {
        if u64le(data, pos) == Some(MARKER) {
            let id = u32le(data, pos + 8).unwrap_or(0);
            let name = utf16le(data, pos + 0x10, 112);
            if id != 0 && !name.is_empty() {
                let end = (pos + record_size).min(data.len());
                raw.push((id, name, &data[pos..end]));
            }
        }
        pos += record_size;
    }

    let records: Vec<&[u8]> = raw.iter().map(|(_, _, record)| *record).collect();
    let def_layout = DefLayout::detect(&records, 0x10);
    raw.into_iter()
        .map(|(id, name, record)| {
            let (unit, unit_source) = def_layout.resolve(record);
            RawChannelDef {
                id,
                name,
                unit,
                unit_source,
                record,
            }
        })
        .collect()
}

fn markerless_defs(data: &[u8], layout: Layout) -> Vec<RawChannelDef<'_>> {
    if layout.defs_count == 0 {
        return Vec::new();
    }
    let span = layout.chunk_offset.saturating_sub(layout.defs_offset);
    let record_size = span / layout.defs_count;
    if !(100..=1024).contains(&record_size) {
        return Vec::new();
    }
    let mut raw: Vec<(u32, String, &[u8])> = Vec::new();
    for i in 0..layout.defs_count {
        let pos = layout.defs_offset + i * record_size;
        if pos + 16 > data.len() {
            break;
        }
        let id = u32le(data, pos).unwrap_or(0);
        let name = utf16le(data, pos + 8, 112);
        if name.is_empty() {
            continue;
        }
        let end = (pos + record_size).min(data.len());
        raw.push((id, name, &data[pos..end]));
    }

    // Toolbox exports can drop the human-readable unit but retain its quantity
    // code. Resolving it dynamically is already the established pattern; the
    // sample-type detector below applies the same rule to type codes.
    let records: Vec<&[u8]> = raw.iter().map(|(_, _, record)| *record).collect();
    let def_layout = DefLayout::detect(&records, 8);
    raw.into_iter()
        .map(|(id, name, record)| {
            let (unit, unit_source) = def_layout.resolve(record);
            RawChannelDef {
                id,
                name,
                unit,
                unit_source,
                record,
            }
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct TypeLayoutScore {
    past_eof: usize,
    continuity_error: u64,
    unknown_codes: usize,
    offset: usize,
}
#[derive(Clone, Copy)]
struct TypeChunk {
    def_index: usize,
    data_ptr: u64,
    sample_count: u64,
}

fn type_layout_score(
    defs: &[RawChannelDef<'_>],
    chunks: &[TypeChunk],
    data_len: usize,
    offset: usize,
) -> Option<TypeLayoutScore> {
    let mut has_type_code = false;
    let mut unknown_codes = 0usize;
    for def in defs {
        let code = u32le(def.record, offset)?;
        if code > u8::MAX.into() {
            return None;
        }
        unknown_codes += usize::from(code > 7);
        has_type_code = true;
    }
    if !has_type_code || chunks.is_empty() {
        return None;
    }

    // `chunks` is sorted once by data pointer in `resolve_sample_types`.
    // Candidate scoring is the hot path (up to 256 offsets over 1400
    // channels); it must not allocate or sort per candidate.
    let mut past_eof = 0usize;
    let mut continuity_error = 0u64;
    let mut previous_end = None::<u64>;
    for chunk in chunks {
        let code = u32le(defs[chunk.def_index].record, offset)?;
        let width = SampleType::from_pds_code(code).byte_width() as u64;
        let end = chunk
            .sample_count
            .checked_mul(width)
            .and_then(|bytes| chunk.data_ptr.checked_add(bytes));
        let Some(end) = end else {
            past_eof += 1;
            continue;
        };
        if end > data_len as u64 {
            past_eof += 1;
        }
        if let Some(previous) = previous_end {
            continuity_error = continuity_error.saturating_add(chunk.data_ptr.abs_diff(previous));
            previous_end = Some(previous.max(end));
        } else {
            previous_end = Some(end);
        }
    }
    Some(TypeLayoutScore {
        past_eof,
        continuity_error,
        unknown_codes,
        offset,
    })
}

fn resolve_sample_types(
    defs: Vec<RawChannelDef<'_>>,
    chunks: &[RawChunk],
    data_len: usize,
    is_export: bool,
    path: &str,
    diagnostics: &mut Diagnostics,
) -> Result<Vec<ChannelDef>, CosworthError> {
    let record_len = defs.iter().map(|def| def.record.len()).min().unwrap_or(0);
    let mut indexes: Vec<(u32, usize)> = defs
        .iter()
        .enumerate()
        .map(|(index, def)| (def.id, index))
        .collect();
    indexes.sort_unstable_by_key(|(id, _)| *id);
    let mut type_chunks = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        let Ok(at) = indexes.binary_search_by_key(&chunk.channel_id, |(id, _)| *id) else {
            continue;
        };
        type_chunks.push(TypeChunk {
            def_index: indexes[at].1,
            data_ptr: chunk.data_ptr,
            sample_count: chunk.sample_count,
        });
    }
    type_chunks.sort_unstable_by_key(|chunk| chunk.data_ptr);

    // Compact Toolbox exports define one representation for the whole export:
    // float64. Their small records contain unrelated low-valued fields that
    // can look like type codes, so applying native-layout detection to them is
    // both unnecessary and unsafe.
    let best = (!is_export)
        .then(|| {
            (0..record_len.saturating_sub(3))
                .step_by(4)
                .filter_map(|offset| type_layout_score(&defs, &type_chunks, data_len, offset))
                .min()
        })
        .flatten();

    let codes = if let Some(score) = best {
        if score.past_eof > 0 {
            diagnostics.warning(
                "pds.sample_data_truncated",
                format!(
                    "{} chunk payloads extend past the {}-byte file using the type field at \
                     offset 0x{:x}; their sample counts will be clamped",
                    score.past_eof, data_len, score.offset
                ),
            );
        }
        if score.unknown_codes > 0 {
            diagnostics.warning(
                "pds.type_code_unrecognized",
                format!(
                    "{} channel definitions carry unsupported sample type codes at offset \
                     0x{:x}; those channels are decoded as float32",
                    score.unknown_codes, score.offset
                ),
            );
        }
        defs.iter()
            .map(|def| u32le(def.record, score.offset).unwrap_or(0))
            .collect::<Vec<_>>()
    } else if is_export {
        vec![7; defs.len()]
    } else {
        return Err(invalid(
            path,
            "no sample-type field agrees with the channel definition and chunk layout",
        ));
    };

    Ok(defs
        .into_iter()
        .zip(codes)
        .map(|(def, code)| ChannelDef {
            id: def.id,
            name: def.name,
            unit: def.unit,
            unit_source: def.unit_source,
            sample_type: SampleType::from_pds_code(code),
        })
        .collect())
}

fn parse_chunks(data: &[u8], layout: Layout, is_export: bool) -> Vec<RawChunk> {
    let span = layout.next_offset - layout.chunk_offset;
    let width = span / layout.chunk_count;
    if width < 0x3c {
        return Vec::new();
    }

    let aligned = (0..4096.min(span))
        .step_by(4)
        .map(|n| layout.chunk_offset + n)
        .find(|&pos| {
            pos + 0x3c <= data.len()
                && u32le(data, pos + 4).unwrap_or(0) > 0
                && u32le(data, pos + 4) == u32le(data, pos + 8)
                && u32le(data, pos + 0x1c).unwrap_or(0) > 0
        });
    if let Some(mut start) = aligned {
        if is_export {
            while start >= layout.chunk_offset + width {
                let p = start - width;
                if u32le(data, p + 4) == u32le(data, p + 8)
                    && u32le(data, p + 0x1c).unwrap_or(0) > 0
                {
                    start = p;
                } else {
                    break;
                }
            }
        }
        let count = layout.chunk_count.min((layout.next_offset - start) / width);
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            let pos = start + i * width;
            let channel_id = u32le(data, pos + 4).unwrap_or(0);
            let duplicate = u32le(data, pos + 8).unwrap_or(u32::MAX);
            let period = u32le(data, pos + 0x18).unwrap_or(0);
            let count = u32le(data, pos + 0x1c).unwrap_or(0) as u64;
            let ptr = u32le(data, pos + 0x38).unwrap_or(u32::MAX) as u64;
            if channel_id == duplicate
                && (channel_id > 0 || is_export)
                && period > 0
                && count > 0
                && ptr < data.len() as u64
            {
                out.push(RawChunk {
                    channel_id,
                    sample_period_ticks: period,
                    sample_count: count,
                    data_ptr: ptr,
                });
            }
        }
        if !out.is_empty() {
            return out;
        }
    }

    // Compact Pi Toolbox exports omit the duplicate channel id. Their chunk
    // directory is channel-interleaved in definition order.
    let mut out = Vec::new();
    for i in 0..layout.chunk_count {
        let pos = layout.chunk_offset + i * width;
        if pos + width > data.len() {
            break;
        }
        let period = u32le(data, pos + 0x18).unwrap_or(0);
        let count = u32le(data, pos + 0x1c).unwrap_or(0) as u64;
        let ptr = u32le(data, pos + 0x38).unwrap_or(0) as u64;
        if period > 0 && count > 0 && ptr > 0 && ptr < data.len() as u64 {
            out.push(RawChunk {
                channel_id: (i % layout.defs_count.max(1)) as u32,
                sample_period_ticks: period,
                sample_count: count,
                data_ptr: ptr,
            });
        }
    }
    out
}

impl CosworthFile {
    #[cfg(not(target_os = "emscripten"))]
    /// Memory-maps and parses a local PDS file.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, CosworthError> {
        let path = path.as_ref();
        let display = path.to_string_lossy().into_owned();
        let file = File::open(path).map_err(|source| CosworthError::Io {
            path: display.clone(),
            source,
        })?;
        let data = unsafe { Mmap::map(&file) }.map_err(|source| CosworthError::Io {
            path: display.clone(),
            source,
        })?;
        Self::parse(display, Storage::Mapped(data))
    }

    /// Parses PDS telemetry from an owned byte buffer.
    pub fn from_bytes(path: impl Into<String>, data: Vec<u8>) -> Result<Self, CosworthError> {
        Self::parse(path.into(), Storage::Owned(data.into_boxed_slice()))
    }

    fn parse(display: String, data: Storage) -> Result<Self, CosworthError> {
        if data.len() < 0x100 {
            return Err(invalid(&display, "file is smaller than 256 bytes"));
        }
        let entries = find_directory(&data);
        if entries.len() < 3 {
            return Err(invalid(&display, "directory has fewer than three entries"));
        }
        let layout = find_layout(&entries, data.len(), &display)?;
        let marked = marker_defs(&data, layout);
        let is_export = marked.is_empty() && layout.defs_count <= 200;
        let raw_defs = if marked.is_empty() {
            markerless_defs(&data, layout)
        } else {
            marked
        };
        if raw_defs.is_empty() {
            return Err(invalid(&display, "no channel definitions found"));
        }
        let raw_chunks = parse_chunks(&data, layout, is_export);
        if raw_chunks.is_empty() {
            return Err(invalid(&display, "no readable sample chunks found"));
        }
        let mut diagnostics = Diagnostics::new();
        let defs = resolve_sample_types(
            raw_defs,
            &raw_chunks,
            data.len(),
            is_export,
            &display,
            &mut diagnostics,
        )?;

        let mut channels = defs
            .into_iter()
            .map(|def| Channel {
                id: def.id,
                name: def.name,
                unit: def.unit,
                unit_source: def.unit_source,
                sample_type: def.sample_type,
                chunks: Vec::new(),
                sample_count: 0,
                duration_ns: 0,
            })
            .collect::<Vec<_>>();
        let by_id = ChannelDispatch::new(&channels);
        // Deliberately preserve chunk-index table order. `order` and data_ptr
        // are not temporal keys in interrupted native logs. Convert each raw
        // descriptor directly into its final channel instead of building and
        // then copying through a second set of per-channel vectors.
        let mut unknown_chunks = 0usize;
        let mut clamped_chunks = 0usize;
        let mut empty_chunks = 0usize;
        for raw in raw_chunks {
            let Some(index) = by_id.get(raw.channel_id) else {
                unknown_chunks += 1;
                continue;
            };
            let channel = &mut channels[index];
            let width = channel.sample_type.byte_width() as u64;
            let max_count = (data.len() as u64).saturating_sub(raw.data_ptr) / width;
            let count = raw.sample_count.min(max_count);
            if count < raw.sample_count {
                clamped_chunks += 1;
            }
            if count == 0 {
                empty_chunks += 1;
                continue;
            }
            channel.chunks.push(Chunk {
                sample_period_ns: raw.sample_period_ticks as u64 * TICK_NS,
                sample_count: count,
                data_ptr: raw.data_ptr,
                sample_base: channel.sample_count,
                time_base_ns: channel.duration_ns,
            });
            channel.sample_count = channel.sample_count.saturating_add(count);
            channel.duration_ns = channel.duration_ns.saturating_add(
                count
                    .saturating_mul(raw.sample_period_ticks as u64)
                    .saturating_mul(TICK_NS),
            );
        }
        if unknown_chunks > 0 {
            diagnostics.warning(
                "pds.chunk_channel_unknown",
                format!(
                    "{unknown_chunks} chunk descriptors name channel ids absent from the \
                     definition table; those chunks were dropped"
                ),
            );
        }
        if clamped_chunks > 0 {
            diagnostics.warning(
                "pds.sample_count_clamped",
                format!(
                    "{clamped_chunks} chunk sample counts exceeded the remaining file bytes \
                     and were clamped"
                ),
            );
        }
        if empty_chunks > 0 {
            diagnostics.warning(
                "pds.chunk_empty_after_clamp",
                format!(
                    "{empty_chunks} chunks contained no complete samples after bounds checking \
                     and were dropped"
                ),
            );
        }
        Ok(Self {
            path: display,
            channels,
            diagnostics: diagnostics.into_items(),
            data,
        })
    }
}

impl TelemetrySource for CosworthFile {
    fn path(&self) -> &str {
        &self.path
    }

    fn format(&self) -> &'static str {
        "pds"
    }

    fn channels(&self) -> &[Channel] {
        &self.channels
    }

    fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    fn chunk_bytes(&self, channel_index: usize, chunk_index: usize) -> Option<&[u8]> {
        let channel = self.channels.get(channel_index)?;
        let chunk = channel.chunks.get(chunk_index)?;
        let width = channel.sample_type.byte_width();
        let start = usize::try_from(chunk.data_ptr).ok()?;
        let len = usize::try_from(chunk.sample_count)
            .ok()?
            .checked_mul(width)?;
        self.data.get(start..start.checked_add(len)?)
    }

    #[inline]
    fn decode(&self, channel_index: usize, chunk_index: usize, local_index: u64) -> f64 {
        let channel = &self.channels[channel_index];
        let chunk = &channel.chunks[chunk_index];
        let offset =
            chunk.data_ptr as usize + local_index as usize * channel.sample_type.byte_width();
        match channel.sample_type {
            SampleType::I8 => self.data[offset] as i8 as f64,
            SampleType::U8 => self.data[offset] as f64,
            SampleType::I16 => {
                i16::from_le_bytes(self.data[offset..offset + 2].try_into().unwrap()) as f64
            }
            SampleType::U16 => {
                u16::from_le_bytes(self.data[offset..offset + 2].try_into().unwrap()) as f64
            }
            SampleType::I32 => {
                i32::from_le_bytes(self.data[offset..offset + 4].try_into().unwrap()) as f64
            }
            SampleType::U32 => {
                u32::from_le_bytes(self.data[offset..offset + 4].try_into().unwrap()) as f64
            }
            SampleType::F32 => {
                f32::from_le_bytes(self.data[offset..offset + 4].try_into().unwrap()) as f64
            }
            SampleType::F64 => {
                f64::from_le_bytes(self.data[offset..offset + 8].try_into().unwrap())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn u32_at(data: &mut [u8], offset: usize, value: u32) {
        data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    fn utf16_at(data: &mut [u8], offset: usize, value: &str) {
        for (index, unit) in value.encode_utf16().enumerate() {
            data[offset + index * 2..offset + index * 2 + 2].copy_from_slice(&unit.to_le_bytes());
        }
    }
    fn directory(data: &mut [u8], at: usize, offset: u32, count: u32, class_b: u32, next: u32) {
        u32_at(data, at, offset);
        u32_at(data, at + 8, count);
        u32_at(data, at + 0x10, 8);
        u32_at(data, at + 0x14, class_b);
        u32_at(data, at + 0x18, next);
    }
    fn write_chunk_table(
        data: &mut [u8],
        chunks: usize,
        table: &[(u32, u32, &[f64])],
        mut ptr: usize,
    ) {
        for (index, (order, id, values)) in table.iter().enumerate() {
            let at = chunks + index * 0x40;
            u32_at(data, at, *order);
            u32_at(data, at + 4, *id);
            u32_at(data, at + 8, *id);
            u32_at(data, at + 0x18, 10_000_000);
            u32_at(data, at + 0x1c, values.len() as u32);
            u32_at(data, at + 0x38, ptr as u32);
            for value in *values {
                data[ptr..ptr + 8].copy_from_slice(&value.to_le_bytes());
                ptr += 8;
            }
        }
    }
    fn fixture() -> tempfile::NamedTempFile {
        let mut data = vec![0u8; 0x700];
        let defs = 0x200;
        let chunks = 0x380;
        directory(&mut data, 0x80, defs, 2, 1, 4);
        directory(&mut data, 0xa0, chunks, 4, 3, 0);
        directory(&mut data, 0xc0, 0x480, 0, 1, 0);
        for (index, (id, name)) in [(1, "Speed"), (2, "Gear")].into_iter().enumerate() {
            let at = defs as usize + index * 0xc0;
            u32_at(&mut data, at, id);
            utf16_at(&mut data, at + 8, name);
        }
        write_chunk_table(
            &mut data,
            chunks as usize,
            &[
                (100, 1, &[10.0, 11.0]),
                (200, 2, &[3.0, 3.0]),
                (1, 1, &[12.0, 13.0]),
                (2, 2, &[4.0, 4.0]),
            ],
            0x580,
        );
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&data).unwrap();
        file
    }
    fn unequal_chunks_fixture() -> tempfile::NamedTempFile {
        let mut data = vec![0u8; 0x800];
        let defs = 0x200;
        let chunks = 0x380;
        let next = 0x4c0;
        directory(&mut data, 0x80, defs, 2, 1, 5);
        directory(&mut data, 0xa0, chunks, 5, 3, 0);
        directory(&mut data, 0xc0, next, 0, 1, 0);
        for (index, (id, name)) in [(1, "Speed"), (2, "Gear")].into_iter().enumerate() {
            let at = defs as usize + index * 0xc0;
            u32_at(&mut data, at, id);
            utf16_at(&mut data, at + 8, name);
        }
        // Five descriptors, three Speed chunks with unequal counts. `order`
        // is not monotonic so a sort would scramble decode order.
        write_chunk_table(
            &mut data,
            chunks as usize,
            &[
                (50, 1, &[1.0, 2.0, 3.0]),
                (90, 2, &[10.0, 11.0]),
                (5, 1, &[4.0]),
                (1, 2, &[12.0, 13.0, 14.0, 15.0]),
                (80, 1, &[5.0, 6.0]),
            ],
            0x580,
        );
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&data).unwrap();
        file
    }
    fn channel_index(file: &CosworthFile, name: &str) -> usize {
        file.channels()
            .iter()
            .position(|channel| channel.name == name)
            .unwrap_or_else(|| panic!("missing {name}"))
    }

    #[test]
    fn preserves_chunk_table_order_and_interpolates() {
        let fixture = fixture();
        let file = CosworthFile::open(fixture.path()).unwrap();
        let in_memory =
            CosworthFile::from_bytes("fixture.pds", std::fs::read(fixture.path()).unwrap())
                .unwrap();
        assert_eq!(in_memory.channels.len(), 2);
        let metadata = read_metadata(fixture.path()).unwrap();
        assert_eq!(metadata.channel_count, 2);
        assert_eq!(metadata.sample_count, 8);
        assert_eq!(file.channels.len(), 2);
        assert_eq!(file.channels[0].sample_count, 4);
        let values = (0..4)
            .map(|index| file.decode(0, usize::from(index >= 2), index % 2))
            .collect::<Vec<_>>();
        assert_eq!(values, [10.0, 11.0, 12.0, 13.0]);
        assert_eq!(file.sample_at(0, 1_500_000_000, true), Some(11.5));
        assert_eq!(file.sample_at(1, 2_500_000_000, true), Some(4.0));
    }

    #[test]
    fn preserves_more_than_two_unequal_chunks_in_table_order() {
        let fixture = unequal_chunks_fixture();
        let file = CosworthFile::open(fixture.path()).unwrap();
        assert_eq!(file.channels.len(), 2);
        assert_eq!(file.channels[0].chunks.len(), 3);
        assert_eq!(file.channels[1].chunks.len(), 2);
        assert_eq!(
            file.channels[0]
                .chunks
                .iter()
                .map(|chunk| chunk.sample_count)
                .collect::<Vec<_>>(),
            [3, 1, 2]
        );
        assert_eq!(
            file.channels[1]
                .chunks
                .iter()
                .map(|chunk| chunk.sample_count)
                .collect::<Vec<_>>(),
            [2, 4]
        );
        let period = 10_000_000u64 * TICK_NS;
        assert_eq!(file.channels[0].chunks[0].time_base_ns, 0);
        assert_eq!(file.channels[0].chunks[1].time_base_ns, 3 * period);
        assert_eq!(file.channels[0].chunks[2].time_base_ns, 4 * period);
        assert_eq!(file.channels[1].chunks[1].time_base_ns, 2 * period);
        let mut speed = Vec::new();
        for chunk in 0..3 {
            for local in 0..file.channels[0].chunks[chunk].sample_count {
                speed.push(file.decode(0, chunk, local));
            }
        }
        assert_eq!(speed, [1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        let mut gear = Vec::new();
        for chunk in 0..2 {
            for local in 0..file.channels[1].chunks[chunk].sample_count {
                gear.push(file.decode(1, chunk, local));
            }
        }
        assert_eq!(gear, [10.0, 11.0, 12.0, 13.0, 14.0, 15.0]);
        assert_eq!(file.channels[0].sample_count, 6);
        assert_eq!(file.channels[1].sample_count, 6);
    }

    #[test]
    fn decodes_native_markerless_type_codes() {
        let defs = 0x200usize;
        let stride = 0xe0usize;
        let chunks = defs + stride * 201;
        let end = chunks + 0x80;
        let mut data = vec![0u8; end + 0x80];
        directory(&mut data, 0x80, defs as u32, 201, 1, 2);
        directory(&mut data, 0xa0, chunks as u32, 2, 3, 0);
        directory(&mut data, 0xc0, end as u32, 0, 1, 0);
        for (index, (id, name, type_code)) in [(1, "Float channel", 6), (2, "Signed channel", 2)]
            .into_iter()
            .enumerate()
        {
            let at = defs + index * stride;
            u32_at(&mut data, at, id);
            utf16_at(&mut data, at + 8, name);
            u32_at(&mut data, at + 0xd0, type_code);
            let chunk = chunks + index * 0x40;
            let ptr = end + index * 0x20;
            u32_at(&mut data, chunk, index as u32);
            u32_at(&mut data, chunk + 4, id);
            u32_at(&mut data, chunk + 8, id);
            u32_at(&mut data, chunk + 0x18, 10_000_000);
            u32_at(&mut data, chunk + 0x1c, 2);
            u32_at(&mut data, chunk + 0x38, ptr as u32);
            if type_code == 6 {
                for (sample, value) in [1.5_f32, -2.25].into_iter().enumerate() {
                    data[ptr + sample * 4..ptr + sample * 4 + 4]
                        .copy_from_slice(&value.to_le_bytes());
                }
            } else {
                for (sample, value) in [-30_000_i16, 30_000].into_iter().enumerate() {
                    data[ptr + sample * 2..ptr + sample * 2 + 2]
                        .copy_from_slice(&value.to_le_bytes());
                }
            }
        }
        let mut fixture = tempfile::NamedTempFile::new().unwrap();
        fixture.write_all(&data).unwrap();
        let file = CosworthFile::open(fixture.path()).unwrap();
        assert_eq!(file.channels[0].sample_type, SampleType::F32);
        assert_eq!(file.decode(0, 0, 1), -2.25);
        assert_eq!(file.channels[1].sample_type, SampleType::I16);
        assert_eq!(file.decode(1, 0, 0), -30_000.0);
    }

    #[test]
    fn rejects_short_files() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"not pds").unwrap();
        assert!(matches!(
            CosworthFile::open(file.path()),
            Err(CosworthError::Invalid { .. })
        ));
    }

    #[test]
    fn committed_fixture_has_three_complete_flying_laps() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/synthetic_cosworth.pds");
        let file = CosworthFile::open(&path).unwrap();
        let metadata = motorsport_telemetry_core::read_source_metadata(&file);
        assert_eq!(file.channels.len(), 10);
        assert_eq!(file.channels()[0].name, "Speed");
        assert_eq!(file.channels()[0].chunks[0].sample_period_ns, 200_000_000);
        assert!(file.channels()[0].sample_count > 1_000);
        assert_eq!(metadata.driver_ids, [7]);
        assert_eq!(metadata.laps.len(), 5);
        assert_eq!(metadata.valid_laps, 3u32);
        assert_eq!(
            metadata
                .laps
                .iter()
                .map(|lap| (lap.number, lap.complete))
                .collect::<Vec<_>>(),
            [(1, false), (2, true), (3, true), (4, true), (5, false)]
        );
        for lap in metadata.laps.iter().filter(|lap| lap.complete) {
            assert!(
                lap.duration_ns >= 10_000_000_000,
                "flying lap {} is too short",
                lap.number
            );
        }
        let fastest = metadata.fastest_lap.expect("flying laps should rank");
        assert_eq!(fastest.number, 2);
        let mid = file
            .sample_at(8, fastest.start_ns + fastest.duration_ns / 2, false)
            .unwrap();
        let lon = file
            .sample_at(9, fastest.start_ns + fastest.duration_ns / 2, false)
            .unwrap();
        assert!((43.78..=43.82).contains(&mid), "lat={mid}");
        assert!((-88.02..=-87.97).contains(&lon), "lon={lon}");
        let speeds: Vec<f64> = (0..file.channels()[0].sample_count)
            .map(|i| file.decode(0, 0, i))
            .collect();
        let min_v = speeds.iter().copied().fold(f64::INFINITY, f64::min);
        let max_v = speeds.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        assert!(min_v > 5.0 && max_v < 80.0, "speed {min_v}..{max_v}");
        assert!(max_v - min_v > 15.0);

        let speed_ch = channel_index(&file, "Speed");
        let throttle_ch = channel_index(&file, "Throttle Pos");
        let brake_ch = channel_index(&file, "Brake Pedal Pos");
        let distance_ch = channel_index(&file, "Lap Distance");
        let lap_ch = channel_index(&file, "Lap Number");
        let lat_ch = channel_index(&file, "GPS Latitude");
        let lon_ch = channel_index(&file, "GPS Longitude");
        let count = file.channels()[speed_ch].sample_count;
        assert_eq!(
            file.channels()[speed_ch].chunks[0].sample_period_ns,
            200_000_000
        );
        let mut early_throttle = Vec::new();
        let mut late_throttle = Vec::new();
        let mut late_brake = Vec::new();
        let mut max_distance = [0.0_f64; 8];
        for local in 0..count {
            let lap = file.decode(lap_ch, 0, local) as usize;
            if lap < max_distance.len() {
                max_distance[lap] = max_distance[lap].max(file.decode(distance_ch, 0, local));
            }
            let lat = file.decode(lat_ch, 0, local);
            let lon = file.decode(lon_ch, 0, local);
            assert!((43.78..=43.82).contains(&lat), "lat={lat} at {local}");
            assert!((-88.02..=-87.97).contains(&lon), "lon={lon} at {local}");
        }
        for local in 0..count {
            let lap = file.decode(lap_ch, 0, local);
            if !matches!(lap, 2.0 | 3.0 | 4.0) {
                continue;
            }
            let distance = file.decode(distance_ch, 0, local);
            let throttle = file.decode(throttle_ch, 0, local);
            if distance < 200.0 {
                early_throttle.push(throttle);
            }
            let lap_max = max_distance[lap as usize];
            if lap_max > 0.0 && distance > 0.96 * lap_max {
                late_throttle.push(throttle);
                late_brake.push(file.decode(brake_ch, 0, local));
            }
        }
        assert!(!early_throttle.is_empty());
        assert!(
            early_throttle.iter().all(|value| *value > 80.0),
            "front-straight throttle after lap-distance reset {early_throttle:?}"
        );
        assert!(!late_throttle.is_empty());
        assert!(
            late_throttle.iter().all(|value| *value >= 50.0)
                && late_brake.iter().all(|value| *value == 0.0),
            "late-lap front straight throttle={late_throttle:?} brake={late_brake:?}"
        );

        for complete in metadata.laps.iter().filter(|lap| lap.complete) {
            let members: Vec<u64> = (0..count)
                .filter(|&local| file.decode(lap_ch, 0, local) == complete.number as f64)
                .collect();
            let slowest = *members
                .iter()
                .min_by(|a, b| {
                    file.decode(speed_ch, 0, **a)
                        .total_cmp(&file.decode(speed_ch, 0, **b))
                })
                .expect("flying lap samples");
            let window: Vec<f64> = members
                .iter()
                .copied()
                .filter(|local| *local < slowest && slowest - *local <= 20)
                .map(|local| file.decode(brake_ch, 0, local))
                .collect();
            assert!(
                !window.is_empty(),
                "no pre-corner samples on lap {}",
                complete.number
            );
            let peak = window.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            assert!(
                peak > 40.0 && peak > window[0],
                "lap {} brake should rise before the slowest corner (first={} peak={})",
                complete.number,
                window[0],
                peak
            );
        }
    }

    #[test]
    fn detects_type_field_in_wide_native_records() {
        // Real DPi/IMSA logs use 0x228-byte definitions with the sample type
        // at 0x48 (and a mirror at 0x104), not the old hard-coded 0xd0.
        // Reading 0xd0 yields zero for every channel and used to default all
        // 1399 channels to F64: 120 MB of claimed samples in a 40 MB file.
        let defs = 0x200usize;
        let stride = 0x228usize;
        let definition_count = 201usize;
        let chunks = defs + stride * definition_count;
        let next = chunks + 0x80;
        let data_start = next + 0x40;
        let mut data = vec![0u8; data_start + 0x40];
        directory(&mut data, 0x80, defs as u32, definition_count as u32, 1, 2);
        directory(&mut data, 0xa0, chunks as u32, 2, 3, 0);
        directory(&mut data, 0xc0, next as u32, 0, 1, 0);

        for (index, (id, name, type_code)) in [(1, "Speed_Wspd_App", 6u32), (2, "Alarm", 3u32)]
            .into_iter()
            .enumerate()
        {
            let at = defs + index * stride;
            u32_at(&mut data, at, id);
            utf16_at(&mut data, at + 8, name);
            u32_at(&mut data, at + 0x48, type_code);
            // Match the duplicate field present in the observed layout.
            u32_at(&mut data, at + 0x104, type_code);

            let chunk = chunks + index * 0x40;
            let ptr = data_start + index * 0x20;
            u32_at(&mut data, chunk, index as u32);
            u32_at(&mut data, chunk + 4, id);
            u32_at(&mut data, chunk + 8, id);
            u32_at(&mut data, chunk + 0x18, 200_000);
            u32_at(&mut data, chunk + 0x1c, 2);
            u32_at(&mut data, chunk + 0x38, ptr as u32);
            if type_code == 6 {
                for (sample, value) in [0.0_f32, 83.6].into_iter().enumerate() {
                    data[ptr + sample * 4..ptr + sample * 4 + 4]
                        .copy_from_slice(&value.to_le_bytes());
                }
            } else {
                for (sample, value) in [0u16, 2].into_iter().enumerate() {
                    data[ptr + sample * 2..ptr + sample * 2 + 2]
                        .copy_from_slice(&value.to_le_bytes());
                }
            }
        }

        let file = CosworthFile::from_bytes("wide.pds", data).unwrap();
        assert_eq!(file.channels[0].sample_type, SampleType::F32);
        assert_eq!(file.channels[1].sample_type, SampleType::U16);
        assert!((file.decode(0, 0, 1) - 83.6).abs() < 1e-4);
        assert_eq!(file.decode(1, 0, 1), 2.0);
        assert!(
            motorsport_telemetry_core::validate_source(&file).is_empty(),
            "{}",
            motorsport_telemetry_core::validate_source(&file)
        );
        assert!(file.diagnostics().is_empty(), "{:?}", file.diagnostics());
    }

    #[test]
    fn decodes_signed_byte_type_code_zero() {
        let defs = 0x200usize;
        let stride = 0xe0usize;
        let chunks = defs + stride * 201;
        let next = chunks + 0x80;
        let ptr = next + 0x40;
        let mut data = vec![0u8; ptr + 0x20];
        directory(&mut data, 0x80, defs as u32, 201, 1, 2);
        directory(&mut data, 0xa0, chunks as u32, 2, 3, 0);
        directory(&mut data, 0xc0, next as u32, 0, 1, 0);
        for (index, (id, name, code)) in [(1, "Float", 6u32), (2, "TPMS_RSSI", 0u32)]
            .into_iter()
            .enumerate()
        {
            let def = defs + index * stride;
            u32_at(&mut data, def, id);
            utf16_at(&mut data, def + 8, name);
            u32_at(&mut data, def + 0xd0, code);
            let chunk = chunks + index * 0x40;
            u32_at(&mut data, chunk + 4, id);
            u32_at(&mut data, chunk + 8, id);
            u32_at(&mut data, chunk + 0x18, 10_000);
            u32_at(&mut data, chunk + 0x1c, 1);
            u32_at(&mut data, chunk + 0x38, (ptr + index * 8) as u32);
        }
        data[ptr..ptr + 4].copy_from_slice(&1.0f32.to_le_bytes());
        data[ptr + 8] = (-73i8) as u8;

        let file = CosworthFile::from_bytes("signed-byte.pds", data).unwrap();
        assert_eq!(file.channels[1].sample_type, SampleType::I8);
        assert_eq!(file.decode(1, 0, 0), -73.0);
        assert!(file.diagnostics().is_empty(), "{:?}", file.diagnostics());
    }

    #[test]
    fn reports_unsupported_type_code_fallback() {
        let defs = 0x200usize;
        let stride = 0xe0usize;
        let chunks = defs + stride * 201;
        let next = chunks + 0x80;
        let ptr = next + 0x40;
        let mut data = vec![0u8; ptr + 0x20];
        directory(&mut data, 0x80, defs as u32, 201, 1, 2);
        directory(&mut data, 0xa0, chunks as u32, 2, 3, 0);
        directory(&mut data, 0xc0, next as u32, 0, 1, 0);
        for (index, (id, name, code)) in [(1, "Known", 6u32), (2, "Future", 8u32)]
            .into_iter()
            .enumerate()
        {
            let def = defs + index * stride;
            u32_at(&mut data, def, id);
            utf16_at(&mut data, def + 8, name);
            u32_at(&mut data, def + 0xd0, code);
            let chunk = chunks + index * 0x40;
            u32_at(&mut data, chunk + 4, id);
            u32_at(&mut data, chunk + 8, id);
            u32_at(&mut data, chunk + 0x18, 10_000);
            u32_at(&mut data, chunk + 0x1c, 1);
            u32_at(&mut data, chunk + 0x38, (ptr + index * 8) as u32);
            data[ptr + index * 8..ptr + index * 8 + 4].copy_from_slice(&1.0f32.to_le_bytes());
        }
        let file = CosworthFile::from_bytes("future-type.pds", data).unwrap();
        assert_eq!(file.channels[1].sample_type, SampleType::F32);
        assert!(
            file.diagnostics()
                .iter()
                .any(|item| item.code == "pds.type_code_unrecognized"),
            "{:?}",
            file.diagnostics()
        );
    }
}
