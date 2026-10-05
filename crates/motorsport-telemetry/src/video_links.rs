//! Bounded companion discovery. Video payloads are never opened.

use motorsport_telemetry_core::VideoFileRef;
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use thiserror::Error;

const MAX_ENTRIES: usize = 4096;
const MAX_HEADER_BYTES: u64 = 1024 * 1024;
const MAX_DUPLICATE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_DUPLICATE_TOTAL_BYTES: u64 = 512 * 1024 * 1024;

/// A source recording that explicitly declares the selected video.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoRecordingLink {
    /// Canonical recording path. Identical copies choose lexical path order.
    pub recording_path: PathBuf,
    /// Source-declared video roll index, never an array position.
    pub file_index: u32,
}

/// An existing file matched to one source video reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedVideoFile {
    /// Source video roll index.
    pub file_index: u32,
    /// Canonical existing video path.
    pub path: PathBuf,
}

/// Explicit discovery failures; no ambiguous candidate is silently selected.
#[derive(Debug, Error)]
pub enum VideoLinkError {
    /// A filesystem operation failed.
    #[error("video linkage filesystem error at {path}: {source}")]
    Io {
        /// Path involved in the operation.
        path: PathBuf,
        /// Underlying error.
        source: std::io::Error,
    },
    /// A target/root is invalid or escapes the canonical search boundary.
    #[error("invalid video linkage path {path}: {message}")]
    InvalidPath {
        /// Rejected path.
        path: PathBuf,
        /// Reason for rejection.
        message: String,
    },
    /// A bounded directory/header/duplicate comparison exceeded its limit.
    #[error("video linkage limit exceeded at {path}: {message}")]
    LimitExceeded {
        /// Path being inspected.
        path: PathBuf,
        /// Limit that was exceeded.
        message: String,
    },
    /// A source catalog name contains a path or invalid filename.
    #[error("unsafe source video basename {0:?}")]
    UnsafeVideoName(String),
    /// Source indices must uniquely identify catalog entries.
    #[error("duplicate source video index {0}")]
    DuplicateVideoIndex(u32),
    /// A declared video is absent from both supported neighboring directories.
    #[error("missing linked video {filename} (source index {file_index})")]
    MissingVideo {
        /// Exact declared basename.
        filename: String,
        /// Source index.
        file_index: u32,
    },
    /// Several nonidentical recordings explicitly claim one video.
    #[error("ambiguous recording for {video_path}: {candidates:?}")]
    AmbiguousRecording {
        /// Requested video.
        video_path: PathBuf,
        /// Canonical candidates, in lexical order.
        candidates: Vec<PathBuf>,
    },
    /// Several existing paths match a declared video basename.
    #[error("ambiguous linked video {filename}: {candidates:?}")]
    AmbiguousVideo {
        /// Exact declared basename.
        filename: String,
        /// Canonical candidates, in lexical order.
        candidates: Vec<PathBuf>,
    },
}

fn io(path: &Path, source: std::io::Error) -> VideoLinkError {
    VideoLinkError::Io {
        path: path.to_owned(),
        source,
    }
}

fn invalid(path: &Path, message: &str) -> VideoLinkError {
    VideoLinkError::InvalidPath {
        path: path.to_owned(),
        message: message.to_owned(),
    }
}

fn limit(path: &Path, message: &str) -> VideoLinkError {
    VideoLinkError::LimitExceeded {
        path: path.to_owned(),
        message: message.to_owned(),
    }
}

fn canonical_file(path: &Path, boundary: Option<&Path>) -> Result<PathBuf, VideoLinkError> {
    let canonical = fs::canonicalize(path).map_err(|error| io(path, error))?;
    if boundary.is_some_and(|root| !canonical.starts_with(root)) {
        return Err(invalid(path, "file escapes the canonical root"));
    }
    if !canonical.is_file() {
        return Err(invalid(path, "expected an existing regular file"));
    }
    Ok(canonical)
}

fn root(path: Option<&Path>) -> Result<Option<PathBuf>, VideoLinkError> {
    path.map(|path| {
        let root = fs::canonicalize(path).map_err(|error| io(path, error))?;
        if !root.is_dir() {
            return Err(invalid(path, "root must be an existing directory"));
        }
        Ok(root)
    })
    .transpose()
}

fn entries(directory: &Path, remaining: &mut usize) -> Result<Vec<PathBuf>, VideoLinkError> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(directory).map_err(|error| io(directory, error))? {
        let entry = entry.map_err(|error| io(directory, error))?;
        *remaining = remaining
            .checked_sub(1)
            .ok_or_else(|| limit(directory, "more than 4096 neighboring entries"))?;
        paths.push(entry.path());
    }
    paths.sort();
    Ok(paths)
}

fn extension_is(path: &Path, extension: &str) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case(extension))
}

