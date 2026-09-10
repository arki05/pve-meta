//! Integration tests for `pve_meta_core::format`: round trips, order
//! preservation, and format-specific rejections. YAML is the only on-disk
//! format; JSON is a wire format only (`docs/DESIGN.md` §2).

use pretty_assertions::assert_eq;
use pve_meta_core::error::Error;
use pve_meta_core::format::{dump, parse, Format};
use serde_json::json;

fn sample_documents() -> Vec<serde_json::Value> {
    vec![
        json!({}),
        json!({"a": 1}),
        json!({
            "name": "web01",
            "cores": 4,
            "memory": 2048,
            "tags": ["prod", "web"],
            "enabled": true,
            "ratio": 1.5,
            "net": {"ip": "10.0.0.1", "vlan": 5},
            "empty_list": [],
            "empty_obj": {},
        }),
        json!({
            "__": "top level doc comment",
            "a": 1,
            "a__": "about a",
            "nested": {"x": 1, "y": {"z": 2}},
            "list_of_objects": [{"n": 1}, {"n": 2}, {"n": 3}],
        }),
        json!({"neg": -5, "zero": 0, "big": 1234567890123_i64}),
    ]
}

#[test]
fn round_trip_every_format_every_sample() {
    for doc in sample_documents() {
        for fmt in Format::ALL {
            let text = dump(fmt, &doc);
            let back = parse(fmt, &text).unwrap_or_else(|e| {
                panic!("failed to reparse {fmt} dump of {doc}: {e}\ntext:\n{text}")
            });
            assert_eq!(back, doc, "round trip mismatch for format {fmt}");
        }
    }
}

#[test]
fn dump_always_single_trailing_newline() {
    for doc in sample_documents() {
        for fmt in Format::ALL {
            let text = dump(fmt, &doc);
            assert!(text.ends_with('\n'), "format {fmt} did not end with newline");
            assert!(!text.ends_with("\n\n"), "format {fmt} had extra trailing newline");
        }
    }
}

#[test]
fn cross_format_conversion_preserves_order_and_content() {
    let doc = json!({"zeta": 1, "alpha": {"delta": 1, "beta": 2}, "gamma": [3, 1, 2]});
    for src in Format::ALL {
        let text = dump(src, &doc);
        let value = parse(src, &text).unwrap();
        for dst in Format::ALL {
            let converted_text = dump(dst, &value);
            let converted_value = parse(dst, &converted_text).unwrap();
            assert_eq!(converted_value, doc, "{src} -> {dst} lost data or order");
        }
    }
}

#[test]
fn yaml_1_1_boolish_words_stay_strings() {
    let text = "a: yes\nb: no\nc: on\nd: off\ne: Yes\nf: No\n";
    let value = parse(Format::Yaml, text).unwrap();
    assert_eq!(
        value,
        json!({"a": "yes", "b": "no", "c": "on", "d": "off", "e": "Yes", "f": "No"})
    );
}

#[test]
fn yaml_true_false_are_real_booleans() {
    let value = parse(Format::Yaml, "a: true\nb: false\n").unwrap();
    assert_eq!(value, json!({"a": true, "b": false}));
}

#[test]
fn yaml_rejects_anchors_aliases_and_tags() {
    for text in [
        "a: &anchor 1\nb: *anchor\n",
        "a: *undefined\n",
        "a: !!str 123\n",
        "a: !custom 123\n",
    ] {
        let err = parse(Format::Yaml, text).unwrap_err();
        assert!(matches!(err, Error::Parse { .. }), "{text:?} -> {err:?}");
    }
}

#[test]
fn yaml_rejects_complex_mapping_keys() {
    let err = parse(Format::Yaml, "? [1, 2]\n: value\n").unwrap_err();
    assert!(matches!(err, Error::Parse { .. }));
}

#[test]
fn yaml_rejects_null() {
    for text in ["a: ~\n", "a: null\n", "a:\n"] {
        let err = parse(Format::Yaml, text).unwrap_err();
        assert!(matches!(err, Error::Lint(_)), "{text:?} -> {err:?}");
    }
}

#[test]
fn json_rejects_comments_and_trailing_commas() {
    assert!(parse(Format::Json, "{ \"a\": 1, }").is_err());
    assert!(parse(Format::Json, "{ // comment\n \"a\": 1 }").is_err());
}

#[test]
fn lint_errors_surface_through_parse() {
    let err = parse(Format::Json, "{\"bad key\": 1}").unwrap_err();
    assert!(matches!(err, Error::Lint(_)));
    let err = parse(Format::Json, "[1, 2, 3]").unwrap_err();
    assert!(matches!(err, Error::Lint(_)));
}

/// The canonical YAML this crate writes, pinned byte for byte against
/// `testdata/yaml-cases.json`.
///
/// The editor used to write documents with a second emitter (js-yaml), and this
/// table was what held the two together. The editor now writes them with this
/// crate, built for the browser (`crates/pve-meta-wasm`), so the table has one
/// job left: to notice when a `serde_yaml_ng` upgrade changes the bytes -- every
/// line that moves is a line the editor's diff would then attribute to whatever
/// was being edited. The editor's suite runs the same document through the
/// wasm as its end-to-end "the core loads and answers" check.
///
/// The values are the ones that historically break hand-written YAML --
/// structural punctuation, quote characters, comment markers, strings that
/// look like numbers, booleans or nulls, and non-ASCII including characters
/// whose UTF-8 carries a byte in the C1 range. Keys are plain on purpose: a
/// document key is limited to the path charset (`docs/DESIGN.md` §2), so only
/// values can be hostile.
#[test]
fn canonical_yaml_is_pinned() {
    let raw = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../testdata/yaml-cases.json"
    ))
    .expect("the shared fixture is part of the repository");
    let doc: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let document = doc["document"].clone();
    let canonical = doc["canonical"].as_str().expect("canonical text");
    assert!(
        document.as_object().map(|m| m.len()).unwrap_or(0) >= 40,
        "the fixture should not have been emptied"
    );

    assert_eq!(dump(Format::Yaml, &document), canonical, "the canonical dump moved");
    assert_eq!(
        parse(Format::Yaml, canonical).unwrap(),
        document,
        "and it must read back as the document it came from"
    );
}
