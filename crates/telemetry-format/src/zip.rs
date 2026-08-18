//! Aligned STORE zip profile used by `.telemetry` files.

use std::io::{Read, Seek, Write};

const LOCAL_SIG: u32 = 0x0403_4b50;
const CENTRAL_SIG: u32 = 0x0201_4b50;
const EOCD_SIG: u32 = 0x0605_4b50;
/// ZIP64 end-of-central-directory record signature.
const ZIP64_EOCD_SIG: u32 = 0x0606_4b50;
/// ZIP64 end-of-central-directory locator signature.
const ZIP64_EOCD_LOC_SIG: u32 = 0x0706_4b50;
/// Header ID for the ZIP64 extended information extra field.
const ZIP64_EXTRA: u16 = 0x0001;
const ALIGN: u64 = 64;

#[derive(Debug)]
pub(crate) struct ZipError(pub String);

impl std::fmt::Display for ZipError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ZipError {}

#[derive(Debug, Clone)]
pub(crate) struct Member {
    pub name: String,
    pub offset: u64,
    pub size: u64,
}

pub(crate) struct ZipWriter<W> {
    inner: W,
    entries: Vec<(String, u64, u64, u32)>,
}

impl<W: Write + Seek> ZipWriter<W> {
    pub(crate) fn new(inner: W) -> Self {
        Self {
            inner,
            entries: Vec::new(),
        }
    }

    pub(crate) fn write_member(&mut self, name: &str, data: &[u8]) -> Result<u64, ZipError> {
        let start = self.inner.stream_position().map_err(io)?;
        let name_bytes = name.as_bytes();
        if name_bytes.len() > u16::MAX as usize {
            return Err(ZipError("member name too long".into()));
        }
        let zip64 = start > u32::MAX as u64 || data.len() as u64 > u32::MAX as u64;
        let mut extra = Vec::new();
        if zip64 {
            extra.extend_from_slice(&ZIP64_EXTRA.to_le_bytes());
            extra.extend_from_slice(&16u16.to_le_bytes());
            extra.extend_from_slice(&(data.len() as u64).to_le_bytes());
            extra.extend_from_slice(&(data.len() as u64).to_le_bytes());
        }
        let header = 30u64 + name_bytes.len() as u64;
        let mut extra_len = extra.len() as u64;
        let pad = (ALIGN - (start + header + extra_len) % ALIGN) % ALIGN;
        if pad > 0 && pad < 4 {
            extra_len += pad + ALIGN;
        } else {
            extra_len += pad;
        }
        extra.resize(extra_len as usize, 0);

        let crc = crc32(data);
        let version = if zip64 { 45u16 } else { 20 };
        let stored32 = if zip64 { u32::MAX } else { data.len() as u32 };

        self.inner.write_all(&LOCAL_SIG.to_le_bytes()).map_err(io)?;
        self.inner.write_all(&version.to_le_bytes()).map_err(io)?;
        self.inner.write_all(&0u16.to_le_bytes()).map_err(io)?;
        self.inner.write_all(&0u16.to_le_bytes()).map_err(io)?;
        self.inner.write_all(&0u16.to_le_bytes()).map_err(io)?;
        self.inner.write_all(&0u16.to_le_bytes()).map_err(io)?;
        self.inner.write_all(&crc.to_le_bytes()).map_err(io)?;
        self.inner.write_all(&stored32.to_le_bytes()).map_err(io)?;
        self.inner.write_all(&stored32.to_le_bytes()).map_err(io)?;
        self.inner
            .write_all(&(name_bytes.len() as u16).to_le_bytes())
            .map_err(io)?;
        self.inner
            .write_all(&(extra.len() as u16).to_le_bytes())
            .map_err(io)?;
        self.inner.write_all(name_bytes).map_err(io)?;
        self.inner.write_all(&extra).map_err(io)?;
        let data_off = self.inner.stream_position().map_err(io)?;
        if data_off % ALIGN != 0 {
            return Err(ZipError(format!(
                "payload for {name} is not {ALIGN}-byte aligned"
            )));
        }
        self.inner.write_all(data).map_err(io)?;
        self.entries
            .push((name.to_owned(), start, data.len() as u64, crc));
        Ok(data_off)
    }

