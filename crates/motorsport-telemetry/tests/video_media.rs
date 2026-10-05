#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration fixtures fail loudly"
)]
use motorsport_telemetry::{inspect_video_media, VideoLinkError, VideoMediaError};
use std::fs;

#[test]
fn media_inspection_distinguishes_unknown_stub_extent_from_path_failures() {
    let temp = tempfile::tempdir().unwrap();
    let stub = temp.path().join("remote.mp4");
    fs::write(&stub, []).unwrap();
    assert!(matches!(
        inspect_video_media(&stub, Some(temp.path())),
        Err(VideoMediaError::Container(_))
    ));
    assert!(matches!(
        inspect_video_media(temp.path(), None),
        Err(VideoMediaError::Path(VideoLinkError::InvalidPath { .. }))
    ));
    assert!(matches!(
        inspect_video_media(temp.path().join("missing.mp4"), None),
        Err(VideoMediaError::Path(VideoLinkError::Io { .. }))
    ));
    assert!(matches!(
        inspect_video_media(&stub, Some(&stub)),
        Err(VideoMediaError::Path(VideoLinkError::InvalidPath { .. }))
    ));
}

#[cfg(unix)]
#[test]
fn escaping_media_symlink_is_rejected_before_container_inspection() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    fs::create_dir(&root).unwrap();
    let outside = temp.path().join("video.mp4");
    fs::write(&outside, []).unwrap();
    let alias = root.join("video.mp4");
    std::os::unix::fs::symlink(outside, &alias).unwrap();
    assert!(matches!(
        inspect_video_media(alias, Some(&root)),
        Err(VideoMediaError::Path(VideoLinkError::InvalidPath { .. }))
    ));
}
