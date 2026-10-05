use super::*;

fn fingerprint(document: &[u8]) -> NativeContentFingerprint {
    fingerprint_reader(document, MAX_DECODED_BYTES).unwrap()
}

#[test]
fn root_origin_and_key_order_are_ignored_but_every_other_raw_value_and_body_byte_matters() {
    let body = "\n{\"laps\":[]}\n{\"n\":\"Speed\",\"v\":[1,2]}\n";
    let first = format!("{{\"mtj\":1,\"q\":1,\"dur\":2,\"srcp\":\"/private/a\",\"meta\":{{\"srcp\":\"nested value\",\"n\":18446744073709551615}}}}{body}");
    let second = format!("{{\"meta\":{{\"srcp\":\"nested value\",\"n\":18446744073709551615}},\"srcp\":\"/much-longer-private/path/b\",\"q\":1,\"mtj\":1,\"dur\":2}}{body}");
    assert_eq!(
        fingerprint(first.as_bytes()).fingerprint,
        fingerprint(second.as_bytes()).fingerprint
    );
    assert_ne!(
        fingerprint(first.as_bytes()).decoded_bytes,
        fingerprint(second.as_bytes()).decoded_bytes
    );
    for altered in [
        first.replace("nested value", "other nested value"),
        first.replace("18446744073709551615", "18446744073709551614"),
        first.replace("\"dur\":2", "\"dur\":3"),
        first.replace("[1,2]", "[1,3]"),
        first.replace("\"laps\":[]", "\"laps\":[[1,0,2]]"),
        first.replace("Speed", "Velocity"),
        first.replace("[1,2]", "[1, 2]"),
    ] {
        assert_ne!(
            fingerprint(first.as_bytes()).fingerprint,
            fingerprint(altered.as_bytes()).fingerprint
        );
    }
    let absent = first.replace(",\"srcp\":\"/private/a\"", "");
    assert_eq!(
        fingerprint(first.as_bytes()).fingerprint,
        fingerprint(absent.as_bytes()).fingerprint
    );
}

#[test]
fn unknown_large_numbers_and_float_lexemes_are_not_rounded_or_normalized() {
    for (one, two) in [
        ("184467440737095516160000001", "184467440737095516160000002"),
        ("1.0000000000000000001", "1.0000000000000000002"),
        ("1", "1.0"),
    ] {
        let first = format!("{{\"mtj\":1,\"unknown\":{one}}}\n{{\"laps\":[]}}\n");
        let second = format!("{{\"mtj\":1,\"unknown\":{two}}}\n{{\"laps\":[]}}\n");
        assert_ne!(
            fingerprint(first.as_bytes()).fingerprint,
            fingerprint(second.as_bytes()).fingerprint
        );
    }
}

#[test]
fn unsupported_ambiguous_or_malformed_envelopes_do_not_establish_identity() {
    for document in [
        "{}\n{}\n",
        "{\"mtj\":2}\n{}\n",
        "{\"mtx\":1}\n{}\n",
        "{\"mtj\":1,\"mtx\":1}\n{}\n",
        "{\"mtj\":1,\"mtj\":1}\n{}\n",
        "{\"mtj\":1,\"srcp\":null}\n{}\n",
        "{\"mtj\":1,\"srcp\":4}\n{}\n",
        "{\"mtj\":1,\"srcp\":\"a\",\"srcp\":\"b\"}\n{}\n",
        "{\"mtj\":1}\n",
        "not a recording",
        "{\"mtj\":1,broken}\n{}\n",
    ] {
        assert!(matches!(
            fingerprint_reader(document.as_bytes(), MAX_DECODED_BYTES),
            Err(NativeContentFingerprintError::Invalid(_))
        ));
    }
}

#[test]
fn decoded_header_and_file_bounds_are_enforced_before_unbounded_work() {
    let document = b"{\"mtj\":1}\n{\"laps\":[]}\n";
    assert!(fingerprint_reader(document.as_slice(), document.len() as u64).is_ok());
    assert!(matches!(
        fingerprint_reader(document.as_slice(), document.len() as u64 - 1),
        Err(NativeContentFingerprintError::LimitExceeded(_))
    ));
    assert!(matches!(
        fingerprint_reader(document.as_slice(), 0),
        Err(NativeContentFingerprintError::LimitExceeded(_))
    ));
    let huge_header = vec![b'x'; MAX_HEADER_BYTES as usize + 1];
    assert!(matches!(
        fingerprint_reader(huge_header.as_slice(), MAX_DECODED_BYTES),
        Err(NativeContentFingerprintError::LimitExceeded(_))
    ));
    let temp = tempfile::NamedTempFile::new().unwrap();
    temp.as_file().set_len(MAX_FILE_BYTES + 1).unwrap();
    assert!(matches!(
        native_recording_content_fingerprint(temp.path(), MAX_DECODED_BYTES),
        Err(NativeContentFingerprintError::LimitExceeded(_))
    ));
}

#[test]
fn public_reader_sniffs_compression_preserves_documents_and_limits_expansion() {
    let temp = tempfile::tempdir().unwrap();
    let document = b"{\"mtj\":1,\"srcp\":\"a\",\"unknown\":1.0000000000000000001}\n{\"laps\":[]}\n";
    let compressed = zstd::encode_all(document.as_slice(), 11).unwrap();
    let plain = temp.path().join("plain.telemetry");
    let packed = temp.path().join("packed.telemetry");
    std::fs::write(&plain, document).unwrap();
    std::fs::write(&packed, &compressed).unwrap();
    let one = native_recording_content_fingerprint(&plain, MAX_DECODED_BYTES).unwrap();
    let two = native_recording_content_fingerprint(&packed, MAX_DECODED_BYTES).unwrap();
    assert_eq!(one.fingerprint, two.fingerprint);
    assert_eq!(two.decoded_bytes, document.len() as u64);
    assert_eq!(std::fs::read(&plain).unwrap(), document);
    assert_eq!(std::fs::read(&packed).unwrap(), compressed);
    assert!(matches!(
        native_recording_content_fingerprint(&packed, document.len() as u64 - 1),
        Err(NativeContentFingerprintError::LimitExceeded(_))
    ));
    let mut broken = compressed;
    broken.pop();
    std::fs::write(&packed, broken).unwrap();
    assert!(native_recording_content_fingerprint(&packed, MAX_DECODED_BYTES).is_err());
}
