//! Native `.telemetry` format: aligned STORE zip + FlatBuffers catalog.

mod catalog;
mod file;
mod migrate;
mod write;
mod zip;

pub use catalog::{needs_update, Catalog, FORMAT_VERSION};
pub use file::NativeRecording;
pub use migrate::apply as apply_migrations;
pub use write::{write_from_source, write_from_source_version, TelemetryFormatError};

/// Reads the catalog format version from `metadata.fb` only.
pub fn read_format_version(path: impl AsRef<std::path::Path>) -> Result<u16, TelemetryFormatError> {
    NativeRecording::read_format_version(path)
}

/// Header-only check: the file is older than [`FORMAT_VERSION`].
pub fn file_needs_update(path: impl AsRef<std::path::Path>) -> Result<bool, TelemetryFormatError> {
    Ok(needs_update(read_format_version(path)?))
}

/// Reads catalog metadata without mapping channel payloads.
pub fn read_metadata(
    path: impl AsRef<std::path::Path>,
) -> Result<motorsport_telemetry_core::FileMetadata, TelemetryFormatError> {
    NativeRecording::read_metadata(path)
}

/// Reads stored laps from `metadata.fb` only.
pub fn read_laps(
    path: impl AsRef<std::path::Path>,
) -> Result<Vec<motorsport_telemetry_core::LapMetadata>, TelemetryFormatError> {
    NativeRecording::read_laps(path)
}

