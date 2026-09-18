//! Integration tests for merge-patch semantics (`pve_meta_core::patch`).

use pretty_assertions::assert_eq;
use pve_meta_core::patch::{apply_patch, Op};
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