fn safe_basename(name: &str) -> Result<(), VideoLinkError> {
    if name.is_empty()
        || matches!(name, "." | "..")
        || name
            .chars()
            .any(|c| matches!(c, '/' | '\\' | ':' | '\0') || c.is_control())
    {
        return Err(VideoLinkError::UnsafeVideoName(name.to_owned()));
    }
    Ok(())
}

/// Read only the native header, stopping before the first data row.
/// VBO permits Latin-1, which its native reader also accepts.
fn avi_header(path: &Path) -> Result<Option<(String, String)>, VideoLinkError> {
    let file = File::open(path).map_err(|error| io(path, error))?;
    let mut reader = BufReader::new(file.take(MAX_HEADER_BYTES + 1));
    let mut bytes = Vec::new();
    let mut consumed = 0u64;
    let mut section = String::new();
    let mut avi = Vec::new();
    loop {
        bytes.clear();
        let count = reader
            .read_until(b'\n', &mut bytes)
            .map_err(|error| io(path, error))?;
        if count == 0 {
            break;
        }
        consumed += count as u64;
        if consumed > MAX_HEADER_BYTES {
            return Err(limit(path, "VBO header exceeds 1 MiB"));
        }
        let text = String::from_utf8(bytes.clone())
            .unwrap_or_else(|_| bytes.iter().map(|byte| char::from(*byte)).collect());
        let line = text.trim().trim_start_matches('\u{feff}');
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].to_ascii_lowercase();
            if section == "data" {
                break;
            }
        } else if section == "avi" && !line.is_empty() {
            if avi.len() >= 16 {
                return Err(limit(path, "too many AVI header entries"));
            }
            avi.push(line.to_owned());
        }
    }
    let Some(prefix) = avi.first() else {
        return Ok(None);
    };
    safe_basename(prefix)?;
    let extension = avi
        .get(1)
        .map_or("avi", String::as_str)
        .trim_start_matches('.');
    safe_basename(extension)?;
    if extension.contains('.') {
        return Err(VideoLinkError::UnsafeVideoName(extension.to_owned()));
    }
    Ok(Some((prefix.clone(), extension.to_owned())))
}

fn native_video_index(video_path: &Path, prefix: &str, extension: &str) -> Option<u32> {
    if !extension_is(video_path, extension) {
        return None;
    }
    let suffix = video_path.file_stem()?.to_str()?.strip_prefix(prefix)?;
    if !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let index: u32 = suffix.parse().ok()?;
    (index > 0 && suffix == format!("{index:04}")).then_some(index)
}

fn duplicate_hash(path: &Path) -> Result<[u8; 32], VideoLinkError> {
    let file = File::open(path).map_err(|error| io(path, error))?;
    let mut reader = file.take(MAX_DUPLICATE_BYTES + 1);
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0u8; 65536];
    let mut consumed = 0u64;
    loop {
        let count = reader.read(&mut buffer).map_err(|error| io(path, error))?;
        if count == 0 {
            break;
        }
        consumed += count as u64;
        if consumed > MAX_DUPLICATE_BYTES {
            return Err(limit(path, "duplicate VBO exceeds 128 MiB"));
        }
        hasher.update(&buffer[..count]);
    }
    Ok(*hasher.finalize().as_bytes())
}

