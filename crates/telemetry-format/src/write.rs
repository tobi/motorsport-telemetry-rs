//! Lossless writer from any [`TelemetrySource`].

use crate::catalog::{unit_fields, Catalog, CatalogChannel};
use crate::zip::{ZipError, ZipWriter};
use motorsport_telemetry_core::{
    read_source_metadata, schema_hash, Channel, SampleType, TelemetrySource,
};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

/// Writes a `.telemetry` zip next to or at `dest`.
pub fn write_from_source(
    source: &dyn TelemetrySource,
    dest: impl AsRef<Path>,
) -> Result<(), TelemetryFormatError> {
    let dest = dest.as_ref();
    let file = File::create(dest).map_err(io_err)?;
    write_to(source, crate::FORMAT_VERSION, BufWriter::new(file))
}

/// Writes a `.telemetry` zip stamped with an explicit catalog version.
pub fn write_from_source_version(
    source: &dyn TelemetrySource,
    dest: impl AsRef<Path>,
    format_version: u16,
) -> Result<(), TelemetryFormatError> {
    let dest = dest.as_ref();
    let file = File::create(dest).map_err(io_err)?;
    write_to(source, format_version, BufWriter::new(file))
}

fn write_to(
    source: &dyn TelemetrySource,
    format_version: u16,
    writer: impl Write + std::io::Seek,
) -> Result<(), TelemetryFormatError> {
    let metadata = read_source_metadata(source);
    let mut catalog_channels = Vec::with_capacity(source.channels().len());
    let mut payloads = Vec::with_capacity(source.channels().len());

    for (index, channel) in source.channels().iter().enumerate() {
        let member = format!("channels/{index:04}.bin");
        let event = is_event(source, index, channel);
        let (values, times) = collect_channel(source, index, channel, event)?;
        let time_member = if event {
            format!("channels/{index:04}.time.bin")
        } else {
            String::new()
        };
        let (scale, bias) = source.sample_affine(index);
        let (unit_canonical, dimension) = unit_fields(channel);
        catalog_channels.push(CatalogChannel {
            id: channel.id,
            name: channel.name.clone(),
            member: member.clone(),
            time_member: time_member.clone(),
            unit_raw: channel.unit.clone(),
            unit_canonical,
            unit_source: channel.unit_source,
            dimension,
            sample_type: if values.is_empty() && channel.sample_count == 0 {
                channel.sample_type
            } else if source.chunk_bytes(index, 0).is_some() {
                channel.sample_type
            } else {
                SampleType::F64
            },
            scale,
            bias,
            uses_step: channel.uses_step_interpolation(),
            sample_count: channel.sample_count,
            duration_ns: channel.duration_ns,
            kind: u8::from(event),
            chunks: channel.chunks.clone(),
        });
        payloads.push((member, values, time_member, times));
    }

    let mut laps = metadata.laps.clone();
    for lap in &mut laps {
        if lap.first_video_frame.is_none() {
            lap.first_video_frame = source.video_frame_at(lap.start_ns);
        }
    }
    let valid_laps = laps.iter().filter(|lap| lap.complete).count() as u32;
    let session_hint = metadata
        .session_key
        .as_deref()
        .and_then(|key| key.rsplit_once(':').map(|(hint, _)| hint.to_owned()))
        .unwrap_or_default();
    let mut catalog = Catalog {
        format_version,
        identity: metadata.identity.clone(),
        laps,
        valid_laps,
        channels: catalog_channels,
        source_format: source.format().to_owned(),
        source_path: source.path().to_owned(),
        schema_hash: schema_hash(source),
        duration_ns: metadata.duration_ns,
        sample_count: metadata.sample_count,
        channel_count: metadata.channel_count as u32,
        sampled_channel_count: metadata.sampled_channel_count as u32,
        session_hint,
        comment: String::new(),
        clock: source.absolute_time_range(),
        driver_stints: metadata.driver_stints.clone(),
        videos: {
            let mut videos = hash_videos(source);
            if let Some(count) = source.video_frame_count() {
                if let Some(video) = videos.first_mut() {
                    video.frame_count = count;
                } else if let Some(name) = Path::new(source.path()).file_name() {
                    videos.push(motorsport_telemetry_core::VideoFileRef {
                        filename: name.to_string_lossy().into_owned(),
                        index: 1,
                        blake3: hash_file(Path::new(source.path())),
                        frame_count: count,
                        presentation_offset_ns: source.video_presentation_offset_ns(),
                    });
                }
            }
            videos
        },
        presentation_offset_ns: source.video_presentation_offset_ns(),
    };
    let offset = catalog.presentation_offset_ns;
    for video in &mut catalog.videos {
        if video.presentation_offset_ns.is_none() {
            video.presentation_offset_ns = offset;
        }
    }

    let mut zip = ZipWriter::new(writer);
    zip.write_member("metadata.fb", &crate::catalog::encode(&catalog)?)?;
    if let Some(times) = source.video_presentation_times_ns() {
        if !times.is_empty() {
            let mut bytes = Vec::with_capacity(times.len() * 8);
            for time in times {
                bytes.extend_from_slice(&time.to_le_bytes());
            }
            zip.write_member("video_frames.bin", &bytes)?;
        }
    }
    for (member, values, time_member, times) in payloads {
        if !values.is_empty() {
            zip.write_member(&member, &values)?;
        }
        if !time_member.is_empty() && !times.is_empty() {
            zip.write_member(&time_member, &times)?;
        }
    }
    zip.finish()?;
    Ok(())
}

