#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration fixtures fail loudly"
)]

use motorsport_telemetry::motorsport_telemetry_core::VideoFileRef;
use motorsport_telemetry::{
    find_video_recording_in_catalogs, VideoLinkError, VideoRecordingCatalog,
};
use std::{fs, path::Path};

fn reference(filename: &str, index: u32) -> VideoFileRef {
    VideoFileRef {
        filename: filename.into(),
        index,
        blake3: None,
        frame_count: 0,
        presentation_offset_ns: None,
    }
}

fn catalog(recording: &Path, native: &Path) -> VideoRecordingCatalog {
    VideoRecordingCatalog {
        recording_path: recording.into(),
        metadata_path: native.into(),
        videos: vec![reference("run_0002.mp4", 2)],
    }
}

#[test]
fn identical_vbo_conversions_with_distinct_staging_origins_resolve_as_one_native_content() {
    let root = tempfile::tempdir().unwrap();
    let one_folder = root.path().join("a");
    let two_folder = root.path().join("longer-private-staging-b");
    fs::create_dir(&one_folder).unwrap();
    fs::create_dir(&two_folder).unwrap();
    let one = one_folder.join("run.vbo");
    let two = two_folder.join("run.vbo");
    let bytes = b"[header]\ntime\nsampleperiod\navifileindex\navisynctime\n[AVI]\nrun_\nmp4\n[column names]\ntime Tsample avifileindex avitime\n[data]\n120000.000 0.040 2 0\n120000.040 0.040 2 33\n";
    fs::write(&one, bytes).unwrap();
    fs::write(&two, bytes).unwrap();
    let source_one = motorsport_telemetry::open_with_options(
        &one,
        &motorsport_telemetry::OpenOptions {
            ignore_track_yml: true,
            ..Default::default()
        },
    )
    .unwrap();
    let source_two = motorsport_telemetry::open_with_options(
        &two,
        &motorsport_telemetry::OpenOptions {
            ignore_track_yml: true,
            ..Default::default()
        },
    )
    .unwrap();
    let native_one = root.path().join("one.telemetry");
    let native_two = root.path().join("two.telemetry");
    telemetry_format::write_telemetry(&source_one, &native_one).unwrap();
    telemetry_format::write_telemetry(&source_two, &native_two).unwrap();
    assert_ne!(
        fs::read(&native_one).unwrap(),
        fs::read(&native_two).unwrap()
    );
    let video = root.path().join("run_0002.mp4");
    fs::write(&video, []).unwrap();
    let result = find_video_recording_in_catalogs(
        &video,
        &[catalog(&two, &native_two), catalog(&one, &native_one)],
        Some(root.path()),
    )
    .unwrap()
    .unwrap();
    assert_eq!(result.recording_path, fs::canonicalize(&one).unwrap());
    assert_eq!(result.file_index, 2);
}

#[test]
fn origin_normalization_does_not_mask_clock_catalog_lap_channel_or_unknown_metadata_changes() {
    let root = tempfile::tempdir().unwrap();
    let video = root.path().join("run_0002.mp4");
    let first = root.path().join("a.vbo");
    let second = root.path().join("b.vbo");
    let native_one = root.path().join("one.telemetry");
    let native_two = root.path().join("two.telemetry");
    for path in [&video, &first, &second] {
        fs::write(path, []).unwrap();
    }
    let base = "{\"mtj\":1,\"q\":40000000,\"dur\":40000000,\"srcp\":\"/staging/a\",\"vf\":[{\"n\":\"run_0002.mp4\",\"i\":2}],\"vmap\":[[2,[[0,0],[40000000,33000000]]]],\"utc\":123,\"meta\":{\"srcp\":\"nested\",\"unknown\":18446744073709551615}}\n{\"laps\":[]}\n{\"n\":\"Speed\",\"v\":[1,2]}\n";
    fs::write(&native_one, base).unwrap();
    let other = base.replace("/staging/a", "/private/staging/longer/b");
    let catalogs = [catalog(&second, &native_two), catalog(&first, &native_one)];
    fs::write(&native_two, &other).unwrap();
    assert_eq!(
        find_video_recording_in_catalogs(&video, &catalogs, Some(root.path()))
            .unwrap()
            .unwrap()
            .recording_path,
        fs::canonicalize(&first).unwrap()
    );
    for altered in [
        other.replace("33000000", "33000001"),
        other.replace("\"utc\":123", "\"utc\":124"),
        other.replace("run_0002.mp4", "other_0002.mp4"),
        other.replace("\"laps\":[]", "\"laps\":[[1,0,40000000]]"),
        other.replace("[1,2]", "[1,3]"),
        other.replace("Speed", "Velocity"),
        other.replace("18446744073709551615", "18446744073709551614"),
        other.replace("nested", "other nested srcp"),
    ] {
        fs::write(&native_two, altered).unwrap();
        assert!(matches!(
            find_video_recording_in_catalogs(&video, &catalogs, Some(root.path())),
            Err(VideoLinkError::AmbiguousRecording { .. })
        ));
    }
    assert_eq!(fs::read(&native_one).unwrap(), base.as_bytes());
}

