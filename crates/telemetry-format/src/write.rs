//! Lossless writer from any [`TelemetrySource`].

use crate::catalog::{unit_fields, Catalog, CatalogChannel};
use crate::zip::{ZipError, ZipWriter};
use motorsport_telemetry_core::{
    read_source_metadata, schema_hash, AbsoluteTimeRange, AppliedPass, Channel, SampleType,
    SourceIdentity, SourceLapMetadata, SourceOrigin, Span, TelemetrySource, VideoFileRef,
};
use std::collections::HashSet;
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

/// Writes `source` back to its raw conversion: every channel named in the
/// recorded applied-pass outputs is dropped and the pass list is cleared.
///
/// Because passes are lossless — they only ever append the channels they
/// name — this recovers the pre-pass file byte for byte. The source's
/// original `source_format`/`source_path` identity is preserved.
pub fn write_from_source_stripped(
    source: &dyn TelemetrySource,
    dest: impl AsRef<Path>,
) -> Result<(), TelemetryFormatError> {
    let stripped = StrippedSource::new(source);
    let dest = dest.as_ref();
    let file = File::create(dest).map_err(io_err)?;
    write_to(&stripped, crate::FORMAT_VERSION, BufWriter::new(file))
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
            visible: source.channel_visible().get(index).copied().unwrap_or(true),
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
    let timezone = crate::placement::resolve_timezone(source);
    let utc_start_ns = source
        .utc_start_ns()
        .or_else(|| crate::placement::utc_from_metadata(&metadata, &timezone));
    // A converted artifact keeps the identity of the file it was originally
    // converted from; only a true origin stamps its own format and path.
    let origin = source.source_origin();
    let catalog = Catalog {
        format_version,
        identity: metadata.identity.clone(),
        laps,
        valid_laps,
        channels: catalog_channels,
        source_format: origin
            .as_ref()
            .map(|origin| origin.format.clone())
            .filter(|format| !format.is_empty())
            .unwrap_or_else(|| source.format().to_owned()),
        source_path: origin
            .map(|origin| origin.path)
            .filter(|path| !path.is_empty())
            .unwrap_or_else(|| source.path().to_owned()),
        schema_hash: schema_hash(source),
        duration_ns: metadata.duration_ns,
        sample_count: metadata.sample_count,
        channel_count: metadata.channel_count as u32,
        sampled_channel_count: metadata.sampled_channel_count as u32,
        session_hint,
        comment: String::new(),
        clock: source.absolute_time_range(),
        utc_start_ns,
        timezone,
        driver_stints: metadata.driver_stints.clone(),
        videos: linked_videos(source),
        presentation_offset_ns: source.video_presentation_offset_ns(),
        spans: source.spans().to_vec(),
        passes: source.applied_passes().to_vec(),
    };

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

/// Collects the linked video files for `source` exactly as the native
/// catalog records them: hash files that are present on disk, fall back to
/// the source container itself when it is the video, and backfill per-file
/// presentation offsets from the recording-level offset. Shared by the
/// native and MTJ writers so both formats stamp identical linkage.
pub(crate) fn linked_videos(
    source: &dyn TelemetrySource,
) -> Vec<motorsport_telemetry_core::VideoFileRef> {
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
    let offset = source.video_presentation_offset_ns();
    for video in &mut videos {
        if video.presentation_offset_ns.is_none() {
            video.presentation_offset_ns = offset;
        }
    }
    videos
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

/// Index-mapped view of `inner` without pass-derived channels and without
/// the applied-pass provenance.
struct StrippedSource<'a> {
    inner: &'a dyn TelemetrySource,
    /// Inner channel index for each retained channel.
    keep: Vec<usize>,
    channels: Vec<Channel>,
    visible: Vec<bool>,
}

impl<'a> StrippedSource<'a> {
    fn new(inner: &'a dyn TelemetrySource) -> Self {
        let outputs: HashSet<&str> = inner
            .applied_passes()
            .iter()
            .flat_map(|pass| pass.outputs.iter().map(String::as_str))
            .collect();
        let inner_visible = inner.channel_visible();
        let mut keep = Vec::new();
        let mut channels = Vec::new();
        let mut visible = Vec::new();
        for (index, channel) in inner.channels().iter().enumerate() {
            if outputs.contains(channel.name.as_str()) {
                continue;
            }
            keep.push(index);
            channels.push(channel.clone());
            visible.push(inner_visible.get(index).copied().unwrap_or(true));
        }
        Self {
            inner,
            keep,
            channels,
            visible,
        }
    }
}

impl TelemetrySource for StrippedSource<'_> {
    fn path(&self) -> &str {
        self.inner.path()
    }

    fn format(&self) -> &'static str {
        self.inner.format()
    }

    fn channels(&self) -> &[Channel] {
        &self.channels
    }

    fn decode(&self, channel_index: usize, chunk_index: usize, local_index: u64) -> f64 {
        self.inner
            .decode(self.keep[channel_index], chunk_index, local_index)
    }

    fn chunk_bytes(&self, channel_index: usize, chunk_index: usize) -> Option<&[u8]> {
        self.inner
            .chunk_bytes(self.keep[channel_index], chunk_index)
    }

    fn sample_affine(&self, channel_index: usize) -> (f64, f64) {
        self.inner.sample_affine(self.keep[channel_index])
    }

    fn absolute_time_range(&self) -> Option<AbsoluteTimeRange> {
        self.inner.absolute_time_range()
    }

    fn utc_start_ns(&self) -> Option<u64> {
        self.inner.utc_start_ns()
    }

    fn timezone(&self) -> String {
        self.inner.timezone()
    }

    fn channel_visible(&self) -> &[bool] {
        &self.visible
    }

    fn spans(&self) -> &[Span] {
        self.inner.spans()
    }

    fn applied_passes(&self) -> &[AppliedPass] {
        // The whole point: the raw conversion has no passes.
        &[]
    }

    fn source_origin(&self) -> Option<SourceOrigin> {
        self.inner.source_origin()
    }

    fn identity(&self) -> SourceIdentity {
        self.inner.identity()
    }

    fn source_lap_metadata(&self) -> Option<SourceLapMetadata> {
        self.inner.source_lap_metadata()
    }

    fn video_files(&self) -> &[VideoFileRef] {
        self.inner.video_files()
    }

    fn video_presentation_times_ns(&self) -> Option<&[u64]> {
        self.inner.video_presentation_times_ns()
    }

    fn video_frame_count(&self) -> Option<u64> {
        self.inner.video_frame_count()
    }

    fn video_frame_at(&self, time_ns: u64) -> Option<u64> {
        self.inner.video_frame_at(time_ns)
    }

    fn video_presentation_offset_ns(&self) -> Option<i128> {
        self.inner.video_presentation_offset_ns()
    }

    fn sample_time_ns(&self, channel_index: usize, chunk_index: usize, local_index: u64) -> u64 {
        self.inner
            .sample_time_ns(self.keep[channel_index], chunk_index, local_index)
    }

    fn sample_at(&self, channel_index: usize, time_ns: u64, linear: bool) -> Option<f64> {
        self.inner
            .sample_at(self.keep[channel_index], time_ns, linear)
    }
}
