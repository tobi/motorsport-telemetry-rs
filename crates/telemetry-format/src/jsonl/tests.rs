use super::align::{collect_aligned, snap_laps, snap_spans, snap_up};
use super::write::{write_jsonl_document, write_number};
use super::*;
use crate::NativeRecording;
use motorsport_telemetry_core::{
    Channel, ChannelPlot, Chunk, LapMetadata, SampleType, SourceIdentity, SourceLapMetadata, Span,
    SpanMetaValue, SpanPrimary, TelemetrySource, UnitSource, VideoFileRef,
};

struct TinySource {
    identity: SourceIdentity,
    channels: Vec<Channel>,
    values: Vec<Vec<f64>>,
    laps: Vec<LapMetadata>,
    utc_start_ns: Option<u64>,
    timezone: String,
    videos: Vec<VideoFileRef>,
    video_times: Vec<u64>,
    video_offset_ns: Option<i128>,
    sample_times: Vec<Vec<u64>>,
    spans: Vec<Span>,
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
    fn sample_time_ns(&self, channel_index: usize, chunk_index: usize, local_index: u64) -> u64 {
        let chunk = &self.channels[channel_index].chunks[chunk_index];
        let index = (chunk.sample_base + local_index) as usize;
        if let Some(&time) = self
            .sample_times
            .get(channel_index)
            .and_then(|times| times.get(index))
        {
            return time;
        }
        chunk.time_base_ns + local_index * chunk.sample_period_ns
    }
    fn identity(&self) -> SourceIdentity {
        self.identity.clone()
    }
    fn utc_start_ns(&self) -> Option<u64> {
        self.utc_start_ns
    }
    fn timezone(&self) -> String {
        self.timezone.clone()
    }
    fn spans(&self) -> &[Span] {
        &self.spans
    }
    fn source_lap_metadata(&self) -> Option<SourceLapMetadata> {
        Some(SourceLapMetadata {
            laps: self.laps.clone(),
            fastest_lap: None,
        })
    }
    fn video_files(&self) -> &[VideoFileRef] {
        &self.videos
    }
    fn video_presentation_times_ns(&self) -> Option<&[u64]> {
        (!self.video_times.is_empty()).then_some(self.video_times.as_slice())
    }
    fn video_frame_count(&self) -> Option<u64> {
        (!self.video_times.is_empty()).then_some(self.video_times.len() as u64)
    }
    fn video_frame_at(&self, time_ns: u64) -> Option<u64> {
        if self.video_times.is_empty() {
            return None;
        }
        let stamp = self.video_presentation_time_ns(time_ns)?;
        let index = self.video_times.partition_point(|time| *time <= stamp);
        Some(index.saturating_sub(1) as u64)
    }
    fn video_presentation_offset_ns(&self) -> Option<i128> {
        self.video_offset_ns
    }
}

fn channel(name: &str, unit: &str, period_ns: u64, count: u64, t0: u64) -> Channel {
    Channel {
        id: 1,
        name: name.into(),
        unit: unit.into(),
        unit_source: if unit.is_empty() {
            UnitSource::Unknown
        } else {
            UnitSource::Declared
        },
        sample_type: SampleType::F64,
        chunks: vec![Chunk {
            sample_period_ns: period_ns,
            sample_count: count,
            data_ptr: 0,
            sample_base: 0,
            time_base_ns: t0,
        }],
        sample_count: count,
        duration_ns: t0 + count * period_ns,
    }
}

fn tiny() -> TinySource {
    TinySource {
        identity: SourceIdentity {
            driver: "Tobi".into(),
            venue: "Road America".into(),
            ..SourceIdentity::default()
        },
        channels: vec![
            channel("Speed", "km/h", 10_000_000, 4, 0),
            channel("GPS Speed", "m/s", 40_000_000, 1, 0),
        ],
        values: vec![vec![10.0, 11.0, 12.5, 13.0], vec![2.8]],
        utc_start_ns: None,
        timezone: String::new(),
        videos: Vec::new(),
        video_times: Vec::new(),
        video_offset_ns: None,
        sample_times: Vec::new(),
        spans: Vec::new(),
        laps: vec![LapMetadata {
            number: 1,
            start_ns: 0,
            end_ns: 40_000_000,
            duration_ns: 40_000_000,
            complete: false,
            first_video_frame: None,
        }],
    }
}

fn jittered_times(period_ns: u64, count: u64, t0_ns: u64, jitter_ns: i64) -> Vec<u64> {
    (0..count)
        .map(|index| {
            let expected = t0_ns + index * period_ns;
            let signed = if index % 2 == 0 {
                jitter_ns
            } else {
                -jitter_ns
            };
            u64::try_from(i128::from(expected) + i128::from(signed)).unwrap()
        })
        .collect()
}

fn alignment_span(name: &str, start_ns: u64, end_ns: u64) -> Span {
    Span {
        name: name.into(),
        start_ns,
        end_ns,
        visible: true,
        color: String::new(),
        primary: SpanPrimary::default(),
        meta: Vec::new(),
    }
}

fn write_alignment_jsonl(source: &TinySource) -> (Vec<u8>, JsonlRecording) {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("align.telemetry.jsonl");
    write_jsonl_from_source_with(source, &dest, false).unwrap();
    let bytes = std::fs::read(&dest).unwrap();
    assert_eq!(bytes.first().copied(), Some(b'{'));
    let opened = JsonlRecording::open(&dest).unwrap();
    let from_bytes = JsonlRecording::from_bytes("align.telemetry.jsonl", &bytes).unwrap();
    assert_eq!(from_bytes.quantum_ns(), opened.quantum_ns());
    assert_eq!(from_bytes.origin_ns(), opened.origin_ns());
    assert_eq!(from_bytes.duration_ns(), opened.duration_ns());
    assert_eq!(from_bytes.channels().len(), opened.channels().len());
    (bytes, opened)
}

#[test]
fn promoted_f32_values_write_short_decimals() {
    let mut short = Vec::new();
    write_number(&mut short, f64::from(0.2f32)).unwrap();
    assert_eq!(String::from_utf8(short).unwrap(), "0.2");
    let mut exact = Vec::new();
    write_number(&mut exact, 1.0 / 3.0).unwrap();
    assert_eq!(String::from_utf8(exact).unwrap(), format!("{}", 1.0 / 3.0));
}

#[test]
fn period_from_integer_hz_is_exact() {
    assert_eq!(period_ns_from_hz(100.0), Some(10_000_000));
    assert_eq!(period_ns_from_hz(25.0), Some(40_000_000));
    assert_eq!(period_ns_from_hz(1.0), Some(1_000_000_000));
}

