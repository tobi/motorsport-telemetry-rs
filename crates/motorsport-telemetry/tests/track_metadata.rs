#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::float_cmp,
    clippy::unreadable_literal,
    reason = "integration tests: fail loudly and compare exact fixture values"
)]

use motorsport_telemetry::{
    load_track_directory_metadata, load_track_metadata,
    motorsport_telemetry_core::{MetadataMap, TelemetrySource, ViewSource},
    open, open_metadata, open_metadata_with_options, open_with_options, read_metadata,
    read_metadata_with_options, read_track_metadata_document, OpenOptions, SourceExt,
};
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}
fn recording(directory: &Path) -> PathBuf {
    fs::create_dir_all(directory).unwrap();
    let path = directory.join("run.mp4");
    fs::copy(fixture("synthetic_aimd.mp4"), &path).unwrap();
    path
}
fn yaml(directory: &Path, text: &str) {
    fs::create_dir_all(directory).unwrap();
    fs::write(directory.join("TRACK.yml"), text).unwrap();
}
fn ignored() -> OpenOptions {
    OpenOptions {
        ignore_track_yml: true,
        ..OpenOptions::default()
    }
}
fn rooted(root: &Path) -> OpenOptions {
    OpenOptions {
        root_path: Some(root.to_owned()),
        ..OpenOptions::default()
    }
}

#[test]
fn default_loads_only_adjacent_metadata_and_leaves_samples_and_clocks_alone() {
    let temp = tempfile::tempdir().unwrap();
    let folder = temp.path().join("run");
    let path = recording(&folder);
    let bytes = fs::read(&path).unwrap();
    yaml(temp.path(), "series: do not inherit\nevent: parent\n");
    yaml(&folder, "schema: '2'\nevent: Weekend\ntrack:\n  name: Override Track\n  slug: sebring\ncar:\n  name: ORECA 07\n  number: '11'\ndriver:\n  name: Test Driver\n  mappings:\n    '3': Tobi\n    '02.500': Fractional Driver\n    '*': Stand-in\nsession: FP1\ndate: '2026-07-10'\narchive:\n  event_date: '2026-07-09'\n");
    let raw = open_with_options(&path, &ignored()).unwrap();
    let file = open(&path).unwrap();
    let m = file.metadata();
    let r = raw.metadata();
    assert_eq!(m.identity.event, "Weekend");
    assert_eq!(m.identity.venue, "Override Track");
    assert_eq!(m.identity.vehicle, "ORECA 07");
    assert_eq!(m.identity.driver, "Test Driver");
    assert_eq!(m.identity.session, "FP1");
    assert_eq!(m.identity.date, "2026-07-10");
    assert_eq!(m.extra["car"]["number"], "11");
    assert_eq!(m.extra["archive"]["event_date"], "2026-07-09");
    assert!(!m.extra.contains_key("schema"));
    assert!(!m.extra.contains_key("series"));
    assert_eq!(m.driver_name_for_id(3.0), Some("Tobi"));
    assert_eq!(m.driver_name_for_id(2.5), Some("Fractional Driver"));
    assert_eq!(m.driver_name_for_id(7.0), Some("Stand-in"));
    for code in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        assert_eq!(m.driver_name_for_id(code), None);
    }
    assert_eq!(
        file.identity(),
        raw.identity(),
        "native identity remains available"
    );
    assert_eq!(m.utc_start_ns, r.utc_start_ns);
    assert_eq!(m.absolute_start_ns, r.absolute_start_ns);
    assert_eq!(m.clock_offset_ns, r.clock_offset_ns);
    assert_eq!(m.timezone, r.timezone);
    assert_eq!(m.session_key, r.session_key);
    assert_eq!(m.laps, r.laps);
    assert_eq!(m.driver_ids, r.driver_ids);
    assert_eq!(file.channels().len(), raw.channels().len());
    for (index, (actual, original)) in file.channels().iter().zip(raw.channels()).enumerate() {
        assert_eq!(actual.name, original.name);
        assert_eq!(actual.unit, original.unit);
        assert_eq!(actual.sample_type, original.sample_type);
        assert_eq!(actual.sample_count, original.sample_count);
        for (chunk, definition) in original.chunks.iter().enumerate() {
            for local in 0..definition.sample_count {
                let a = file.decode(index, chunk, local);
                let b = raw.decode(index, chunk, local);
                assert!(a == b || (a.is_nan() && b.is_nan()));
                assert_eq!(
                    file.sample_time_ns(index, chunk, local),
                    raw.sample_time_ns(index, chunk, local)
                );
            }
        }
    }
    assert_eq!(file.video_reference_at(0), raw.video_reference_at(0));
    assert_eq!(file.normalizer().sample(0), raw.normalizer().sample(0));
    assert_eq!(read_metadata(&path).unwrap().extra, m.extra);
    assert_eq!(
        open_metadata(&path).unwrap().metadata().identity,
        m.identity
    );
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert!(open_with_options(&path, &ignored())
        .unwrap()
        .metadata()
        .extra
        .is_empty());
}

