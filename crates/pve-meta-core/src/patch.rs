//! Merge-patch semantics (RFC 7386) with explicit delete, plus the
//! structural diff that turns a whole-document replace into the same
//! leaf-granular `Touched` list.

use serde::{Deserialize, Serialize};

use crate::model::Value;
use crate::path::Path;

/// What happened to a path when a patch was applied (or when two documents
/// were diffed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
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
/// An object patch value always *applies* — it is never stored verbatim — so
/// its `null` delete markers delete rather than being written into the
/// document. Against an absent container that means a
/// patch of nothing but deletes is a no-op and creates no container; against
/// an existing scalar or array it means the value becomes a map (a change in
/// itself, reported at the container's own path).
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
            // A *non-empty* object patch over a target that is absent or is
            // not an object. Applying it verbatim spliced the patch's own
            // `null` delete markers into the document as literal nulls, which
            // `model::lint` then rejects — so the documented combined
            // set+delete patch shape (`{"sub": {"x": 1, "gone": null}}`)
            // would be unusable against a container that does not exist yet.
            // It is applied to an empty scratch map instead —
            // the same trick `view::merge` already used for the value at the
            // view prefix itself, one level down.
            Value::Object(patch_map) if !patch_map.is_empty() => {
                // Not an object (the arm above matched that case), so this is
                // "the key is absent" vs. "the key holds a scalar or array".
                let existed = doc_map.contains_key(k);
                let mut scratch = Value::Object(serde_json::Map::new());
                let mut sub = Vec::new();
                apply_obj(&mut scratch, v, &child_path, &mut sub);
                // Replacing an existing scalar/array with a map is a change
                // in itself, even if the patch body wrote no leaves; creating
                // a container the patch then writes nothing into is not (a
                // merge that touches nothing changes nothing,
                // `docs/DESIGN.md` §7). Either way the *container's* path is
                // what is reported, keeping this arm's "replacing a whole
                // subtree yields its root path" convention.
                if existed || !sub.is_empty() {
                    doc_map.insert(k.clone(), scratch);
                    touched.push(Touched {
                        path: child_path,
                        op: Op::Set,
                    });
                }
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
    use crate::model;
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
    fn a_nested_delete_marker_is_applied_not_spliced_into_the_document() {
        // A nested `null` delete marker must never land in the document as a
        // literal null, even against a container that does not exist yet: a
        // merge payload may carry those nulls on purpose, so nothing it
        // accepts may trip `model::lint`.

        // (a) Absent container, nothing but deletes: a no-op that creates
        //     nothing and touches nothing.
        let mut doc = json!({"a": 1});
        let touched = apply_patch(&mut doc, &json!({"sub": {"gone": null}}));
        assert_eq!(doc, json!({"a": 1}));
        assert!(touched.is_empty());

        // ... at any depth.
        let mut deep = json!({});
        assert!(apply_patch(&mut deep, &json!({"a": {"b": {"gone": null}}})).is_empty());
        assert_eq!(deep, json!({}));

        // (b) Absent container, a set and a delete: the set lands, the
        //     delete deletes nothing, no null is stored.
        let mut doc2 = json!({});
        let touched2 = apply_patch(&mut doc2, &json!({"sub": {"x": 1, "gone": null}}));
        assert_eq!(doc2, json!({"sub": {"x": 1}}));
        assert_eq!(touched2, vec![Touched { path: Path::parse("sub").unwrap(), op: Op::Set }]);

        // (c) Over a scalar: the scalar becomes a map, which is a change in
        //     itself and is reported at the container's own path.
        let mut doc3 = json!({"a": "scalar"});
        let touched3 = apply_patch(&mut doc3, &json!({"a": {"x": 1, "y": null}}));
        assert_eq!(doc3, json!({"a": {"x": 1}}));
        assert_eq!(touched3, vec![Touched { path: Path::parse("a").unwrap(), op: Op::Set }]);

        let mut doc4 = json!({"a": "scalar"});
        let touched4 = apply_patch(&mut doc4, &json!({"a": {"deep": null}}));
        assert_eq!(doc4, json!({"a": {}}));
        assert_eq!(touched4, vec![Touched { path: Path::parse("a").unwrap(), op: Op::Set }]);

        // No legal patch may produce a document `lint` rejects: a `null` is
        // the delete marker and is applied, never stored.
        for patch in [
            json!({"sub": {"gone": null}}),
            json!({"sub": {"x": 1, "gone": null}}),
            json!({"a": {"deep": null}}),
            json!({"a": {"b": {"c": null}}}),
        ] {
            let mut target = json!({"a": "scalar", "keep": 1});
            apply_patch(&mut target, &patch);
            assert!(
                model::lint(&target).is_empty(),
                "{patch} produced a document the lint refuses: {target}"
            );
        }
    }

    #[test]
    fn a_patch_key_that_only_deletes_never_reaches_the_document() {
        // One lint runs on the planned document (`docs/DESIGN.md` §7). A patch
        // key that would be invalid as a document key is harmless as long as
        // it only ever deletes.
        let mut doc = json!({"a": 1});
        assert!(apply_patch(&mut doc, &json!({"bad key": null})).is_empty());
        assert_eq!(doc, json!({"a": 1}));
        assert!(model::lint(&doc).is_empty());
    }
}