/// Find a native VBO whose `[AVI]` header declares this existing video.
///
/// Searches the video's directory and its immediate subdirectories only;
/// no source filename heuristic or sample decode is used. Stems are exact,
/// extensions are ASCII case-insensitive. Identical VBO copies choose the
/// lexical canonical path; nonidentical matches are an explicit ambiguity.
/// With a root, escaping target/candidate symlinks are rejected. Without one,
/// the video's canonical parent bounds discovery. Headers are limited to
/// 1 MiB, discovery to 4096 entries, and duplicate hashes to 128 MiB per VBO /
/// 512 MiB total. Video bytes are never read, including zero-byte stubs.
pub fn find_video_recording(
    video_path: impl AsRef<Path>,
    root_path: Option<&Path>,
) -> Result<Option<VideoRecordingLink>, VideoLinkError> {
    let root = root(root_path)?;
    let video = canonical_file(video_path.as_ref(), root.as_deref())?;
    let parent = video
        .parent()
        .ok_or_else(|| invalid(&video, "video has no parent directory"))?;
    let boundary = root.as_deref().unwrap_or(parent);
    let mut remaining = MAX_ENTRIES;
    let parent_entries = entries(parent, &mut remaining)?;
    let mut directories = BTreeSet::from([parent.to_owned()]);
    for path in &parent_entries {
        if path.is_dir() {
            let canonical = fs::canonicalize(path).map_err(|error| io(path, error))?;
            if !canonical.starts_with(boundary) {
                continue;
            }
            directories.insert(canonical);
        }
    }
    let mut candidates = Vec::new();
    for directory in directories {
        let paths = if directory == parent {
            parent_entries.clone()
        } else {
            entries(&directory, &mut remaining)?
        };
        for path in paths {
            if !extension_is(&path, "vbo") || path.is_dir() {
                continue;
            }
            let recording = canonical_file(&path, Some(boundary))?;
            let Some((prefix, extension)) = avi_header(&recording)? else {
                continue;
            };
            if let Some(file_index) = native_video_index(&video, &prefix, &extension) {
                candidates.push(VideoRecordingLink {
                    recording_path: recording,
                    file_index,
                });
            }
        }
    }
    candidates.sort_by(|left, right| left.recording_path.cmp(&right.recording_path));
    candidates.dedup();
    let Some(first) = candidates.first() else {
        return Ok(None);
    };
    if candidates.len() == 1 {
        return Ok(Some(first.clone()));
    }
    let mut total = 0u64;
    let mut identity = None;
    for candidate in &candidates {
        let size = fs::metadata(&candidate.recording_path)
            .map_err(|error| io(&candidate.recording_path, error))?
            .len();
        total = total
            .checked_add(size)
            .ok_or_else(|| limit(parent, "duplicate comparison size overflow"))?;
        if size > MAX_DUPLICATE_BYTES || total > MAX_DUPLICATE_TOTAL_BYTES {
            return Err(limit(
                parent,
                "duplicate comparison exceeds telemetry byte budget",
            ));
        }
        let hash = (
            candidate.file_index,
            size,
            duplicate_hash(&candidate.recording_path)?,
        );
        if identity.as_ref().is_some_and(|previous| *previous != hash) {
            return Err(VideoLinkError::AmbiguousRecording {
                video_path: video,
                candidates: candidates
                    .iter()
                    .map(|candidate| candidate.recording_path.clone())
                    .collect(),
            });
        }
        identity = Some(hash);
    }
    Ok(Some(first.clone()))
}

/// Resolve existing files using exact source-declared video basenames.
///
/// Searches the recording's canonical folder and its parent (for the common
/// `telemetry/` layout), constrained by an optional canonical collection root.
/// Extensions alone are ASCII case-insensitive; stem/extension collisions
/// and missing declarations are errors. Results retain source catalog order
/// and indices. This is format-independent and never reads video payloads.
pub fn resolve_linked_videos(
    recording_path: impl AsRef<Path>,
    videos: &[VideoFileRef],
    root_path: Option<&Path>,
) -> Result<Vec<ResolvedVideoFile>, VideoLinkError> {
    let root = root(root_path)?;
    let recording = canonical_file(recording_path.as_ref(), root.as_deref())?;
    let mut indices = BTreeSet::new();
    for video in videos {
        safe_basename(&video.filename)?;
        if !indices.insert(video.index) {
            return Err(VideoLinkError::DuplicateVideoIndex(video.index));
        }
    }
    if videos.is_empty() {
        return Ok(Vec::new());
    }
    let folder = recording
        .parent()
        .ok_or_else(|| invalid(&recording, "recording has no parent directory"))?;
    let mut directories = vec![folder.to_owned()];
    if let Some(parent) = folder.parent() {
        if root.as_deref().is_none_or(|root| parent.starts_with(root)) {
            directories.push(parent.to_owned());
        }
    }
    let boundary = root
        .as_deref()
        .unwrap_or_else(|| directories.last().map_or(folder, PathBuf::as_path));
    let mut remaining = MAX_ENTRIES;
    let mut paths = Vec::new();
    for directory in &directories {
        paths.extend(entries(directory, &mut remaining)?);
    }
    let mut result = Vec::with_capacity(videos.len());
    for video in videos {
        let declared = Path::new(&video.filename);
        let mut matches = BTreeSet::new();
        for path in &paths {
            if path.file_stem() == declared.file_stem()
                && (path.extension().is_none() && declared.extension().is_none()
                    || path
                        .extension()
                        .and_then(|value| value.to_str())
                        .zip(declared.extension().and_then(|value| value.to_str()))
                        .is_some_and(|(actual, expected)| actual.eq_ignore_ascii_case(expected)))
                && !path.is_dir()
            {
                matches.insert(canonical_file(path, Some(boundary))?);
            }
        }
        match matches.len() {
            0 => {
                return Err(VideoLinkError::MissingVideo {
                    filename: video.filename.clone(),
                    file_index: video.index,
                })
            }
            1 => result.push(ResolvedVideoFile {
                file_index: video.index,
                path: matches
                    .into_iter()
                    .next()
                    .ok_or_else(|| invalid(declared, "missing matched video"))?,
            }),
            _ => {
                return Err(VideoLinkError::AmbiguousVideo {
                    filename: video.filename.clone(),
                    candidates: matches.into_iter().collect(),
                })
            }
        }
    }
    Ok(result)
}
