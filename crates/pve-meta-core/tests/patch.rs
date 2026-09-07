//! Integration tests for merge-patch semantics (`pve_meta_core::patch`).

use pretty_assertions::assert_eq;
use pve_meta_core::patch::{apply_patch, diff, make_patch, Op};
use pve_meta_core::path::Path;
use serde_json::json;

fn touched_paths(touched: &[pve_meta_core::patch::Touched]) -> Vec<(String, bool)> {
    let mut v: Vec<(String, bool)> = touched
        .iter()
        .map(|t| (t.path.to_string(), t.op == Op::Set))
        .collect();
    v.sort();
    v
}

#[test]
fn apply_patch_full_scenario() {
    let mut doc = json!({
        "name": "web01",
        "tags": ["prod"],
        "net": {"ip": "10.0.0.1", "vlan": 5},
        "extra": {"keep": true},
    });
    let patch = json!({
        "name": "web01",              // no-op (same value)
        "tags": ["prod", "web"],       // array replace (atomic)
        "net": {"vlan": 10, "gw": "10.0.0.254"}, // recurse: one set, one add
        "extra": null,                 // delete whole subtree
        "new_ns": {"a": 1},            // brand new nested object
    });
    let touched = apply_patch(&mut doc, &patch);
    assert_eq!(
        doc,
        json!({
            "name": "web01",
            "tags": ["prod", "web"],
            "net": {"ip": "10.0.0.1", "vlan": 10, "gw": "10.0.0.254"},
            "new_ns": {"a": 1},
        })
    );
    assert_eq!(
        touched_paths(&touched),
        vec![
            ("extra".to_string(), false),
            ("net.gw".to_string(), true),
            ("net.vlan".to_string(), true),
            ("new_ns".to_string(), true),
            ("tags".to_string(), true),
        ]
    );
}

#[test]
fn apply_patch_nested_delete_yields_leaf_path() {
    let mut doc = json!({"a": {"b": {"c": 1, "d": 2}}});
    let patch = json!({"a": {"b": {"c": null}}});
    let touched = apply_patch(&mut doc, &patch);
    assert_eq!(doc, json!({"a": {"b": {"d": 2}}}));
    assert_eq!(touched.len(), 1);
    assert_eq!(touched[0].path, Path::parse("a.b.c").unwrap());
    assert_eq!(touched[0].op, Op::Delete);
}

#[test]
fn apply_patch_noop_set_yields_nothing() {
    let mut doc = json!({"a": {"b": 1}});
    let patch = json!({"a": {"b": 1}});
    let touched = apply_patch(&mut doc, &patch);
    assert!(touched.is_empty());
    assert_eq!(doc, json!({"a": {"b": 1}}));
}

#[test]
fn apply_patch_delete_missing_key_is_noop() {
    let mut doc = json!({"a": 1});
    let touched = apply_patch(&mut doc, &json!({"nope": null}));
    assert!(touched.is_empty());
    assert_eq!(doc, json!({"a": 1}));
}

#[test]
fn apply_patch_arrays_always_atomic_replace() {
    let mut doc = json!({"a": [1, {"x": 1}]});
    let touched = apply_patch(&mut doc, &json!({"a": [1, {"x": 2}]}));
    assert_eq!(doc, json!({"a": [1, {"x": 2}]}));
    assert_eq!(touched, vec![pve_meta_core::patch::Touched {
        path: Path::parse("a").unwrap(),
        op: Op::Set,
    }]);
}

#[test]
fn diff_full_document_scenarios() {
    let old = json!({
        "a": {"b": 1, "c": 2, "same": true},
        "removed_top": 1,
        "arr": [1, 2],
    });
    let new = json!({
        "a": {"b": 10, "c": 2, "same": true, "new_leaf": 5},
        "arr": [1, 2, 3],
        "added_top": 1,
    });
    let touched = diff(&old, &new);
    assert_eq!(
        touched_paths(&touched),
        vec![
            ("a.b".to_string(), true),
            ("a.new_leaf".to_string(), true),
            ("added_top".to_string(), true),
            ("arr".to_string(), true),
            ("removed_top".to_string(), false),
        ]
    );
}

#[test]
fn make_patch_and_apply_patch_round_trip_various_docs() {
    let samples: Vec<(serde_json::Value, serde_json::Value)> = vec![
        (json!({}), json!({})),
        (json!({"a": 1}), json!({"a": 1})),
        (json!({}), json!({"a": {"b": {"c": 1}}})),
        (json!({"a": {"b": {"c": 1}}}), json!({})),
        (
            json!({"a": [1, 2], "b": {"x": 1, "y": 2}}),
            json!({"a": [1, 2, 3], "b": {"x": 1, "z": 3}}),
        ),
        (
            json!({"list": [{"n": 1}, {"n": 2}]}),
            json!({"list": [{"n": 1}]}),
        ),
        (json!({"a__": "comment", "a": 1}), json!({"a__": "new comment", "a": 2})),
    ];
    for (old, new) in samples {
        let patch = make_patch(&old, &new);
        let mut applied = old.clone();
        apply_patch(&mut applied, &patch);
        assert_eq!(applied, new, "round trip failed for old={old} new={new} patch={patch}");
    }
}
