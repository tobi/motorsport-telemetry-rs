//! Byte storage shared by memory-mapped and in-memory parsers.

use std::fmt;
use std::io::Read;
use std::ops::Deref;
use std::path::Path;

/// Backing bytes for a parsed recording: memory-mapped or owned.
///
/// Parsers that can safely mmap a local file avoid copying the whole recording
/// into the heap; embedded and WASM callers use [`Storage::from_vec`]. Both
/// deref to the raw `[u8]` payload.
pub enum Storage {
    /// A read-only memory map over a local file.
    Mapped(memmap2::Mmap),
    /// An owned in-memory buffer.
    Owned(Vec<u8>),
}

impl Storage {
    /// Memory-maps `path` read-only, reading it into owned bytes when the
    /// filesystem refuses to map it.
    ///
    /// Some network filesystems (observed on an SMB/NAS mount, where `rg`
    /// fails the same way) return `EINVAL` from `mmap` for ordinary regular
    /// files. The bytes are still readable, so that is not a reason to fail
    /// the open; an empty file cannot be mapped at all and is returned as an
    /// empty buffer for the parser to reject with its own message.
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let mut file = std::fs::File::open(path)?;
        if file.metadata()?.len() == 0 {
            return Ok(Self::Owned(Vec::new()));
        }
        // SAFETY: the file is mapped read-only; the caller must ensure no
        // external process truncates or mutates it while samples are decoded,
        // the same contract every mmap-based parser already holds.
        match unsafe { memmap2::Mmap::map(&file) } {
            Ok(mmap) => Ok(Self::Mapped(mmap)),
            Err(_) => {
                // Read the already-open file, not a possibly replaced path.
                let mut bytes = Vec::new();
                file.read_to_end(&mut bytes)?;
                Ok(Self::Owned(bytes))
            }
        }
    }

    /// Wraps an owned buffer.
    pub fn from_vec(bytes: Vec<u8>) -> Self {
        Self::Owned(bytes)
    }
}

impl Deref for Storage {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        match self {
            Self::Mapped(mmap) => mmap.as_ref(),
            Self::Owned(bytes) => bytes.as_slice(),
        }
    }
}

impl fmt::Debug for Storage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (variant, len) = match self {
            Self::Mapped(mmap) => ("Mapped", mmap.len()),
            Self::Owned(bytes) => ("Owned", bytes.len()),
        };
        formatter.debug_tuple(variant).field(&len).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_vec_derefs_and_debugs() {
        let storage = Storage::from_vec(vec![1, 2, 3]);
        assert_eq!(&*storage, &[1, 2, 3]);
        assert_eq!(format!("{storage:?}"), "Owned(3)");
    }

    #[test]
    fn open_maps_a_real_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bytes.bin");
        std::fs::write(&path, [10, 20, 30, 40]).unwrap();
        let storage = Storage::open(&path).unwrap();
        assert_eq!(&*storage, &[10, 20, 30, 40]);
        assert_eq!(format!("{storage:?}"), "Mapped(4)");
    }

    #[test]
    fn open_returns_an_empty_buffer_for_an_empty_file() {
        // `mmap` of a zero-length file fails with EINVAL; that must surface as
        // an empty buffer, not an I/O error, so the parser can say "too small".
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.bin");
        std::fs::write(&path, []).unwrap();
        let storage = Storage::open(&path).unwrap();
        assert!(storage.is_empty());
        assert_eq!(format!("{storage:?}"), "Owned(0)");
    }
}