#[test]
fn writes_header_laps_then_compact_channels() {
    let source = tiny();
    let mut bytes = Vec::new();
    write_jsonl_to(&source, &mut bytes).unwrap();
    let text = String::from_utf8(bytes).unwrap();
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 4);
    assert!(lines[0].starts_with("{\"mtj\":1,\"q\":10000000,\"dur\":40000000"));
    assert!(lines[0].contains("\"src\":\"pds\""));
    assert!(lines[0].contains("\"drv\":\"Tobi\""));
    assert!(
        !lines[0].contains(": ") && !lines[0].contains(", "),
        "header has insignificant whitespace: {}",
        lines[0]
    );
    assert_eq!(lines[1], "[[1,0,40000000,0]]");
    assert_eq!(
        lines[2],
        "{\"n\":\"Speed\",\"hz\":100,\"u\":\"km/h\",\"v\":[10,11,12.5,13]}"
    );
    assert_eq!(
        lines[3],
        "{\"n\":\"GPS Speed\",\"hz\":25,\"u\":\"m/s\",\"v\":[2.8]}"
    );
}

#[test]
fn round_trip_preserves_alignment_and_values() {
    let source = tiny();
    let mut bytes = Vec::new();
    write_jsonl_to(&source, &mut bytes).unwrap();
    let opened = JsonlRecording::from_bytes("tiny.jsonl", &bytes).unwrap();
    assert_eq!(opened.quantum_ns(), 10_000_000);
    assert_eq!(opened.format(), "pds");
    assert_eq!(opened.identity().driver, "Tobi");
    assert_eq!(opened.channels().len(), 2);
    assert_eq!(opened.decode(0, 0, 0), 10.0);
    assert_eq!(opened.decode(0, 0, 2), 12.5);
    assert_eq!(opened.sample_time_ns(0, 0, 1), 10_000_000);
    assert_eq!(opened.sample_time_ns(1, 0, 0), 0);
    assert_eq!(opened.metadata().laps[0].end_ns, 40_000_000);
}

#[test]
fn rejects_channel_off_the_lattice() {
    let text = concat!(
        "{\"mtj\":1,\"q\":10000000,\"dur\":40000000}\n",
        "[]\n",
        "{\"n\":\"Speed\",\"hz\":30,\"v\":[1,2,3]}\n",
    );
    let err = JsonlRecording::from_bytes("bad.jsonl", text.as_bytes()).unwrap_err();
    assert!(err.to_string().contains("not a multiple of q"));
}

#[test]
fn video_linkage_round_trips() {
    let mut source = tiny();
    source.videos = vec![VideoFileRef {
        filename: "SCHD0060.MP4".into(),
        index: 1,
        blake3: Some([0xab; 32]),
        frame_count: 4,
        presentation_offset_ns: Some(101_333_333),
    }];
    source.video_times = vec![101_333_333, 134_700_000, 168_066_666, 201_433_333];
    source.video_offset_ns = Some(101_333_333);

    let mut bytes = Vec::new();
    write_jsonl_to(&source, &mut bytes).unwrap();
    let text = String::from_utf8(bytes.clone()).unwrap();
    let header = text.lines().next().unwrap();
    assert!(header.contains(",\"vo\":101333333,"));
    assert!(header.contains(&format!(
        ",\"vf\":[{{\"n\":\"SCHD0060.MP4\",\"i\":1,\"fc\":4,\"b3\":\"{}\",\"po\":101333333}}]",
        "ab".repeat(32)
    )));
    assert!(header.contains(",\"vpts\":[101333333,134700000,168066666,201433333]"));
    assert!(header.ends_with('}'));
    assert!(
        header.rfind("\"hash\":").unwrap() > header.rfind("\"vpts\":").unwrap(),
        "hash must stay the last header key"
    );
    // The lap line picks up the first video frame (5th element).
    assert_eq!(text.lines().nth(1).unwrap(), "[[1,0,40000000,0,0]]");

    let opened = JsonlRecording::from_bytes("tiny.mtj", &bytes).unwrap();
    assert_eq!(opened.video_files(), source.videos.as_slice());
    assert_eq!(
        opened.video_presentation_times_ns().unwrap(),
        source.video_times.as_slice()
    );
    assert_eq!(opened.video_presentation_offset_ns(), Some(101_333_333));
    assert_eq!(opened.video_frame_count(), Some(4));
    // Same binary-search semantics as the native reader.
    assert_eq!(opened.video_frame_at(0), Some(0));
    assert_eq!(opened.video_frame_at(40_000_000), Some(1));
    assert_eq!(opened.video_frame_at(u64::MAX / 2), Some(3));
    assert_eq!(opened.metadata().video_frame_count, Some(4));

    // The native hop keeps the linkage bit for bit.
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("tiny.telemetry");
    crate::write_from_source(&opened, &dest).unwrap();
    let native = NativeRecording::open(&dest).unwrap();
    assert_eq!(native.video_files(), source.videos.as_slice());
    assert_eq!(
        native.video_presentation_times_ns().unwrap(),
        source.video_times.as_slice()
    );
    assert_eq!(native.video_presentation_offset_ns(), Some(101_333_333));
    assert_eq!(native.video_frame_at(40_000_000), Some(1));
}

#[test]
fn sidecars_reject_video_linkage() {
    let text = concat!(
        "{\"mtx\":1,\"q\":10000000,\"dur\":40000000,",
        "\"n\":\"tires\",\"vis\":true,\"utc\":1700000000000000000,\"tz\":\"UTC\",",
        "\"vf\":[{\"n\":\"clip.mp4\",\"i\":1,\"fc\":1}]}\n",
    );
    let err = JsonlRecording::from_bytes("bad.mtjx", text.as_bytes()).unwrap_err();
    assert!(err.to_string().contains("video belongs to the host"));
}

#[test]
fn rejects_vpts_without_vf() {
    let text = concat!(
        "{\"mtj\":1,\"q\":10000000,\"dur\":40000000,\"vpts\":[0,1]}\n",
        "[]\n",
    );
    let err = JsonlRecording::from_bytes("bad.mtj", text.as_bytes()).unwrap_err();
    assert!(err.to_string().contains("vpts requires vf"));
}

#[test]
fn rejects_decreasing_vpts() {
    let text = concat!(
        "{\"mtj\":1,\"q\":10000000,\"dur\":40000000,",
        "\"vf\":[{\"n\":\"clip.mp4\",\"i\":1,\"fc\":2}],\"vpts\":[5,4]}\n",
        "[]\n",
    );
    let err = JsonlRecording::from_bytes("bad.mtj", text.as_bytes()).unwrap_err();
    assert!(err.to_string().contains("non-decreasing"));
}

