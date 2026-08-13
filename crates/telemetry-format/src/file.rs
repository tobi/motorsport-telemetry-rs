//! mmap reader for `.telemetry` files.

use crate::catalog::{decode, Catalog};
use crate::write::TelemetryFormatError;
use crate::zip::{parse_members, read_first_member};
use memmap2::Mmap;
use motorsport_telemetry_core::{
    Channel, FileMetadata, SampleType, SourceIdentity, SourceLapMetadata, TelemetrySource,
};
use std::fs::File;
use std::path::Path;

/// An opened native `.telemetry` recording.
#[derive(Debug)]
pub struct NativeRecording {
    path: String,
    catalog: Catalog,
    channels: Vec<Channel>,
    affines: Vec<(f64, f64)>,
    times: Vec<Option<(usize, usize)>>,
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
    pub fn open(path: impl AsRef<Path>) -> Result<Self, TelemetryFormatError> {
        let path = path.as_ref();
        let display = path.to_string_lossy().into_owned();
        let file = File::open(path)?;
        let mapped = unsafe { Mmap::map(&file)? };
        Self::from_storage(display, Storage::Mapped(mapped))
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
        let mut catalog = catalog;
        catalog.source_path = path.clone();
        Ok(Self {
            path,
            catalog,
            channels,
            affines,
            times,
            data,
        })
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

    fn identity(&self) -> SourceIdentity {
        self.catalog.identity.clone()
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