#[test]
fn root_to_leaf_and_ordered_globs_override_embedded_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("collection");
    let event = root.join("2026/event");
    let folder = event.join("FP1");
    let path = recording(&folder);
    yaml(temp.path(), "outside: forbidden\n");
    yaml(&root, "car: {class: LMP2, number: '18'}\nseries: IMSA\noverrides:\n  - match: '2026/**'\n    metadata:\n      car: {number: '11'}\n      year: 2026\n      session: ancestor\n  - match: ['**/FP1/**', '*.vbo']\n    metadata: {session: FP1, ordered: first}\n  - match: '**/*.mp4'\n    metadata: {ordered: last}\n  - match: '*.mp4'\n    metadata: {bad_basename_match: true}\n");
    yaml(&event, "event: Event\ncar: {number: null}\nseries: ''\n");
    yaml(&folder, "session: Local session\noverrides:\n  - match: run.mp4\n    metadata: {local_match: true}\n");
    let m = open_with_options(&path, &rooted(&root)).unwrap().metadata();
    assert_eq!(m.identity.session, "Local session");
    assert_eq!(m.extra["car"], json!({"class":"LMP2","number":null}));
    assert_eq!(m.extra["series"], "IMSA");
    assert_eq!(m.extra["ordered"], "last");
    assert_eq!(m.extra["local_match"], true);
    assert!(!m.extra.contains_key("bad_basename_match"));
    assert!(!m.extra.contains_key("outside"));
    assert!(!m.extra.contains_key("overrides"));
    assert_eq!(
        read_metadata_with_options(&path, &rooted(&root))
            .unwrap()
            .extra,
        m.extra
    );
    assert_eq!(
        open_metadata_with_options(&path, &rooted(&root))
            .unwrap()
            .metadata()
            .extra,
        m.extra
    );
    assert!(!open(&path).unwrap().metadata().extra.contains_key("year"));
    let options = OpenOptions {
        ignore_track_yml: true,
        ..rooted(&root)
    };
    assert!(open_with_options(&path, &options)
        .unwrap()
        .metadata()
        .extra
        .is_empty());
}

#[test]
fn invalid_roots_are_rejected_even_when_metadata_is_ignored() {
    let temp = tempfile::tempdir().unwrap();
    let parent = temp.path().join("collection-other");
    let sibling = temp.path().join("collection");
    fs::create_dir_all(&sibling).unwrap();
    let file = recording(&parent);
    for root in [&sibling, &file, &parent.join("missing")] {
        for ignore_track_yml in [false, true] {
            let options = OpenOptions {
                root_path: Some(root.clone()),
                ignore_track_yml,
            };
            assert!(open_with_options(&file, &options).is_err());
            assert!(read_metadata_with_options(&file, &options).is_err());
        }
    }
    assert!(open_with_options(&file, &rooted(&parent)).is_ok());
    assert!(open_with_options(&file, &rooted(&parent.join("../collection-other"))).is_ok());
}

#[test]
fn missing_empty_and_legacy_documents_are_harmless() {
    let temp = tempfile::tempdir().unwrap();
    let file = recording(temp.path());
    assert!(open(&file).unwrap().metadata().extra.is_empty());
    for text in [
        "",
        "# comment\n",
        "{}",
        "schema: '2'\n",
        "schema: whatever\n",
    ] {
        yaml(temp.path(), text);
        assert!(open(&file).unwrap().metadata().extra.is_empty());
    }
}