#[test]
fn rejects_unaligned_lap() {
    let text = concat!(
        "{\"mtj\":1,\"q\":10000000,\"dur\":40000000}\n",
        "[[1,1,40000000,0]]\n",
    );
    let err = JsonlRecording::from_bytes("bad.jsonl", text.as_bytes()).unwrap_err();
    assert!(err.to_string().contains("not on the time lattice"));
}

#[test]
fn fills_gaps_with_null_and_keeps_indexes() {
    let mut source = tiny();
    source.channels[0].chunks = vec![
        Chunk {
            sample_period_ns: 10_000_000,
            sample_count: 2,
            data_ptr: 0,
            sample_base: 0,
            time_base_ns: 0,
        },
        Chunk {
            sample_period_ns: 10_000_000,
            sample_count: 1,
            data_ptr: 0,
            sample_base: 2,
            time_base_ns: 30_000_000,
        },
    ];
    source.channels[0].sample_count = 3;
    source.values[0] = vec![10.0, 11.0, 13.0];
    let mut bytes = Vec::new();
    write_jsonl_to(&source, &mut bytes).unwrap();
    let text = String::from_utf8(bytes.clone()).unwrap();
    assert!(text.contains("\"v\":[10,11,null,13]"));
    let opened = JsonlRecording::from_bytes("gap.jsonl", &bytes).unwrap();
    assert!(opened.decode(0, 0, 2).is_nan());
    assert_eq!(opened.decode(0, 0, 3), 13.0);
    assert_eq!(opened.sample_time_ns(0, 0, 2), 20_000_000);
}

#[test]
fn drops_irregular_channels() {
    let mut source = tiny();
    source.channels.push(Channel {
        id: 3,
        name: "Beacon".into(),
        unit: String::new(),
        unit_source: UnitSource::Unknown,
        sample_type: SampleType::F64,
        chunks: vec![Chunk {
            sample_period_ns: 0,
            sample_count: 2,
            data_ptr: 0,
            sample_base: 0,
            time_base_ns: 0,
        }],
        sample_count: 2,
        duration_ns: 1,
    });
    source.values.push(vec![1.0, 2.0]);
    let mut bytes = Vec::new();
    write_jsonl_to(&source, &mut bytes).unwrap();
    let opened = JsonlRecording::from_bytes("skip.jsonl", &bytes).unwrap();
    assert_eq!(opened.channels().len(), 2);
    assert!(opened.channels().iter().all(|ch| ch.name != "Beacon"));
}

#[test]
fn recognizes_telemetry_jsonl_and_zstd_names() {
    assert!(is_jsonl_path("run.telemetry.jsonl"));
    assert!(is_jsonl_path("run.telemetry.jsonl.zstd"));
    assert!(is_jsonl_path("run.TELEMETRY.JSONL.ZST"));
    assert!(is_jsonl_zstd_path("run.telemetry.jsonl.zstd"));
    assert!(is_jsonl_zstd_path("run.jsonl.zst"));
    assert!(!is_jsonl_zstd_path("run.telemetry.jsonl"));
    assert!(!is_jsonl_path("run.telemetry"));
    assert!(!is_jsonl_path("run.pds"));
    assert!(is_jsonl_ext_path("run.telemetry.ext.jsonl"));
    assert!(is_jsonl_ext_path("run.telemetry.ext.jsonl.zstd"));
    assert!(is_jsonl_zstd_path("run.telemetry.ext.jsonl.zstd"));
    assert!(!is_jsonl_ext_path("run.telemetry.jsonl"));
}

#[test]
fn write_jsonl_from_source_compresses_at_level_11_by_default() {
    assert_eq!(JSONL_ZSTD_LEVEL, 11);
    let source = tiny();
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("tiny.telemetry.jsonl");
    write_jsonl_from_source(&source, &dest).unwrap();
    let bytes = std::fs::read(&dest).unwrap();
    assert_eq!(&bytes[..4], &ZSTD_MAGIC);
    let opened = JsonlRecording::open(&dest).unwrap();
    assert_eq!(opened.decode(0, 0, 0), 10.0);
    write_jsonl_from_source_with(&source, &dest, false).unwrap();
    let plain = std::fs::read(&dest).unwrap();
    assert_eq!(plain[0], b'{');
}

#[test]
fn zstd_round_trip_is_sniffed_by_magic() {
    let source = tiny();
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("tiny.telemetry.jsonl.zstd");
    write_jsonl_from_source(&source, &dest).unwrap();
    let bytes = std::fs::read(&dest).unwrap();
    assert_eq!(&bytes[..4], &ZSTD_MAGIC);
    assert!(bytes.len() < 400);
    let opened = JsonlRecording::open(&dest).unwrap();
    assert_eq!(opened.decode(0, 0, 0), 10.0);
    assert_eq!(opened.channels()[0].name, "Speed");
    let renamed = dir.path().join("tiny.telemetry.jsonl");
    std::fs::copy(&dest, &renamed).unwrap();
    let sniffed = JsonlRecording::open(&renamed).unwrap();
    assert_eq!(sniffed.decode(0, 0, 2), 12.5);
}

#[test]
fn jsonl_and_zstd_decompress_to_identical_bytes_and_payload() {
    let source = tiny();
    let dir = tempfile::tempdir().unwrap();
    let plain = dir.path().join("tiny.telemetry.jsonl");
    let zstd = dir.path().join("tiny.telemetry.jsonl.zstd");
    write_jsonl_from_source_with(&source, &plain, false).unwrap();
    write_jsonl_from_source(&source, &zstd).unwrap();

    let plain_bytes = std::fs::read(&plain).unwrap();
    let mut decoded = Vec::new();
    zstd::Decoder::new(std::fs::File::open(&zstd).unwrap())
        .unwrap()
        .read_to_end(&mut decoded)
        .unwrap();
    assert_eq!(
        decoded, plain_bytes,
        "zstd frame must be the same UTF-8 document"
    );

    let a = JsonlRecording::open(&plain).unwrap();
    let b = JsonlRecording::open(&zstd).unwrap();
    assert_ne!(a.path(), b.path());
    assert_eq!(a.source_format, b.source_format);
    assert_eq!(a.identity, b.identity);
    assert_eq!(a.clock, b.clock);
    assert_eq!(a.utc_start_ns, b.utc_start_ns);
    assert_eq!(a.timezone, b.timezone);
    assert_eq!(a.laps, b.laps);
    assert_eq!(a.quantum_ns, b.quantum_ns);
    assert_eq!(a.origin_ns, b.origin_ns);
    assert_eq!(a.duration_ns, b.duration_ns);
    assert_eq!(a.schema_hash, b.schema_hash);
    assert_eq!(a.channels.len(), b.channels.len());
    for (left, right) in a.channels.iter().zip(&b.channels) {
        assert_eq!(left.name, right.name);
        assert_eq!(left.unit, right.unit);
        assert_eq!(left.sample_count, right.sample_count);
        assert_eq!(left.duration_ns, right.duration_ns);
        assert_eq!(
            left.chunks[0].sample_period_ns,
            right.chunks[0].sample_period_ns
        );
        assert_eq!(left.chunks[0].time_base_ns, right.chunks[0].time_base_ns);
    }
    assert_eq!(a.values.len(), b.values.len());
    for (left, right) in a.values.iter().zip(&b.values) {
        assert_eq!(left.len(), right.len());
        for (l, r) in left.iter().zip(right) {
            assert_eq!(l.to_bits(), r.to_bits());
        }
    }
}

