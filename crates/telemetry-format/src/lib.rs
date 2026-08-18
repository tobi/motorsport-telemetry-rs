//! Native `.telemetry` format: aligned STORE zip + FlatBuffers catalog.
//! Time-aligned JSONL interchange is documented in `JSONL.md`.

mod catalog;
mod file;
mod jsonl;
mod migrate;
mod placement;
mod write;
mod zip;

pub use catalog::{needs_update, Catalog, FORMAT_VERSION};
pub use file::NativeRecording;
pub use jsonl::{
    is_jsonl_ext_path, is_jsonl_path, is_jsonl_zstd_path, period_ns_from_hz,
    write_jsonl_extension_from_source, write_jsonl_extension_from_source_with,
    write_jsonl_from_source, write_jsonl_from_source_with, write_jsonl_timeline,
    write_jsonl_timeline_with, write_jsonl_to, HeaderChrome, JsonlRecording, SidecarGroup,
    SidecarHeader, Span, SpanPrimary, JSONL_EXT_VERSION, JSONL_VERSION, JSONL_ZSTD_LEVEL,
};
pub use migrate::apply as apply_migrations;
pub use placement::{
    civil_ns_to_utc_ns, resolve_timezone, resolve_utc_start_ns, utc_from_metadata,
};
pub use write::{
    write_from_source, write_from_source_stripped, write_from_source_version, TelemetryFormatError,
};

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
                visible: true,
                labels: vec![motorsport_telemetry_core::ChannelLabel {
                    time_ns: 1_000_000,
                    text: "note".into(),
                }],
                display: motorsport_telemetry_core::ChannelDisplay::trace(),
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
            utc_start_ns: Some(1_700_000_000_000_000_000),
            timezone: "America/Chicago".into(),
            driver_stints: Vec::new(),
            videos: Vec::new(),
            presentation_offset_ns: Some(104_000_000),
            spans: vec![motorsport_telemetry_core::Span {
                name: "443-1".into(),
                start_ns: 0,
                end_ns: 1_000_000,
                visible: true,
                color: "#e11d48".into(),
                primary: motorsport_telemetry_core::SpanPrimary {
                    title: "#443".into(),
                    subtitle: "EL".into(),
                },
                meta: vec![(
                    "Laps".into(),
                    motorsport_telemetry_core::SpanMetaValue::Text("18".into()),
                )],
            }],
            passes: vec![motorsport_telemetry_core::AppliedPass {
                name: "gps.clean".into(),
                version: 1,
                params: vec![("max_speed_mps".into(), "150".into())],
                inputs: vec!["GPS Latitude".into(), "GPS Longitude".into()],
                outputs: vec!["GPS Latitude Clean".into(), "GPS Longitude Clean".into()],
            }],
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
        assert_eq!(decoded.utc_start_ns, Some(1_700_000_000_000_000_000));
        assert_eq!(decoded.timezone, "America/Chicago");
        assert_eq!(decoded.spans.len(), 1);
        assert_eq!(decoded.spans[0].primary.title, "#443");
        assert_eq!(
            decoded.spans[0].meta[0],
            (
                "Laps".into(),
                motorsport_telemetry_core::SpanMetaValue::Text("18".into())
            )
        );
        assert!(decoded.channels[0].visible);
        assert_eq!(decoded.channels[0].labels[0].text, "note");
        assert_eq!(decoded.passes, catalog.passes);
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
    fn schema_documents_every_convertible_unit() {
        let schema = include_str!("../../../telemetry.schema.json");
        for def in motorsport_telemetry_core::UNITS {
            if !def.dimension.is_convertible() {
                continue;
            }
            assert!(
                schema.contains(&format!("\"{}\"", def.canonical)),
                "telemetry.schema.json is missing convertible unit {}",
                def.canonical
            );
        }
        assert!(schema.contains("\"mp/h\""));
        assert!(schema.contains("timespan_ms"));
        assert!(schema.contains("360000000"));
    }

    #[test]
    fn mtx_example_sidecars_validate() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let script = root.join("scripts/validate-mtx.py");
        for name in [
            "sebring-lmp2.telemetry.ext.jsonl",
            "multi-folder.telemetry.ext.jsonl",
        ] {
            let example = root.join("schema/examples").join(name);
            let status = std::process::Command::new("python3")
                .arg(&script)
                .arg(&example)
                .status()
                .expect("python3");
            assert!(
                status.success(),
                "validate-mtx.py failed on {}",
                example.display()
            );
        }
        let self_check = std::process::Command::new("python3")
            .arg(&script)
            .arg("--self-check")
            .output()
            .expect("python3");
        if !self_check.status.success() {
            let stderr = String::from_utf8_lossy(&self_check.stderr);
            assert!(
                stderr.contains("jsonschema is required"),
                "validate-mtx.py --self-check rejected a schema example: {stderr}"
            );
        }
    }

    #[test]
    fn native_preserves_jsonl_spans_and_visibility() {
        use motorsport_telemetry_core::TelemetrySource;
        let host = JsonlRecording::from_bytes(
            "host.jsonl",
            concat!(
                "{\"mtj\":1,\"q\":1000000000,\"dur\":2000000000,\"utc\":1000,\"tz\":\"UTC\"}\n",
                "[]\n",
                "{\"n\":\"Speed\",\"hz\":1,\"vis\":0,\"v\":[1,2],\"lbl\":[[0,\"brake lock\"]]}\n",
                "{\"k\":\"s\",\"n\":\"443-1\",\"s\":0,\"e\":1000000000,\"vis\":1,\"c\":\"#e11d48\",",
                "\"p\":{\"title\":\"#443\",\"sub\":\"EL\"},\"m\":[[\"Laps\",\"18\"],",
                "[\"Best\",{\"v\":110332,\"u\":\"timespan_ms\"}]]}\n",
            )
            .as_bytes(),
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("run.telemetry");
        write_from_source(&host, &dest).unwrap();
        let opened = NativeRecording::open(&dest).unwrap();
        assert_eq!(opened.channel_visible(), [false]);
        assert_eq!(opened.spans().len(), 1);
        assert_eq!(opened.spans()[0].name, "443-1");
        assert_eq!(opened.spans()[0].primary.title, "#443");
        assert_eq!(
            opened.spans()[0].meta,
            [
                (
                    "Laps".into(),
                    motorsport_telemetry_core::SpanMetaValue::Text("18".into())
                ),
                (
                    "Best".into(),
                    motorsport_telemetry_core::SpanMetaValue::TimeMs(110_332)
                ),
            ]
        );
        assert_eq!(opened.decode(0, 0, 0), 1.0);
        assert_eq!(opened.channel_labels(0).len(), 1);
        assert_eq!(opened.channel_labels(0)[0].text, "brake lock");

        let back = dir.path().join("back.telemetry.jsonl");
        write_jsonl_from_source_with(&opened, &back, false).unwrap();
        let again = JsonlRecording::open(&back).unwrap();
        assert_eq!(again.channel_visible(), [false]);
        assert_eq!(again.spans(), opened.spans());
        assert_eq!(again.channel_labels(0)[0].text, "brake lock");
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
        let meta = opened.metadata();
        let venue_tz = motorsport_track_atlas::timezone_for_venue(&source.identity().venue);
        assert_eq!(
            meta.timezone.as_str(),
            venue_tz.unwrap_or(""),
            "timezone should come from the venue atlas, never invented"
        );
        if source.absolute_time_range().is_some() {
            // Motec stamps a civil "utc" clock; GPS clocks copy through.
            // Either way, a known zone or a gps clock should produce utc.
            if venue_tz.is_some()
                || source
                    .absolute_time_range()
                    .is_some_and(|clock| clock.clock == "gps")
            {
                assert!(
                    meta.utc_start_ns.is_some()
                        || source
                            .absolute_time_range()
                            .is_some_and(|clock| clock.clock != "gps" && clock.clock != "utc")
                );
            }
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
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            permissions.set_mode(permissions.mode() | 0o200);
        }
        #[cfg(not(unix))]
        permissions.set_readonly(false);
        std::fs::set_permissions(&dest, permissions).unwrap();
    }

    #[test]
    fn corrupt_catalog_root_offset_returns_invalid_not_panic() {
        use crate::catalog::{decode, encode, Catalog, CatalogChannel};
        use motorsport_telemetry_core::{Chunk, SampleType, UnitSource};
        let catalog = Catalog {
            format_version: FORMAT_VERSION,
            identity: Default::default(),
            laps: Vec::new(),
            valid_laps: 0,
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
                sample_count: 0,
                duration_ns: 0,
                kind: 0,
                visible: true,
                labels: Vec::new(),
                display: motorsport_telemetry_core::ChannelDisplay::trace(),
                chunks: vec![Chunk {
                    sample_period_ns: 1_000_000,
                    sample_count: 0,
                    data_ptr: 0,
                    sample_base: 0,
                    time_base_ns: 0,
                }],
            }],
            source_format: "pds".into(),
            source_path: "x.pds".into(),
            schema_hash: 1,
            duration_ns: 0,
            sample_count: 0,
            channel_count: 1,
            sampled_channel_count: 1,
            session_hint: String::new(),
            comment: String::new(),
            clock: None,
            utc_start_ns: Some(1_700_000_000_000_000_000),
            timezone: "America/Chicago".into(),
            driver_stints: Vec::new(),
            videos: Vec::new(),
            presentation_offset_ns: None,
            spans: Vec::new(),
            passes: Vec::new(),
        };
        let mut bytes = encode(&catalog).unwrap();
        // Overwrite the root table offset (first 4 bytes, u32 LE) to point
        // past the buffer. This must return Invalid, not panic.
        let past_end = (bytes.len() + 1000) as u32;
        bytes[0..4].copy_from_slice(&past_end.to_le_bytes());
        let result = decode(&bytes);
        assert!(result.is_err(), "expected Invalid, got Ok");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("out of range") || msg.contains("overflow"),
            "expected out-of-range error, got: {msg}"
        );

        // Also test a vtable offset that points past the buffer: corrupt the
        // root offset to point near the end so root+4 is valid but the
        // vtable read lands out of bounds.
        let mut bytes2 = encode(&catalog).unwrap();
        let near_end = (bytes2.len() - 2) as u32;
        bytes2[0..4].copy_from_slice(&near_end.to_le_bytes());
        // This should NOT panic — slot() returns None for out-of-bounds vtable.
        let _ = decode(&bytes2);
    }

    #[test]
    fn zip_header_declaring_member_larger_than_file_returns_invalid() {
        use crate::zip::{read_first_member, ZipWriter};
        use std::io::{Cursor, Seek};
        // Build a valid zip local header for "metadata.fb" with a declared
        // size far larger than the actual file.
        let mut cursor = Cursor::new(Vec::new());
        let mut writer = ZipWriter::new(&mut cursor);
        writer.write_member("metadata.fb", b"hello").unwrap();
        writer.finish().unwrap();
        let mut bytes = cursor.into_inner();
        // Overwrite the compressed/uncompressed size fields (bytes 22..26
        // and 26..30 in the local header are size and... actually 22..26 is
        // the uncompressed size, 26..28 is name length, 28..30 is extra len).
        // The local header starts at byte 0. Set the declared size to 1 GiB.
        let declared: u32 = 0x4000_0000;
        bytes[22..26].copy_from_slice(&declared.to_le_bytes());
        let mut reader = Cursor::new(&bytes[..]);
        let result = read_first_member(&mut reader);
        assert!(result.is_err(), "expected Invalid, got Ok");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("exceeds remaining file bytes"),
            "expected size exceeds remaining, got: {msg}"
        );
        // Verify we did NOT allocate the declared buffer: the reader position
        // should still be just past the header (no 1 GiB read attempted).
        let pos = reader.stream_position().unwrap();
        assert!(
            pos < 200,
            "reader advanced to {pos}, suggesting a huge allocation was attempted"
        );
    }

    #[test]
    fn missing_video_frames_bin_reports_diagnostic() {
        use crate::catalog::{encode, Catalog, CatalogChannel};
        use motorsport_telemetry_core::{
            Chunk, SampleType, TelemetrySource, UnitSource, VideoFileRef,
        };
        // Build a .telemetry with a video handle in the catalog but no
        // video_frames.bin member in the archive.
        let catalog = Catalog {
            format_version: FORMAT_VERSION,
            identity: Default::default(),
            laps: Vec::new(),
            valid_laps: 0,
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
                sample_count: 4,
                duration_ns: 4_000_000,
                kind: 0,
                visible: true,
                labels: Vec::new(),
                display: motorsport_telemetry_core::ChannelDisplay::trace(),
                chunks: vec![Chunk {
                    sample_period_ns: 1_000_000,
                    sample_count: 4,
                    data_ptr: 0,
                    sample_base: 0,
                    time_base_ns: 0,
                }],
            }],
            source_format: "pds".into(),
            source_path: "x.pds".into(),
            schema_hash: 1,
            duration_ns: 4_000_000,
            sample_count: 4,
            channel_count: 1,
            sampled_channel_count: 1,
            session_hint: String::new(),
            comment: String::new(),
            clock: None,
            utc_start_ns: Some(1_700_000_000_000_000_000),
            timezone: "America/Chicago".into(),
            driver_stints: Vec::new(),
            videos: vec![VideoFileRef {
                filename: "video.mp4".into(),
                index: 0,
                blake3: None,
                frame_count: 0,
                presentation_offset_ns: None,
            }],
            presentation_offset_ns: Some(0),
            spans: Vec::new(),
            passes: Vec::new(),
        };
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("novideo.telemetry");
        // Write using the public writer, which creates a valid .telemetry zip.
        // We need a TelemetrySource to write from. Use from_bytes with a
        // manually constructed archive.
        let catalog_bytes = encode(&catalog).unwrap();
        // Build a zip manually using the crate's ZipWriter.
        use crate::zip::ZipWriter as InnerZipWriter;
        let file = std::fs::File::create(&dest).unwrap();
        let mut zip = InnerZipWriter::new(std::io::BufWriter::new(file));
        zip.write_member("metadata.fb", &catalog_bytes).unwrap();
        zip.finish().unwrap();

        let opened = NativeRecording::open(&dest).unwrap();
        let diags = opened.diagnostics();
        assert!(
            diags
                .iter()
                .any(|d| d.code == "telemetry.video_frames_unusable"),
            "expected video_frames_unusable diagnostic, got: {:?}",
            diags.iter().map(|d| d.code).collect::<Vec<_>>()
        );
        assert!(
            diags.iter().any(|d| d.code == "telemetry.member_missing"),
            "expected member_missing diagnostic, got: {:?}",
            diags.iter().map(|d| d.code).collect::<Vec<_>>()
        );
        assert!(opened.channels()[0].chunks.is_empty());
        assert_eq!(opened.channels()[0].sample_count, 0);
    }

    #[test]
    fn i8_channel_round_trips_sign_extended() {
        use crate::catalog::{encode, Catalog, CatalogChannel};
        use motorsport_telemetry_core::{Chunk, SampleType, TelemetrySource, UnitSource};
        // I8 channel with bytes [-128, -73, 0, 127] → sign-extended f64.
        let raw: [i8; 4] = [-128, -73, 0, 127];
        let raw_bytes: [u8; 4] = raw.map(|v| v as u8);
        let catalog = Catalog {
            format_version: FORMAT_VERSION,
            identity: Default::default(),
            laps: Vec::new(),
            valid_laps: 0,
            channels: vec![CatalogChannel {
                id: 1,
                name: "TPMS_RSSI".into(),
                member: "channels/0000.bin".into(),
                time_member: String::new(),
                unit_raw: "dBm".into(),
                unit_canonical: "dBm".into(),
                unit_source: UnitSource::Declared,
                dimension: 0,
                sample_type: SampleType::I8,
                scale: 1.0,
                bias: 0.0,
                uses_step: false,
                sample_count: 4,
                duration_ns: 4_000_000,
                kind: 0,
                visible: true,
                labels: Vec::new(),
                display: motorsport_telemetry_core::ChannelDisplay::trace(),
                chunks: vec![Chunk {
                    sample_period_ns: 1_000_000,
                    sample_count: 4,
                    data_ptr: 0,
                    sample_base: 0,
                    time_base_ns: 0,
                }],
            }],
            source_format: "pds".into(),
            source_path: "x.pds".into(),
            schema_hash: 1,
            duration_ns: 4_000_000,
            sample_count: 4,
            channel_count: 1,
            sampled_channel_count: 1,
            session_hint: String::new(),
            comment: String::new(),
            clock: None,
            utc_start_ns: Some(1_700_000_000_000_000_000),
            timezone: "America/Chicago".into(),
            driver_stints: Vec::new(),
            videos: Vec::new(),
            presentation_offset_ns: None,
            spans: Vec::new(),
            passes: Vec::new(),
        };
        let catalog_bytes = encode(&catalog).unwrap();
        // Verify the packed channel uses sample_type code 0.
        // The channel vector starts after the count u32. The sample_type byte
        // is at offset: 4 (count) + 4 (id) + 4+0 (name len+data) + 4+0 (member)
        // + 4+0 (time_member) + 4+4 (unit_raw) + 4+4 (unit_canonical) + 5 bytes
        // (unit_source, dimension, sample_type, uses_step, kind) → sample_type
        // is the 3rd byte of the 5-byte block.
        // Instead of fragile offset math, just verify via round-trip decode.
        use crate::zip::ZipWriter as InnerZipWriter;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("i8.telemetry");
        let file = std::fs::File::create(&dest).unwrap();
        let mut zip = InnerZipWriter::new(std::io::BufWriter::new(file));
        zip.write_member("metadata.fb", &catalog_bytes).unwrap();
        zip.write_member("channels/0000.bin", &raw_bytes).unwrap();
        zip.finish().unwrap();

        let opened = NativeRecording::open(&dest).unwrap();
        assert_eq!(opened.channels().len(), 1);
        assert_eq!(opened.channels()[0].sample_type, SampleType::I8);
        assert_eq!(opened.decode(0, 0, 0), -128.0);
        assert_eq!(opened.decode(0, 0, 1), -73.0);
        assert_eq!(opened.decode(0, 0, 2), 0.0);
        assert_eq!(opened.decode(0, 0, 3), 127.0);
    }
}
