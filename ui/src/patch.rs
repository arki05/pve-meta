//! `pve_meta`-style JSON merge patch: diffing two `Value`s into a patch, applying a
//! patch back onto a `Value`, and small path helpers used by the form editor to mutate
//! a working copy in place.
//!
//! Semantics match [RFC 7386](https://www.rfc-editor.org/rfc/rfc7386) merge patch:
//! removed object keys become `null`, changed leaves are replaced wholesale, and arrays
//! are always atomic (a changed array is never merged element-by-element, only replaced).
//!
//! Pure module: no `web-sys`/`wasm-bindgen`, safe to unit-test natively.

use serde_json::{Map, Value};

/// Compute the merge patch that turns `old` into `new`.
///
/// Both must normally be objects for a meaningful per-key patch; if either side is not
/// an object the whole `new` value is returned (matching merge-patch semantics: you
/// cannot patch a non-object).
pub fn make_patch(old: &Value, new: &Value) -> Value {
    match (old, new) {
        (Value::Object(o), Value::Object(n)) => {
            let mut patch = Map::new();
            for key in o.keys() {
                if !n.contains_key(key) {
                    patch.insert(key.clone(), Value::Null);
                }
            }
            for (key, nv) in n {
                match o.get(key) {
                    None => {
                        patch.insert(key.clone(), nv.clone());
                    }
                    Some(ov) if ov != nv => {
                        patch.insert(key.clone(), make_patch(ov, nv));
                    }
                    _ => {}
                }
            }
            Value::Object(patch)
        }
        _ => new.clone(),
    }
}

/// Apply a merge patch to `target`, returning the result (RFC 7386 semantics).
pub fn apply_patch(target: &Value, patch: &Value) -> Value {
    match patch {
        Value::Object(patch_obj) => {
            let mut result = match target {
                Value::Object(m) => m.clone(),
                _ => Map::new(),
            };
            for (k, v) in patch_obj {
                if v.is_null() {
                    result.remove(k);
                } else {
                    let base = result.get(k).cloned().unwrap_or(Value::Null);
                    result.insert(k.clone(), apply_patch(&base, v));
                }
            }
            Value::Object(result)
        }
        _ => patch.clone(),
    }
}

/// True if a patch (as produced by [`make_patch`]) has no effect.
pub fn patch_is_empty(patch: &Value) -> bool {
    matches!(patch, Value::Object(m) if m.is_empty())
}

/// Read a nested value at a dotted path (given as segments).
pub fn get_path<'a>(value: &'a Value, path: &[String]) -> Option<&'a Value> {
    let mut cur = value;
    for seg in path {
        cur = cur.get(seg)?;
    }
    Some(cur)
}

/// Set a nested value at `path`, creating intermediate objects as needed. Replaces any
/// non-object value found along the way with an object so the path can be created.
pub fn set_path(value: &mut Value, path: &[String], new: Value) {
    if path.is_empty() {
        *value = new;
        return;
    }
    if !value.is_object() {
        *value = Value::Object(Map::new());
    }
    let obj = value.as_object_mut().expect("just ensured object");
    if path.len() == 1 {
        obj.insert(path[0].clone(), new);
    } else {
        let entry = obj
            .entry(path[0].clone())
            .or_insert_with(|| Value::Object(Map::new()));
        set_path(entry, &path[1..], new);
    }
}

/// Delete the value at `path`, if present. A no-op if any intermediate segment is
/// missing or not an object.
pub fn delete_path(value: &mut Value, path: &[String]) {
    if path.is_empty() {
        return;
    }
    if let Some(obj) = value.as_object_mut() {
        if path.len() == 1 {
            obj.remove(&path[0]);
        } else if let Some(entry) = obj.get_mut(&path[0]) {
            delete_path(entry, &path[1..]);
        }
    }
}

/// Flatten a merge patch into a human-readable list of `(dotted.path, op)` pairs, for
/// the "Pending" summary bar (`traefik.spec.host (set)`). `op` is `"removed"` for a
/// `null` leaf, `"set"` otherwise. An empty patch yields an empty list.
pub fn describe_patch(patch: &Value) -> Vec<(String, &'static str)> {
    let mut out = Vec::new();
    describe_patch_into("", patch, &mut out);
    out
}