#[test]
fn extension_is_header_then_channels() {
    let text = concat!(
        "{\"mtx\":1,\"n\":\"Ride height\",\"q\":10000000,\"dur\":50000000,\"vis\":1,\"utc\":1700000000000000000,\"tz\":\"America/New_York\"}\n",
        "{\"n\":\"Ride Height FL\",\"hz\":100,\"u\":\"mm\",\"vis\":1,\"v\":[42,41,40,39],\"t0\":10000000}\n",
    );
    let opened = JsonlRecording::from_bytes("ride.telemetry.ext.jsonl", text.as_bytes()).unwrap();
    assert!(opened.is_extension());
    assert_eq!(opened.sidecar_groups()[0].header.name, "Ride height");
    assert!(opened.sidecar_groups()[0].header.visible);
    assert!(opened.metadata().laps.is_empty());
    assert_eq!(opened.channels().len(), 1);
    assert_eq!(opened.channel_visible(), [true]);
    assert_eq!(opened.channels()[0].name, "Ride Height FL");
    assert_eq!(opened.sample_time_ns(0, 0, 0), 10_000_000);
    assert_eq!(opened.decode(0, 0, 0), 42.0);
    assert_eq!(opened.decode(0, 0, 3), 39.0);
}

#[test]
fn repeated_mtx_headers_start_independent_groups() {
    let text = concat!(
        "{\"mtx\":1,\"n\":\"Ride height\",\"q\":10000000,\"dur\":20000000,\"vis\":1,\"utc\":1000,\"tz\":\"UTC\"}\n",
        "{\"n\":\"Ride Height FL\",\"hz\":100,\"vis\":1,\"v\":[42,41]}\n",
        "{\"k\":\"s\",\"n\":\"Bottoming\",\"s\":0,\"e\":10000000,\"vis\":1}\n",
        "{\"mtx\":1,\"n\":\"Stints\",\"q\":5000000,\"dur\":10000000,\"vis\":0,\"utc\":20001000,\"tz\":\"UTC\"}\n",
        "{\"n\":\"Fuel Delta\",\"hz\":200,\"vis\":1,\"v\":[3,2]}\n",
        "{\"k\":\"s\",\"n\":\"Driver A\",\"s\":0,\"e\":5000000,\"vis\":1}\n",
    );
    let extension =
        JsonlRecording::from_bytes("groups.telemetry.ext.jsonl", text.as_bytes()).unwrap();
    let groups = extension.sidecar_groups();
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].header.name, "Ride height");
    assert_eq!(groups[0].channel_range, 0..1);
    assert_eq!(groups[0].span_range, 0..1);
    assert_eq!(groups[1].header.name, "Stints");
    assert!(!groups[1].header.visible);
    assert_eq!(groups[1].quantum_ns, 5_000_000);
    assert_eq!(groups[1].channel_range, 1..2);
    assert_eq!(groups[1].span_range, 1..2);

    let host = JsonlRecording::from_bytes(
        "host.jsonl",
        concat!(
            "{\"mtj\":1,\"q\":5000000,\"dur\":50000000,\"utc\":1000,\"tz\":\"UTC\"}\n",
            "[]\n",
            "{\"n\":\"Speed\",\"hz\":200,\"v\":[1,2]}\n",
        )
        .as_bytes(),
    )
    .unwrap();
    let merged = host.attach(&extension).unwrap();
    assert_eq!(merged.sample_time_ns(1, 0, 0), 0);
    assert_eq!(merged.sample_time_ns(2, 0, 0), 20_000_000);
    assert_eq!(merged.spans()[0].start_ns, 0);
    assert_eq!(merged.spans()[1].start_ns, 20_000_000);
    assert_eq!(merged.spans()[1].end_ns, 25_000_000);
}

#[test]
fn writes_extension_without_laps() {
    let mut source = tiny();
    source.utc_start_ns = Some(1_700_000_000_000_000_000);
    source.timezone = "America/Chicago".into();
    let mut bytes = Vec::new();
    write_jsonl_document(&source, &mut bytes, true).unwrap();
    let text = String::from_utf8(bytes).unwrap();
    let mut lines = text.lines();
    let header = lines.next().unwrap();
    assert!(header.starts_with("{\"mtx\":1,\"n\":"));
    assert!(header.contains("\"vis\":1"));
    assert!(header.contains("\"utc\":1700000000000000000"));
    assert!(header.contains("\"tz\":\"America/Chicago\""));
    assert!(!header.contains("\"mtj\""));
    let second = lines.next().unwrap();
    assert!(second.contains("\"vis\":1"), "{second}");
}

#[test]
fn attach_joins_on_file_relative_time() {
    let host = JsonlRecording::from_bytes(
        "host.jsonl",
        concat!(
            "{\"mtj\":1,\"q\":10000000,\"dur\":40000000}\n",
            "[]\n",
            "{\"n\":\"Speed\",\"hz\":100,\"u\":\"km/h\",\"v\":[10,11,12,13]}\n",
        )
        .as_bytes(),
    )
    .unwrap();
    let ext = JsonlRecording::from_bytes(
        "extra.telemetry.ext.jsonl",
        concat!(
            "{\"mtx\":1,\"n\":\"Ride\",\"q\":10000000,\"dur\":40000000,\"vis\":1,\"utc\":1700000000000000000,\"tz\":\"America/New_York\"}\n",
            "{\"n\":\"Ride Height FL\",\"hz\":100,\"u\":\"mm\",\"vis\":1,\"v\":[42,41],\"t0\":20000000}\n",
        )
        .as_bytes(),
    )
    .unwrap();
    let merged = host.attach(&ext).unwrap();
    assert!(!merged.is_extension());
    assert_eq!(merged.channels().len(), 2);
    assert_eq!(merged.sample_time_ns(1, 0, 0), 20_000_000);
    assert_eq!(merged.decode(1, 0, 0), 42.0);
    assert_eq!(merged.sample_at(1, 20_000_000, false), Some(42.0));
    assert_eq!(merged.sample_at(1, 0, false), None);
}

