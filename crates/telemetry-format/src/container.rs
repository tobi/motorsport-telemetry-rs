//! `.telemetry` container sniffing and the default `.telemetry` writer.
//!
//! A `.telemetry` file is one of two containers, told apart by content, never
//! by name:
//!
//! * a **zstd frame** holding an MTJ JSONL document — what every writer
//!   produces now ([`write_telemetry`]);
//! * a **STORE zip** with a FlatBuffers catalog — the legacy native layout,
//!   still read (and migrated in place) by [`crate::NativeRecording`].
//!
//! Plain UTF-8 MTJ under a `.telemetry` name is also accepted so a
//! hand-decompressed file still opens.

use crate::jsonl::{write_jsonl_from_source_with, JsonlRecording, ZSTD_MAGIC};
use crate::write::{stripped_view, TelemetryFormatError};
use crate::NativeRecording;
use motorsport_telemetry_core::TelemetrySource;
use std::fs::File;
use std::io::Read;
use std::path::Path;

/// Local-file-header signature of a zip archive (`PK\x03\x04`).
const ZIP_MAGIC: [u8; 4] = [0x50, 0x4B, 0x03, 0x04];

/// Physical container of a `.telemetry` file, decided from its first bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    /// zstd-compressed MTJ JSONL — the current default.
    JsonlZstd,
    /// Uncompressed MTJ JSONL (first byte `{`).
    Jsonl,
    /// Legacy aligned STORE zip with `metadata.fb`.
    NativeZip,
    /// Empty file or unrecognised leading bytes.
    Unknown,
}

impl Container {
    /// True for either MTJ container.
    pub fn is_jsonl(self) -> bool {
        matches!(self, Container::JsonlZstd | Container::Jsonl)
    }
}

/// Classifies a buffer by its leading bytes.
pub fn sniff_container_bytes(bytes: &[u8]) -> Container {
    if bytes.starts_with(&ZSTD_MAGIC) {
        Container::JsonlZstd
    } else if bytes.starts_with(&ZIP_MAGIC) {
        Container::NativeZip
    } else if bytes
        .iter()
        .find(|byte| !byte.is_ascii_whitespace())
        .is_some_and(|byte| *byte == b'{')
    {
        Container::Jsonl
    } else {
        Container::Unknown
    }
}

/// Reads the first bytes of `path` and classifies the container.
pub fn sniff_container(path: impl AsRef<Path>) -> Result<Container, TelemetryFormatError> {
    let mut file = File::open(path.as_ref())?;
    let mut head = [0u8; 16];
    let mut read = 0;
    while read < head.len() {
        match file.read(&mut head[read..])? {
            0 => break,
            n => read += n,
        }
    }
    Ok(sniff_container_bytes(&head[..read]))
}

/// A `.telemetry` file opened by content.
#[derive(Debug)]
pub enum TelemetryRecording {
    /// Legacy native zip. Writable older catalogs were migrated in place.
    Native(NativeRecording),
    /// MTJ document (zstd or plain).
    Jsonl(JsonlRecording),
}

impl TelemetryRecording {
    /// Opens a `.telemetry` file, dispatching on its container.
    ///
    /// A legacy zip goes through [`NativeRecording::open`] and may be
    /// rewritten to the current catalog version; MTJ files are never touched.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, TelemetryFormatError> {
        let path = path.as_ref();
        match sniff_container(path)? {
            Container::NativeZip => Ok(Self::Native(NativeRecording::open(path)?)),
            Container::JsonlZstd | Container::Jsonl => Ok(Self::Jsonl(JsonlRecording::open(path)?)),
            Container::Unknown => Err(unknown_container(path)),
        }
    }

    /// Opens without rewriting a legacy zip.
    pub fn open_unchanged(path: impl AsRef<Path>) -> Result<Self, TelemetryFormatError> {
        let path = path.as_ref();
        match sniff_container(path)? {
            Container::NativeZip => Ok(Self::Native(NativeRecording::open_unchanged(path)?)),
            Container::JsonlZstd | Container::Jsonl => Ok(Self::Jsonl(JsonlRecording::open(path)?)),
            Container::Unknown => Err(unknown_container(path)),
        }
    }

    /// Which container backed this recording.
    pub fn container(&self) -> Container {
        match self {
            Self::Native(_) => Container::NativeZip,
            Self::Jsonl(_) => Container::JsonlZstd,
        }
    }

    /// Format-neutral summary.
    pub fn metadata(&self) -> motorsport_telemetry_core::FileMetadata {
        match self {
            Self::Native(native) => native.metadata(),
            Self::Jsonl(jsonl) => jsonl.metadata(),
        }
    }

    /// Erases the container into the shared source trait object.
    pub fn into_source(self) -> Box<dyn TelemetrySource> {
        match self {
            Self::Native(native) => Box::new(native),
            Self::Jsonl(jsonl) => Box::new(jsonl),
        }
    }
}

fn unknown_container(path: &Path) -> TelemetryFormatError {
    TelemetryFormatError::Invalid(format!(
        "{}: not a .telemetry file (neither a zstd MTJ frame nor a native zip)",
        path.display()
    ))
}

/// Writes `source` to `dest` in the default `.telemetry` container: a zstd
/// MTJ document. Use [`crate::write_from_source`] only when the legacy native
/// zip is explicitly wanted.
pub fn write_telemetry(
    source: &dyn TelemetrySource,
    dest: impl AsRef<Path>,
) -> Result<(), TelemetryFormatError> {
    write_jsonl_from_source_with(source, dest, true)
}

/// Writes `source` without its applied-pass outputs, in the default container.
/// See [`crate::write_from_source_stripped`] for the strip semantics.
pub fn write_telemetry_stripped(
    source: &dyn TelemetrySource,
    dest: impl AsRef<Path>,
) -> Result<(), TelemetryFormatError> {
    let view = stripped_view(source);
    write_jsonl_from_source_with(&view, dest, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_by_leading_bytes() {
        assert_eq!(
            sniff_container_bytes(&[0x28, 0xB5, 0x2F, 0xFD, 0, 0]),
            Container::JsonlZstd
        );
        assert_eq!(
            sniff_container_bytes(b"PK\x03\x04rest"),
            Container::NativeZip
        );
        assert_eq!(sniff_container_bytes(b"  \n{\"v\":1}"), Container::Jsonl);
        assert_eq!(sniff_container_bytes(b""), Container::Unknown);
        assert_eq!(sniff_container_bytes(b"MoTeC"), Container::Unknown);
    }
}