fn describe_patch_into(prefix: &str, value: &Value, out: &mut Vec<(String, &'static str)>) {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                let path = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                describe_patch_into(&path, v, out);
            }
        }
        Value::Null => out.push((prefix.to_string(), "removed")),
        _ => out.push((prefix.to_string(), "set")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn patch_detects_added_key() {
        let old = json!({"a": 1});
        let new = json!({"a": 1, "b": 2});
        assert_eq!(make_patch(&old, &new), json!({"b": 2}));
    }

    #[test]
    fn patch_detects_removed_key() {
        let old = json!({"a": 1, "b": 2});
        let new = json!({"a": 1});
        assert_eq!(make_patch(&old, &new), json!({"b": null}));
    }

    #[test]
    fn patch_detects_changed_leaf() {
        let old = json!({"a": 1});
        let new = json!({"a": 2});
        assert_eq!(make_patch(&old, &new), json!({"a": 2}));
    }

    #[test]
    fn patch_is_empty_when_nothing_changed() {
        let old = json!({"a": {"b": 1}});
        let new = json!({"a": {"b": 1}});
        let p = make_patch(&old, &new);
        assert!(patch_is_empty(&p));
    }

    #[test]
    fn patch_recurses_into_nested_objects() {
        let old = json!({"traefik": {"spec": {"host": "old.example.com", "port": 80}}});
        let new = json!({"traefik": {"spec": {"host": "new.example.com", "port": 80}}});
        assert_eq!(
            make_patch(&old, &new),
            json!({"traefik": {"spec": {"host": "new.example.com"}}})
        );
    }

    #[test]
    fn patch_treats_arrays_as_atomic() {
        let old = json!({"a": [1, 2, 3]});
        let new = json!({"a": [1, 2]});
        assert_eq!(make_patch(&old, &new), json!({"a": [1, 2]}));
    }

    #[test]
    fn patch_replaces_when_type_changes() {
        let old = json!({"a": {"b": 1}});
        let new = json!({"a": "now a string"});
        assert_eq!(make_patch(&old, &new), json!({"a": "now a string"}));
    }

    #[test]
    fn apply_patch_sets_and_removes() {
        let target = json!({"a": 1, "b": 2});
        let patch = json!({"a": 5, "b": null, "c": 3});
        assert_eq!(apply_patch(&target, &patch), json!({"a": 5, "c": 3}));
    }

    #[test]
    fn apply_patch_recurses_into_nested_objects() {
        let target = json!({"traefik": {"spec": {"host": "old.example.com", "port": 80}}});
        let patch = json!({"traefik": {"spec": {"host": "new.example.com"}}});
        assert_eq!(
            apply_patch(&target, &patch),
            json!({"traefik": {"spec": {"host": "new.example.com", "port": 80}}})
        );
    }

    #[test]
    fn make_then_apply_round_trips() {
        let old = json!({"traefik": {"spec": {"host": "old.example.com", "port": 80}}, "netbird": {"enabled": true}});
        let new = json!({"traefik": {"spec": {"host": "new.example.com", "port": 80}}});
        let p = make_patch(&old, &new);
        assert_eq!(apply_patch(&old, &p), new);
    }

    #[test]
    fn get_set_delete_path_helpers() {
        let mut v = json!({"traefik": {"spec": {"port": 8080}}});
        assert_eq!(
            get_path(&v, &["traefik".into(), "spec".into(), "port".into()]),
            Some(&json!(8080))
        );
        set_path(
            &mut v,
            &["traefik".into(), "spec".into(), "host".into()],
            json!("wiki.example.com"),
        );
        assert_eq!(v["traefik"]["spec"]["host"], json!("wiki.example.com"));

        set_path(&mut v, &["netbird".into(), "enabled".into()], json!(true));
        assert_eq!(v["netbird"]["enabled"], json!(true));

        delete_path(&mut v, &["traefik".into(), "spec".into(), "port".into()]);
        assert_eq!(v["traefik"]["spec"].get("port"), None);
    }

    #[test]
    fn set_path_replaces_non_object_ancestor() {
        let mut v = json!({"traefik": "not an object"});
        set_path(&mut v, &["traefik".into(), "spec".into()], json!(1));
        assert_eq!(v, json!({"traefik": {"spec": 1}}));
    }

    #[test]
    fn describe_patch_lists_leaves_with_ops() {
        let old = json!({"traefik": {"spec": {"host": "old.example.com", "port": 80}}});
        let new = json!({"traefik": {"spec": {"host": "new.example.com"}}});
        let p = make_patch(&old, &new);
        let mut described = describe_patch(&p);
        described.sort();
        assert_eq!(
            described,
            vec![
                ("traefik.spec.host".to_string(), "set"),
                ("traefik.spec.port".to_string(), "removed"),
            ]
        );
    }

    #[test]
    fn describe_patch_empty_patch_is_empty() {
        assert_eq!(describe_patch(&json!({})), Vec::<(String, &str)>::new());
    }

    #[test]
    fn describe_patch_atomic_array_is_one_entry() {
        let p = make_patch(&json!({"tags": [1, 2, 3]}), &json!({"tags": [1, 2]}));
        assert_eq!(describe_patch(&p), vec![("tags".to_string(), "set")]);
    }
}