#[test]
fn attach_translates_on_utc_nanoseconds() {
    let host = JsonlRecording::from_bytes(
        "host.jsonl",
        concat!(
            "{\"mtj\":1,\"q\":10000000,\"dur\":40000000,\"utc\":1000,\"tz\":\"UTC\"}\n",
            "[]\n",
            "{\"n\":\"Speed\",\"hz\":100,\"v\":[1,2,3,4]}\n",
        )
        .as_bytes(),
    )
    .unwrap();
    let ext = JsonlRecording::from_bytes(
        "extra.telemetry.ext.jsonl",
        concat!(
            "{\"mtx\":1,\"n\":\"Extra group\",\"q\":10000000,\"dur\":20000000,\"vis\":1,\"utc\":20001000,\"tz\":\"UTC\"}\n",
            "{\"n\":\"Extra\",\"hz\":100,\"vis\":1,\"v\":[9,8]}\n",
        )
        .as_bytes(),
    )
    .unwrap();
    let merged = host.attach(&ext).unwrap();
    assert_eq!(merged.sample_time_ns(1, 0, 0), 20_000_000);
    assert_eq!(merged.decode(1, 0, 0), 9.0);
}

#[test]
fn parses_and_attaches_channel_labels() {
    let host = JsonlRecording::from_bytes(
        "host.jsonl",
        concat!(
            "{\"mtj\":1,\"q\":10000000,\"dur\":40000000,\"utc\":1000,\"tz\":\"UTC\"}\n",
            "[]\n",
            "{\"n\":\"Speed\",\"hz\":100,\"v\":[1,2,3,4],\"lbl\":[[10000000,\"brake lock\"]]}\n",
        )
        .as_bytes(),
    )
    .unwrap();
    assert_eq!(host.channel_labels(0)[0].time_ns, 10_000_000);
    assert_eq!(host.channel_labels(0)[0].text, "brake lock");
    let ext = JsonlRecording::from_bytes(
        "extra.telemetry.ext.jsonl",
        concat!(
            "{\"mtx\":1,\"n\":\"Notes\",\"q\":10000000,\"dur\":20000000,\"vis\":1,\"utc\":20001000,\"tz\":\"UTC\"}\n",
            "{\"n\":\"Ride\",\"hz\":100,\"vis\":1,\"v\":[9,8],\"lbl\":[[0,\"pit in\"]]}\n",
        )
        .as_bytes(),
    )
    .unwrap();
    let merged = host.attach(&ext).unwrap();
    assert_eq!(merged.channel_labels(0)[0].text, "brake lock");
    assert_eq!(merged.channel_labels(1)[0].time_ns, 20_000_000);
    assert_eq!(merged.channel_labels(1)[0].text, "pit in");
}

#[test]
fn rejects_labels_on_foreign_plot() {
    let err = JsonlRecording::from_bytes(
        "bad.jsonl",
        concat!(
            "{\"mtj\":1,\"q\":1000000000,\"dur\":2000000000}\n",
            "[]\n",
            "{\"n\":\"Heart Rate\",\"hz\":1,\"plt\":\"gauge\",\"v\":[140],\"lbl\":[[0,\"spike\"]]}\n",
        )
        .as_bytes(),
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("only allowed on plt=trace"),
        "{err}"
    );
}

#[test]
fn parses_foreign_channel_display() {
    let opened = JsonlRecording::from_bytes(
        "bio.jsonl",
        concat!(
            "{\"mtj\":1,\"q\":1000000000,\"dur\":2000000000}\n",
            "[]\n",
            "{\"n\":\"Water Temp\",\"hz\":1,\"u\":\"°C\",\"plt\":\"gauge\",\"sc\":[60,120],\"rnd\":1,\"fmt\":\"0.0°C\",\"v\":[88.4,89.1]}\n",
        )
        .as_bytes(),
    )
    .unwrap();
    let display = opened.channel_display(0);
    assert_eq!(display.plot, ChannelPlot::Gauge);
    assert_eq!(display.scale_min, Some(60.0));
    assert_eq!(display.scale_max, Some(120.0));
    assert_eq!(display.decimals, Some(1));
    assert_eq!(display.format, "0.0°C");
    assert!(opened.channel_labels(0).is_empty());
}

