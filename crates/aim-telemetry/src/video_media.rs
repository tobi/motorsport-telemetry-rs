//! Seek-only inspection of independently measured video presentation extents.

use super::{be32, be64, bei32, bei64, boxes, invalid, AimError, BoxRef};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const MAX_MOOV: u64 = 32 * 1024 * 1024;
const MAX_FTYP: u64 = 64 * 1024;
const MAX_BOXES: usize = 4096;

#[cfg(test)]
mod tests;

/// Bounded container metadata, independent of telemetry synchronization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoMediaMetadata {
    /// Actual source file size, including media payloads.
    pub byte_size: u64,
    /// BLAKE3 of framed source size and `ftyp`/`moov` bytes.
    /// This is a header/cache fingerprint, NOT video content authentication.
    pub header_fingerprint: [u8; 32],
    /// Video streams in native track order; audio/telemetry are excluded.
    pub video_streams: Vec<VideoStreamMetadata>,
}

/// Encoded sample count and presentation extent measured from MP4 tables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoStreamMetadata {
    /// Native MP4 track identifier.
    pub track_id: u32,
    /// Number of encoded samples, including samples excluded by edit lists.
    pub frame_count: u64,
    /// Earliest presented sample time (inclusive), in nanoseconds.
    pub presentation_start_ns: i64,
    /// End of the last presented sample (exclusive), in nanoseconds.
    pub presentation_end_ns: i64,
}

fn filesystem(path: &str, source: std::io::Error) -> AimError {
    AimError::Io {
        path: path.into(),
        source,
    }
}

fn strict_boxes(data: &[u8], parent: BoxRef, path: &str) -> Result<Vec<BoxRef>, AimError> {
    let children = boxes(data, parent.payload, parent.end)
        .take(MAX_BOXES + 1)
        .collect::<Vec<_>>();
    if children.len() > MAX_BOXES
        || children.last().map_or(parent.payload, |item| item.end) != parent.end
    {
        return Err(invalid(path, "malformed nested MP4 box"));
    }
    Ok(children)
}

fn one(data: &[u8], parent: BoxRef, kind: [u8; 4], path: &str) -> Result<Option<BoxRef>, AimError> {
    let mut found = None;
    for item in strict_boxes(data, parent, path)? {
        if item.kind == kind && found.replace(item).is_some() {
            return Err(invalid(path, "duplicate MP4 metadata box"));
        }
    }
    Ok(found)
}

fn required(data: &[u8], parent: BoxRef, kind: [u8; 4], path: &str) -> Result<BoxRef, AimError> {
    one(data, parent, kind, path)?.ok_or_else(|| invalid(path, "missing MP4 timing box"))
}

fn payload(data: &[u8], item: BoxRef) -> &[u8] {
    &data[item.payload..item.end]
}

fn timescale(data: &[u8], item: BoxRef, path: &str) -> Result<u32, AimError> {
    let bytes = payload(data, item);
    let offset = match bytes.first() {
        Some(0) => 12,
        Some(1) => 20,
        _ => return Err(invalid(path, "unsupported timescale box version")),
    };
    be32(bytes, offset)
        .filter(|value| *value > 0)
        .ok_or_else(|| invalid(path, "invalid MP4 timescale"))
}

fn table<'a>(data: &'a [u8], item: BoxRef, width: usize, path: &str) -> Result<&'a [u8], AimError> {
    let bytes = payload(data, item);
    let count = be32(bytes, 4).ok_or_else(|| invalid(path, "truncated timing table"))? as usize;
    if count > 1_000_000 {
        return Err(invalid(path, "too many timing table runs"));
    }
    let end = count
        .checked_mul(width)
        .and_then(|size| size.checked_add(8))
        .ok_or_else(|| invalid(path, "timing table size overflow"))?;
    if end != bytes.len() {
        return Err(invalid(path, "truncated or overlong timing table"));
    }
    Ok(&bytes[8..])
}

