//! mmap reader for `.telemetry` files.

use crate::catalog::{decode, Catalog};
use crate::write::TelemetryFormatError;
use crate::zip::{parse_members, read_first_member, ZipWriter};
use memmap2::Mmap;
use motorsport_telemetry_core::{
    Channel, FileMetadata, SampleType, SourceIdentity, SourceLapMetadata, TelemetrySource,
    VideoFileRef,
};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;

/// An opened native `.telemetry` recording.
#[derive(Debug)]
pub struct NativeRecording {
    path: String,
    catalog: Catalog,
    channels: Vec<Channel>,
    affines: Vec<(f64, f64)>,
    times: Vec<Option<(usize, usize)>>,
    video_times: Option<(usize, usize)>,
    data: Storage,
}

#[derive(Debug)]
enum Storage {
    Mapped(Mmap),
    Owned(Box<[u8]>),
}

impl std::ops::Deref for Storage {
    type Target = [u8];
    fn deref(&self) -> &Self::Target {
        match self {
            Self::Mapped(value) => value,
            Self::Owned(value) => value,
        }
    }
}

impl NativeRecording {
    /// Memory-maps a `.telemetry` file for sample access.
    ///
    /// Older writable files are migrated in place to [`crate::FORMAT_VERSION`].
    /// Read-only files stay as they are; [`Self::needs_update`] reports them.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, TelemetryFormatError> {
        let path = path.as_ref();
        let opened = Self::open_unchanged(path)?;
        if !opened.needs_update() || !writable(path) {
            return Ok(opened);
        }
        opened.rewrite_migrated(path)?;
        Self::open_unchanged(path)
    }

    /// Maps the file without rewriting an older catalog.
    pub fn open_unchanged(path: impl AsRef<Path>) -> Result<Self, TelemetryFormatError> {
        let path = path.as_ref();
        let display = path.to_string_lossy().into_owned();
        let file = File::open(path)?;
        let mapped = unsafe { Mmap::map(&file)? };
        Self::from_storage(display, Storage::Mapped(mapped))
    }

    fn rewrite_migrated(&self, path: &Path) -> Result<(), TelemetryFormatError> {
        let mut catalog = self.catalog.clone();
        crate::migrate::apply(&mut catalog);
        for lap in &mut catalog.laps {
            if lap.first_video_frame.is_none() {
                lap.first_video_frame = self.video_frame_at(lap.start_ns);
            }
        }
        let members = parse_members(&self.data)?;
        let tmp = path.with_extension("telemetry.tmp");
        let result = (|| {
            let file = File::create(&tmp)?;
            let mut zip = ZipWriter::new(std::io::BufWriter::new(file));
            zip.write_member("metadata.fb", &crate::catalog::encode(&catalog)?)?;
            for member in &members {
                if member.name == "metadata.fb" {
                    continue;
                }
                let start = member.offset as usize;
                let end = start + member.size as usize;
                let bytes = self.data.get(start..end).ok_or_else(|| {
                    TelemetryFormatError::Invalid(format!("{} is out of range", member.name))
                })?;
                zip.write_member(&member.name, bytes)?;
            }
            let mut writer = zip.finish()?;
            writer.flush()?;
            drop(writer);
            fs::rename(&tmp, path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result
    }

    /// Parses an owned `.telemetry` buffer.
    pub fn from_bytes(
        path: impl Into<String>,
        data: Vec<u8>,
    ) -> Result<Self, TelemetryFormatError> {
        Self::from_storage(path.into(), Storage::Owned(data.into_boxed_slice()))
    }

    /// Reads only `metadata.fb`. Cost is independent of channel payload size.
    pub fn read_header(path: impl AsRef<Path>) -> Result<Catalog, TelemetryFormatError> {
        Ok(decode(&Self::read_catalog_bytes(path)?)?)
    }

    /// Catalog format version from `metadata.fb` only.
    pub fn read_format_version(path: impl AsRef<Path>) -> Result<u16, TelemetryFormatError> {
        Ok(crate::catalog::decode_format_version(
            &Self::read_catalog_bytes(path)?,
        )?)
    }

    /// True when this recording was written by an older catalog.
    pub fn needs_update(&self) -> bool {
        crate::needs_update(self.catalog.format_version)
    }

    /// Header-only metadata. Does not map or checksum channel members.
    pub fn read_metadata(path: impl AsRef<Path>) -> Result<FileMetadata, TelemetryFormatError> {
        Ok(Self::read_header(path)?.to_file_metadata())
    }

    /// Stored laps from the catalog. Does not unpack the channel directory.
    pub fn read_laps(
        path: impl AsRef<Path>,
    ) -> Result<Vec<motorsport_telemetry_core::LapMetadata>, TelemetryFormatError> {
        let bytes = Self::read_catalog_bytes(path)?;
        Ok(crate::catalog::decode_laps(&bytes)?)
    }

    /// Stored complete-lap count. A single scalar in the catalog root.
    pub fn read_valid_laps(path: impl AsRef<Path>) -> Result<u32, TelemetryFormatError> {
        let bytes = Self::read_catalog_bytes(path)?;
        Ok(crate::catalog::decode_valid_laps(&bytes)?)
    }

    fn read_catalog_bytes(path: impl AsRef<Path>) -> Result<Vec<u8>, TelemetryFormatError> {
        let mut file = File::open(path)?;
        let (name, bytes) = read_first_member(&mut file)?;
        if name != "metadata.fb" {
            return Err(TelemetryFormatError::Invalid(
                "first zip member is not metadata.fb".into(),
            ));
        }
        Ok(bytes)
    }

    /// Catalog already loaded with this recording.
    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    /// Format-neutral metadata copied out of the catalog.
    pub fn metadata(&self) -> FileMetadata {
        self.catalog.to_file_metadata()
    }

    fn from_storage(path: String, data: Storage) -> Result<Self, TelemetryFormatError> {
        let members = parse_members(&data)?;
        let meta = members
            .iter()
            .find(|member| member.name == "metadata.fb")
            .ok_or_else(|| TelemetryFormatError::Invalid("missing metadata.fb".into()))?;
        let start = meta.offset as usize;
        let end = start + meta.size as usize;
        let catalog =
            decode(data.get(start..end).ok_or_else(|| {
                TelemetryFormatError::Invalid("metadata.fb is out of range".into())
            })?)?;
        let by_name = members
            .iter()
            .map(|member| (member.name.as_str(), member))
            .collect::<std::collections::HashMap<_, _>>();
        let mut channels = Vec::with_capacity(catalog.channels.len());
        let mut affines = Vec::with_capacity(catalog.channels.len());
        let mut times = Vec::with_capacity(catalog.channels.len());
        for channel in &catalog.channels {
            let member = by_name.get(channel.member.as_str()).copied();
            let mut chunks = channel.chunks.clone();
            if let Some(member) = member {
                let mut cursor = member.offset;
                let width = channel.sample_type.byte_width() as u64;
                for chunk in &mut chunks {
                    chunk.data_ptr = cursor;
                    cursor = cursor.saturating_add(chunk.sample_count.saturating_mul(width));
                }
            }
            times.push(
                by_name
                    .get(channel.time_member.as_str())
                    .copied()
                    .map(|member| (member.offset as usize, member.size as usize)),
            );
            channels.push(Channel {
                id: channel.id,
                name: channel.name.clone(),
                unit: channel.unit_raw.clone(),
                unit_source: channel.unit_source,
                sample_type: channel.sample_type,
                chunks,
                sample_count: channel.sample_count,
                duration_ns: channel.duration_ns,
            });
            affines.push((channel.scale, channel.bias));
        }
        let video_times = by_name
            .get("video_frames.bin")
            .map(|member| (member.offset as usize, member.size as usize));
        let mut catalog = catalog;
        catalog.source_path = path.clone();
        if let Some((_, size)) = video_times {
            if let Some(video) = catalog.videos.first_mut() {
                video.frame_count = (size / 8) as u64;
            }
        }
        if let Some(offset) = catalog.presentation_offset_ns {
            for video in &mut catalog.videos {
                if video.presentation_offset_ns.is_none() {
                    video.presentation_offset_ns = Some(offset);
                }
            }
        }
        Ok(Self {
            path,
            catalog,
            channels,
            affines,
            times,
            video_times,
            data,
        })
    }

    fn video_time_at(&self, index: usize) -> Option<u64> {
        let (start, size) = self.video_times?;
        let at = start + index * 8;
        (at + 8 <= start + size)
            .then(|| u64::from_le_bytes(self.data[at..at + 8].try_into().unwrap()))
    }
}

impl TelemetrySource for NativeRecording {
    fn path(&self) -> &str {
        &self.path
    }

    fn format(&self) -> &'static str {
        match self.catalog.source_format.as_str() {
            "aimd" => "aimd",
            "pds" => "pds",
            "motec" => "motec",
            "vbo" => "vbo",
            _ => "telemetry",
        }
    }

    fn channels(&self) -> &[Channel] {
        &self.channels
    }

    fn decode(&self, channel_index: usize, chunk_index: usize, local_index: u64) -> f64 {
        let channel = &self.channels[channel_index];
        let chunk = &channel.chunks[chunk_index];
        let width = channel.sample_type.byte_width();
        let offset = chunk.data_ptr as usize + local_index as usize * width;
        let raw = match channel.sample_type {
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
        };
        let (scale, bias) = self.affines[channel_index];
        raw.mul_add(scale, bias)
    }

    fn chunk_bytes(&self, channel_index: usize, chunk_index: usize) -> Option<&[u8]> {
        let channel = self.channels.get(channel_index)?;
        let chunk = channel.chunks.get(chunk_index)?;
        let start = usize::try_from(chunk.data_ptr).ok()?;
        let len = usize::try_from(chunk.sample_count).ok()? * channel.sample_type.byte_width();
        self.data.get(start..start + len)
    }

    fn sample_affine(&self, channel_index: usize) -> (f64, f64) {
        self.affines
            .get(channel_index)
            .copied()
            .unwrap_or((1.0, 0.0))
    }

    fn sample_time_ns(&self, channel_index: usize, chunk_index: usize, local_index: u64) -> u64 {
        if let Some((start, size)) = self.times[channel_index] {
            let at = start + local_index as usize * 8;
            if at + 8 <= start + size {
                return u64::from_le_bytes(self.data[at..at + 8].try_into().unwrap());
            }
        }
        let chunk = &self.channels[channel_index].chunks[chunk_index];
        chunk.time_base_ns + local_index * chunk.sample_period_ns
    }

    fn sample_at(&self, channel_index: usize, time_ns: u64, linear: bool) -> Option<f64> {
        let Some((start, size)) = self.times.get(channel_index).copied().flatten() else {
            return default_dense_sample_at(self, channel_index, time_ns, linear);
        };
        let count = size / 8;
        if count == 0 {
            return None;
        }
        let time_at = |index: usize| {
            u64::from_le_bytes(
                self.data[start + index * 8..start + index * 8 + 8]
                    .try_into()
                    .unwrap(),
            )
        };
        let mut lo = 0usize;
        let mut hi = count;
        while lo < hi {
            let mid = (lo + hi) / 2;
            if time_at(mid) <= time_ns {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        let lower = lo.saturating_sub(1).min(count - 1);
        let a = self.decode(channel_index, 0, lower as u64);
        if !linear || self.channels[channel_index].uses_step_interpolation() || lo >= count {
            return Some(a);
        }
        let interval = time_at(lo).saturating_sub(time_at(lower));
        if interval == 0 {
            return Some(a);
        }
        let fraction = time_ns.saturating_sub(time_at(lower)) as f64 / interval as f64;
        Some(a + (self.decode(channel_index, 0, lo as u64) - a) * fraction)
    }

    fn identity(&self) -> SourceIdentity {
        self.catalog.identity.clone()
    }

    fn video_files(&self) -> &[VideoFileRef] {
        &self.catalog.videos
    }

    fn video_presentation_times_ns(&self) -> Option<&[u64]> {
        let (start, size) = self.video_times?;
        let bytes = self.data.get(start..start + size)?;
        if bytes.len() % 8 != 0 {
            return None;
        }
        Some(unsafe { std::slice::from_raw_parts(bytes.as_ptr().cast::<u64>(), bytes.len() / 8) })
    }

    fn video_frame_count(&self) -> Option<u64> {
        self.video_times
            .map(|(_, size)| (size / 8) as u64)
            .filter(|count| *count > 0)
    }

    fn video_frame_at(&self, time_ns: u64) -> Option<u64> {
        let (_, size) = self.video_times?;
        let count = size / 8;
        if count == 0 {
            return None;
        }
        let time_ns = self.video_presentation_time_ns(time_ns)?;
        let mut lo = 0usize;
        let mut hi = count;
        while lo < hi {
            let mid = (lo + hi) / 2;
            match self.video_time_at(mid) {
                Some(stamp) if stamp <= time_ns => lo = mid + 1,
                _ => hi = mid,
            }
        }
        Some(lo.saturating_sub(1) as u64)
    }

    fn video_presentation_offset_ns(&self) -> Option<i128> {
        self.catalog.presentation_offset_ns
    }

    fn source_lap_metadata(&self) -> Option<SourceLapMetadata> {
        Some(SourceLapMetadata {
            laps: self.catalog.laps.clone(),
            fastest_lap: self
                .catalog
                .laps
                .iter()
                .filter(|lap| lap.complete)
                .min_by_key(|lap| lap.duration_ns)
                .cloned(),
        })
    }

    fn absolute_time_range(&self) -> Option<motorsport_telemetry_core::AbsoluteTimeRange> {
        self.catalog.clock.clone()
    }
}

fn default_dense_sample_at(
    source: &NativeRecording,
    channel_index: usize,
    time_ns: u64,
    linear: bool,
) -> Option<f64> {
    let channel = source.channels().get(channel_index)?;
    if time_ns >= channel.duration_ns || channel.chunks.is_empty() {
        return None;
    }
    let chunk_index = channel.chunks.partition_point(|chunk| {
        chunk
            .time_base_ns
            .saturating_add(chunk.sample_count.saturating_mul(chunk.sample_period_ns))
            <= time_ns
    });
    let chunk = channel.chunks.get(chunk_index)?;
    let sample = (time_ns.saturating_sub(chunk.time_base_ns) / chunk.sample_period_ns)
        .min(chunk.sample_count - 1);
    let a = source.decode(channel_index, chunk_index, sample);
    if !linear || channel.uses_step_interpolation() {
        return Some(a);
    }
    if sample + 1 >= chunk.sample_count {
        return Some(a);
    }
    let interval = chunk.sample_period_ns;
    if interval == 0 {
        return Some(a);
    }
    let fraction =
        time_ns.saturating_sub(chunk.time_base_ns + sample * interval) as f64 / interval as f64;
    Some(a + (source.decode(channel_index, chunk_index, sample + 1) - a) * fraction)
}

fn writable(path: &Path) -> bool {
    OpenOptions::new().write(true).open(path).is_ok()
}