#[test]
fn rejects_label_off_the_lattice() {
    let err = JsonlRecording::from_bytes(
        "bad.jsonl",
        concat!(
            "{\"mtj\":1,\"q\":10000000,\"dur\":40000000}\n",
            "[]\n",
            "{\"n\":\"Speed\",\"hz\":100,\"v\":[1,2],\"lbl\":[[1,\"x\"]]}\n",
        )
        .as_bytes(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("lattice"), "{err}");
}

#[test]
fn attach_joins_on_utc_ns_not_clk_abs() {
    let host = JsonlRecording::from_bytes(
        "host.jsonl",
        concat!(
            "{\"mtj\":1,\"q\":10000000,\"dur\":40000000,\"utc\":1000,\"tz\":\"UTC\",",
            "\"clk\":\"gps\",\"abs\":1}\n",
            "[]\n",
            "{\"n\":\"Speed\",\"hz\":100,\"v\":[1,2,3,4]}\n",
        )
        .as_bytes(),
    )
    .unwrap();
    let ext = JsonlRecording::from_bytes(
        "extra.telemetry.ext.jsonl",
        concat!(
            "{\"mtx\":1,\"n\":\"Extra group\",\"q\":10000000,\"dur\":20000000,\"vis\":1,",
            "\"utc\":1000,\"tz\":\"UTC\",\"clk\":\"gps\",\"abs\":999999}\n",
            "{\"n\":\"Extra\",\"hz\":100,\"vis\":1,\"v\":[9,8],\"t0\":10000000}\n",
        )
        .as_bytes(),
    )
    .unwrap();
    let merged = host.attach(&ext).unwrap();
    assert_eq!(merged.sample_time_ns(1, 0, 0), 10_000_000);
}

#[test]
fn attach_rejects_duplicate_channel_names() {
    let host = JsonlRecording::from_bytes(
        "host.jsonl",
        concat!(
            "{\"mtj\":1,\"q\":10000000,\"dur\":40000000}\n",
            "[]\n",
            "{\"n\":\"Speed\",\"hz\":100,\"v\":[1,2]}\n",
        )
        .as_bytes(),
    )
    .unwrap();
    let ext = JsonlRecording::from_bytes(
        "extra.telemetry.ext.jsonl",
        concat!(
            "{\"mtx\":1,\"n\":\"Dup\",\"q\":10000000,\"dur\":20000000,\"vis\":1,\"utc\":1700000000000000000,\"tz\":\"UTC\"}\n",
            "{\"n\":\"Speed\",\"hz\":100,\"vis\":0,\"v\":[9,8]}\n",
        )
        .as_bytes(),
    )
    .unwrap();
    let err = host.attach(&ext).unwrap_err();
    assert!(err.to_string().contains("already exists"));
}

#[test]
fn parses_sidecar_group_of_stint_spans() {
    let text = concat!(
        "{\"mtx\":1,\"n\":\"Sebring 12H 2025\",\"q\":1000000,\"dur\":12600000000000,\"vis\":0,",
        "\"utc\":1742040000000000000,\"tz\":\"America/New_York\",",
        "\"r\":[{\"t\":\"LMP2 stints during the race\"},{\"p\":[\"Avg lap\",\"1:52.1\"]}]}\n",
        "{\"k\":\"s\",\"n\":\"443-1\",\"s\":0,\"e\":5400000000000,\"vis\":1,\"c\":\"#e11d48\",",
        "\"p\":{\"title\":\"#443\",\"sub\":\"EL · 1:52.1\"},",
        "\"m\":[[\"Laps\",\"28\"],[\"Best\",\"1:50.332\"],[\"Avg\",\"1:52.104\"],[\"License\",\"IMSA\"]]}\n",
        "{\"k\":\"s\",\"n\":\"443-2\",\"s\":5400000000000,\"e\":12600000000000,\"vis\":0,\"c\":\"#2563eb\",",
        "\"p\":{\"title\":\"#443\",\"sub\":\"MB · 1:51.8\"},",
        "\"m\":[[\"Laps\",\"38\"],[\"Best\",\"1:50.110\"],[\"Avg\",\"1:51.804\"],[\"License\",\"IMSA\"]]}\n",
    );
    let opened = JsonlRecording::from_bytes("lmp2.telemetry.ext.jsonl", text.as_bytes()).unwrap();
    assert!(opened.is_extension());
    let group = &opened.sidecar_groups()[0].header;
    assert_eq!(group.name, "Sebring 12H 2025");
    assert!(!group.visible);
    assert_eq!(
        group.right,
        [
            HeaderChrome::Text("LMP2 stints during the race".into()),
            HeaderChrome::Pill {
                label: "Avg lap".into(),
                value: "1:52.1".into()
            }
        ]
    );
    assert!(opened.channels().is_empty());
    assert_eq!(opened.spans().len(), 2);
    let stint = &opened.spans()[0];
    assert!(stint.visible);
    assert!(!opened.spans()[1].visible);
    assert_eq!(stint.primary.title, "#443");
    assert_eq!(stint.primary.subtitle, "EL · 1:52.1");
    assert_eq!(stint.color, "#e11d48");
    assert_eq!(
        stint.meta,
        [
            ("Laps".into(), SpanMetaValue::Text("28".into())),
            ("Best".into(), SpanMetaValue::TimeMs(110_332)),
            ("Avg".into(), SpanMetaValue::TimeMs(112_104)),
            ("License".into(), SpanMetaValue::Text("IMSA".into())),
        ]
    );
    assert_eq!(stint.end_ns - stint.start_ns, 5_400_000_000_000);
}

#[test]
fn timespan_ms_meta_is_integer_and_averages() {
    let opened = JsonlRecording::from_bytes(
        "stints.telemetry.ext.jsonl",
        concat!(
            "{\"mtx\":1,\"n\":\"Times\",\"q\":1000000,\"dur\":2000000,\"vis\":1,",
            "\"utc\":1742040000000000000,\"tz\":\"UTC\"}\n",
            "{\"k\":\"s\",\"n\":\"a\",\"s\":0,\"e\":1000000,\"vis\":1,\"m\":",
            "[[\"Best\",{\"v\":110332,\"u\":\"timespan_ms\"}]]}\n",
            "{\"k\":\"s\",\"n\":\"b\",\"s\":1000000,\"e\":2000000,\"vis\":1,\"m\":",
            "[[\"Best\",110110]]}\n",
        )
        .as_bytes(),
    )
    .unwrap();
    let times: Vec<u32> = opened
        .spans()
        .iter()
        .map(|span| span.meta[0].1.as_timespan_ms().unwrap())
        .collect();
    assert_eq!(times, [110_332, 110_110]);
    assert_eq!(
        motorsport_telemetry_core::average_timespan_ms(&times),
        Some(110_221)
    );
    assert_eq!(
        motorsport_telemetry_core::format_timespan_ms(times[0]),
        "1:50.332"
    );
    assert_eq!(opened.spans()[0].meta[0].1.display(), "1:50.332");
}

#[test]
fn timeline_round_trip_preserves_primary_and_meta() {
    let header = SidecarHeader {
        name: "Sebring 12H 2025".into(),
        visible: true,
        right: vec![
            HeaderChrome::Text("LMP2 stints during the race".into()),
            HeaderChrome::Pill {
                label: "Avg lap".into(),
                value: "1:52.1".into(),
            },
        ],
        utc_start_ns: 1_742_040_000_000_000_000,
        timezone: "America/New_York".into(),
    };
    let spans = [Span {
        name: "443-1".into(),
        start_ns: 0,
        end_ns: 1_000_000_000,
        visible: true,
        color: "#e11d48".into(),
        primary: SpanPrimary {
            title: "#443".into(),
            subtitle: "EL".into(),
        },
        meta: vec![
            ("Laps".into(), SpanMetaValue::Text("18".into())),
            ("Total drive time".into(), SpanMetaValue::TimeMs(5_400_000)),
            ("Driver License".into(), SpanMetaValue::Text("IMSA".into())),
        ],
    }];
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("stints.telemetry.ext.jsonl");
    write_jsonl_timeline_with(&dest, &header, 1_000_000, 0, &spans, false).unwrap();
    let text = std::fs::read_to_string(&dest).unwrap();
    assert!(text.starts_with("{\"mtx\":1,\"n\":\"Sebring 12H 2025\""));
    assert!(text.contains("\"k\":\"s\""));
    assert!(!text.contains("\"k\":\"f\""));
    assert!(text.contains("\"title\":\"#443\""));
    assert!(text.contains("[\"Driver License\",\"IMSA\"]"));
    let opened = JsonlRecording::open(&dest).unwrap();
    assert_eq!(opened.sidecar_groups()[0].header, header);
    assert_eq!(opened.spans(), spans.as_slice());
}

#[test]
fn attach_shifts_span_times_with_utc_ns() {
    let host = JsonlRecording::from_bytes(
        "host.jsonl",
        concat!(
            "{\"mtj\":1,\"q\":1000000,\"dur\":4000000000,\"utc\":1000,\"tz\":\"UTC\"}\n",
            "[]\n",
            "{\"n\":\"Speed\",\"hz\":1,\"v\":[1,2,3,4]}\n",
        )
        .as_bytes(),
    )
    .unwrap();
    let ext = JsonlRecording::from_bytes(
        "stints.telemetry.ext.jsonl",
        concat!(
            "{\"mtx\":1,\"n\":\"Sebring 12H 2025\",\"q\":1000000,\"dur\":2000000,\"vis\":1,\"utc\":2001000,\"tz\":\"UTC\"}\n",
            "{\"k\":\"s\",\"s\":0,\"e\":1000000,\"vis\":1,\"p\":{\"title\":\"#443\"}}\n",
        )
        .as_bytes(),
    )
    .unwrap();
    let merged = host.attach(&ext).unwrap();
    let span = &merged.spans()[0];
    assert_eq!(span.start_ns, 2_000_000);
    assert_eq!(span.end_ns, 3_000_000);
    assert_eq!(span.primary.title, "#443");
}

#[test]
fn rejects_invalid_span_color() {
    let text = concat!(
        "{\"mtx\":1,\"n\":\"Bad\",\"q\":1000000,\"dur\":2000000,\"vis\":1,\"utc\":1700000000000000000,\"tz\":\"UTC\"}\n",
        "{\"k\":\"s\",\"s\":0,\"e\":1000000,\"vis\":1,\"c\":\"red\"}\n",
    );
    let err = JsonlRecording::from_bytes("bad.ext.jsonl", text.as_bytes()).unwrap_err();
    assert!(err.to_string().contains("#RRGGBB"));
}

#[test]
fn rejects_mtx_missing_utc_or_tz() {
    let missing_utc =
        "{\"mtx\":1,\"n\":\"X\",\"q\":1000000,\"dur\":1000000,\"vis\":1,\"tz\":\"UTC\"}\n";
    let err = JsonlRecording::from_bytes("bad.ext.jsonl", missing_utc.as_bytes()).unwrap_err();
    assert!(err.to_string().contains("missing utc"), "{err}");
    let missing_tz =
        "{\"mtx\":1,\"n\":\"X\",\"q\":1000000,\"dur\":1000000,\"vis\":1,\"utc\":1700000000000000000}\n";
    let err = JsonlRecording::from_bytes("bad.ext.jsonl", missing_tz.as_bytes()).unwrap_err();
    assert!(err.to_string().contains("missing tz"), "{err}");
}

#[test]
fn rejects_folder_records() {
    let text = concat!(
        "{\"mtx\":1,\"n\":\"X\",\"q\":1000000,\"dur\":1000000,\"vis\":1,\"utc\":1700000000000000000,\"tz\":\"UTC\"}\n",
        "{\"k\":\"f\",\"n\":\"LMP2\",\"x\":[]}\n",
    );
    let err = JsonlRecording::from_bytes("bad.ext.jsonl", text.as_bytes()).unwrap_err();
    assert!(err.to_string().contains("folder records are not used"));
}

#[test]
fn alignment_snaps_jittered_samples_to_the_nearest_slot() {
    let period_ns = 10_000_000u64;
    // ±0.5 ms and ±3 ms of logger jitter both land every sample in its own
    // nearest slot; the old writer dropped the whole channel past 2 ms.
    for jitter_ns in [500_000_i64, 3_000_000_i64] {
        let mut source = tiny();
        source.sample_times = vec![jittered_times(period_ns, 4, 0, jitter_ns)];
        assert!(collect_aligned(&source, 0, &source.channels[0]).is_some());
        assert!(collect_aligned(&source, 1, &source.channels[1]).is_some());

        let (bytes, opened) = write_alignment_jsonl(&source);
        let names: Vec<&str> = opened
            .channels()
            .iter()
            .map(|ch| ch.name.as_str())
            .collect();
        assert_eq!(names, ["Speed", "GPS Speed"], "jitter_ns={jitter_ns}");
        assert_eq!(opened.quantum_ns(), period_ns);
        assert_eq!(opened.origin_ns(), 0);
        assert_eq!(opened.duration_ns(), 40_000_000);
        assert_eq!(opened.channels()[0].sample_count, 4);
        assert_eq!(opened.channels()[0].chunks[0].sample_period_ns, period_ns);
        assert_eq!(opened.decode(0, 0, 0), 10.0);
        assert_eq!(opened.decode(0, 0, 2), 12.5);
        assert_eq!(opened.decode(0, 0, 3), 13.0);
        for index in 0..4u64 {
            assert_eq!(opened.sample_time_ns(0, 0, index), index * period_ns);
        }
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("\"n\":\"Speed\""));
        assert!(text.contains("\"hz\":100"));
    }
}

