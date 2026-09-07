//! Merge-patch semantics (RFC 7386) with explicit delete, plus the
//! structural diff that turns a whole-document replace into the same
//! leaf-granular `Touched` list.

use crate::model::{self, Lint, Value};
use crate::path::Path;

/// What happened to a path when a patch was applied (or when two documents
/// were diffed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// The value at this path was created or replaced.
    Set,
    /// The value at this path was removed.
    Delete,
}

/// One path that was affected by [`apply_patch`] or [`diff`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Touched {
    /// The affected path.
    pub path: Path,
    /// What happened there.
    pub op: Op,
}

/// Validates `patch` with the same key rules as a document (see
/// [`model::lint`]), except that `null` is permitted anywhere as a delete
/// marker.
pub fn lint_patch(patch: &Value) -> Vec<Lint> {
    let mut out = Vec::new();
    if !patch.is_object() {
        out.push(Lint {
            path: Path::root(),
            msg: "patch must be an object".to_string(),
        });
    }
    walk_patch(patch, &Path::root(), &mut out);
    out
}

fn walk_patch(v: &Value, path: &Path, out: &mut Vec<Lint>) {
    match v {
        Value::Object(map) => {
            for (k, val) in map.iter() {
                let child_path = path.join(k.clone());
                if !model::is_valid_key(k) {
                    out.push(Lint {
                        path: child_path.clone(),
                        msg: format!(
                            "invalid key '{k}': keys must match ^[A-Za-z0-9_@!-]+$ and contain no dots"
                        ),
                    });
                }
                if model::is_comment_key(k) && !(val.is_string() || val.is_null()) {
                    out.push(Lint {
                        path: child_path.clone(),
                        msg: "comment key value must be a string".to_string(),
                    });
                }
                walk_patch(val, &child_path, out);
            }
        }
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                walk_patch(item, &path.join(i.to_string()), out);
            }
        }
        // Null is allowed anywhere in a patch (it is the delete marker);
        // other scalars need no further checking.
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

/// Applies `patch` to `doc` in place, using merge-patch semantics (RFC 7386)
/// with explicit delete: for each `(k, v)` in a patch object — if `v` is an
/// object and the target already has an object at `k`, recurse; if `v` is
/// `null`, delete `k`; otherwise set `k = v` (replacing whatever was there,
/// including objects).
///
/// Returns the minimal set of paths that actually changed: recursing into an
/// object yields leaf paths; replacing or deleting a whole subtree yields its
/// root path; setting a key to the value it already has yields nothing;
/// deleting a missing key yields nothing.
///
/// Both `doc` and `patch` are expected to have an object at the top level;
/// if either does not, this is a no-op.
pub fn apply_patch(doc: &mut Value, patch: &Value) -> Vec<Touched> {
    let mut touched = Vec::new();
    if doc.is_object() && patch.is_object() {
        apply_obj(doc, patch, &Path::root(), &mut touched);
    }
    touched
}

pub(crate) fn apply_obj(doc: &mut Value, patch: &Value, path: &Path, touched: &mut Vec<Touched>) {
    let patch_map = match patch.as_object() {
        Some(m) => m,
        None => return,
    };
    let doc_map = match doc.as_object_mut() {
        Some(m) => m,
        None => return,
    };
    for (k, v) in patch_map.iter() {
        let child_path = path.join(k.clone());
        match v {
            Value::Null => {
                if doc_map.shift_remove(k).is_some() {
                    touched.push(Touched {
                        path: child_path,
                        op: Op::Delete,
                    });
                }
            }
            Value::Object(_) if matches!(doc_map.get(k), Some(Value::Object(_))) => {
                let child = doc_map.get_mut(k).expect("checked above");
                apply_obj(child, v, &child_path, touched);
            }
            _ => {
                if doc_map.get(k) != Some(v) {
                    doc_map.insert(k.clone(), v.clone());
                    touched.push(Touched {
                        path: child_path,
                        op: Op::Set,
                    });
                }
            }
        }
    }
}

/// Computes the minimal set of paths that changed between `old` and `new`,
/// for a full-document replace: recurses into objects that exist (as
/// objects) on both sides; a key only in `new` is a `Set`; a key only in
/// `old` is a `Delete`; differing non-object values (including arrays, which
/// are atomic) are a `Set` at that path.
pub fn diff(old: &Value, new: &Value) -> Vec<Touched> {
    let mut out = Vec::new();
    diff_at(old, new, &Path::root(), &mut out);
    out
}