    pub(crate) fn finish(mut self) -> Result<W, ZipError> {
        let cd_start = self.inner.stream_position().map_err(io)?;
        for (name, offset, size, crc) in &self.entries {
            let name_bytes = name.as_bytes();
            let zip64 = *offset > u32::MAX as u64 || *size > u32::MAX as u64;
            let mut extra = Vec::new();
            if zip64 {
                extra.extend_from_slice(&ZIP64_EXTRA.to_le_bytes());
                extra.extend_from_slice(&24u16.to_le_bytes());
                extra.extend_from_slice(&size.to_le_bytes());
                extra.extend_from_slice(&size.to_le_bytes());
                extra.extend_from_slice(&offset.to_le_bytes());
            }
            let version = if zip64 { 45u16 } else { 20 };
            let size32 = if zip64 { u32::MAX } else { *size as u32 };
            let off32 = if zip64 { u32::MAX } else { *offset as u32 };
            self.inner
                .write_all(&CENTRAL_SIG.to_le_bytes())
                .map_err(io)?;
            self.inner.write_all(&version.to_le_bytes()).map_err(io)?;
            self.inner.write_all(&version.to_le_bytes()).map_err(io)?;
            self.inner.write_all(&0u16.to_le_bytes()).map_err(io)?;
            self.inner.write_all(&0u16.to_le_bytes()).map_err(io)?;
            self.inner.write_all(&0u16.to_le_bytes()).map_err(io)?;
            self.inner.write_all(&0u16.to_le_bytes()).map_err(io)?;
            self.inner.write_all(&crc.to_le_bytes()).map_err(io)?;
            self.inner.write_all(&size32.to_le_bytes()).map_err(io)?;
            self.inner.write_all(&size32.to_le_bytes()).map_err(io)?;
            self.inner
                .write_all(&(name_bytes.len() as u16).to_le_bytes())
                .map_err(io)?;
            self.inner
                .write_all(&(extra.len() as u16).to_le_bytes())
                .map_err(io)?;
            self.inner.write_all(&0u16.to_le_bytes()).map_err(io)?;
            self.inner.write_all(&0u16.to_le_bytes()).map_err(io)?;
            self.inner.write_all(&0u16.to_le_bytes()).map_err(io)?;
            self.inner.write_all(&0u32.to_le_bytes()).map_err(io)?;
            self.inner.write_all(&off32.to_le_bytes()).map_err(io)?;
            self.inner.write_all(name_bytes).map_err(io)?;
            self.inner.write_all(&extra).map_err(io)?;
        }
        let cd_size = self.inner.stream_position().map_err(io)? - cd_start;
        write_eocd(&mut self.inner, self.entries.len(), cd_size, cd_start)?;
        Ok(self.inner)
    }
}
/// Writes the end-of-central-directory record, emitting a ZIP64 EOCD record
/// and locator (followed by the classic EOCD with sentinel values) when the
/// archive exceeds the u16 entry count or the u32 central-directory size or
/// offset. Otherwise the classic EOCD is written alone.
fn write_eocd(
    writer: &mut impl Write,
    entries_len: usize,
    cd_size: u64,
    cd_start: u64,
) -> Result<(), ZipError> {
    let need_zip64 =
        entries_len > u16::MAX as usize || cd_size > u32::MAX as u64 || cd_start > u32::MAX as u64;
    if need_zip64 {
        // The ZIP64 EOCD record sits immediately after the central directory.
        let zip64_eocd_offset = cd_start
            .checked_add(cd_size)
            .ok_or_else(|| ZipError("central directory offset overflows u64".into()))?;
        // ZIP64 EOCD record. The size field counts the bytes after itself
        // (44): two version u16s, two disk u32s, two entry-count u64s, the
        // central-directory size u64, and its offset u64.
        writer
            .write_all(&ZIP64_EOCD_SIG.to_le_bytes())
            .map_err(io)?;
        writer.write_all(&44u64.to_le_bytes()).map_err(io)?;
        writer.write_all(&45u16.to_le_bytes()).map_err(io)?;
        writer.write_all(&45u16.to_le_bytes()).map_err(io)?;
        writer.write_all(&0u32.to_le_bytes()).map_err(io)?;
        writer.write_all(&0u32.to_le_bytes()).map_err(io)?;
        let total = u64::try_from(entries_len).map_err(|_| ZipError("too many entries".into()))?;
        writer.write_all(&total.to_le_bytes()).map_err(io)?;
        writer.write_all(&total.to_le_bytes()).map_err(io)?;
        writer.write_all(&cd_size.to_le_bytes()).map_err(io)?;
        writer.write_all(&cd_start.to_le_bytes()).map_err(io)?;
        // ZIP64 EOCD locator.
        writer
            .write_all(&ZIP64_EOCD_LOC_SIG.to_le_bytes())
            .map_err(io)?;
        writer.write_all(&0u32.to_le_bytes()).map_err(io)?;
        writer
            .write_all(&zip64_eocd_offset.to_le_bytes())
            .map_err(io)?;
        writer.write_all(&1u32.to_le_bytes()).map_err(io)?;
        // Classic EOCD with sentinel values pointing at the ZIP64 record.
        writer.write_all(&EOCD_SIG.to_le_bytes()).map_err(io)?;
        writer.write_all(&0xFFFFu16.to_le_bytes()).map_err(io)?;
        writer.write_all(&0xFFFFu16.to_le_bytes()).map_err(io)?;
        writer.write_all(&0xFFFFu16.to_le_bytes()).map_err(io)?;
        writer.write_all(&0xFFFFu16.to_le_bytes()).map_err(io)?;
        writer.write_all(&0xFFFFFFFFu32.to_le_bytes()).map_err(io)?;
        writer.write_all(&0xFFFFFFFFu32.to_le_bytes()).map_err(io)?;
        writer.write_all(&0u16.to_le_bytes()).map_err(io)?;
    } else {
        let count = u16::try_from(entries_len).expect("checked by need_zip64");
        writer.write_all(&EOCD_SIG.to_le_bytes()).map_err(io)?;
        writer.write_all(&0u16.to_le_bytes()).map_err(io)?;
        writer.write_all(&0u16.to_le_bytes()).map_err(io)?;
        writer.write_all(&count.to_le_bytes()).map_err(io)?;
        writer.write_all(&count.to_le_bytes()).map_err(io)?;
        writer
            .write_all(&(cd_size as u32).to_le_bytes())
            .map_err(io)?;
        writer
            .write_all(&(cd_start as u32).to_le_bytes())
            .map_err(io)?;
        writer.write_all(&0u16.to_le_bytes()).map_err(io)?;
    }
    Ok(())
}