#[test]
fn alignment_mixed_period_chunks_land_on_the_dominant_lattice() {
    let mut source = tiny();
    source.channels.push(Channel {
        id: 3,
        name: "Beacon".into(),
        unit: String::new(),
        unit_source: UnitSource::Unknown,
        sample_type: SampleType::F64,
        chunks: vec![
            Chunk {
                sample_period_ns: 10_000_000,
                sample_count: 3,
                data_ptr: 0,
                sample_base: 0,
                time_base_ns: 0,
            },
            Chunk {
                sample_period_ns: 20_000_000,
                sample_count: 2,
                data_ptr: 0,
                sample_base: 3,
                time_base_ns: 40_000_000,
            },
        ],
        sample_count: 5,
        duration_ns: 60_000_000,
    });
    source.values.push(vec![1.0, 2.0, 3.0, 4.0, 5.0]);
    // Lattice is the 10 ms period (three samples beat two). Samples at 0, 10,
    // 20, 40, 60 ms occupy slots 0, 1, 2, 4, 6; slots 3 and 5 are null.
    let series = collect_aligned(&source, 2, &source.channels[2]).unwrap();
    assert_eq!(series.period_ns, 10_000_000);
    assert_eq!(
        series.values,
        vec![
            Some(1.0),
            Some(2.0),
            Some(3.0),
            None,
            Some(4.0),
            None,
            Some(5.0)
        ]
    );

    let (_, opened) = write_alignment_jsonl(&source);
    assert_eq!(opened.channels().len(), 3);
    let beacon = opened
        .channels()
        .iter()
        .position(|ch| ch.name == "Beacon")
        .unwrap();
    assert_eq!(opened.decode(beacon, 0, 0), 1.0);
    assert_eq!(opened.decode(beacon, 0, 2), 3.0);
    assert_eq!(opened.decode(beacon, 0, 4), 4.0);
    assert_eq!(opened.decode(beacon, 0, 6), 5.0);
    assert_eq!(opened.quantum_ns(), 10_000_000);
}
#[test]
fn alignment_two_holes_fill_null_and_keep_indexes() {
    let period_ns = 10_000_000u64;
    let t0_ns = 10_000_000u64;
    let mut source = tiny();
    source.channels[0].chunks = vec![
        Chunk {
            sample_period_ns: period_ns,
            sample_count: 2,
            data_ptr: 0,
            sample_base: 0,
            time_base_ns: t0_ns,
        },
        Chunk {
            sample_period_ns: period_ns,
            sample_count: 1,
            data_ptr: 0,
            sample_base: 2,
            time_base_ns: t0_ns + 3 * period_ns,
        },
        Chunk {
            sample_period_ns: period_ns,
            sample_count: 1,
            data_ptr: 0,
            sample_base: 3,
            time_base_ns: t0_ns + 5 * period_ns,
        },
    ];
    source.channels[0].sample_count = 4;
    source.channels[0].duration_ns = t0_ns + 6 * period_ns;
    source.values[0] = vec![10.0, 11.0, 13.0, 15.0];

    let (bytes, opened) = write_alignment_jsonl(&source);
    let text = String::from_utf8(bytes).unwrap();
    assert!(text.contains("\"v\":[10,11,null,13,null,15]"));
    assert!(text.contains("\"t0\":10000000"));
    assert_eq!(opened.quantum_ns(), period_ns);
    assert_eq!(opened.origin_ns(), 0);
    assert_eq!(opened.channels()[0].name, "Speed");
    assert_eq!(opened.channels()[0].sample_count, 6);
    assert_eq!(opened.channels()[0].chunks[0].sample_period_ns, period_ns);
    assert_eq!(opened.channels()[0].chunks[0].time_base_ns, t0_ns);
    let count = opened.channels()[0].sample_count;
    for index in 0..count {
        assert_eq!(
            opened.sample_time_ns(0, 0, index),
            t0_ns + index * period_ns
        );
    }
    assert_eq!(opened.decode(0, 0, 0), 10.0);
    assert_eq!(opened.decode(0, 0, 1), 11.0);
    assert!(opened.decode(0, 0, 2).is_nan());
    assert_eq!(opened.decode(0, 0, 3), 13.0);
    assert!(opened.decode(0, 0, 4).is_nan());
    assert_eq!(opened.decode(0, 0, 5), 15.0);
}