#[test]
fn nulls_mask_identity_and_driver_wildcards() {
    let temp = tempfile::tempdir().unwrap();
    let file = recording(temp.path());
    yaml(
        temp.path(),
        "event: null\ntrack: null\ndriver:\n  mappings:\n    '*': Fallback\n    3: null\n",
    );
    let m = open(&file).unwrap().metadata();
    assert!(m.identity.event.is_empty());
    assert!(m.identity.venue.is_empty());
    assert_eq!(m.driver_name_for_id(3.0), None);
    assert_eq!(m.driver_name_for_id(4.0), Some("Fallback"));
}

#[test]
fn malformed_metadata_and_unmatched_bad_rules_fail_with_a_path() {
    let temp = tempfile::tempdir().unwrap();
    let path = recording(temp.path());
    for text in [
        "event: [",
        "- not a mapping",
        "event: first\nevent: duplicate\n",
        "driver: []\n",
        "track: {name: []}",
        "overrides: {}",
        "overrides: null",
        "event: .nan",
        "overrides: [{match: '*.vbo', metadata: {event: []}}]",
        "overrides: [{match: '[', metadata: {}}]",
        "overrides: [{match: '../*', metadata: {}}]",
        "overrides: [{match: '/absolute/*', metadata: {}}]",
        "overrides: [{match: [], metadata: {}}]",
        "overrides: [{match: '*', metadata: {}, typo: true}]",
        "overrides: [{match: '*', metadata: {overrides: []}}]",
        "custom: !tag value",
        "custom: {1: x, '1': y}",
    ] {
        yaml(temp.path(), text);
        let Err(error) = open(&path) else {
            panic!("accepted {text}");
        };
        assert!(error.to_string().contains("TRACK.yml"), "{error}");
        assert!(read_metadata(&path).is_err(), "{text}");
        assert!(open_with_options(&path, &ignored()).is_ok(), "{text}");
    }
}

#[test]
fn metadata_survives_views_passes_conversion_and_header_only_reads() {
    let temp = tempfile::tempdir().unwrap();
    let path = recording(temp.path());
    yaml(temp.path(), "event: TRACK event\nsession: FP1\ntrack: {name: Other venue}\ncustom: {items: [one, 2, null], flag: true}\n");
    let source = open(&path).unwrap();
    let expected = source.metadata();
    let arc = Arc::new(source);
    let view = ViewSource::new(arc.clone());
    assert_eq!(view.extra_metadata(), expected.extra);
    assert_eq!(
        (&view as &dyn TelemetrySource).metadata().extra,
        expected.extra
    );
    let (passed, _) = telemetry_passes::apply_registry(&view).unwrap();
    assert_eq!(passed.metadata().extra, expected.extra);
    let output = temp.path().join("elsewhere");
    fs::create_dir_all(&output).unwrap();
    let compressed = output.join("run.telemetry");
    telemetry_format::write_telemetry(&passed, &compressed).unwrap();
    let reopened = open(&compressed).unwrap();
    let summary = read_metadata(&compressed).unwrap();
    assert_eq!(reopened.metadata().extra, expected.extra);
    assert_eq!(summary.extra, expected.extra);
    assert_eq!(summary.identity, expected.identity);
    assert_eq!(summary.utc_start_ns, expected.utc_start_ns);
    assert_eq!(summary.timezone, expected.timezone);
    let stripped = output.join("stripped.telemetry");
    telemetry_format::write_telemetry_stripped(&reopened, &stripped).unwrap();
    assert_eq!(read_metadata(&stripped).unwrap().extra, expected.extra);

    let plain = output.join("header.telemetry.jsonl");
    telemetry_format::write_jsonl_from_source_with(&view, &plain, false).unwrap();
    let text = fs::read_to_string(&plain).unwrap();
    fs::write(
        &plain,
        format!(
            "{}\nnot channel json\n",
            text.lines().take(2).collect::<Vec<_>>().join("\n")
        ),
    )
    .unwrap();
    yaml(&output, "session: Header-only override\n");
    let header = read_metadata(&plain).unwrap();
    assert_eq!(header.identity.session, "Header-only override");
    assert_eq!(header.identity.event, expected.identity.event);
    assert!(open(&plain).is_err());
}