fn unsigned_runs(data: &[u8], item: BoxRef, path: &str) -> Result<Vec<(u64, u64)>, AimError> {
    if payload(data, item).first() != Some(&0) {
        return Err(invalid(path, "unsupported sample timing version"));
    }
    table(data, item, 8, path)?
        .chunks_exact(8)
        .map(|entry| {
            let count =
                u64::from(be32(entry, 0).ok_or_else(|| invalid(path, "truncated timing run"))?);
            let delta =
                u64::from(be32(entry, 4).ok_or_else(|| invalid(path, "truncated timing run"))?);
            if count == 0 || delta == 0 {
                return Err(invalid(path, "empty/zero-duration timing run"));
            }
            Ok((count, delta))
        })
        .collect()
}

#[derive(Clone, Copy)]
struct Edit {
    movie_start: i128,
    media_start: i128,
    duration: i128,
}

fn edits(
    data: &[u8],
    trak: BoxRef,
    movie_scale: u32,
    media_scale: u32,
    path: &str,
) -> Result<Option<Vec<Edit>>, AimError> {
    let Some(edts) = one(data, trak, *b"edts", path)? else {
        return Ok(None);
    };
    let item = required(data, edts, *b"elst", path)?;
    let version = *payload(data, item)
        .first()
        .ok_or_else(|| invalid(path, "truncated edit list"))?;
    if be32(payload(data, item), 4).is_none_or(|count| count > 64) {
        return Err(invalid(path, "too many or truncated MP4 edits"));
    }
    if version > 1 {
        return Err(invalid(path, "unsupported edit list version"));
    }
    let mut result = Vec::new();
    let mut movie_time = 0i128;
    for entry in table(data, item, if version == 1 { 20 } else { 12 }, path)?
        .chunks_exact(if version == 1 { 20 } else { 12 })
    {
        let (duration, media_time, rate) = if version == 1 {
            (be64(entry, 0), bei64(entry, 8), be32(entry, 16))
        } else {
            (
                be32(entry, 0).map(u64::from),
                bei32(entry, 4).map(i64::from),
                be32(entry, 8),
            )
        };
        let duration =
            i128::from(duration.ok_or_else(|| invalid(path, "truncated edit duration"))?);
        let media_time = media_time.ok_or_else(|| invalid(path, "truncated edit media time"))?;
        if rate != Some(0x0001_0000) || media_time < -1 || duration == 0 {
            return Err(invalid(
                path,
                "unsupported edit rate, media time or duration",
            ));
        }
        let start_ns = movie_time * 1_000_000_000 / i128::from(movie_scale);
        let end_ns = (movie_time + duration) * 1_000_000_000 / i128::from(movie_scale);
        if media_time >= 0 {
            result.push(Edit {
                movie_start: start_ns,
                media_start: i128::from(media_time) * 1_000_000_000 / i128::from(media_scale),
                duration: end_ns - start_ns,
            });
        }
        movie_time += duration;
    }
    if result.is_empty() {
        return Err(invalid(path, "edit list contains no presented media"));
    }
    Ok(Some(result))
}

