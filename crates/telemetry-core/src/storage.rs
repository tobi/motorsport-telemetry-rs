//! Byte storage shared by memory-mapped and in-memory parsers.

use std::fmt;
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
    /// Memory-maps `path` read-only. Falls back to owned bytes only via
    /// [`Storage::from_vec`]; this method always maps.
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let file = std::fs::File::open(path)?;
        // SAFETY: the file is mapped read-only; the caller must ensure no
        // external process truncates or mutates it while samples are decoded,
        // the same contract every mmap-based parser already holds.
        let mmap = unsafe { memmap2::Mmap::map(&file)? };
        Ok(Self::Mapped(mmap))
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
}