#[test]
fn embedded_map_clear_then_child_map_does_not_resurrect_removed_values() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("collection");
    let folder = root.join("run");
    fs::create_dir_all(&folder).unwrap();
    let source = open_with_options(fixture("synthetic_aimd.mp4"), &ignored()).unwrap();
    let native = source.identity();
    let extra: MetadataMap =
        json!({"custom":{"old":true,"nested":1},"car":{"name":"Embedded car"}})
            .as_object()
            .unwrap()
            .clone();
    let source = ViewSource::new(source).with_extra_metadata(&extra);
    let path = folder.join("run.telemetry");
    telemetry_format::write_telemetry(&source, &path).unwrap();
    yaml(&root, "custom: null\ncar: null\n");
    yaml(&folder, "custom: {new: true}\ncar: {number: 7}\n");
    let opened = open_with_options(&path, &rooted(&root)).unwrap();
    let result = opened.metadata();
    assert_eq!(result.extra["custom"], json!({"new":true}));
    assert_eq!(result.extra["car"], json!({"number":7}));
    assert_eq!(result.identity.vehicle, native.vehicle);
    assert_eq!(result.source_identity, native);
    let header = read_metadata_with_options(&path, &rooted(&root)).unwrap();
    assert_eq!(header.extra, result.extra);
    assert_eq!(header.identity, result.identity);
    assert_eq!(header.source_identity, native);
    let converted = temp.path().join("converted.telemetry");
    telemetry_format::write_telemetry(&opened, &converted).unwrap();
    assert_eq!(
        open(&converted).unwrap().metadata().identity,
        result.identity
    );
    assert_eq!(read_metadata(&converted).unwrap().identity, result.identity);
}

#[test]
fn yaml_expansion_is_bounded_before_materializing_aliases() {
    let temp = tempfile::tempdir().unwrap();
    let path = recording(temp.path());
    let expanded_text = format!(
        "value: &v {}\nrefs: [{}]\n",
        "x".repeat(32768),
        vec!["*v"; 256].join(",")
    );
    yaml(temp.path(), &expanded_text);
    let Err(error) = open(&path) else {
        panic!("unbounded alias text accepted")
    };
    assert!(
        error.to_string().contains("expanded metadata text"),
        "{error}"
    );
    let many_nodes = format!(
        "value: &v [1,2,3,4,5,6,7,8]\nrefs: [{}]\n",
        vec!["*v"; 10000].join(",")
    );
    yaml(temp.path(), &many_nodes);
    let Err(error) = open(&path) else {
        panic!("unbounded alias nodes accepted")
    };
    assert!(
        error.to_string().contains("expanded metadata exceeds"),
        "{error}"
    );
    yaml(
        temp.path(),
        &format!("value: {}0{}\n", "[".repeat(70), "]".repeat(70)),
    );
    assert!(open(&path).is_err());
    yaml(temp.path(), &format!("value: {}", "x".repeat(1048576)));
    assert!(open(&path).is_err());
    yaml(temp.path(), "event: One\n---\nevent: Two\n");
    assert!(open(&path).is_err());
    yaml(temp.path(), "value: &v {label: '*literal'}\ncopy: *v\n");
    let metadata = open(&path).unwrap().metadata();
    assert_eq!(metadata.extra["copy"], json!({"label":"*literal"}));
}