pub(crate) fn parse_members(data: &[u8]) -> Result<Vec<Member>, ZipError> {
    let mut members = Vec::new();
    let mut cursor = 0usize;
    while cursor + 4 <= data.len() {
        let sig = u32::from_le_bytes(
            data.get(cursor..cursor + 4)
                .and_then(|b| b.try_into().ok())
                .unwrap_or([0; 4]),
        );
        if sig != LOCAL_SIG {
            break;
        }
        let (member, next) = parse_local(data, cursor)?;
        members.push(member);
        cursor = next;
    }
    if members.is_empty() {
        return Err(ZipError("no zip members".into()));
    }
    if members[0].name != "metadata.fb" {
        return Err(ZipError("first member must be metadata.fb".into()));
    }
    Ok(members)
}

/// Reads only the first STORE member. Independent of archive length.
///
/// Requires `Seek` so the declared member size can be checked against the
/// actual remaining bytes before any allocation — a 16-byte hostile file
/// can otherwise request a multi-gigabyte buffer via the header u32.
pub(crate) fn read_first_member(
    reader: &mut (impl Read + Seek),
) -> Result<(String, Vec<u8>), ZipError> {
    use std::io::SeekFrom;
    let mut header = [0u8; 30];
    reader.read_exact(&mut header).map_err(io)?;
    let sig = u32::from_le_bytes(
        header
            .get(0..4)
            .and_then(|b| b.try_into().ok())
            .unwrap_or([0; 4]),
    );
    if sig != LOCAL_SIG {
        return Err(ZipError("not a zip local header".into()));
    }
    let method = u16::from_le_bytes(
        header
            .get(8..10)
            .and_then(|b| b.try_into().ok())
            .unwrap_or([0; 2]),
    );
    if method != 0 {
        return Err(ZipError("only STORE zip members are allowed".into()));
    }
    let name_len = u16::from_le_bytes(
        header
            .get(26..28)
            .and_then(|b| b.try_into().ok())
            .unwrap_or([0; 2]),
    ) as usize;
    let extra_len = u16::from_le_bytes(
        header
            .get(28..30)
            .and_then(|b| b.try_into().ok())
            .unwrap_or([0; 2]),
    ) as usize;
    let mut name = vec![0u8; name_len];
    reader.read_exact(&mut name).map_err(io)?;
    let mut extra = vec![0u8; extra_len];
    reader.read_exact(&mut extra).map_err(io)?;
    let mut size = u32::from_le_bytes(
        header
            .get(22..26)
            .and_then(|b| b.try_into().ok())
            .unwrap_or([0; 4]),
    ) as u64;
    if size == u32::MAX as u64 {
        size = zip64_size(&extra)?;
    }
    // The true upper bound on a member is the remaining bytes in the file.
    // A hostile header can declare gigabytes in a 16-byte file; reject that
    // before allocating.
    let pos = reader.stream_position().map_err(io)?;
    let end = reader.seek(SeekFrom::End(0)).map_err(io)?;
    reader.seek(SeekFrom::Start(pos)).map_err(io)?;
    let available = end - pos;
    if size > available {
        return Err(ZipError(format!(
            "declared member size {size} exceeds remaining file bytes {available}"
        )));
    }
    let mut data =
        vec![0u8; usize::try_from(size).map_err(|_| ZipError("member too large".into()))?];
    reader.read_exact(&mut data).map_err(io)?;
    let name = String::from_utf8(name).map_err(|_| ZipError("member name is not utf-8".into()))?;
    Ok((name, data))
}