fn stream(
    data: &[u8],
    trak: BoxRef,
    movie_scale: u32,
    path: &str,
) -> Result<Option<VideoStreamMetadata>, AimError> {
    let mdia = required(data, trak, *b"mdia", path)?;
    let hdlr = required(data, mdia, *b"hdlr", path)?;
    if payload(data, hdlr).get(8..12) != Some(b"vide") {
        return Ok(None);
    }
    let tkhd = required(data, trak, *b"tkhd", path)?;
    let tkhd = payload(data, tkhd);
    let id_offset = match tkhd.first() {
        Some(0) => 12,
        Some(1) => 20,
        _ => return Err(invalid(path, "unsupported track header version")),
    };
    let track_id = be32(tkhd, id_offset)
        .filter(|id| *id > 0)
        .ok_or_else(|| invalid(path, "missing video track identifier"))?;
    let media_scale = timescale(data, required(data, mdia, *b"mdhd", path)?, path)?;
    let minf = required(data, mdia, *b"minf", path)?;
    let stbl = required(data, minf, *b"stbl", path)?;
    let stts = required(data, stbl, *b"stts", path)?;
    let runs = unsigned_runs(data, stts, path)?;
    let frame_count = runs
        .iter()
        .try_fold(0u64, |sum, (count, _)| sum.checked_add(*count))
        .ok_or_else(|| invalid(path, "sample count overflow"))?;
    if frame_count == 0 {
        return Err(invalid(path, "video has no timed samples"));
    }
    let stsz = payload(data, required(data, stbl, *b"stsz", path)?);
    let size = be32(stsz, 4).ok_or_else(|| invalid(path, "truncated sample size table"))?;
    let count = be32(stsz, 8).ok_or_else(|| invalid(path, "truncated sample count"))?;
    let expected = if size == 0 {
        12u64 + u64::from(count) * 4
    } else {
        12
    };
    if u64::from(count) != frame_count || stsz.len() as u64 != expected {
        return Err(invalid(
            path,
            "sample size/timing counts differ or table is truncated",
        ));
    }
    let mut compositions = Vec::new();
    if let Some(ctts) = one(data, stbl, *b"ctts", path)? {
        let version = *payload(data, ctts)
            .first()
            .ok_or_else(|| invalid(path, "truncated composition table"))?;
        if version > 1 {
            return Err(invalid(path, "unsupported composition table version"));
        }
        for entry in table(data, ctts, 8, path)?.chunks_exact(8) {
            let count = u64::from(
                be32(entry, 0).ok_or_else(|| invalid(path, "truncated composition run"))?,
            );
            let offset = if version == 1 {
                bei32(entry, 4).map(i64::from)
            } else {
                be32(entry, 4).map(i64::from)
            }
            .ok_or_else(|| invalid(path, "truncated composition offset"))?;
            if count == 0 {
                return Err(invalid(path, "empty composition run"));
            }
            compositions.push((count, offset));
        }
        if compositions
            .iter()
            .try_fold(0u64, |sum, (count, _)| sum.checked_add(*count))
            != Some(frame_count)
        {
            return Err(invalid(path, "composition/sample timing counts differ"));
        }
    } else {
        compositions.push((frame_count, 0));
    }
    let edits = edits(data, trak, movie_scale, media_scale, path)?;
    let mut start = i128::MAX;
    let mut end = i128::MIN;
    let mut composition_index = 0usize;
    let mut composition_remaining = compositions[0].0;
    let mut decode = 0u64;
    for (mut remaining, delta) in runs {
        while remaining > 0 {
            let count = remaining.min(composition_remaining);
            let next = count
                .checked_mul(delta)
                .and_then(|duration| decode.checked_add(duration))
                .ok_or_else(|| invalid(path, "video timing overflow"))?;
            let offset = i128::from(compositions[composition_index].1);
            let low = (i128::from(decode) + offset) * 1_000_000_000 / i128::from(media_scale);
            let high = (i128::from(next) + offset) * 1_000_000_000 / i128::from(media_scale);
            if let Some(edits) = &edits {
                for edit in edits {
                    let from = low.max(edit.media_start);
                    let to = high.min(edit.media_start + edit.duration);
                    if from < to {
                        start = start.min(from - edit.media_start + edit.movie_start);
                        end = end.max(to - edit.media_start + edit.movie_start);
                    }
                }
            } else {
                start = start.min(low);
                end = end.max(high);
            }
            decode = next;
            remaining -= count;
            composition_remaining -= count;
            if composition_remaining == 0 {
                composition_index += 1;
                if let Some((count, _)) = compositions.get(composition_index) {
                    composition_remaining = *count;
                }
            }
        }
    }
    if start >= end {
        return Err(invalid(path, "video has no presented samples"));
    }
    Ok(Some(VideoStreamMetadata {
        track_id,
        frame_count,
        presentation_start_ns: i64::try_from(start)
            .map_err(|_| invalid(path, "presentation extent overflow"))?,
        presentation_end_ns: i64::try_from(end)
            .map_err(|_| invalid(path, "presentation extent overflow"))?,
    }))
}

/// Inspect nonfragmented MP4 metadata without reading `mdat` or other payloads.
///
/// Seeks over at most 4096 top-level box headers and reads only `ftyp` (64 KiB)
/// and `moov` (32 MiB). Counts and extents come from sample timing/composition
/// tables and rate-one edit lists; no frame arrays, FPS assumptions or source
/// telemetry values are used. Fragmented, malformed or unsupported files fail
/// explicitly. Header fingerprints are not video content hashes.
pub fn inspect_mp4_media(path: impl AsRef<Path>) -> Result<VideoMediaMetadata, AimError> {
    let path = path.as_ref();
    let name = path.to_string_lossy();
    let mut file = File::open(path).map_err(|error| filesystem(&name, error))?;
    let metadata = file.metadata().map_err(|error| filesystem(&name, error))?;
    if !metadata.is_file() {
        return Err(invalid(&name, "expected regular MP4 file"));
    }
    let byte_size = metadata.len();
    inspect_reader(&mut file, byte_size, &name)
}

