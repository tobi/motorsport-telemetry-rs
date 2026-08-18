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
            sample_type: if (values.is_empty() && channel.sample_count == 0)
                || source.chunk_bytes(index, 0).is_some()
            {
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
            labels: if source.channel_display(index).plot.is_trace() {
                source.channel_labels(index).to_vec()
            } else {
                Vec::new()
            },
            display: source.channel_display(index),
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

    fn diagnostics(&self) -> &[motorsport_telemetry_core::Diagnostic] {
        self.inner.diagnostics()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zip::parse_members;
    use crate::NativeRecording;
    use motorsport_telemetry_core::{Channel, Chunk, SampleType, TelemetrySource, UnitSource};

    /// 50 Hz. Same threshold as [`is_event`].
    const PERIOD_NS: u64 = 20_000_000;
    const JITTER_NS: u64 = 2_000_000;
    const SAMPLE_COUNT: u64 = 4;
    const VALUES: [f64; 4] = [10.0, 11.0, 12.5, 13.0];

    struct TinySource {
        channels: Vec<Channel>,
        values: Vec<Vec<f64>>,
        times: Vec<Vec<u64>>,
    }

    impl TelemetrySource for TinySource {
        fn path(&self) -> &str {
            "tiny"
        }
        fn format(&self) -> &'static str {
            "pds"
        }
        fn channels(&self) -> &[Channel] {
            &self.channels
        }
        fn decode(&self, channel_index: usize, chunk_index: usize, local_index: u64) -> f64 {
            let base = self.channels[channel_index].chunks[chunk_index].sample_base;
            self.values[channel_index][(base + local_index) as usize]
        }
        fn sample_time_ns(
            &self,
            channel_index: usize,
            chunk_index: usize,
            local_index: u64,
        ) -> u64 {
            let base = self.channels[channel_index].chunks[chunk_index].sample_base;
            self.times[channel_index][(base + local_index) as usize]
        }
    }

    fn hz50_channel(name: &str, id: u32, count: u64) -> Channel {
        Channel {
            id,
            name: name.into(),
            unit: "km/h".into(),
            unit_source: UnitSource::Declared,
            sample_type: SampleType::F64,
            chunks: vec![Chunk {
                sample_period_ns: PERIOD_NS,
                sample_count: count,
                data_ptr: 0,
                sample_base: 0,
                time_base_ns: 0,
            }],
            sample_count: count,
            duration_ns: count * PERIOD_NS,
        }
    }

    fn empty_channel(name: &str, id: u32) -> Channel {
        Channel {
            id,
            name: name.into(),
            unit: String::new(),
            unit_source: UnitSource::Unknown,
            sample_type: SampleType::F64,
            chunks: Vec::new(),
            sample_count: 0,
            duration_ns: 0,
        }
    }

    fn grid_times(count: u64) -> Vec<u64> {
        (0..count).map(|i| i * PERIOD_NS).collect()
    }

    /// Alternate `+amp` / `-amp` around the 50 Hz lattice.
    fn jittered_times(count: u64, amp: u64) -> Vec<u64> {
        (0..count)
            .map(|i| {
                let expected = i * PERIOD_NS;
                if i % 2 == 0 {
                    expected + amp
                } else {
                    expected - amp
                }
            })
            .collect()
    }

    fn speed(times: Vec<u64>) -> TinySource {
        TinySource {
            channels: vec![hz50_channel("Speed", 1, SAMPLE_COUNT)],
            values: vec![VALUES.to_vec()],
            times: vec![times],
        }
    }

    fn write_tiny(source: &TinySource) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("run.telemetry");
        write_from_source(source, &dest).unwrap();
        (dir, dest)
    }

    fn zip_names(path: &std::path::Path) -> Vec<String> {
        parse_members(&std::fs::read(path).unwrap())
            .unwrap()
            .into_iter()
            .map(|member| member.name)
            .collect()
    }

    fn assert_first_last_decode(source: &TinySource, opened: &NativeRecording, channel: usize) {
        let last = source.channels[channel].sample_count - 1;
        assert_eq!(opened.decode(channel, 0, 0), source.decode(channel, 0, 0));
        assert_eq!(
            opened.decode(channel, 0, last),
            source.decode(channel, 0, last)
        );
    }

    #[test]
    fn regular_50hz_writes_kind0_without_time_member() {
        let source = speed(grid_times(SAMPLE_COUNT));
        let (_dir, dest) = write_tiny(&source);
        let header = NativeRecording::read_header(&dest).unwrap();
        assert_eq!(header.channels.len(), 1);
        assert_eq!(header.channels[0].kind, 0);
        assert!(header.channels[0].time_member.is_empty());
        assert_eq!(header.channels[0].member, "channels/0000.bin");
        let names = zip_names(&dest);
        assert!(names.contains(&"channels/0000.bin".into()));
        assert!(!names.iter().any(|name| name.ends_with(".time.bin")));

        let opened = NativeRecording::open_unchanged(&dest).unwrap();
        assert_eq!(opened.decode(0, 0, 0), 10.0);
        assert_eq!(opened.decode(0, 0, 3), 13.0);
        assert_eq!(opened.sample_time_ns(0, 0, 0), 0);
        assert_eq!(opened.sample_time_ns(0, 0, 3), 3 * PERIOD_NS);
    }

    #[test]
    fn sub_2ms_jitter_still_regular() {
        let amp = 400_000;
        assert!(amp < JITTER_NS);
        let source = speed(jittered_times(SAMPLE_COUNT, amp));
        let (_dir, dest) = write_tiny(&source);
        let header = NativeRecording::read_header(&dest).unwrap();
        assert_eq!(header.channels[0].kind, 0);
        assert!(header.channels[0].time_member.is_empty());
        assert!(!zip_names(&dest)
            .iter()
            .any(|name| name.ends_with(".time.bin")));

        // Regular write discards per-sample times; readers reconstruct the lattice.
        let opened = NativeRecording::open_unchanged(&dest).unwrap();
        assert_first_last_decode(&source, &opened, 0);
        assert_eq!(opened.sample_time_ns(0, 0, 0), 0);
        assert_eq!(opened.sample_time_ns(0, 0, 1), PERIOD_NS);
        assert_ne!(
            opened.sample_time_ns(0, 0, 0),
            source.sample_time_ns(0, 0, 0)
        );
    }

    #[test]
    fn over_2ms_jitter_becomes_event_with_time_column() {
        let amp = 5_000_000;
        assert!(amp > JITTER_NS);
        let source = speed(jittered_times(SAMPLE_COUNT, amp));
        let (_dir, dest) = write_tiny(&source);
        let header = NativeRecording::read_header(&dest).unwrap();
        assert_eq!(header.channels[0].kind, 1);
        assert_eq!(header.channels[0].time_member, "channels/0000.time.bin");
        let names = zip_names(&dest);
        assert!(names.contains(&"channels/0000.bin".into()));
        assert!(names.contains(&"channels/0000.time.bin".into()));

        let opened = NativeRecording::open_unchanged(&dest).unwrap();
        assert_first_last_decode(&source, &opened, 0);
        for local in 0..SAMPLE_COUNT {
            assert_eq!(
                opened.sample_time_ns(0, 0, local),
                source.sample_time_ns(0, 0, local)
            );
        }
    }

    #[test]
    fn zero_sample_channels_have_no_payload_members() {
        let source = TinySource {
            channels: vec![
                empty_channel("Empty", 1),
                hz50_channel("Speed", 2, SAMPLE_COUNT),
                empty_channel("AlsoEmpty", 3),
            ],
            values: vec![Vec::new(), VALUES.to_vec(), Vec::new()],
            times: vec![Vec::new(), grid_times(SAMPLE_COUNT), Vec::new()],
        };
        let (_dir, dest) = write_tiny(&source);
        let header = NativeRecording::read_header(&dest).unwrap();
        assert_eq!(header.channels.len(), 3);
        assert_eq!(header.channels[0].sample_count, 0);
        assert_eq!(header.channels[2].sample_count, 0);
        assert_eq!(header.channels[0].member, "channels/0000.bin");
        assert_eq!(header.channels[1].member, "channels/0001.bin");
        assert_eq!(header.channels[2].member, "channels/0002.bin");
        let names = zip_names(&dest);
        assert!(!names.contains(&"channels/0000.bin".into()));
        assert!(names.contains(&"channels/0001.bin".into()));
        assert!(!names.contains(&"channels/0002.bin".into()));
        assert!(!names.iter().any(|name| name.ends_with(".time.bin")));
    }

    #[test]
    fn open_unchanged_decodes_first_and_last_samples() {
        let source = speed(grid_times(SAMPLE_COUNT));
        let (_dir, dest) = write_tiny(&source);
        let unchanged = NativeRecording::open_unchanged(&dest).unwrap();
        assert_first_last_decode(&source, &unchanged, 0);
        let opened = NativeRecording::open(&dest).unwrap();
        assert_first_last_decode(&source, &opened, 0);
        assert_eq!(opened.catalog().format_version, crate::FORMAT_VERSION);
    }
}