fn parse_local(data: &[u8], at: usize) -> Result<(Member, usize), ZipError> {
    let at_end = at
        .checked_add(30)
        .ok_or_else(|| ZipError("local header offset overflow".into()))?;
    if at_end > data.len() {
        return Err(ZipError("truncated local header".into()));
    }
    let method = u16::from_le_bytes(
        data.get(at + 8..at + 10)
            .and_then(|b| b.try_into().ok())
            .unwrap_or([0; 2]),
    );
    if method != 0 {
        return Err(ZipError("only STORE zip members are allowed".into()));
    }
    let name_len = u16::from_le_bytes(
        data.get(at + 26..at + 28)
            .and_then(|b| b.try_into().ok())
            .unwrap_or([0; 2]),
    ) as usize;
    let extra_len = u16::from_le_bytes(
        data.get(at + 28..at + 30)
            .and_then(|b| b.try_into().ok())
            .unwrap_or([0; 2]),
    ) as usize;
    let name_at = at + 30;
    let extra_at = name_at
        .checked_add(name_len)
        .ok_or_else(|| ZipError("zip member name length overflows".into()))?;
    let data_at = extra_at
        .checked_add(extra_len)
        .ok_or_else(|| ZipError("zip member extra length overflows".into()))?;
    if data_at > data.len() {
        return Err(ZipError("truncated zip member header".into()));
    }
    let mut size = u32::from_le_bytes(
        data.get(at + 22..at + 26)
            .and_then(|b| b.try_into().ok())
            .unwrap_or([0; 4]),
    ) as u64;
    if size == u32::MAX as u64 {
        size = zip64_size(data.get(extra_at..data_at).unwrap_or(&[]))?;
    }
    let end = data_at
        .checked_add(usize::try_from(size).map_err(|_| ZipError("member too large".into()))?)
        .ok_or_else(|| ZipError("member overflow".into()))?;
    if end > data.len() {
        return Err(ZipError("truncated zip member data".into()));
    }
    if data_at as u64 % ALIGN != 0 {
        return Err(ZipError("zip payload is not 64-byte aligned".into()));
    }
    let name = std::str::from_utf8(data.get(name_at..extra_at).unwrap_or(&[]))
        .map_err(|_| ZipError("member name is not utf-8".into()))?
        .to_owned();
    Ok((
        Member {
            name,
            offset: data_at as u64,
            size,
        },
        end,
    ))
}

