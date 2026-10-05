use super::*;
use std::io::Cursor;

fn atom(kind: [u8; 4], bytes: &[u8]) -> Vec<u8> {
    let mut out = ((bytes.len() + 8) as u32).to_be_bytes().to_vec();
    out.extend(kind);
    out.extend(bytes);
    out
}

fn words(values: &[u32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_be_bytes())
        .collect()
}

fn movie(
    runs: &[(u32, u32)],
    composition: Option<(u8, &[(u32, i32)])>,
    edits: Option<&[(u32, i32, u32)]>,
) -> Vec<u8> {
    let mut stts = words(&[0, runs.len() as u32]);
    for (count, delta) in runs {
        stts.extend(words(&[*count, *delta]));
    }
    let count: u32 = runs.iter().map(|(count, _)| *count).sum();
    let mut stbl = atom(*b"stts", &stts);
    stbl.extend(atom(*b"stsz", &words(&[0, 1, count])));
    if let Some((version, runs)) = composition {
        let mut ctts = words(&[u32::from(version) << 24, runs.len() as u32]);
        for (count, offset) in runs {
            ctts.extend(words(&[*count, *offset as u32]));
        }
        stbl.extend(atom(*b"ctts", &ctts));
    }
    let mut mdia = atom(*b"mdhd", &words(&[0, 0, 0, 1000]));
    let mut handler = words(&[0, 0]);
    handler.extend(b"vide");
    mdia.extend(atom(*b"hdlr", &handler));
    mdia.extend(atom(*b"minf", &atom(*b"stbl", &stbl)));
    let mut trak = atom(*b"tkhd", &words(&[0, 0, 0, 7]));
    trak.extend(atom(*b"mdia", &mdia));
    if let Some(edits) = edits {
        let mut elst = words(&[0, edits.len() as u32]);
        for (duration, media_time, rate) in edits {
            elst.extend(words(&[*duration, *media_time as u32, *rate]));
        }
        trak.extend(atom(*b"edts", &atom(*b"elst", &elst)));
    }
    let mut moov = atom(*b"mvhd", &words(&[0, 0, 0, 1000]));
    moov.extend(atom(*b"trak", &trak));
    atom(*b"moov", &moov)
}

fn inspect(bytes: Vec<u8>) -> Result<VideoMediaMetadata, AimError> {
    let size = bytes.len() as u64;
    inspect_reader(&mut Cursor::new(bytes), size, "fixture.mp4")
}

#[test]
fn run_tables_count_billions_of_samples_without_per_frame_expansion() {
    let metadata = inspect(movie(&[(1_000_000_000, 40)], None, None)).unwrap();
    assert_eq!(
        metadata.video_streams[0],
        VideoStreamMetadata {
            track_id: 7,
            frame_count: 1_000_000_000,
            presentation_start_ns: 0,
            presentation_end_ns: 40_000_000_000_000_000,
        }
    );
}

#[test]
fn composition_runs_and_variable_decode_deltas_determine_extent() {
    let metadata = inspect(movie(
        &[(2, 40), (2, 60)],
        Some((1, &[(1, -20), (2, 30), (1, 0)])),
        None,
    ))
    .unwrap();
    assert_eq!(metadata.video_streams[0].frame_count, 4);
    assert_eq!(metadata.video_streams[0].presentation_start_ns, -20_000_000);
    assert_eq!(metadata.video_streams[0].presentation_end_ns, 200_000_000);
}

#[test]
fn edit_lists_clip_media_and_preserve_empty_lead_in_and_multiple_edits() {
    let metadata = inspect(movie(
        &[(10, 100)],
        None,
        Some(&[(200, -1, 65536), (500, 100, 65536), (100, 800, 65536)]),
    ))
    .unwrap();
    assert_eq!(metadata.video_streams[0].frame_count, 10);
    assert_eq!(metadata.video_streams[0].presentation_start_ns, 200_000_000);
    assert_eq!(metadata.video_streams[0].presentation_end_ns, 800_000_000);
    assert!(inspect(movie(&[(10, 100)], None, Some(&[(100, 0, 32768)]))).is_err());
    assert!(inspect(movie(&[(10, 100)], None, Some(&[(100, 2000, 65536)]))).is_err());
}

