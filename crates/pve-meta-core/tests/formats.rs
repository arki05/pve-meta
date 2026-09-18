//! Integration tests for `pve_meta_core::format`, through its public API:
//! a round-trip smoke test, and the pinned canonical-YAML fixture. Format
//! rejections and other per-rule behaviour are `format.rs`'s own `#[cfg(test)]`
//! module.

use pretty_assertions::assert_eq;
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

/// The canonical YAML this crate writes, pinned byte for byte against
/// `testdata/yaml-cases.json`.
///
/// The editor writes documents with this crate, built for the browser
/// (`crates/pve-meta-wasm`), so the table's job is to notice when a
/// `serde_yaml_ng` upgrade changes the bytes -- every line that moves is a
/// line the editor's diff would then attribute to whatever was being edited.
/// The editor's suite runs the same document through the wasm as its
/// end-to-end "the core loads and answers" check.
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