fn zip64_size(extra: &[u8]) -> Result<u64, ZipError> {
    let mut cursor = 0;
    while cursor + 4 <= extra.len() {
        let tag = u16::from_le_bytes(
            extra
                .get(cursor..cursor + 2)
                .and_then(|b| b.try_into().ok())
                .unwrap_or([0; 2]),
        );
        let len = u16::from_le_bytes(
            extra
                .get(cursor + 2..cursor + 4)
                .and_then(|b| b.try_into().ok())
                .unwrap_or([0; 2]),
        ) as usize;
        let start = cursor + 4;
        let end = start.saturating_add(len);
        if end > extra.len() {
            break;
        }
        if tag == ZIP64_EXTRA && len >= 8 {
            return Ok(u64::from_le_bytes(
                extra
                    .get(start..start + 8)
                    .and_then(|b| b.try_into().ok())
                    .unwrap_or([0; 8]),
            ));
        }
        cursor = end;
    }
    Err(ZipError("missing zip64 size".into()))
}

fn io(err: std::io::Error) -> ZipError {
    ZipError(err.to_string())
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn first_member_is_aligned_metadata() {
        let mut cursor = Cursor::new(Vec::new());
        let mut writer = ZipWriter::new(&mut cursor);
        writer.write_member("metadata.fb", b"hello").unwrap();
        writer
            .write_member("channels/0000.bin", &[1, 2, 3, 4])
            .unwrap();
        writer.finish().unwrap();
        let bytes = cursor.into_inner();
        let members = parse_members(&bytes).unwrap();
        assert_eq!(members[0].name, "metadata.fb");
        assert_eq!(members[0].offset % 64, 0);
        assert_eq!(members[1].offset % 64, 0);
        let mut reader = Cursor::new(bytes);
        let (name, data) = read_first_member(&mut reader).unwrap();
        assert_eq!(name, "metadata.fb");
        assert_eq!(data, b"hello");
    }

    #[test]
    fn classic_eocd_records_count_and_offsets() {
        // Two small entries: the central directory fits in u32 and the entry
        // count fits in u16, so the classic EOCD is written without ZIP64.
        let mut buf = Vec::new();
        write_eocd(&mut buf, 2, 100, 200).unwrap();
        // EOCD: 4 (sig) + 2 + 2 + 2 + 2 + 4 + 4 + 2 = 22 bytes, no ZIP64.
        assert_eq!(buf.len(), 22);
        assert_eq!(u32::from_le_bytes(buf[0..4].try_into().unwrap()), EOCD_SIG);
        assert_eq!(u16::from_le_bytes(buf[4..6].try_into().unwrap()), 0);
        assert_eq!(u16::from_le_bytes(buf[6..8].try_into().unwrap()), 0);
        assert_eq!(u16::from_le_bytes(buf[8..10].try_into().unwrap()), 2);
        assert_eq!(u16::from_le_bytes(buf[10..12].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(buf[12..16].try_into().unwrap()), 100);
        assert_eq!(u32::from_le_bytes(buf[16..20].try_into().unwrap()), 200);
        assert_eq!(u16::from_le_bytes(buf[20..22].try_into().unwrap()), 0);
    }

    #[test]
    fn zip64_eocd_emitted_when_entries_exceed_u16() {
        // Mocked sizes: more than u16::MAX entries forces the ZIP64 path
        // even though the central directory itself is small.
        let cd_start = 200u64;
        let cd_size = 100u64;
        let mut buf = Vec::new();
        write_eocd(&mut buf, u16::MAX as usize + 1, cd_size, cd_start).unwrap();
        let zip64_eocd_offset = cd_start + cd_size;

        // ZIP64 EOCD record (56 bytes).
        let mut cursor = 0usize;
        assert_eq!(
            u32::from_le_bytes(buf[cursor..cursor + 4].try_into().unwrap()),
            ZIP64_EOCD_SIG
        );
        cursor += 4;
        assert_eq!(
            u64::from_le_bytes(buf[cursor..cursor + 8].try_into().unwrap()),
            44
        );
        cursor += 8;
        // version made by / needed.
        assert_eq!(
            u16::from_le_bytes(buf[cursor..cursor + 2].try_into().unwrap()),
            45
        );
        cursor += 4;
        // two disk u32s.
        cursor += 8;
        let total = u16::MAX as u64 + 1;
        assert_eq!(
            u64::from_le_bytes(buf[cursor..cursor + 8].try_into().unwrap()),
            total
        );
        cursor += 8;
        assert_eq!(
            u64::from_le_bytes(buf[cursor..cursor + 8].try_into().unwrap()),
            total
        );
        cursor += 8;
        assert_eq!(
            u64::from_le_bytes(buf[cursor..cursor + 8].try_into().unwrap()),
            cd_size
        );
        cursor += 8;
        assert_eq!(
            u64::from_le_bytes(buf[cursor..cursor + 8].try_into().unwrap()),
            cd_start
        );
        cursor += 8;
        // ZIP64 EOCD locator (20 bytes).
        assert_eq!(
            u32::from_le_bytes(buf[cursor..cursor + 4].try_into().unwrap()),
            ZIP64_EOCD_LOC_SIG
        );
        cursor += 4;
        cursor += 4; // disk number
        assert_eq!(
            u64::from_le_bytes(buf[cursor..cursor + 8].try_into().unwrap()),
            zip64_eocd_offset
        );
        cursor += 8;
        assert_eq!(
            u32::from_le_bytes(buf[cursor..cursor + 4].try_into().unwrap()),
            1
        );
        cursor += 4;
        // Classic EOCD with sentinel values.
        assert_eq!(
            u32::from_le_bytes(buf[cursor..cursor + 4].try_into().unwrap()),
            EOCD_SIG
        );
        cursor += 4;
        assert_eq!(
            u16::from_le_bytes(buf[cursor..cursor + 2].try_into().unwrap()),
            0xFFFF
        );
        cursor += 8; // four u16 sentinels (disk/disk/entries/entries)
        assert_eq!(
            u32::from_le_bytes(buf[cursor..cursor + 4].try_into().unwrap()),
            0xFFFFFFFF
        );
        cursor += 4;
        assert_eq!(
            u32::from_le_bytes(buf[cursor..cursor + 4].try_into().unwrap()),
            0xFFFFFFFF
        );
        cursor += 4;
        assert_eq!(
            u16::from_le_bytes(buf[cursor..cursor + 2].try_into().unwrap()),
            0
        );
        cursor += 2;
        assert_eq!(cursor, buf.len());
    }

    #[test]
    fn classic_path_still_round_trips() {
        let mut cursor = Cursor::new(Vec::new());
        let mut writer = ZipWriter::new(&mut cursor);
        writer.write_member("metadata.fb", b"hello").unwrap();
        writer
            .write_member("channels/0000.bin", &[1, 2, 3, 4])
            .unwrap();
        writer.finish().unwrap();
        let bytes = cursor.into_inner();
        // The archive ends with a classic EOCD (no ZIP64) and parses back.
        assert_eq!(
            u32::from_le_bytes(
                bytes[bytes.len() - 22..bytes.len() - 18]
                    .try_into()
                    .unwrap()
            ),
            EOCD_SIG
        );
        let members = parse_members(&bytes).unwrap();
        assert_eq!(members.len(), 2);
        assert_eq!(members[1].size, 4);
    }
}