#[test]
fn remote_stub_pairing_uses_native_declarations_with_external_cache_and_no_source_reads() {
    let collection = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let video = collection.path().join("run_0002.MP4");
    let recording = collection.path().join("renamed.vbo");
    let native = cache.path().join("native.telemetry");
    fs::write(&video, []).unwrap();
    fs::write(&recording, []).unwrap();
    fs::write(
        &native,
        b"opaque cache identity; catalog is already supplied",
    )
    .unwrap();
    let catalogs = [catalog(&recording, &native)];
    let link = find_video_recording_in_catalogs(&video, &catalogs, Some(collection.path()))
        .unwrap()
        .unwrap();
    assert_eq!(link.recording_path, fs::canonicalize(&recording).unwrap());
    assert_eq!(link.file_index, 2);
    let other = collection.path().join("other_0002.mp4");
    fs::write(&other, []).unwrap();
    assert!(
        find_video_recording_in_catalogs(other, &catalogs, Some(collection.path()))
            .unwrap()
            .is_none()
    );
}

#[test]
fn identical_stubs_do_not_hide_distinct_native_objects_and_identical_native_bytes_choose_lexically()
{
    let root = tempfile::tempdir().unwrap();
    let video = root.path().join("run_0002.mp4");
    fs::write(&video, []).unwrap();
    let first = root.path().join("a.vbo");
    let second = root.path().join("b.vbo");
    fs::write(&first, []).unwrap();
    fs::write(&second, []).unwrap();
    let one = root.path().join("one.telemetry");
    let two = root.path().join("two.telemetry");
    fs::write(&one, b"one native recording").unwrap();
    fs::write(&two, b"two native recording").unwrap();
    let catalogs = [catalog(&second, &two), catalog(&first, &one)];
    assert!(matches!(
        find_video_recording_in_catalogs(&video, &catalogs, Some(root.path())),
        Err(VideoLinkError::AmbiguousRecording { .. })
    ));
    fs::copy(&one, &two).unwrap();
    assert_eq!(
        find_video_recording_in_catalogs(&video, &catalogs, Some(root.path()))
            .unwrap()
            .unwrap()
            .recording_path,
        fs::canonicalize(&first).unwrap()
    );
    let catalogs = [catalog(&second, &one), catalog(&first, &one)];
    // Same canonical object requires no hash even when larger than the hash budget.
    fs::File::options()
        .write(true)
        .open(&one)
        .unwrap()
        .set_len(128 * 1024 * 1024 + 1)
        .unwrap();
    assert_eq!(
        find_video_recording_in_catalogs(&video, &catalogs, Some(root.path()))
            .unwrap()
            .unwrap()
            .recording_path,
        fs::canonicalize(&first).unwrap()
    );
}