#[test]
fn alignment_snap_laps_and_spans_to_lattice() {
    let quantum_ns = 10_000_000u64;
    let lap_cases = [
        (0, 40_000_000, 0, 40_000_000),
        (2_000_000, 38_000_000, 0, 40_000_000),
        (8_000_000, 42_000_000, 10_000_000, 40_000_000),
        (12_000_000, 13_000_000, 10_000_000, 20_000_000),
    ];
    for (start_ns, end_ns, expect_start, expect_end) in lap_cases {
        let snapped = snap_laps(
            &[LapMetadata {
                number: 1,
                start_ns,
                end_ns,
                duration_ns: end_ns - start_ns,
                complete: true,
                first_video_frame: None,
            }],
            quantum_ns,
        )
        .unwrap();
        assert_eq!(snapped[0].start_ns, expect_start, "lap start {start_ns}");
        assert_eq!(snapped[0].end_ns, expect_end, "lap end {end_ns}");
        assert_eq!(snapped[0].duration_ns, expect_end - expect_start);
    }

    let span_cases = [
        (0, 40_000_000, 0, 40_000_000),
        (1_000_000, 22_000_000, 0, 20_000_000),
        (8_000_000, 42_000_000, 10_000_000, 40_000_000),
        (12_000_000, 13_000_000, 10_000_000, 20_000_000),
    ];
    for (start_ns, end_ns, expect_start, expect_end) in span_cases {
        let snapped = snap_spans(&[alignment_span("pit", start_ns, end_ns)], quantum_ns).unwrap();
        assert_eq!(snapped[0].start_ns, expect_start, "span start {start_ns}");
        assert_eq!(snapped[0].end_ns, expect_end, "span end {end_ns}");
    }

    let mut source = tiny();
    source.laps = vec![
        LapMetadata {
            number: 1,
            start_ns: 2_000_000,
            end_ns: 38_000_000,
            duration_ns: 36_000_000,
            complete: false,
            first_video_frame: None,
        },
        LapMetadata {
            number: 2,
            start_ns: 12_000_000,
            end_ns: 13_000_000,
            duration_ns: 1_000_000,
            complete: true,
            first_video_frame: None,
        },
    ];
    source.spans = vec![
        alignment_span("near", 1_000_000, 22_000_000),
        alignment_span("collapse", 12_000_000, 13_000_000),
    ];
    let (bytes, opened) = write_alignment_jsonl(&source);
    assert_eq!(opened.quantum_ns(), quantum_ns);
    assert_eq!(opened.origin_ns(), 0);
    assert_eq!(opened.metadata().laps[0].start_ns, 0);
    assert_eq!(opened.metadata().laps[0].end_ns, 40_000_000);
    assert_eq!(opened.metadata().laps[0].duration_ns, 40_000_000);
    assert_eq!(opened.metadata().laps[1].start_ns, 10_000_000);
    assert_eq!(opened.metadata().laps[1].end_ns, 20_000_000);
    assert_eq!(opened.metadata().laps[1].duration_ns, 10_000_000);
    assert_eq!(opened.spans()[0].name, "near");
    assert_eq!(opened.spans()[0].start_ns, 0);
    assert_eq!(opened.spans()[0].end_ns, 20_000_000);
    assert_eq!(opened.spans()[1].name, "collapse");
    assert_eq!(opened.spans()[1].start_ns, 10_000_000);
    assert_eq!(opened.spans()[1].end_ns, 20_000_000);
    let text = String::from_utf8(bytes).unwrap();
    assert!(text.contains("[[1,0,40000000,0],[2,10000000,20000000,1]]"));
    assert!(text.contains("\"s\":0,\"e\":20000000"));
    assert!(text.contains("\"s\":10000000,\"e\":20000000"));
}

#[test]
fn alignment_snap_up_duration_to_lattice() {
    let quantum_ns = 10_000_000u64;
    let cases = [
        (0, 0),
        (10_000_000, 10_000_000),
        (10_000_001, 20_000_000),
        (45_000_000, 50_000_000),
        (7, 7),
    ];
    for (value, expect) in cases {
        let q = if value == 7 { 1 } else { quantum_ns };
        assert_eq!(snap_up(value, q).unwrap(), expect, "snap_up({value}, {q})");
    }

    let mut source = tiny();
    source.channels[0].duration_ns = 45_000_000;
    source.laps.clear();
    let (bytes, opened) = write_alignment_jsonl(&source);
    assert_eq!(opened.quantum_ns(), quantum_ns);
    assert_eq!(opened.origin_ns(), 0);
    assert_eq!(opened.duration_ns(), 50_000_000);
    assert_eq!(opened.channels()[0].name, "Speed");
    assert_eq!(opened.channels()[0].sample_count, 4);
    let text = String::from_utf8(bytes).unwrap();
    assert!(text.contains("\"dur\":50000000"));
}
