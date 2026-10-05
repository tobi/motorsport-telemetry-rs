//! Canonically bounded, payload-free MP4 metadata inspection.

use crate::video_links::{canonical_file, root};
use crate::VideoLinkError;
use std::path::Path;
use thiserror::Error;

pub use aim_telemetry::{VideoMediaMetadata, VideoStreamMetadata};

/// Explicit path/boundary or container timing inspection failure.
#[derive(Debug, Error)]
pub enum VideoMediaError {
    /// Invalid or unavailable target/root, including escaping symlinks.
    #[error(transparent)]
    Path(#[from] VideoLinkError),
    /// Unsupported, malformed or unavailable MP4 container.
    #[error(transparent)]
    Container(#[from] aim_telemetry::AimError),
}

/// Inspect an existing nonfragmented MP4 within an optional canonical root.
///
/// Reads bounded `ftyp`/`moov` metadata and top-level box headers only, seeking
/// over video payloads. Extents are measured from native timing/composition
/// tables and rate-one edits, never telemetry timestamps or assumed FPS.
/// Zero-byte remote stubs return a container error: their playable extent is
/// unknown. The header fingerprint is not a payload identity guarantee.
pub fn inspect_video_media(
    path: impl AsRef<Path>,
    root_path: Option<&Path>,
) -> Result<VideoMediaMetadata, VideoMediaError> {
    let root = root(root_path)?;
    let path = canonical_file(path.as_ref(), root.as_deref())?;
    Ok(aim_telemetry::inspect_mp4_media(path)?)
}