fn inspect_reader(
    file: &mut (impl Read + Seek),
    byte_size: u64,
    name: &str,
) -> Result<VideoMediaMetadata, AimError> {
    let mut position = 0u64;
    let mut top_count = 0usize;
    let mut moov = None;
    let mut ftyp = None;
    while position < byte_size {
        top_count += 1;
        if top_count > MAX_BOXES || byte_size - position < 8 {
            return Err(invalid(name, "too many or truncated MP4 box headers"));
        }
        file.seek(SeekFrom::Start(position))
            .map_err(|error| filesystem(name, error))?;
        let mut header = [0u8; 16];
        file.read_exact(&mut header[..8])
            .map_err(|error| filesystem(name, error))?;
        let size32 = be32(&header, 0).ok_or_else(|| invalid(name, "truncated box size"))?;
        let (size, header_size) = match size32 {
            0 => (byte_size - position, 8usize),
            1 => {
                if byte_size - position < 16 {
                    return Err(invalid(name, "truncated extended box header"));
                }
                file.read_exact(&mut header[8..])
                    .map_err(|error| filesystem(name, error))?;
                (
                    be64(&header, 8).ok_or_else(|| invalid(name, "truncated extended size"))?,
                    16,
                )
            }
            size => (u64::from(size), 8),
        };
        let next = position
            .checked_add(size)
            .filter(|next| *next <= byte_size && size >= header_size as u64)
            .ok_or_else(|| invalid(name, "MP4 box exceeds source size"))?;
        let kind = &header[4..8];
        if kind == b"moof" {
            return Err(invalid(name, "fragmented MP4 is unsupported"));
        }
        if kind == b"moov" || kind == b"ftyp" {
            let (target, limit) = if kind == b"moov" {
                (&mut moov, MAX_MOOV)
            } else {
                (&mut ftyp, MAX_FTYP)
            };
            if target.is_some() || size > limit {
                return Err(invalid(name, "duplicate or oversized MP4 metadata box"));
            }
            let mut bytes = vec![0u8; size as usize];
            bytes[..header_size].copy_from_slice(&header[..header_size]);
            file.read_exact(&mut bytes[header_size..])
                .map_err(|error| filesystem(name, error))?;
            *target = Some(bytes);
        }
        position = next;
    }
    let moov = moov.ok_or_else(|| invalid(name, "MP4 has no moov box"))?;
    let moov_box = boxes(&moov, 0, moov.len())
        .next()
        .ok_or_else(|| invalid(name, "invalid moov"))?;
    if one(&moov, moov_box, *b"mvex", name)?.is_some() {
        return Err(invalid(name, "fragmented MP4 is unsupported"));
    }
    let movie_scale = timescale(&moov, required(&moov, moov_box, *b"mvhd", name)?, name)?;
    let mut video_streams = Vec::new();
    for trak in strict_boxes(&moov, moov_box, name)?
        .into_iter()
        .filter(|item| item.kind == *b"trak")
    {
        if let Some(stream) = stream(&moov, trak, movie_scale, name)? {
            if video_streams
                .iter()
                .any(|previous: &VideoStreamMetadata| previous.track_id == stream.track_id)
            {
                return Err(invalid(name, "duplicate video track identifier"));
            }
            video_streams.push(stream);
        }
    }
    if video_streams.is_empty() {
        return Err(invalid(name, "MP4 has no timed video track"));
    }
    let mut fingerprint = blake3::Hasher::new();
    fingerprint.update(b"mp4-header-v1\0");
    fingerprint.update(&byte_size.to_le_bytes());
    for bytes in [ftyp.as_deref().unwrap_or_default(), moov.as_slice()] {
        fingerprint.update(&(bytes.len() as u64).to_le_bytes());
        fingerprint.update(bytes);
    }
    Ok(VideoMediaMetadata {
        byte_size,
        header_fingerprint: *fingerprint.finalize().as_bytes(),
        video_streams,
    })
}
