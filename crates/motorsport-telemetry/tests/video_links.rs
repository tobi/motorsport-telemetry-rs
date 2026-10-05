#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration fixtures fail loudly"
)]

use motorsport_telemetry::motorsport_telemetry_core::VideoFileRef;
use motorsport_telemetry::{find_video_recording, resolve_linked_videos, VideoLinkError};
use std::{fs, path::Path};

fn vbo(path: &Path, prefix: &str, data: &str) {
    fs::write(path, format!("File created on 11/10/2025 @ 15:03:06\n[AVI]\n{prefix}\nmp4\nmp4\n[column names]\ntime avifileindex avitime\n[data]\n{data}" )).unwrap();
}

fn reference(name: &str, index: u32) -> VideoFileRef {
    VideoFileRef {
        filename: name.to_owned(),
        index,
        blake3: None,
        frame_count: 0,
        presentation_offset_ns: None,
    }
}

#[test]
fn native_header_links_renamed_recording_in_sibling_telemetry_without_decoding_samples() {
    let temp = tempfile::tempdir().unwrap();
    let telemetry = temp.path().join("telemetry");
    fs::create_dir(&telemetry).unwrap();
    let recording = telemetry.join("unrelated-renamed-vbox.VBO");
    vbo(
        &recording,
        "25IR07_PLM_R_Run07_DHH_",
        "deliberately invalid channel payload\n",
    );
    let video = temp.path().join("25IR07_PLM_R_Run07_DHH_0002.MP4");
    fs::write(&video, []).unwrap();
    let found = find_video_recording(&video, Some(temp.path()))
        .unwrap()
        .unwrap();
    assert_eq!(found.recording_path, fs::canonicalize(&recording).unwrap());
    assert_eq!(found.file_index, 2);
    let resolved = resolve_linked_videos(
        &recording,
        &[reference("25IR07_PLM_R_Run07_DHH_0002.mp4", 2)],
        Some(temp.path()),
    )
    .unwrap();
    assert_eq!(resolved[0].file_index, 2);
    assert_eq!(resolved[0].path, fs::canonicalize(&video).unwrap());
}

#[test]
fn stems_do_not_supply_a_link_when_native_header_disagrees() {
    let temp = tempfile::tempdir().unwrap();
    let video = temp.path().join("run_0001.mp4");
    fs::write(&video, []).unwrap();
    vbo(&temp.path().join("run.vbo"), "other_", "");
    assert!(find_video_recording(&video, None).unwrap().is_none());
}

#[test]
fn byte_identical_telemetry_copies_choose_lexical_canonical_path() {
    let temp = tempfile::tempdir().unwrap();
    let first = temp.path().join("a.vbo");
    let second = temp.path().join("b-vbox.vbo");
    vbo(&first, "run_", "200306.600 1 16233\n");
    fs::copy(&first, &second).unwrap();
    let video = temp.path().join("run_0001.mp4");
    fs::write(&video, []).unwrap();
    assert_eq!(
        find_video_recording(&video, None)
            .unwrap()
            .unwrap()
            .recording_path,
        fs::canonicalize(first).unwrap()
    );
    fs::write(
        &second,
        fs::read(&second)
            .unwrap()
            .into_iter()
            .chain(b"200306.640 1 16266\n".iter().copied())
            .collect::<Vec<_>>(),
    )
    .unwrap();
    assert!(matches!(
        find_video_recording(&video, None),
        Err(VideoLinkError::AmbiguousRecording { .. })
    ));
}

#[test]
fn missing_video_is_explicit_and_source_indices_and_catalog_order_survive() {
    let temp = tempfile::tempdir().unwrap();
    let recording = temp.path().join("source.telemetry");
    fs::write(&recording, []).unwrap();
    for name in ["run_0001.mp4", "run_0002.mp4"] {
        fs::write(temp.path().join(name), []).unwrap();
    }
    let videos = [reference("run_0002.mp4", 2), reference("run_0001.mp4", 1)];
    let resolved = resolve_linked_videos(&recording, &videos, Some(temp.path())).unwrap();
    assert_eq!(
        resolved
            .iter()
            .map(|file| file.file_index)
            .collect::<Vec<_>>(),
        [2, 1]
    );
    assert!(matches!(
        resolve_linked_videos(&recording, &[reference("missing.mp4", 9)], None),
        Err(VideoLinkError::MissingVideo { file_index: 9, .. })
    ));
}

#[test]
fn unsafe_declared_names_and_avi_prefixes_are_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let recording = temp.path().join("source.vbo");
    let video = temp.path().join("run_0001.mp4");
    fs::write(&video, []).unwrap();
    vbo(&recording, "../run_", "");
    assert!(matches!(
        find_video_recording(&video, None),
        Err(VideoLinkError::UnsafeVideoName(_))
    ));
    for name in [
        "../run.mp4",
        "dir/run.mp4",
        "dir\\run.mp4",
        "C:run.mp4",
        "run\n.mp4",
        "",
    ] {
        assert!(matches!(
            resolve_linked_videos(&recording, &[reference(name, 1)], None),
            Err(VideoLinkError::UnsafeVideoName(_))
        ));
    }
}