#[cfg(unix)]
#[test]
fn symlinks_cannot_escape_an_explicit_root() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("collection");
    let file = recording(&root);
    let outside = recording(&temp.path().join("outside"));
    let alias = root.join("outside.mp4");
    symlink(&outside, &alias).unwrap();
    assert!(open_with_options(&alias, &rooted(&root)).is_err());
    yaml(outside.parent().unwrap(), "event: outside\n");
    symlink(
        outside.parent().unwrap().join("TRACK.yml"),
        root.join("TRACK.yml"),
    )
    .unwrap();
    assert!(open_with_options(&file, &rooted(&root)).is_err());
    assert!(open(&file).is_err());
    assert!(open_with_options(&file, &ignored()).is_ok());
    fs::remove_file(root.join("TRACK.yml")).unwrap();
    let root_alias = temp.path().join("alias");
    symlink(&root, &root_alias).unwrap();
    yaml(&root, "event: Inside\n");
    assert_eq!(
        open_with_options(root_alias.join("run.mp4"), &rooted(&root_alias))
            .unwrap()
            .metadata()
            .identity
            .event,
        "Inside"
    );
}

#[test]
fn public_recording_resolver_supports_empty_and_non_vendor_files_without_decoding() {
    let temp = tempfile::tempdir().unwrap();
    let folder = temp.path().join("run");
    yaml(temp.path(), "outside: do not inherit\n");
    yaml(
        &folder,
        "event: Video\noverrides:\n  - match: '*.MOV'\n    metadata: {session: MOV session}\n",
    );
    for name in ["clip.MOV", "clip.mkv", "remote.mp4", "arbitrary.bin"] {
        let path = folder.join(name);
        fs::write(&path, []).unwrap();
        let resolved = load_track_metadata(&path, &OpenOptions::default()).unwrap();
        assert_eq!(resolved.paths, vec![folder.join("TRACK.yml")]);
        let mut extra = MetadataMap::new();
        resolved.apply_to(&mut extra);
        assert_eq!(extra["event"], "Video");
        assert!(!extra.contains_key("outside"));
        assert_eq!(
            extra.get("session").and_then(serde_json::Value::as_str),
            (name == "clip.MOV").then_some("MOV session")
        );
        assert_eq!(fs::metadata(path).unwrap().len(), 0);
    }
}

#[test]
fn public_layers_preserve_null_masks_rule_order_and_contributing_paths() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("collection");
    let folder = root.join("event/run");
    let path = recording(&folder);
    yaml(temp.path(), "outside: excluded\n");
    yaml(&root, "schema: '2'\ncustom: null\noverrides:\n  - match: '**/*.mp4'\n    metadata: {event: First}\n  - match: '**'\n    metadata: {event: Last}\n");
    yaml(&folder, "custom: {new: true}\n");
    let resolved = load_track_metadata(&path, &rooted(&root)).unwrap();
    assert_eq!(
        resolved.paths,
        vec![root.join("TRACK.yml"), folder.join("TRACK.yml")]
    );
    assert_eq!(resolved.layers.len(), 4);
    assert!(resolved.layers[0]["custom"].is_null());
    let mut embedded = json!({"custom":{"old":true},"embedded":true})
        .as_object()
        .unwrap()
        .clone();
    resolved.apply_to(&mut embedded);
    assert_eq!(
        embedded,
        json!({"custom":{"new":true},"embedded":true,"event":"Last"})
            .as_object()
            .unwrap()
            .clone()
    );
    assert_eq!(
        open_with_options(&path, &rooted(&root))
            .unwrap()
            .metadata()
            .extra,
        {
            let mut empty = MetadataMap::new();
            resolved.apply_to(&mut empty);
            empty
        }
    );
}

