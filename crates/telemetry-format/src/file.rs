//! mmap reader for `.telemetry` files.

use crate::catalog::{decode, Catalog};
use crate::write::TelemetryFormatError;
use crate::zip::{parse_members, read_first_member, ZipWriter};
use motorsport_telemetry_core::{
    storage::Storage, AppliedPass, Channel, Diagnostic, FileMetadata, SampleTimes, SourceIdentity,
    SourceLapMetadata, SourceOrigin, Span, TelemetrySource, VideoFileRef,
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
    channel_visible: Vec<bool>,
    affines: Vec<(f64, f64)>,
    times: Vec<Option<(usize, usize)>>,
    video_times: Option<(usize, usize)>,
    diagnostics: Vec<Diagnostic>,
    data: Storage,
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
        let storage = Storage::open(path)?;
        Self::from_storage(display, storage)
    }

    fn rewrite_migrated(&self, path: &Path) -> Result<(), TelemetryFormatError> {
        let mut catalog = self.catalog.clone();
        crate::migrate::apply(&mut catalog)
            .map_err(|err| TelemetryFormatError::Invalid(err.to_string()))?;
        // v4+ requires `utc_start_ns` and `timezone`. Pre-v4 catalogs have
        // neither; recover them from the recording's clocks and venue through
        // core placement (never inventing a value) instead of leaving the
        // migrated catalog without absolute placement.
        if catalog.format_version >= 4 {
            let metadata = motorsport_telemetry_core::read_source_metadata(self);
            if catalog.timezone.is_empty() {
                catalog.timezone = metadata.timezone.clone();
            }
            if catalog.utc_start_ns.is_none() {
                catalog.utc_start_ns = metadata.utc_start_ns;
            }
        }
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
                let start = usize::try_from(member.offset).map_err(|_| {
                    TelemetryFormatError::Invalid(format!("{} offset overflows usize", member.name))
                })?;
                let size = usize::try_from(member.size).map_err(|_| {
                    TelemetryFormatError::Invalid(format!("{} size overflows usize", member.name))
                })?;
                let end = start.checked_add(size).ok_or_else(|| {
                    TelemetryFormatError::Invalid(format!("{} range overflows usize", member.name))
                })?;
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
        Self::from_storage(path.into(), Storage::from_vec(data))
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
        let path = path.as_ref();
        Ok(Self::read_header(path)?.to_file_metadata(&path.to_string_lossy()))
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
        self.catalog.to_file_metadata(&self.path)
    }

    /// Processing passes recorded as applied to this recording, in order.
    pub fn passes(&self) -> &[AppliedPass] {
        &self.catalog.passes
    }

    /// Interval annotations stored in the catalog. Same model as JSONL spans.
    pub fn spans(&self) -> &[Span] {
        &self.catalog.spans
    }

    /// Default visibility of each sample channel, aligned with [`TelemetrySource::channels`].
    pub fn channel_visible(&self) -> &[bool] {
        &self.channel_visible
    }

    fn from_storage(path: String, data: Storage) -> Result<Self, TelemetryFormatError> {
        let members = parse_members(&data)?;
        let meta = members
            .iter()
            .find(|member| member.name == "metadata.fb")
            .ok_or_else(|| TelemetryFormatError::Invalid("missing metadata.fb".into()))?;
        let start = usize::try_from(meta.offset).map_err(|_| {
            TelemetryFormatError::Invalid("metadata.fb offset overflows usize".into())
        })?;
        let size = usize::try_from(meta.size).map_err(|_| {
            TelemetryFormatError::Invalid("metadata.fb size overflows usize".into())
        })?;
        let end = start.checked_add(size).ok_or_else(|| {
            TelemetryFormatError::Invalid("metadata.fb range overflows usize".into())
        })?;
        let catalog =
            decode(data.get(start..end).ok_or_else(|| {
                TelemetryFormatError::Invalid("metadata.fb is out of range".into())
            })?)?;
        let by_name = members
            .iter()
            .map(|member| (member.name.as_str(), member))
            .collect::<std::collections::HashMap<_, _>>();
        let mut diagnostics = Vec::new();
        let mut channels = Vec::with_capacity(catalog.channels.len());
        let mut affines = Vec::with_capacity(catalog.channels.len());
        let mut times = Vec::with_capacity(catalog.channels.len());
        for channel in &catalog.channels {
            let member = by_name.get(channel.member.as_str()).copied();
            let mut chunks = channel.chunks.clone();
            let mut sample_count = channel.sample_count;
            let mut duration_ns = channel.duration_ns;
            if let Some(member) = member {
                let width = channel.sample_type.byte_width() as u64;
                let required = chunks
                    .iter()
                    .map(|chunk| chunk.sample_count.saturating_mul(width))
                    .fold(0u64, u64::saturating_add);
                let mut cursor = member.offset;
                if required <= member.size {
                    for chunk in &mut chunks {
                        chunk.data_ptr = cursor;
                        cursor = cursor.saturating_add(chunk.sample_count.saturating_mul(width));
                    }
                } else {
                    let declared_chunks = chunks.len();
                    let mut remaining = member.size;
                    let mut actual_count = 0u64;
                    let mut actual_duration = 0u64;
                    let mut available = Vec::with_capacity(chunks.len());
                    for mut chunk in chunks {
                        let count = chunk.sample_count.min(remaining / width);
                        if count == 0 {
                            break;
                        }
                        chunk.data_ptr = cursor;
                        chunk.sample_base = actual_count;
                        chunk.sample_count = count;
                        cursor = cursor.saturating_add(count.saturating_mul(width));
                        remaining = remaining.saturating_sub(count.saturating_mul(width));
                        actual_count = actual_count.saturating_add(count);
                        actual_duration = actual_duration.max(
                            chunk
                                .time_base_ns
                                .saturating_add(count.saturating_mul(chunk.sample_period_ns)),
                        );
                        available.push(chunk);
                    }
                    chunks = available;
                    sample_count = actual_count;
                    duration_ns = actual_duration;
                    diagnostics.push(
                        Diagnostic::warning(
                            "telemetry.member_truncated",
                            format!(
                                "channel \"{}\" needs {required} bytes across {declared_chunks} \
                                 chunks, but member \"{}\" holds {}; retained {} complete samples",
                                channel.name, channel.member, member.size, actual_count,
                            ),
                        )
                        .with_channel(&channel.name),
                    );
                }
            } else if !channel.member.is_empty() {
                // Never leave stale pointers decodeable: offset zero is the zip
                // header, not channel data. Keep the channel metadata but make
                // the unavailable payload explicitly empty.
                let dropped = chunks.len();
                chunks.clear();
                sample_count = 0;
                duration_ns = 0;
                diagnostics.push(
                    Diagnostic::warning(
                        "telemetry.member_missing",
                        format!(
                            "channel \"{}\" references member \"{}\" which is absent from the \
                             archive; {dropped} chunks were dropped",
                            channel.name, channel.member,
                        ),
                    )
                    .with_channel(&channel.name),
                );
            }
            let mut time_member =
                by_name
                    .get(channel.time_member.as_str())
                    .copied()
                    .and_then(|member| {
                        let offset = usize::try_from(member.offset).ok()?;
                        let size = usize::try_from(member.size).ok()?;
                        Some((offset, size))
                    });
            if time_member.is_none() && !channel.time_member.is_empty() {
                diagnostics.push(
                    Diagnostic::warning(
                        "telemetry.member_missing",
                        format!(
                            "channel \"{}\" references timestamp member \"{}\" which is absent \
                             from the archive; irregular timestamps were dropped",
                            channel.name, channel.time_member,
                        ),
                    )
                    .with_channel(&channel.name),
                );
            } else if let Some((offset, size)) = time_member {
                // The timestamp member is a little-endian u64 STORE. The zip
                // profile 64-byte-aligns the payload; confirm the actual byte
                // address is 8-byte aligned (mmap base + offset) and that the
                // size is a whole number of u64s. A misaligned or odd-sized
                // member is not safe to reinterpret as &[u64], so drop it to
                // the grid model and report the recovery.
                let ptr_aligned = (data.as_ptr() as usize).wrapping_add(offset) % 8 == 0;
                let required = sample_count.saturating_mul(8);
                if !ptr_aligned || size % 8 != 0 {
                    diagnostics.push(
                        Diagnostic::warning(
                            "telemetry.member_unaligned",
                            format!(
                                "channel \"{}\" timestamp member \"{}\" is not 8-byte aligned \
                                 or not a multiple of 8 bytes; irregular timestamps were dropped",
                                channel.name, channel.time_member,
                            ),
                        )
                        .with_channel(&channel.name),
                    );
                    time_member = None;
                } else if (size as u64) < required {
                    diagnostics.push(
                        Diagnostic::warning(
                            "telemetry.member_truncated",
                            format!(
                                "channel \"{}\" needs {required} timestamp bytes, but member \
                                 \"{}\" holds {size}; irregular timestamps were dropped",
                                channel.name, channel.time_member,
                            ),
                        )
                        .with_channel(&channel.name),
                    );
                    time_member = None;
                }
            }
            times.push(time_member);
            channels.push(Channel {
                id: channel.id,
                name: channel.name.clone(),
                unit: channel.unit_raw.clone(),
                unit_source: channel.unit_source,
                sample_type: channel.sample_type,
                chunks,
                sample_count,
                duration_ns,
            });
            affines.push((channel.scale, channel.bias));
        }
        let mut video_times = by_name
            .get("video_frames.bin")
            .map(|member| (member.offset as usize, member.size as usize));
        // `catalog.source_path` keeps the original vendor path from disk; the
        // opened file's own path lives in `self.path`.
        let mut catalog = catalog;
        if let Some((_, size)) = video_times {
            if size % 8 != 0 {
                diagnostics.push(Diagnostic::warning(
                    "telemetry.video_frames_unusable",
                    format!(
                        "video_frames.bin is {size} bytes, not a multiple of 8; \
                         video frame linkage dropped",
                    ),
                ));
                video_times = None;
            } else if let Some(video) = catalog.videos.first_mut() {
                video.frame_count = (size / 8) as u64;
            }
        } else if !catalog.videos.is_empty() {
            diagnostics.push(Diagnostic::warning(
                "telemetry.video_frames_unusable",
                format!(
                    "video_frames.bin is absent; {} video handle(s) have no frame linkage",
                    catalog.videos.len(),
                ),
            ));
        }
        if let Some(offset) = catalog.presentation_offset_ns {
            for video in &mut catalog.videos {
                if video.presentation_offset_ns.is_none() {
                    video.presentation_offset_ns = Some(offset);
                }
            }
        }
        // v4+ requires utc_start_ns and timezone. If they are missing the
        // catalog decoder silently defaulted them; report that.
        if catalog.format_version >= 4 && catalog.utc_start_ns.is_none() {
            diagnostics.push(Diagnostic::warning(
                "telemetry.catalog_field_defaulted",
                format!(
                    "utc_start_ns is required by catalog v{} but is absent; defaulted to None",
                    catalog.format_version
                ),
            ));
        }
        if catalog.format_version >= 4 && catalog.timezone.is_empty() {
            diagnostics.push(Diagnostic::warning(
                "telemetry.catalog_field_defaulted",
                format!(
                    "timezone is required by catalog v{} but is empty; defaulted to \"\"",
                    catalog.format_version
                ),
            ));
        }
        let channel_visible = catalog.channels.iter().map(|ch| ch.visible).collect();
        Ok(Self {
            path,
            catalog,
            channels,
            channel_visible,
            affines,
            times,
            video_times,
            diagnostics,
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

    fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    fn decode(&self, channel_index: usize, chunk_index: usize, local_index: u64) -> f64 {
        let Some(channel) = self.channels.get(channel_index) else {
            return f64::NAN;
        };
        let Some(chunk) = channel.chunks.get(chunk_index) else {
            return f64::NAN;
        };
        let width = channel.sample_type.byte_width();
        let raw = motorsport_telemetry_core::sample_bytes(&self.data, chunk, local_index, width)
            .and_then(|bytes| channel.sample_type.decode_le(bytes))
            .unwrap_or(f64::NAN);
        let (scale, bias) = self
            .affines
            .get(channel_index)
            .copied()
            .unwrap_or((1.0, 0.0));
        raw.mul_add(scale, bias)
    }

    fn chunk_bytes(&self, channel_index: usize, chunk_index: usize) -> Option<&[u8]> {
        let channel = self.channels.get(channel_index)?;
        let chunk = channel.chunks.get(chunk_index)?;
        motorsport_telemetry_core::chunk_bytes(&self.data, chunk, channel.sample_type.byte_width())
    }

    fn sample_affine(&self, channel_index: usize) -> (f64, f64) {
        self.affines
            .get(channel_index)
            .copied()
            .unwrap_or((1.0, 0.0))
    }

    fn sample_times(&self, channel_index: usize) -> SampleTimes<'_> {
        match self.times.get(channel_index).copied().flatten() {
            Some((start, size)) if size % 8 == 0 => {
                // Alignment and whole-u64 size were verified at open; the
                // payload is a little-endian u64 STORE, reinterpreted as
                // &[u64] without copying.
                let bytes = &self.data[start..start + size];
                let stamps = unsafe {
                    std::slice::from_raw_parts(bytes.as_ptr().cast::<u64>(), bytes.len() / 8)
                };
                SampleTimes::Explicit(stamps)
            }
            _ => SampleTimes::Grid,
        }
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

    fn utc_start_ns(&self) -> Option<u64> {
        self.catalog.utc_start_ns
    }

    fn timezone(&self) -> String {
        self.catalog.timezone.clone()
    }

    fn channel_visible(&self) -> &[bool] {
        &self.channel_visible
    }

    fn spans(&self) -> &[Span] {
        &self.catalog.spans
    }

    fn channel_labels(&self, channel_index: usize) -> &[motorsport_telemetry_core::ChannelLabel] {
        self.catalog
            .channels
            .get(channel_index)
            .map(|channel| channel.labels.as_slice())
            .unwrap_or(&[])
    }

    fn channel_display(&self, channel_index: usize) -> motorsport_telemetry_core::ChannelDisplay {
        self.catalog
            .channels
            .get(channel_index)
            .map(|channel| channel.display.clone())
            .unwrap_or_default()
    }

    fn applied_passes(&self) -> &[AppliedPass] {
        &self.catalog.passes
    }

    fn source_origin(&self) -> Option<SourceOrigin> {
        (!self.catalog.source_format.is_empty() || !self.catalog.source_path.is_empty()).then(
            || SourceOrigin {
                format: self.catalog.source_format.clone(),
                path: self.catalog.source_path.clone(),
            },
        )
    }

    fn metadata(&self) -> FileMetadata {
        self.catalog.to_file_metadata(&self.path)
    }
}

fn writable(path: &Path) -> bool {
    OpenOptions::new().write(true).open(path).is_ok()
}
