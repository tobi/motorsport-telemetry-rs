//! Lossless writer from any [`TelemetrySource`].

use crate::catalog::{unit_fields, Catalog, CatalogChannel};
use crate::zip::{ZipError, ZipWriter};
use motorsport_telemetry_core::{
    read_source_metadata, schema_hash, Channel, SampleTimes, SampleType, TelemetrySource,
};
use std::collections::HashSet;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

/// Errors raised while writing a `.telemetry` archive.
#[derive(Debug)]
pub enum TelemetryFormatError {
    /// A structural/content problem with the source or requested layout.
    Invalid(String),
    /// An underlying I/O failure.
    Io(std::io::Error),
}

impl From<ZipError> for TelemetryFormatError {
    fn from(err: ZipError) -> Self {
        TelemetryFormatError::Invalid(err.to_string())
    }
}

impl From<std::io::Error> for TelemetryFormatError {
    fn from(err: std::io::Error) -> Self {
        TelemetryFormatError::Io(err)
    }
}

fn io_err(err: std::io::Error) -> TelemetryFormatError {
    TelemetryFormatError::Io(err)
}

impl std::fmt::Display for TelemetryFormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TelemetryFormatError::Invalid(message) => f.write_str(message),
            TelemetryFormatError::Io(err) => write!(f, "io: {err}"),
        }
    }
}

impl std::error::Error for TelemetryFormatError {}

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
    let outputs: HashSet<&str> = source
        .applied_passes()
        .iter()
        .flat_map(|pass| pass.outputs.iter().map(String::as_str))
        .collect();
    let mut view = motorsport_telemetry_core::ViewSource::new(source);
    view.retain(|_, channel| !outputs.contains(channel.name.as_str()));
    view.passes_mut().clear();
    let dest = dest.as_ref();
    let file = File::create(dest).map_err(io_err)?;
    write_to(&view, crate::FORMAT_VERSION, BufWriter::new(file))
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
        let event = is_event(source, index);
        let (values, times, all_native) = collect_channel(source, index, channel)?;
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
            sample_type: if channel.sample_count == 0 || all_native {
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
    let timezone = motorsport_telemetry_core::placement::resolve_timezone(source);
    let utc_start_ns = source
        .utc_start_ns()
        .or_else(|| motorsport_telemetry_core::placement::utc_from_metadata(&metadata, &timezone));
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
) -> Result<(Vec<u8>, Vec<u8>, bool), TelemetryFormatError> {
    if channel.sample_count == 0 {
        return Ok((Vec::new(), Vec::new(), true));
    }
    // Explicit sample stamps come straight from the slice exposed by
    // `sample_times`; grid channels write no time column.
    let stamps = match source.sample_times(index) {
        SampleTimes::Explicit(stamps) => Some(stamps),
        SampleTimes::Grid => None,
    };
    let mut values = Vec::new();
    let mut times = Vec::new();
    let mut all_native = true;
    for (chunk_index, chunk) in channel.chunks.iter().enumerate() {
        if let Some(bytes) = source.chunk_bytes(index, chunk_index) {
            values.extend_from_slice(bytes);
        } else {
            all_native = false;
            for local in 0..chunk.sample_count {
                values.extend_from_slice(&source.decode(index, chunk_index, local).to_le_bytes());
            }
        }
        if let Some(stamps) = stamps {
            let base = chunk.sample_base;
            for local in 0..chunk.sample_count {
                let stamp = stamps.get((base + local) as usize).copied().unwrap_or(0);
                times.extend_from_slice(&stamp.to_le_bytes());
            }
        }
    }
    Ok((values, times, all_native))
}

/// An event channel carries explicit per-sample timestamps rather than a
/// constant-rate grid. That is exactly what `sample_times` reports.
fn is_event(source: &dyn TelemetrySource, index: usize) -> bool {
    matches!(source.sample_times(index), SampleTimes::Explicit(_))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zip::parse_members;
    use crate::NativeRecording;
    use motorsport_telemetry_core::{
        Channel, Chunk, SampleTimes, SampleType, TelemetrySource, UnitSource,
    };

    /// 50 Hz grid period.
    const PERIOD_NS: u64 = 20_000_000;
    const SAMPLE_COUNT: u64 = 4;
    const VALUES: [f64; 4] = [10.0, 11.0, 12.5, 13.0];

    struct TinySource {
        channels: Vec<Channel>,
        values: Vec<Vec<f64>>,
        times: Vec<Vec<u64>>,
        /// When true, the channel exposes its stamps as `SampleTimes::Explicit`.
        explicit: Vec<bool>,
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
        fn sample_times(&self, channel_index: usize) -> SampleTimes<'_> {
            if self.explicit.get(channel_index).copied().unwrap_or(false) {
                if let Some(stamps) = self.times.get(channel_index) {
                    return SampleTimes::Explicit(stamps);
                }
            }
            SampleTimes::Grid
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

    /// Irregular stamps that do not sit on the 50 Hz lattice.
    fn irregular_times(count: u64) -> Vec<u64> {
        (0..count)
            .map(|i| i * PERIOD_NS + if i % 2 == 0 { 5_000_000 } else { 0 })
            .collect()
    }

    fn speed(times: Vec<u64>, explicit: bool) -> TinySource {
        TinySource {
            channels: vec![hz50_channel("Speed", 1, SAMPLE_COUNT)],
            values: vec![VALUES.to_vec()],
            times: vec![times],
            explicit: vec![explicit],
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
        let source = speed(grid_times(SAMPLE_COUNT), false);
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
    fn explicit_stamps_become_event_with_time_column() {
        // A source that exposes SampleTimes::Explicit is an event channel:
        // kind 1, a .time.bin member, and the stamps round-trip verbatim.
        let source = speed(irregular_times(SAMPLE_COUNT), true);
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
    fn grid_source_with_irregular_stamps_stays_grid() {
        // sample_times reports Grid, so even non-lattice stamps do not make
        // an event channel: no time column is written and the reader
        // reconstructs the grid.
        let source = speed(irregular_times(SAMPLE_COUNT), false);
        let (_dir, dest) = write_tiny(&source);
        let header = NativeRecording::read_header(&dest).unwrap();
        assert_eq!(header.channels[0].kind, 0);
        assert!(header.channels[0].time_member.is_empty());
        assert!(!zip_names(&dest)
            .iter()
            .any(|name| name.ends_with(".time.bin")));

        let opened = NativeRecording::open_unchanged(&dest).unwrap();
        assert_first_last_decode(&source, &opened, 0);
        assert_eq!(opened.sample_time_ns(0, 0, 0), 0);
        assert_eq!(opened.sample_time_ns(0, 0, 1), PERIOD_NS);
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
            explicit: vec![false, false, false],
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
        let source = speed(grid_times(SAMPLE_COUNT), false);
        let (_dir, dest) = write_tiny(&source);
        let unchanged = NativeRecording::open_unchanged(&dest).unwrap();
        assert_first_last_decode(&source, &unchanged, 0);
        let opened = NativeRecording::open(&dest).unwrap();
        assert_first_last_decode(&source, &opened, 0);
        assert_eq!(opened.catalog().format_version, crate::FORMAT_VERSION);
    }
}