#[test]
fn directory_resolution_reads_only_defaults_and_can_exclude_target() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("collection");
    let child = root.join("event/run");
    yaml(temp.path(), "outside: forbidden\n");
    yaml(&root, "event: Ancestor\ncustom: null\noverrides:\n  - match: '**'\n    metadata: {event: Must not apply, rule_only: true}\n");
    yaml(&child, "event: Child\ncustom: {new: true}\n");
    let all = load_track_directory_metadata(&child, &rooted(&root), false).unwrap();
    assert_eq!(all.layers.len(), 2);
    assert_eq!(
        all.paths,
        vec![root.join("TRACK.yml"), child.join("TRACK.yml")]
    );
    let mut extra = json!({"custom":{"old":true}}).as_object().unwrap().clone();
    all.apply_to(&mut extra);
    assert_eq!(
        extra,
        json!({"event":"Child","custom":{"new":true}})
            .as_object()
            .unwrap()
            .clone()
    );
    let inherited = load_track_directory_metadata(&child, &rooted(&root), true).unwrap();
    assert_eq!(inherited.paths, vec![root.join("TRACK.yml")]);
    assert_eq!(
        inherited.layers,
        vec![json!({"event":"Ancestor","custom":null})
            .as_object()
            .unwrap()
            .clone()]
    );
    assert!(
        load_track_directory_metadata(&child, &OpenOptions::default(), true)
            .unwrap()
            .layers
            .is_empty()
    );
    assert!(load_track_directory_metadata(&root, &rooted(&root), true)
        .unwrap()
        .paths
        .is_empty());
    // Exclusion does not read the document being edited, but inherited files
    // still use the exact same validation policy as recording resolution.
    yaml(&child, "event: [\n");
    assert!(load_track_directory_metadata(&child, &rooted(&root), true).is_ok());
    assert!(load_track_directory_metadata(&child, &rooted(&root), false).is_err());
}

#[test]
fn raw_document_reader_preserves_editor_data_and_never_reads_ancestors() {
    let temp = tempfile::tempdir().unwrap();
    let child = temp.path().join("child");
    yaml(temp.path(), "invalid: [\n");
    let text = "schema: '2'\ncustom: {items: [one, 2, null], flag: true}\noverrides:\n  - match: ['*.MOV', '**/*.mkv']\n    metadata: {schema: legacy, driver: {mappings: {'2.5': Tobi, '*': Guest}}, unknown: [1, false]}\n";
    yaml(&child, text);
    let path = child.join("TRACK.yml");
    let document = read_track_metadata_document(&path, Some(temp.path())).unwrap();
    assert_eq!(document["schema"], "2");
    assert_eq!(
        document["custom"],
        json!({"items":["one",2,null],"flag":true})
    );
    assert_eq!(document["overrides"][0]["metadata"]["schema"], "legacy");
    assert_eq!(
        document["overrides"][0]["match"],
        json!(["*.MOV", "**/*.mkv"])
    );
    assert_eq!(
        document["overrides"][0]["metadata"]["unknown"],
        json!([1, false])
    );
    assert_eq!(read_track_metadata_document(&path, None).unwrap(), document);
    assert_eq!(fs::read_to_string(&path).unwrap(), text);
    yaml(&child, "");
    assert!(read_track_metadata_document(&path, None)
        .unwrap()
        .is_empty());
}

#[test]
fn public_resolvers_validate_all_rules_even_without_a_recording_match() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("clip.MOV");
    fs::write(&path, []).unwrap();
    for text in [
        "overrides: [{match: '*.vbo', metadata: {driver: []}}]",
        "overrides: [{match: ['*.MOV', '../*'], metadata: {}}]",
        "overrides: [{match: '[', metadata: {}}]",
        "overrides: [{match: '*.vbo', metadata: {overrides: []}}]",
        "overrides: [{match: '*.vbo', metadata: {}, unknown_rule_key: true}]",
    ] {
        yaml(temp.path(), text);
        for error in [
            load_track_metadata(&path, &OpenOptions::default()).unwrap_err(),
            load_track_directory_metadata(temp.path(), &OpenOptions::default(), false).unwrap_err(),
            read_track_metadata_document(temp.path().join("TRACK.yml"), None).unwrap_err(),
        ] {
            assert!(error.to_string().contains("TRACK.yml"));
        }
    }
}

#[test]
fn public_document_reader_enforces_shared_yaml_budgets_and_single_document_policy() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("TRACK.yml");
    for text in [
        format!("value: {}", "x".repeat(1048576)),
        format!("value: {}0{}", "[".repeat(70), "]".repeat(70)),
        format!(
            "v: &v {}\nrefs: [{}]",
            "x".repeat(32768),
            vec!["*v"; 256].join(",")
        ),
        format!(
            "v: &v [1,2,3,4,5,6,7,8]\nrefs: [{}]",
            vec!["*v"; 10000].join(",")
        ),
        "event: First\n---\nevent: Second".into(),
        "custom: !tag value".into(),
        "custom: .inf".into(),
        "event: duplicate\nevent: duplicate".into(),
        "- not a mapping".into(),
    ] {
        fs::write(&path, text).unwrap();
        assert!(read_track_metadata_document(&path, None).is_err());
        assert!(
            load_track_directory_metadata(temp.path(), &OpenOptions::default(), false).is_err()
        );
    }
}