#[test]
fn discovery_does_not_recurse_and_does_not_escape_an_explicit_root() {
    let temp = tempfile::tempdir().unwrap();
    let scope = temp.path().join("scope");
    fs::create_dir_all(scope.join("deep/nested")).unwrap();
    let video = scope.join("run_0001.mp4");
    fs::write(&video, []).unwrap();
    vbo(&scope.join("deep/nested/source.vbo"), "run_", "");
    assert!(find_video_recording(&video, Some(&scope))
        .unwrap()
        .is_none());
    vbo(&temp.path().join("source.vbo"), "run_", "");
    assert!(find_video_recording(&video, Some(&scope))
        .unwrap()
        .is_none());
    assert!(matches!(
        find_video_recording(&video, Some(&scope.join("deep"))),
        Err(VideoLinkError::InvalidPath { .. })
    ));
    let recording = scope.join("source.vbo");
    vbo(&recording, "outside_", "");
    fs::write(temp.path().join("outside_0001.mp4"), []).unwrap();
    assert!(matches!(
        resolve_linked_videos(
            &recording,
            &[reference("outside_0001.mp4", 1)],
            Some(&scope)
        ),
        Err(VideoLinkError::MissingVideo { .. })
    ));
}

#[test]
fn huge_header_is_bounded_even_when_no_data_section_exists() {
    let temp = tempfile::tempdir().unwrap();
    let video = temp.path().join("run_0001.mp4");
    fs::write(&video, []).unwrap();
    fs::write(temp.path().join("source.vbo"), vec![b'x'; 1024 * 1024 + 1]).unwrap();
    assert!(matches!(
        find_video_recording(video, None),
        Err(VideoLinkError::LimitExceeded { .. })
    ));
}

#[test]
fn target_and_root_path_errors_are_explicit() {
    let temp = tempfile::tempdir().unwrap();
    assert!(matches!(
        find_video_recording(temp.path(), None),
        Err(VideoLinkError::InvalidPath { .. })
    ));
    assert!(matches!(
        find_video_recording(temp.path().join("missing.mp4"), None),
        Err(VideoLinkError::Io { .. })
    ));
    let video = temp.path().join("run_0001.mp4");
    fs::write(&video, []).unwrap();
    assert!(matches!(
        find_video_recording(&video, Some(&video)),
        Err(VideoLinkError::InvalidPath { .. })
    ));
}

#[test]
fn duplicate_indices_are_invalid_and_empty_catalog_requires_no_neighbor_scan() {
    let temp = tempfile::tempdir().unwrap();
    let recording = temp.path().join("source.vbo");
    vbo(&recording, "run_", "");
    assert!(matches!(
        resolve_linked_videos(
            &recording,
            &[reference("one.mp4", 1), reference("two.mp4", 1)],
            Some(temp.path())
        ),
        Err(VideoLinkError::DuplicateVideoIndex(1))
    ));
    assert!(resolve_linked_videos(&recording, &[], Some(temp.path()))
        .unwrap()
        .is_empty());
}

#[test]
fn excessive_neighbor_count_is_explicit() {
    let temp = tempfile::tempdir().unwrap();
    let video = temp.path().join("run_0001.mp4");
    fs::write(&video, []).unwrap();
    for index in 0..4096 {
        fs::write(temp.path().join(format!("entry{index}")), []).unwrap();
    }
    assert!(matches!(
        find_video_recording(&video, Some(temp.path())),
        Err(VideoLinkError::LimitExceeded { .. })
    ));
}

#[cfg(unix)]
#[test]
fn case_extension_collisions_and_two_locations_remain_ambiguous() {
    let temp = tempfile::tempdir().unwrap();
    let folder = temp.path().join("telemetry");
    fs::create_dir(&folder).unwrap();
    let recording = folder.join("source.vbo");
    vbo(&recording, "run_", "");
    fs::write(temp.path().join("run_0001.mp4"), []).unwrap();
    fs::write(temp.path().join("run_0001.MP4"), []).unwrap();
    if fs::read_dir(temp.path()).unwrap().count() == 3 {
        assert!(matches!(
            resolve_linked_videos(&recording, &[reference("run_0001.mp4", 1)], None),
            Err(VideoLinkError::AmbiguousVideo { .. })
        ));
    }
    fs::remove_file(temp.path().join("run_0001.MP4")).unwrap();
    fs::write(temp.path().join("run_0001.mp4"), []).unwrap();
    fs::write(folder.join("run_0001.mp4"), []).unwrap();
    assert!(matches!(
        resolve_linked_videos(&recording, &[reference("run_0001.mp4", 1)], None),
        Err(VideoLinkError::AmbiguousVideo { .. })
    ));
}

#[cfg(unix)]
#[test]
fn escaping_symlinks_are_rejected_and_same_identity_aliases_are_deduplicated() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    fs::create_dir(&root).unwrap();
    let outside = temp.path().join("source.vbo");
    vbo(&outside, "run_", "");
    symlink(&outside, root.join("source.vbo")).unwrap();
    let video = root.join("run_0001.mp4");
    fs::write(&video, []).unwrap();
    assert!(matches!(
        find_video_recording(&video, Some(&root)),
        Err(VideoLinkError::InvalidPath { .. })
    ));
    fs::remove_file(root.join("source.vbo")).unwrap();
    let recording = root.join("source.vbo");
    vbo(&recording, "run_", "");
    symlink(&recording, root.join("alias.vbo")).unwrap();
    assert_eq!(
        find_video_recording(&video, Some(&root))
            .unwrap()
            .unwrap()
            .recording_path,
        fs::canonicalize(&recording).unwrap()
    );
    symlink(&video, root.join("run_0001.MP4")).unwrap();
    assert_eq!(
        resolve_linked_videos(&recording, &[reference("run_0001.mp4", 1)], Some(&root))
            .unwrap()
            .len(),
        1
    );
    fs::remove_file(root.join("run_0001.MP4")).unwrap();
    fs::remove_file(&video).unwrap();
    fs::write(temp.path().join("run_0001.mp4"), []).unwrap();
    symlink(temp.path().join("run_0001.mp4"), &video).unwrap();
    assert!(matches!(
        resolve_linked_videos(&recording, &[reference("run_0001.mp4", 1)], Some(&root)),
        Err(VideoLinkError::InvalidPath { .. })
    ));
}
