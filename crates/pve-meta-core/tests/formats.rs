//! Integration tests for `pve_meta_core::format`: round trips, order
//! preservation, and format-specific rejections.

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
fn toml_rejects_datetime_with_path_in_message() {
    let err = parse(Format::Toml, "[a]\nwhen = 1979-05-27T07:32:00Z\n").unwrap_err();
    match err {
        Error::Parse { format, msg } => {
            assert_eq!(format, Format::Toml);
            assert!(msg.contains("a.when"), "message should mention path a.when: {msg}");
        }
        other => panic!("expected Parse, got {other:?}"),
    }
}

#[test]
fn toml_dump_uses_explicit_tables_and_array_of_tables() {
    let doc = json!({
        "top": 1,
        "section": {"x": 1, "y": 2},
        "items": [{"n": 1}, {"n": 2}],
    });
    let text = dump(Format::Toml, &doc);
    assert!(text.contains("[section]"));
    assert!(text.contains("[[items]]"));
    assert!(!text.contains('{'), "should never use inline tables:\n{text}");
    let back = parse(Format::Toml, &text).unwrap();
    assert_eq!(back, doc);
}

#[test]
fn toml_dump_key_order_matches_document_order() {
    let doc = json!({"zebra": 1, "apple": 2, "mango": 3});
    let text = dump(Format::Toml, &doc);
    let zebra = text.find("zebra").unwrap();
    let apple = text.find("apple").unwrap();
    let mango = text.find("mango").unwrap();
    assert!(zebra < apple && apple < mango, "order not preserved:\n{text}");
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