#[test]
fn cached_catalog_collisions_unsafe_names_and_invalid_indices_are_explicit() {
    let root = tempfile::tempdir().unwrap();
    let video = root.path().join("run_0002.mp4");
    let recording = root.path().join("stub.vbo");
    let native = root.path().join("native.telemetry");
    fs::write(&video, []).unwrap();
    fs::write(&recording, []).unwrap();
    fs::write(&native, b"native").unwrap();
    let mut item = catalog(&recording, &native);
    item.videos.push(reference("run_0002.MP4", 3));
    assert!(matches!(
        find_video_recording_in_catalogs(&video, &[item.clone()], Some(root.path())),
        Err(VideoLinkError::AmbiguousRecording { .. })
    ));
    item.videos[1].index = 2;
    assert!(matches!(
        find_video_recording_in_catalogs(&video, &[item.clone()], Some(root.path())),
        Err(VideoLinkError::DuplicateVideoIndex(2))
    ));
    item.videos = vec![reference("../run_0002.mp4", 2)];
    assert!(matches!(
        find_video_recording_in_catalogs(&video, &[item.clone()], Some(root.path())),
        Err(VideoLinkError::UnsafeVideoName(_))
    ));
    item.videos = vec![reference("run_0002.mp4", 0)];
    assert!(matches!(
        find_video_recording_in_catalogs(&video, &[item], Some(root.path())),
        Err(VideoLinkError::InvalidVideoIndex(0))
    ));
}

#[test]
fn native_object_and_catalog_limits_are_checked_without_reading_large_objects() {
    let root = tempfile::tempdir().unwrap();
    let video = root.path().join("run_0002.mp4");
    let recording = root.path().join("stub.vbo");
    let native = root.path().join("native.telemetry");
    fs::write(&video, []).unwrap();
    fs::write(&recording, []).unwrap();
    fs::write(&native, []).unwrap();
    let mut item = catalog(&recording, &native);
    assert!(matches!(
        find_video_recording_in_catalogs(&video, &[item.clone()], Some(root.path())),
        Err(VideoLinkError::InvalidPath { .. })
    ));
    fs::write(&native, b"native").unwrap();
    item.videos = vec![reference("run_0002.mp4", 2); 4097];
    assert!(matches!(
        find_video_recording_in_catalogs(&video, &[item], Some(root.path())),
        Err(VideoLinkError::LimitExceeded { .. })
    ));
    let second = root.path().join("second.telemetry");
    fs::File::create(&second)
        .unwrap()
        .set_len(128 * 1024 * 1024 + 1)
        .unwrap();
    assert!(matches!(
        find_video_recording_in_catalogs(
            &video,
            &[catalog(&recording, &native), catalog(&recording, &second)],
            Some(root.path())
        ),
        Err(VideoLinkError::LimitExceeded { .. })
    ));
    fs::remove_file(&native).unwrap();
    assert!(matches!(
        find_video_recording_in_catalogs(
            &video,
            &[catalog(&recording, &native)],
            Some(root.path())
        ),
        Err(VideoLinkError::Io { .. })
    ));
}

#[cfg(unix)]
#[test]
fn original_paths_cannot_escape_root_but_canonical_cache_aliases_share_identity() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let video = root.path().join("run_0002.mp4");
    let recording = root.path().join("stub.vbo");
    let native = cache.path().join("native.telemetry");
    let alias = cache.path().join("alias.telemetry");
    fs::write(&video, []).unwrap();
    fs::write(&recording, []).unwrap();
    fs::write(&native, b"native").unwrap();
    std::os::unix::fs::symlink(&native, &alias).unwrap();
    assert!(find_video_recording_in_catalogs(
        &video,
        &[catalog(&recording, &native), catalog(&recording, &alias)],
        Some(root.path())
    )
    .unwrap()
    .is_some());
    fs::remove_file(&recording).unwrap();
    let outside = cache.path().join("stub.vbo");
    fs::write(&outside, []).unwrap();
    std::os::unix::fs::symlink(outside, &recording).unwrap();
    assert!(matches!(
        find_video_recording_in_catalogs(
            &video,
            &[catalog(&recording, &native)],
            Some(root.path())
        ),
        Err(VideoLinkError::InvalidPath { .. })
    ));
}
