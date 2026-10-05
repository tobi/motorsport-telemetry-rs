//! Path-independent identity for otherwise exact converted-native documents.

use crate::{jsonl::ZSTD_MAGIC, JSONL_VERSION};
use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::value::RawValue;
use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use thiserror::Error;

const MAX_FILE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_HEADER_BYTES: u64 = 32 * 1024 * 1024;
const MAX_DECODED_BYTES: u64 = 512 * 1024 * 1024;

/// Content fingerprint plus the actual decoded comparison cost.
#[derive(Debug, Clone)]
pub struct NativeContentFingerprint {
    /// BLAKE3 of framed header values excluding only root `srcp`, plus exact body.
    /// This identifies converted-native content, not source or video bytes.
    pub fingerprint: [u8; 32],
    /// Decoded bytes read, including excluded provenance, for budget accounting.
    /// This is not part of identity and may differ for matching fingerprints.
    pub decoded_bytes: u64,
}

/// Why bounded native-content comparison could not establish a fingerprint.
#[derive(Debug, Error)]
pub enum NativeContentFingerprintError {
    /// The document could not be read or decompressed.
    #[error("native content io: {0}")]
    Io(#[from] std::io::Error),
    /// The input is not an unambiguous supported MTJ header/body envelope.
    #[error("invalid native content: {0}")]
    Invalid(String),
    /// A file, header or decoded comparison exceeds its resource bound.
    #[error("native content comparison limit exceeded: {0}")]
    LimitExceeded(String),
}

struct RawHeader(BTreeMap<String, Box<RawValue>>);

impl<'de> Deserialize<'de> for RawHeader {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct HeaderVisitor;
        impl<'de> Visitor<'de> for HeaderVisitor {
            type Value = RawHeader;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an MTJ header object without duplicate keys")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<RawHeader, A::Error> {
                let mut fields = BTreeMap::new();
                while let Some((key, value)) = access.next_entry::<String, Box<RawValue>>()? {
                    if fields.insert(key, value).is_some() {
                        return Err(de::Error::custom("duplicate native header key"));
                    }
                }
                Ok(RawHeader(fields))
            }
        }
        deserializer.deserialize_map(HeaderVisitor)
    }
}

/// Fingerprint converted MTJ content independently of origin-path provenance.
///
/// Excludes only the documented root header `srcp` string; source documents are
/// never changed. All other header values (including unknown fields and nested
/// `srcp`) retain their exact raw JSON, framed in sorted root-key order. All
/// decoded post-header bytes, including laps/channel records, remain exact.
/// Numeric values are never converted to floating point or reserialized.
/// Compression and root-key order do not affect identity; body/value whitespace
/// does. This is not full channel validation or original/media authentication.
///
/// Accepts plain MTJ or zstd magic, rejecting sidecars, duplicate root keys and
/// invalid provenance types. Limits: 128 MiB input, 32 MiB header, decoded bytes
/// capped at the supplied limit or 512 MiB, and a 32 MiB zstd window. No channel
/// samples are parsed. `decoded_bytes` supports a caller's shared total budget.
pub fn native_recording_content_fingerprint(
    path: impl AsRef<Path>,
    decoded_byte_limit: u64,
) -> Result<NativeContentFingerprint, NativeContentFingerprintError> {
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(NativeContentFingerprintError::Invalid(
            "expected regular recording file".into(),
        ));
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err(NativeContentFingerprintError::LimitExceeded(
            "input exceeds 128 MiB".into(),
        ));
    }
    let mut input = BufReader::new(file.take(MAX_FILE_BYTES + 1));
    if input.fill_buf()?.starts_with(&ZSTD_MAGIC) {
        let mut decoder = zstd::Decoder::with_buffer(input)?;
        decoder.window_log_max(25)?;
        fingerprint_reader(decoder, decoded_byte_limit)
    } else {
        fingerprint_reader(input, decoded_byte_limit)
    }
}

fn fingerprint_reader(
    reader: impl Read,
    decoded_byte_limit: u64,
) -> Result<NativeContentFingerprint, NativeContentFingerprintError> {
    let limit = decoded_byte_limit.min(MAX_DECODED_BYTES);
    let mut reader = BufReader::new(reader.take(limit + 1));
    let mut header = Vec::new();
    (&mut reader)
        .take(MAX_HEADER_BYTES + 1)
        .read_until(b'\n', &mut header)?;
    let mut decoded_bytes = header.len() as u64;
    if decoded_bytes > limit || decoded_bytes > MAX_HEADER_BYTES {
        return Err(NativeContentFingerprintError::LimitExceeded(
            "header or decoded byte budget exceeded".into(),
        ));
    }
    let RawHeader(mut fields) = serde_json::from_slice(&header)
        .map_err(|error| NativeContentFingerprintError::Invalid(error.to_string()))?;
    if fields.contains_key("mtx")
        || fields
            .get("mtj")
            .and_then(|value| serde_json::from_str::<u16>(value.get()).ok())
            != Some(JSONL_VERSION)
    {
        return Err(NativeContentFingerprintError::Invalid(
            "expected supported MTJ recording header".into(),
        ));
    }
    if let Some(path) = fields.remove("srcp") {
        serde_json::from_str::<String>(path.get())
            .map_err(|_| NativeContentFingerprintError::Invalid("srcp must be a string".into()))?;
    }
    let mut hash = blake3::Hasher::new();
    hash.update(b"mtj-native-content-excluding-root-srcp-v1\0");
    hash.update(&(fields.len() as u64).to_le_bytes());
    for (key, value) in fields {
        hash.update(&(key.len() as u64).to_le_bytes());
        hash.update(key.as_bytes());
        hash.update(&(value.get().len() as u64).to_le_bytes());
        hash.update(value.get().as_bytes());
    }
    hash.update(b"\0body\0");
    let mut buffer = vec![0u8; 65536];
    let mut body_bytes = 0u64;
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        decoded_bytes += count as u64;
        if decoded_bytes > limit {
            return Err(NativeContentFingerprintError::LimitExceeded(
                "decoded byte budget exceeded".into(),
            ));
        }
        body_bytes += count as u64;
        hash.update(&buffer[..count]);
    }
    if body_bytes == 0 {
        return Err(NativeContentFingerprintError::Invalid(
            "recording has no body".into(),
        ));
    }
    hash.update(&body_bytes.to_le_bytes());
    Ok(NativeContentFingerprint {
        fingerprint: *hash.finalize().as_bytes(),
        decoded_bytes,
    })
}

#[cfg(test)]
mod tests;