pub(crate) fn diff_at(old: &Value, new: &Value, path: &Path, out: &mut Vec<Touched>) {
    match (old, new) {
        (Value::Object(om), Value::Object(nm)) => {
            for (k, nv) in nm.iter() {
                let child_path = path.join(k.clone());
                match om.get(k) {
                    Some(ov) => {
                        if ov.is_object() && nv.is_object() {
                            diff_at(ov, nv, &child_path, out);
                        } else if ov != nv {
                            out.push(Touched {
                                path: child_path,
                                op: Op::Set,
                            });
                        }
                    }
                    None => out.push(Touched {
                        path: child_path,
                        op: Op::Set,
                    }),
                }
            }
            for k in om.keys() {
                if !nm.contains_key(k) {
                    out.push(Touched {
                        path: path.join(k.clone()),
                        op: Op::Delete,
                    });
                }
            }
        }
        _ => {
            if old != new {
                out.push(Touched {
                    path: path.clone(),
                    op: Op::Set,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    #[test]
    fn apply_patch_sets_nested_and_new_keys() {
        let mut doc = json!({"a": {"b": 1, "c": 2}});
        let patch = json!({"a": {"b": 10, "d": 3}, "e": 5});
        let touched = apply_patch(&mut doc, &patch);
        assert_eq!(doc, json!({"a": {"b": 10, "c": 2, "d": 3}, "e": 5}));
        let mut paths: Vec<String> = touched.iter().map(|t| t.path.to_string()).collect();
        paths.sort();
        assert_eq!(paths, vec!["a.b".to_string(), "a.d".to_string(), "e".to_string()]);
        assert!(touched.iter().all(|t| t.op == Op::Set));
    }

    #[test]
    fn apply_patch_deletes_with_null() {
        let mut doc = json!({"a": 1, "b": 2});
        let patch = json!({"a": null});
        let touched = apply_patch(&mut doc, &patch);
        assert_eq!(doc, json!({"b": 2}));
        assert_eq!(touched, vec![Touched{path: Path::parse("a").unwrap(), op: Op::Delete}]);
    }

    #[test]
    fn apply_patch_deleting_missing_key_is_noop() {
        let mut doc = json!({"a": 1});
        let patch = json!({"missing": null});
        let touched = apply_patch(&mut doc, &patch);
        assert_eq!(doc, json!({"a": 1}));
        assert!(touched.is_empty());
    }

    #[test]
    fn apply_patch_setting_same_value_is_noop() {
        let mut doc = json!({"a": 1});
        let patch = json!({"a": 1});
        let touched = apply_patch(&mut doc, &patch);
        assert_eq!(doc, json!({"a": 1}));
        assert!(touched.is_empty());
    }

    #[test]
    fn apply_patch_replaces_whole_subtree_root_path_only() {
        let mut doc = json!({"a": {"b": 1, "c": 2}});
        // target has an array (not object) at "a", so the object patch value
        // replaces it wholesale rather than recursing.
        let mut doc2 = json!({"a": [1, 2, 3]});
        let patch = json!({"a": {"x": 1}});
        let touched = apply_patch(&mut doc2, &patch);
        assert_eq!(doc2, json!({"a": {"x": 1}}));
        assert_eq!(touched, vec![Touched{path: Path::parse("a").unwrap(), op: Op::Set}]);

        // deleting a nested subtree yields only its root path
        let patch_del = json!({"a": null});
        let touched_del = apply_patch(&mut doc, &patch_del);
        assert_eq!(doc, json!({}));
        assert_eq!(touched_del, vec![Touched{path: Path::parse("a").unwrap(), op: Op::Delete}]);
    }

    #[test]
    fn apply_patch_arrays_are_atomic() {
        let mut doc = json!({"a": [1, 2, 3]});
        let patch = json!({"a": [1, 2]});
        let touched = apply_patch(&mut doc, &patch);
        assert_eq!(doc, json!({"a": [1, 2]}));
        assert_eq!(touched, vec![Touched{path: Path::parse("a").unwrap(), op: Op::Set}]);
    }

    #[test]
    fn diff_detects_set_delete_and_recurse() {
        let old = json!({"a": {"b": 1, "c": 2}, "d": 4, "gone": 1});
        let new = json!({"a": {"b": 10, "c": 2}, "d": 4, "new": 1});
        let touched = diff(&old, &new);
        let mut paths: Vec<(String, bool)> = touched
            .iter()
            .map(|t| (t.path.to_string(), t.op == Op::Set))
            .collect();
        paths.sort();
        assert_eq!(
            paths,
            vec![
                ("a.b".to_string(), true),
                ("gone".to_string(), false),
                ("new".to_string(), true),
            ]
        );
    }

    #[test]
    fn diff_array_atomic() {
        let old = json!({"a": [1, 2]});
        let new = json!({"a": [1, 2, 3]});
        let touched = diff(&old, &new);
        assert_eq!(touched, vec![Touched{path: Path::parse("a").unwrap(), op: Op::Set}]);
    }

    #[test]
    fn lint_patch_allows_null_but_checks_keys() {
        assert!(lint_patch(&json!({"a": null, "b": {"c": null}})).is_empty());
        let lints = lint_patch(&json!({"bad key": 1}));
        assert_eq!(lints.len(), 1);
        let lints = lint_patch(&json!({"foo__": 5}));
        assert_eq!(lints.len(), 1);
        assert!(lint_patch(&json!({"foo__": null})).is_empty());
    }

    #[test]
    fn lint_patch_requires_object_top_level() {
        let lints = lint_patch(&json!([1, 2]));
        assert_eq!(lints.len(), 1);
    }
}