fn hash_videos(source: &dyn TelemetrySource) -> Vec<motorsport_telemetry_core::VideoFileRef> {
    let parent = Path::new(source.path()).parent();
    source
        .video_files()
        .iter()
        .cloned()
        .map(|mut video| {
            if video.blake3.is_none() {
                if let Some(path) = parent.map(|dir| dir.join(&video.filename)) {
                    if path.is_file() {
                        video.blake3 = hash_file(&path);
                    }
                }
            }
            video
        })
        .collect()
}

fn hash_file(path: &Path) -> Option<[u8; 32]> {
    let mut file = File::open(path).ok()?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = [0u8; 1 << 16];
    loop {
        let read = std::io::Read::read(&mut file, &mut buf).ok()?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Some(*hasher.finalize().as_bytes())
}

fn collect_channel(
    source: &dyn TelemetrySource,
    index: usize,
    channel: &Channel,
    event: bool,
) -> Result<(Vec<u8>, Vec<u8>), TelemetryFormatError> {
    if channel.sample_count == 0 {
        return Ok((Vec::new(), Vec::new()));
    }
    let mut values = Vec::new();
    let mut times = Vec::new();
    for (chunk_index, chunk) in channel.chunks.iter().enumerate() {
        if let Some(bytes) = source.chunk_bytes(index, chunk_index) {
            values.extend_from_slice(bytes);
        } else {
            for local in 0..chunk.sample_count {
                values.extend_from_slice(&source.decode(index, chunk_index, local).to_le_bytes());
            }
        }
        if event {
            for local in 0..chunk.sample_count {
                times.extend_from_slice(
                    &source
                        .sample_time_ns(index, chunk_index, local)
                        .to_le_bytes(),
                );
            }
        }
    }
    Ok((values, times))
}

fn is_event(source: &dyn TelemetrySource, index: usize, channel: &Channel) -> bool {
    if source.chunk_bytes(index, 0).is_some() {
        return false;
    }
    const JITTER_NS: u64 = 2_000_000;
    channel
        .chunks
        .iter()
        .enumerate()
        .any(|(chunk_index, chunk)| {
            chunk.sample_period_ns == 0
                || (0..chunk.sample_count).any(|local| {
                    let actual = source.sample_time_ns(index, chunk_index, local);
                    let expected = chunk.time_base_ns + local * chunk.sample_period_ns;
                    actual.abs_diff(expected) > JITTER_NS
                })
        })
}

fn io_err(err: std::io::Error) -> TelemetryFormatError {
    TelemetryFormatError::Io(err)
}

/// Errors from reading or writing a `.telemetry` file.
#[derive(Debug, thiserror::Error)]
pub enum TelemetryFormatError {
    /// Filesystem failure.
    #[error("telemetry I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// The zip profile or catalog is invalid.
    #[error("invalid .telemetry file: {0}")]
    Invalid(String),
}

impl From<ZipError> for TelemetryFormatError {
    fn from(err: ZipError) -> Self {
        Self::Invalid(err.0)
    }
}

impl std::fmt::Display for ZipError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