#[test]
fn public_resolvers_report_explicit_target_and_root_errors_including_opt_out() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("clip.MOV");
    fs::write(&file, []).unwrap();
    let sibling = temp.path().join("sibling");
    fs::create_dir(&sibling).unwrap();
    for ignore_track_yml in [false, true] {
        for root in [&sibling, &file, &temp.path().join("missing")] {
            let options = OpenOptions {
                root_path: Some(root.clone()),
                ignore_track_yml,
            };
            assert!(load_track_metadata(&file, &options).is_err());
            assert!(load_track_directory_metadata(temp.path(), &options, true).is_err());
        }
        let options = OpenOptions {
            ignore_track_yml,
            ..OpenOptions::default()
        };
        assert!(load_track_metadata(temp.path(), &options).is_err());
        assert!(load_track_directory_metadata(&file, &options, false).is_err());
        assert!(load_track_metadata(temp.path().join("missing"), &options).is_err());
        assert!(
            load_track_directory_metadata(temp.path().join("missing"), &options, true).is_err()
        );
    }
    assert!(read_track_metadata_document(&file, Some(&sibling)).is_err());
    assert!(read_track_metadata_document(&file, Some(&file)).is_err());
    assert!(read_track_metadata_document(temp.path(), None).is_err());
    assert!(read_track_metadata_document(temp.path().join("missing"), None).is_err());
    yaml(temp.path(), "invalid: [");
    assert!(load_track_metadata(&file, &ignored())
        .unwrap()
        .paths
        .is_empty());
    assert!(
        load_track_directory_metadata(temp.path(), &ignored(), false)
            .unwrap()
            .layers
            .is_empty()
    );
}

#[cfg(unix)]
#[test]
fn public_resolvers_enforce_canonical_roots_and_metadata_symlink_boundaries() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("collection");
    let child = root.join("child");
    let outside = temp.path().join("outside");
    fs::create_dir_all(&child).unwrap();
    yaml(&outside, "event: Outside");
    let file = child.join("clip.MOV");
    fs::write(&file, []).unwrap();
    symlink(outside.join("TRACK.yml"), child.join("TRACK.yml")).unwrap();
    assert!(load_track_metadata(&file, &rooted(&root)).is_err());
    assert!(load_track_directory_metadata(&child, &rooted(&root), false).is_err());
    assert!(read_track_metadata_document(child.join("TRACK.yml"), Some(&root)).is_err());
    assert!(read_track_metadata_document(child.join("TRACK.yml"), None).is_err());
    fs::remove_file(child.join("TRACK.yml")).unwrap();
    yaml(&root, "event: Inside");
    symlink(root.join("TRACK.yml"), child.join("TRACK.yml")).unwrap();
    let resolved = load_track_metadata(&file, &rooted(&root)).unwrap();
    assert_eq!(
        resolved.paths,
        vec![root.join("TRACK.yml"), child.join("TRACK.yml")]
    );
    assert_eq!(
        read_track_metadata_document(child.join("TRACK.yml"), Some(&root)).unwrap()["event"],
        "Inside"
    );
    assert!(read_track_metadata_document(child.join("TRACK.yml"), None).is_err());
    let alias = temp.path().join("root-alias");
    symlink(&root, &alias).unwrap();
    assert_eq!(
        load_track_metadata(alias.join("child/clip.MOV"), &rooted(&alias)).unwrap(),
        resolved
    );
    symlink(&outside, root.join("escape")).unwrap();
    assert!(load_track_directory_metadata(root.join("escape"), &rooted(&root), false).is_err());
    fs::write(outside.join("stub.MKV"), []).unwrap();
    symlink(outside.join("stub.MKV"), child.join("escape.MKV")).unwrap();
    assert!(load_track_metadata(child.join("escape.MKV"), &rooted(&root)).is_err());
}