#[test]
fn mismatched_composition_tables_and_truncated_children_fail() {
    assert!(inspect(movie(&[(4, 40)], Some((0, &[(3, 0)])), None)).is_err());
    assert!(inspect(movie(&[(4, 40)], Some((2, &[(4, 0)])), None)).is_err());
    assert!(inspect(movie(&[(4, 0)], None, None)).is_err());
    let mut truncated = movie(&[(4, 40)], None, None);
    truncated.pop();
    assert!(inspect(truncated).is_err());
    let mut bad_table = movie(&[(4, 40)], None, None);
    let location = bad_table
        .windows(4)
        .position(|bytes| bytes == b"stts")
        .unwrap();
    bad_table[location + 8..location + 12].copy_from_slice(&2u32.to_be_bytes());
    assert!(inspect(bad_table).is_err());
}

#[test]
fn fingerprints_track_headers_and_source_size_but_not_payload_identity() {
    let mut first = atom(*b"ftyp", b"isom");
    first.extend(atom(*b"mdat", b"first payload"));
    first.extend(movie(&[(4, 40)], None, None));
    let metadata = inspect(first.clone()).unwrap();
    let payload = first
        .windows(13)
        .position(|bytes| bytes == b"first payload")
        .unwrap();
    first[payload] = b'X';
    assert_eq!(
        metadata.header_fingerprint,
        inspect(first.clone()).unwrap().header_fingerprint
    );
    first.extend(atom(*b"free", &[]));
    assert_ne!(
        metadata.header_fingerprint,
        inspect(first).unwrap().header_fingerprint
    );
    assert_ne!(
        metadata.header_fingerprint,
        inspect(movie(&[(4, 41)], None, None))
            .unwrap()
            .header_fingerprint
    );
}

/// A fake 3 GB file whose media bytes panic if read, with moov at its end.
struct SparseMedia {
    position: u64,
    header: Vec<u8>,
    moov: Vec<u8>,
    moov_at: u64,
}

impl Read for SparseMedia {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let available = if self.position < self.header.len() as u64 {
            &self.header[self.position as usize..]
        } else if self.position >= self.moov_at {
            &self.moov[(self.position - self.moov_at) as usize..]
        } else {
            panic!("inspector attempted to read video payload");
        };
        let count = available.len().min(buffer.len());
        buffer[..count].copy_from_slice(&available[..count]);
        self.position += count as u64;
        Ok(count)
    }
}

impl Seek for SparseMedia {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        let SeekFrom::Start(position) = position else {
            panic!("unexpected relative seek");
        };
        self.position = position;
        Ok(position)
    }
}

#[test]
fn large_payload_is_never_read_and_metadata_limits_fail_before_reading() {
    let moov_at = 3_000_000_000u64;
    let mut header = words(&[1]);
    header.extend(b"mdat");
    header.extend(moov_at.to_be_bytes());
    let moov = movie(&[(4, 40)], None, None);
    let size = moov_at + moov.len() as u64;
    let mut sparse = SparseMedia {
        position: 0,
        header,
        moov,
        moov_at,
    };
    let metadata = inspect_reader(&mut sparse, size, "sparse.mp4").unwrap();
    assert_eq!(metadata.byte_size, size);
    assert_eq!(metadata.video_streams[0].presentation_end_ns, 160_000_000);

    let header = [((MAX_MOOV + 1) as u32).to_be_bytes().as_slice(), b"moov"].concat();
    let mut oversized = SparseMedia {
        position: 0,
        header,
        moov: vec![],
        moov_at: MAX_MOOV + 1,
    };
    assert!(inspect_reader(&mut oversized, MAX_MOOV + 1, "oversized.mp4").is_err());
}

#[test]
fn fragmented_and_duplicate_containers_are_explicitly_unsupported() {
    let mut bytes = movie(&[(4, 40)], None, None);
    bytes.extend(atom(*b"moof", &[]));
    assert!(inspect(bytes).is_err());
    let mut bytes = movie(&[(4, 40)], None, None);
    bytes.extend(bytes.clone());
    assert!(inspect(bytes).is_err());
    assert!(inspect(vec![]).is_err());
}

#[test]
fn container_mutations_return_results_without_panicking() {
    let bytes = movie(
        &[(4, 40)],
        Some((1, &[(2, -20), (2, 30)])),
        Some(&[(100, 0, 65536)]),
    );
    for index in 0..bytes.len() {
        for value in [0u8, 255] {
            let mut altered = bytes.clone();
            altered[index] = value;
            let _result = inspect(altered);
        }
        let _result = inspect(bytes[..index].to_vec());
    }
}