/// Reads the stored complete-lap count. A header scalar; no lap vector walk.
pub fn read_valid_laps(path: impl AsRef<std::path::Path>) -> Result<u32, TelemetryFormatError> {
    NativeRecording::read_valid_laps(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use motorsport_telemetry_core::TelemetrySource;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures")
            .join(name)
    }

    #[test]
    fn catalog_round_trips_chunks_and_valid_laps() {
        use crate::catalog::{decode, encode, Catalog, CatalogChannel};
        use motorsport_telemetry_core::{Channel, Chunk, SampleType, UnitSource};
        let catalog = Catalog {
            format_version: FORMAT_VERSION,
            identity: Default::default(),
            laps: vec![motorsport_telemetry_core::LapMetadata {
                number: 2,
                start_ns: 1,
                end_ns: 2,
                duration_ns: 1,
                complete: true,
                first_video_frame: Some(4),
            }],
            valid_laps: 1,
            channels: vec![CatalogChannel {
                id: 1,
                name: "Speed".into(),
                member: "channels/0000.bin".into(),
                time_member: String::new(),
                unit_raw: "km/h".into(),
                unit_canonical: "km/h".into(),
                unit_source: UnitSource::Declared,
                dimension: 2,
                sample_type: SampleType::F32,
                scale: 1.0,
                bias: 0.0,
                uses_step: false,
                sample_count: 3,
                duration_ns: 3,
                kind: 0,
                chunks: vec![Chunk {
                    sample_period_ns: 1_000_000,
                    sample_count: 3,
                    data_ptr: 0,
                    sample_base: 0,
                    time_base_ns: 0,
                }],
            }],
            source_format: "pds".into(),
            source_path: "x.pds".into(),
            schema_hash: 1,
            duration_ns: 3,
            sample_count: 3,
            channel_count: 1,
            sampled_channel_count: 1,
            session_hint: String::new(),
            comment: String::new(),
            clock: None,
            driver_stints: Vec::new(),
            videos: Vec::new(),
            presentation_offset_ns: Some(104_000_000),
        };
        let bytes = encode(&catalog).unwrap();
        let decoded = decode(&bytes).unwrap();
        assert_eq!(decoded.format_version, FORMAT_VERSION);
        assert!(!needs_update(decoded.format_version));
        let mut stale = catalog.clone();
        stale.format_version = 1;
        assert!(needs_update(
            decode(&encode(&stale).unwrap()).unwrap().format_version
        ));
        assert_eq!(decoded.presentation_offset_ns, Some(104_000_000));
        assert_eq!(decoded.laps[0].first_video_frame, Some(4));
        assert_eq!(decoded.valid_laps, 1);
        assert_eq!(decoded.laps.len(), 1);
        assert_eq!(decoded.channels.len(), 1);
        assert_eq!(decoded.channels[0].name, "Speed");
        assert_eq!(decoded.channels[0].chunks.len(), 1);
        assert_eq!(decoded.channels[0].chunks[0].sample_count, 3);
        let _ = Channel {
            id: 1,
            name: "Speed".into(),
            unit: "km/h".into(),
            unit_source: UnitSource::Declared,
            sample_type: SampleType::F32,
            chunks: decoded.channels[0].chunks.clone(),
            sample_count: 3,
            duration_ns: 3,
        };
    }

    #[test]
    fn round_trips_synthetic_pds_and_preserves_decode() {
        let source =
            cosworth_telemetry::CosworthFile::open(fixture("synthetic_cosworth.pds")).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("run.telemetry");
        write_from_source(&source, &dest).unwrap();

        let header = NativeRecording::read_header(&dest).unwrap();
        assert_eq!(header.format_version, FORMAT_VERSION);
        assert_eq!(read_format_version(&dest).unwrap(), FORMAT_VERSION);
        assert!(!file_needs_update(&dest).unwrap());
        assert_eq!(header.valid_laps, source.metadata().valid_laps);
        assert_eq!(header.source_format, "pds");
        assert_eq!(read_valid_laps(&dest).unwrap(), header.valid_laps);
        assert!(
            header
                .channels
                .iter()
                .any(|channel| !channel.chunks.is_empty()),
            "catalog lost chunks: {:?}",
            header
                .channels
                .iter()
                .map(|c| (c.name.as_str(), c.sample_count, c.chunks.len()))
                .collect::<Vec<_>>()
        );

        let opened = NativeRecording::open(&dest).unwrap();
        assert!(
            opened.channels().iter().any(|ch| !ch.chunks.is_empty()),
            "open lost chunks; header chunks {:?}",
            header
                .channels
                .iter()
                .map(|c| (c.name.clone(), c.chunks.len(), c.member.clone()))
                .collect::<Vec<_>>()
        );
        assert_eq!(opened.format(), "pds");
        assert_eq!(opened.channels().len(), source.channels().len());
        for (index, channel) in source.channels().iter().enumerate() {
            if channel.sample_count == 0 {
                continue;
            }
            assert!(
                !opened.channels()[index].chunks.is_empty(),
                "channel {index} {} has samples but no chunks (catalog chunks {})",
                channel.name,
                header.channels[index].chunks.len()
            );
            assert_eq!(
                opened.decode(index, 0, 0),
                source.decode(index, 0, 0),
                "{}",
                channel.name
            );
        }
    }

    #[test]
    fn round_trips_synthetic_motec_affine() {
        let source = motec_telemetry::MotecFile::open(fixture("synthetic_motec.ld")).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("run.telemetry");
        write_from_source(&source, &dest).unwrap();
        let opened = NativeRecording::open(&dest).unwrap();
        for (index, channel) in source.channels().iter().enumerate() {
            if channel.sample_count == 0 {
                continue;
            }
            assert_eq!(opened.decode(index, 0, 0), source.decode(index, 0, 0));
            assert_eq!(opened.sample_affine(index), source.sample_affine(index));
        }
    }

    #[test]
    fn open_rewrites_older_writable_files() {
        let source =
            cosworth_telemetry::CosworthFile::open(fixture("synthetic_cosworth.pds")).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("stale.telemetry");
        write_from_source_version(&source, &dest, 1).unwrap();
        assert_eq!(read_format_version(&dest).unwrap(), 1);
        assert!(file_needs_update(&dest).unwrap());

        let opened = NativeRecording::open(&dest).unwrap();
        assert_eq!(opened.catalog().format_version, FORMAT_VERSION);
        assert!(!opened.needs_update());
        assert_eq!(read_format_version(&dest).unwrap(), FORMAT_VERSION);
        assert!(!file_needs_update(&dest).unwrap());
        assert_eq!(opened.decode(0, 0, 0), source.decode(0, 0, 0));
    }

    #[test]
    fn open_leaves_read_only_older_files() {
        let source =
            cosworth_telemetry::CosworthFile::open(fixture("synthetic_cosworth.pds")).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("readonly.telemetry");
        write_from_source_version(&source, &dest, 1).unwrap();
        let mut permissions = std::fs::metadata(&dest).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&dest, permissions).unwrap();

        let opened = NativeRecording::open(&dest).unwrap();
        assert_eq!(opened.catalog().format_version, 1);
        assert!(opened.needs_update());
        assert_eq!(read_format_version(&dest).unwrap(), 1);

        let mut permissions = std::fs::metadata(&dest).unwrap().permissions();
        permissions.set_readonly(false);
        std::fs::set_permissions(&dest, permissions).unwrap();
    }
}
