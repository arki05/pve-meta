//! Views: prefix-addressed reads/writes into a document (`docs/DESIGN.md` §1).
//!
//! A *view* is a key-path [`Path`] prefix into a document's object tree,
//! addressing only through maps, never through an array: [`extract`] reads
//! the subtree at a prefix (with the prefix itself stripped); [`replace`]
//! and [`merge`] write it back (whole-subtree replace vs. RFC 7386-style
//! merge-patch, both scoped to the prefix); [`remove`] deletes it.
//! [`filter`] builds the "no view" read: the union of several readable
//! prefixes, unstripped and in the document's own order.
//! [`render`]/[`parse`]/[`parse_patch`] convert a view's value to and from
//! wire text (YAML/JSON), independent of the whole-document
//! [`crate::format`] contract (a view's value need not itself be an object;
//! a *merge patch* may additionally contain `null` delete markers).
//!
//! The touched paths returned by [`replace`], [`merge`] and [`remove`] are
//! reported *below* the view prefix rather than at it (see
//! [`patch::diff`]/[`patch::apply_patch`]): unlike [`patch::apply_patch`]'s
//! "replacing/deleting a whole subtree yields its root path" convention
//! (which exists to keep a merge-patch's own change summary concise), this
//! module's touched list feeds [`crate::scopes::Grants::check_write`], which
//! must be able to reject a write that reaches outside the caller's scope
//! inside a replaced or removed subtree. Where a whole subtree disappears at
//! once the report names that subtree's own path, not each leaf under it —
//! which only makes the write check *stricter*, since every reported path is
//! an ancestor of everything it stands for.
//!
//! **The touched list is complete** (`docs/DESIGN.md` §8, review F2): every
//! one of these operations reports at least one touched path whenever it
//! changes the document at all — including the corner cases where the
//! leaf-granular diff of the *content* is empty because the content is an
//! empty map (creating `a: {}` where `a` was absent, or removing an existing
//! `a: {}`). A caller must never be able to create or destroy structure with
//! a vacuous `touched: []`. Correspondingly, a [`merge`] that touches
//! nothing does not mutate `doc` at all: it creates no containers.

use std::collections::{HashMap, HashSet};

use serde_json::Map;

use crate::error::{Error, Result};
use crate::format::{self, Format};
use crate::model::{self, Value};
use crate::patch::{self, Op, Touched};
use crate::path::Path;

fn blocked(prefix: &Path) -> Error {
    Error::InvalidPath(format!(
        "{prefix}: a view prefix may only address into maps, never through an array or a scalar"
    ))
}

/// Read-only descent through `doc`'s object tree along `path`. `Ok(None)`
/// means some segment is simply missing (a legitimate "nothing there yet").
fn descend<'a>(doc: &'a Value, path: &Path) -> Result<Option<&'a Value>> {
    let mut cur = doc;
    for seg in path.segments() {
        match cur {
            Value::Object(map) => match map.get(seg) {
                Some(v) => cur = v,
                None => return Ok(None),
            },
            _ => return Err(blocked(path)),
        }
    }
    Ok(Some(cur))
}

/// Like [`descend`], but mutable.
fn descend_mut<'a>(doc: &'a mut Value, path: &Path) -> Result<Option<&'a mut Value>> {
    let mut cur = doc;
    for seg in path.segments() {
        match cur {
            Value::Object(map) => match map.get_mut(seg) {
                Some(v) => cur = v,
                None => return Ok(None),
            },
            _ => return Err(blocked(path)),
        }
    }
    Ok(Some(cur))
}

/// Like [`descend_mut`], but creates missing intermediate objects along the
/// way (used by [`replace`] and [`merge`], which write *into* a prefix that
/// may not exist yet). Still refuses to walk through an existing array or
/// scalar.
fn descend_creating<'a>(doc: &'a mut Value, path: &Path) -> Result<&'a mut Value> {
    let mut cur = doc;
    for seg in path.segments() {
        let map = cur.as_object_mut().ok_or_else(|| blocked(path))?;
        cur = map
            .entry(seg.clone())
            .or_insert_with(|| Value::Object(Map::new()));
    }
    if !cur.is_object() {
        return Err(blocked(path));
    }
    Ok(cur)
}

/// Extracts the subtree at `prefix` (the prefix itself is not part of the
/// result). The root prefix returns the whole document, unchanged. `None` if
/// nothing exists there, or if `prefix` runs through an array or a scalar (a
/// view prefix only ever addresses into maps).
pub fn extract(doc: &Value, prefix: &Path) -> Option<Value> {
    descend(doc, prefix).ok().flatten().cloned()
}

/// Replaces the subtree at `prefix` with `subtree` wholesale (not a merge),
/// creating any missing intermediate maps along the way. An empty-object
/// `subtree` stores an **empty map** — it does not remove the key
/// (`docs/DESIGN.md` §8; deleting a view is [`remove`], i.e.
/// `DELETE …?view=`). Replacing at the root replaces the whole document
/// (`subtree` must then itself satisfy the full document rules: an object,
/// no nulls, valid keys).
///
/// # Errors
/// [`Error::InvalidPath`] if `prefix` runs through an array or a scalar.
/// [`Error::Lint`] if `subtree` (or, at the root, the whole new document)
/// fails the document-model rules.
pub fn replace(doc: &mut Value, prefix: &Path, subtree: Value) -> Result<Vec<Touched>> {
    if prefix.is_root() {
        let lints = model::lint(&subtree);
        if !lints.is_empty() {
            return Err(Error::Lint(lints));
        }
        let old = doc.clone();
        *doc = subtree;
        return Ok(patch::diff(&old, doc));
    }

    let lints = model::lint_relaxed(&subtree);
    if !lints.is_empty() {
        return Err(Error::Lint(lints));
    }

    let parent_path = prefix.parent().expect("non-root path has a parent");
    let key = prefix.last().expect("non-root path has a last segment").to_string();

    // Refuse an impossible path (through an array/scalar) before touching
    // anything, then materialise the parent chain.
    let parent = descend_creating(doc, &parent_path)?;
    let map = parent
        .as_object_mut()
        .expect("descend_creating always returns an object");
    let old = map.get(&key).cloned();
    map.insert(key, subtree.clone());

    let mut touched = Vec::new();
    match &old {
        Some(old) => patch::diff_at(old, &subtree, prefix, &mut touched),
        None => {
            patch::diff_at(&Value::Object(Map::new()), &subtree, prefix, &mut touched);
            // Creating a key whose value is an empty map is a real change
            // even though the content diff is empty: report it, so it can
            // never slip past `Grants::check_write` (review F2).
            if touched.is_empty() {
                touched.push(Touched {
                    path: prefix.clone(),
                    op: Op::Set,
                });
            }
        }
    }
    Ok(touched)
}

/// Applies `patch` (RFC 7386 merge-patch semantics, see [`patch::apply_patch`])
/// to the subtree at `prefix`. Merging away every key of a map leaves that
/// map in place, empty: merge never prunes an emptied parent (the document
/// model has no concept of "absent vs. empty map" beyond what is literally
/// written).
///
/// A merge that changes nothing **mutates nothing**: intermediate maps are
/// only materialised once the patch is known to write something
/// (`docs/DESIGN.md` §8, review F2). A non-object value at `prefix` is
/// merged over as if it were `{}`.
///
/// # Errors
/// [`Error::InvalidPath`] if `prefix` runs through an array or a scalar.
/// [`Error::Lint`] if `patch` fails [`patch::lint_patch`].
pub fn merge(doc: &mut Value, prefix: &Path, patch_value: &Value) -> Result<Vec<Touched>> {
    let lints = patch::lint_patch(patch_value);
    if !lints.is_empty() {
        return Err(Error::Lint(lints));
    }

    if prefix.is_root() {
        return Ok(patch::apply_patch(doc, patch_value));
    }

    let parent_path = prefix.parent().expect("non-root path has a parent");
    let key = prefix.last().expect("non-root path has a last segment").to_string();

    // Read-only descent first: this both rejects a prefix that runs through
    // an array/scalar and tells us what is currently at `prefix`, without
    // creating anything.
    let existing = descend(doc, prefix)?;
    let existing_is_object = matches!(existing, Some(Value::Object(_)));
    let existed = existing.is_some();
    // The scratch subtree the patch is applied to: the current object, or
    // `{}` for an absent (or non-object) value. Merging against `{}` is what
    // makes a delete of a not-yet-existing field a no-op instead of splicing
    // a literal `null` into the document.
    let mut scratch = match existing {
        Some(Value::Object(m)) => Value::Object(m.clone()),
        _ => Value::Object(Map::new()),
    };

    let mut touched = Vec::new();
    patch::apply_obj(&mut scratch, patch_value, prefix, &mut touched);

    // Replacing a scalar/array at `prefix` with a map is itself a change,
    // even when the patch body wrote no leaves.
    if existed && !existing_is_object && touched.is_empty() {
        touched.push(Touched {
            path: prefix.clone(),
            op: Op::Set,
        });
    }

    if touched.is_empty() {
        // Nothing to write: leave `doc` exactly as it was. In particular,
        // create no containers along `prefix`.
        return Ok(Vec::new());
    }

    let parent = descend_creating(doc, &parent_path)?;
    parent
        .as_object_mut()
        .expect("descend_creating always returns an object")
        .insert(key, scratch);
    Ok(touched)
}

/// Removes the subtree at `prefix` (empties the document, if `prefix` is
/// root). A no-op (no touched paths) if nothing exists there.
///
/// # Errors
/// [`Error::InvalidPath`] if `prefix` runs through an array or a scalar.
pub fn remove(doc: &mut Value, prefix: &Path) -> Result<Vec<Touched>> {
    if prefix.is_root() {
        let old = doc.clone();
        *doc = Value::Object(Map::new());
        return Ok(patch::diff(&old, doc));
    }

    let parent_path = prefix.parent().expect("non-root path has a parent");
    let key = prefix.last().expect("non-root path has a last segment").to_string();

    let Some(parent) = descend_mut(doc, &parent_path)? else {
        return Ok(Vec::new());
    };
    let Some(map) = parent.as_object_mut() else {
        return Err(blocked(prefix));
    };
    match map.shift_remove(&key) {
        Some(Value::Object(old_map)) => {
            // Report *below* `prefix`, so a write-scope check can catch
            // removed content outside the caller's scope even when it is
            // nested under `prefix`.
            let mut touched = Vec::new();
            patch::diff_at(
                &Value::Object(old_map),
                &Value::Object(Map::new()),
                prefix,
                &mut touched,
            );
            // Removing an existing (but empty) map is still a change.
            if touched.is_empty() {
                touched.push(Touched {
                    path: prefix.clone(),
                    op: Op::Delete,
                });
            }
            Ok(touched)
        }
        Some(_) => Ok(vec![Touched {
            path: prefix.clone(),
            op: Op::Delete,
        }]),
        None => Ok(Vec::new()),
    }
}

/// Builds the "no view" read: the union of `readable_prefixes`, unstripped
/// (comment keys travel with whatever they document) and in the document's
/// own key order. A prefix list containing the root returns the whole
/// document, unchanged. Unlike [`extract`]/[`replace`]/[`merge`]/[`remove`],
/// this never errors: a prefix that runs through an array or a scalar simply
/// never matches anything below that point (this is a read-only,
/// best-effort union over whatever prefixes a caller happens to have).
pub fn filter(doc: &Value, readable_prefixes: &[Path]) -> Value {
    if readable_prefixes.iter().any(|p| p.is_root()) {
        return doc.clone();
    }
    match doc {
        Value::Object(map) => Value::Object(filter_map(map, &Path::root(), readable_prefixes)),
        other => other.clone(),
    }
}

fn filter_map(map: &Map<String, Value>, path: &Path, readable: &[Path]) -> Map<String, Value> {
    // First pass: decide which non-comment keys are included (and, for a
    // partial match, what their filtered value looks like), without regard
    // to order.
    let mut include: HashMap<&str, Value> = HashMap::new();
    for (k, v) in map.iter() {
        if model::is_comment_key(k) {
            continue;
        }
        let child_path = path.join(k.clone());
        if readable.iter().any(|p| p.is_prefix_of(&child_path)) {
            // Fully readable: the whole subtree, unstripped.
            include.insert(k.as_str(), v.clone());
        } else if let Value::Object(sub) = v {
            if readable.iter().any(|p| child_path.is_prefix_of(p)) {
                // Only part of this map is readable: recurse.
                let filtered = filter_map(sub, &child_path, readable);
                if !filtered.is_empty() {
                    include.insert(k.as_str(), Value::Object(filtered));
                }
            }
        }
        // Otherwise: not readable (including a non-object node -- e.g. an
        // array -- that a readable prefix would have to run through).
    }

    // Second pass: build the result in the map's own order, bringing along
    // comment keys for included siblings (and the bare `__` map comment, if
    // anything in this map survived).
    //
    // The set of included base keys is snapshotted *before* the pass:
    // `include` is drained as the pass consumes it, so testing it directly
    // would drop `<key>__` whenever it follows its own key in document
    // order — the natural authoring order (review F10).
    let included_keys: HashSet<&str> = include.keys().copied().collect();
    let any_included = !included_keys.is_empty();
    let mut out = Map::new();
    for (k, v) in map.iter() {
        if model::is_comment_key(k) {
            if k == model::COMMENT_SUFFIX {
                if any_included {
                    out.insert(k.clone(), v.clone());
                }
            } else {
                let base = &k[..k.len() - model::COMMENT_SUFFIX.len()];
                if included_keys.contains(base) {
                    out.insert(k.clone(), v.clone());
                }
            }
        } else if let Some(val) = include.remove(k.as_str()) {
            out.insert(k.clone(), val);
        }
    }
    out
}

/// Renders `value` (a view's extracted subtree, or a whole document) as
/// `format`'s canonical text. Unlike a stored document, a view's value need
/// not be an object at the top level (e.g. a view of an array-valued key).
pub fn render(value: &Value, format: Format) -> String {
    format::dump(format, value)
}

/// Parses `text` as `format` into a `mode=replace` payload: like
/// [`format::parse`], but does not require the top level to be an object (a
/// view's payload can legitimately be an array or a scalar).
///
/// `null` is rejected here — a replace payload is document content, and
/// documents have no nulls. A `mode=merge` payload goes through
/// [`parse_patch`] instead.
///
/// # Errors
/// [`Error::Parse`] on a syntax error. [`Error::Lint`] if the parsed value
/// otherwise fails the document-model rules (no nulls, valid keys, comment
/// key values are strings).
pub fn parse(text: &str, format: Format) -> Result<Value> {
    let value = format::parse_raw(format, text)?;
    let lints = model::lint_relaxed(&value);
    if !lints.is_empty() {
        return Err(Error::Lint(lints));
    }
    Ok(value)
}

/// Parses `text` as `format` into a `mode=merge` payload: an RFC 7386 merge
/// patch, validated with [`patch::lint_patch`], which permits `null`
/// *anywhere* as the delete marker (`docs/DESIGN.md` §3, §8: "`merge` with
/// `null` deletes").
///
/// This is what makes the documented delete reachable through the API: the
/// replace-shaped [`parse`] rejects `null` outright, so routing a merge
/// payload through it would make `null` unreachable (review F8).
///
/// # Errors
/// [`Error::Parse`] on a syntax error. [`Error::Lint`] if the patch is not
/// an object at the top level, has invalid keys, or gives a comment key a
/// non-string, non-null value.
pub fn parse_patch(text: &str, format: Format) -> Result<Value> {
    let value = format::parse_raw(format, text)?;
    let lints = patch::lint_patch(&value);
    if !lints.is_empty() {
        return Err(Error::Lint(lints));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    fn p(s: &str) -> Path {
        Path::parse(s).unwrap()
    }

    fn paths(touched: &[Touched]) -> Vec<String> {
        let mut v: Vec<String> = touched.iter().map(|t| t.path.to_string()).collect();
        v.sort();
        v
    }

    // -- extract --------------------------------------------------------

    #[test]
    fn extract_root_returns_whole_doc() {
        let doc = json!({"a": 1, "b": {"c": 2}});
        assert_eq!(extract(&doc, &Path::root()), Some(doc.clone()));
    }

    #[test]
    fn extract_nested_subtree_strips_prefix() {
        let doc = json!({"traefik": {"spec": {"host": "x"}}});
        assert_eq!(extract(&doc, &p("traefik.spec")), Some(json!({"host": "x"})));
    }

    #[test]
    fn extract_missing_is_none() {
        let doc = json!({"a": 1});
        assert_eq!(extract(&doc, &p("missing")), None);
        assert_eq!(extract(&doc, &p("a.deeper")), None);
    }

    #[test]
    fn extract_through_array_is_none() {
        let doc = json!({"a": [1, 2, 3]});
        assert_eq!(extract(&doc, &p("a.0")), None);
    }

    #[test]
    fn extract_comment_keys_travel_with_subtree() {
        let doc = json!({"a": {"__": "doc", "x__": "about x", "x": 1}});
        assert_eq!(extract(&doc, &p("a")), Some(json!({"__": "doc", "x__": "about x", "x": 1})));
    }

    // -- replace ----------------------------------------------------------

    #[test]
    fn replace_at_root_replaces_whole_document() {
        let mut doc = json!({"a": 1, "b": 2});
        let touched = replace(&mut doc, &Path::root(), json!({"b": 2, "c": 3})).unwrap();
        assert_eq!(doc, json!({"b": 2, "c": 3}));
        assert_eq!(paths(&touched), vec!["a".to_string(), "c".to_string()]);
    }

    #[test]
    fn replace_at_root_rejects_non_object() {
        let mut doc = json!({});
        assert!(matches!(replace(&mut doc, &Path::root(), json!([1, 2])), Err(Error::Lint(_))));
    }

    #[test]
    fn replace_creates_intermediate_maps() {
        let mut doc = json!({});
        let touched = replace(&mut doc, &p("a.b.c"), json!({"x": 1})).unwrap();
        assert_eq!(doc, json!({"a": {"b": {"c": {"x": 1}}}}));
        assert_eq!(paths(&touched), vec!["a.b.c.x".to_string()]);
    }

    #[test]
    fn replace_overwrites_existing_subtree_atomically() {
        let mut doc = json!({"traefik": {"spec": {"host": "old", "port": 1}}});
        let touched = replace(&mut doc, &p("traefik.spec"), json!({"host": "new"})).unwrap();
        assert_eq!(doc, json!({"traefik": {"spec": {"host": "new"}}}));
        let mut ps = paths(&touched);
        ps.sort();
        assert_eq!(ps, vec!["traefik.spec.host".to_string(), "traefik.spec.port".to_string()]);
    }

    #[test]
    fn replace_with_empty_object_stores_an_empty_map() {
        // docs/DESIGN.md §8: `replace` with `{}` stores an empty map;
        // deleting a view is `DELETE ?view=` (review F9).
        let mut doc = json!({"traefik": {"spec": {"host": "x"}}, "other": 1});
        let touched = replace(&mut doc, &p("traefik"), json!({})).unwrap();
        assert_eq!(doc, json!({"traefik": {}, "other": 1}));
        assert_eq!(paths(&touched), vec!["traefik.spec".to_string()]);
    }

    #[test]
    fn replace_empty_object_round_trips_an_empty_namespace() {
        // Opening a view whose value is legitimately `{}` and applying it
        // unchanged must not delete the key.
        let mut doc = json!({"traefik": {}, "other": 1});
        let touched = replace(&mut doc, &p("traefik"), json!({})).unwrap();
        assert_eq!(doc, json!({"traefik": {}, "other": 1}));
        assert!(touched.is_empty());
    }

    #[test]
    fn replace_with_empty_object_on_scalar_sets_an_empty_map() {
        let mut doc = json!({"tags": "prod"});
        let touched = replace(&mut doc, &p("tags"), json!({})).unwrap();
        assert_eq!(doc, json!({"tags": {}}));
        assert_eq!(touched, vec![Touched { path: p("tags"), op: Op::Set }]);
    }

    #[test]
    fn replace_with_empty_object_on_missing_key_creates_it_and_reports_touched() {
        // Creating structure must never report `touched: []` -- that is the
        // authorization bypass of review F2.
        let mut doc = json!({"a": 1});
        let touched = replace(&mut doc, &p("missing"), json!({})).unwrap();
        assert_eq!(doc, json!({"a": 1, "missing": {}}));
        assert_eq!(touched, vec![Touched { path: p("missing"), op: Op::Set }]);
    }

    #[test]
    fn replace_deep_empty_object_reports_touched_for_the_structure_it_creates() {
        let mut doc = json!({});
        let touched = replace(&mut doc, &p("zzz.deep"), json!({})).unwrap();
        assert_eq!(doc, json!({"zzz": {"deep": {}}}));
        assert_eq!(touched, vec![Touched { path: p("zzz.deep"), op: Op::Set }]);
    }

    #[test]
    fn replace_through_array_is_invalid_path() {
        let mut doc = json!({"a": [1, 2, 3]});
        assert!(matches!(replace(&mut doc, &p("a.0.b"), json!({"x": 1})), Err(Error::InvalidPath(_))));
    }

    #[test]
    fn replace_through_scalar_is_invalid_path() {
        let mut doc = json!({"a": "scalar"});
        assert!(matches!(replace(&mut doc, &p("a.b"), json!({"x": 1})), Err(Error::InvalidPath(_))));
    }

    #[test]
    fn replace_rejects_invalid_subtree() {
        let mut doc = json!({});
        assert!(matches!(replace(&mut doc, &p("a"), json!({"bad key": 1})), Err(Error::Lint(_))));
    }

    #[test]
    fn replace_comment_keys_travel_with_subtree() {
        let mut doc = json!({});
        replace(&mut doc, &p("a"), json!({"__": "doc", "x": 1})).unwrap();
        assert_eq!(doc, json!({"a": {"__": "doc", "x": 1}}));
    }

    #[test]
    fn replace_empty_map_stays_when_set_explicitly_via_non_empty_parent() {
        // Setting a *non-final* key's value to an object that itself
        // contains an explicitly-empty nested map (not the removal marker,
        // which only applies to the subtree at `prefix` itself) keeps that
        // empty map.
        let mut doc = json!({});
        replace(&mut doc, &p("a"), json!({"empty": {}})).unwrap();
        assert_eq!(doc, json!({"a": {"empty": {}}}));
    }

    // -- merge --------------------------------------------------------------

    #[test]
    fn merge_at_root_behaves_like_apply_patch() {
        let patch_value = json!({"a": null, "b": 2});
        let mut via_merge = json!({"a": 1});
        let touched = merge(&mut via_merge, &Path::root(), &patch_value).unwrap();
        assert_eq!(via_merge, json!({"b": 2}));

        let mut via_apply_patch = json!({"a": 1});
        let expected_touched = patch::apply_patch(&mut via_apply_patch, &patch_value);
        assert_eq!(via_merge, via_apply_patch);
        assert_eq!(touched, expected_touched);
    }

    #[test]
    fn merge_creates_intermediate_maps() {
        let mut doc = json!({});
        let touched = merge(&mut doc, &p("a.b"), &json!({"x": 1})).unwrap();
        assert_eq!(doc, json!({"a": {"b": {"x": 1}}}));
        assert_eq!(paths(&touched), vec!["a.b.x".to_string()]);
    }

    #[test]
    fn merge_recurses_into_existing_subtree() {
        let mut doc = json!({"traefik": {"spec": {"host": "old", "port": 1}}});
        let touched = merge(&mut doc, &p("traefik.spec"), &json!({"host": "new"})).unwrap();
        assert_eq!(doc, json!({"traefik": {"spec": {"host": "new", "port": 1}}}));
        assert_eq!(paths(&touched), vec!["traefik.spec.host".to_string()]);
    }

    #[test]
    fn merge_delete_does_not_prune_the_parent_when_it_becomes_empty() {
        let mut doc = json!({"traefik": {"spec": {"host": "x"}}});
        let touched = merge(&mut doc, &p("traefik.spec"), &json!({"host": null})).unwrap();
        // The spec says merge does *not* remove an emptied parent: the map
        // stays, empty.
        assert_eq!(doc, json!({"traefik": {"spec": {}}}));
        assert_eq!(touched, vec![Touched { path: p("traefik.spec.host"), op: Op::Delete }]);
    }

    #[test]
    fn merge_delete_against_a_not_yet_existing_prefix_is_a_noop_not_a_literal_null() {
        let mut doc = json!({});
        let touched = merge(&mut doc, &p("a.b"), &json!({"gone": null})).unwrap();
        // "delete a key that never existed" is a no-op, same as apply_patch;
        // critically this must not splice a literal `null` into the doc --
        // and (review F2) it must not create `a.b` either.
        assert_eq!(doc, json!({}));
        assert!(touched.is_empty());
    }

    #[test]
    fn merge_with_empty_patch_never_mutates_the_document() {
        // The live reproduction of review F2: an empty merge at an arbitrary
        // deep prefix must create nothing and touch nothing, so that a
        // vacuous `check_write([])` cannot be used to write structure.
        for prefix in ["zzz_hacked", "zzz_hacked.deep", "traefik.spec.deeper"] {
            let mut doc = json!({"traefik": {"spec": {"host": "x"}}});
            let before = doc.clone();
            let touched = merge(&mut doc, &p(prefix), &json!({})).unwrap();
            assert!(touched.is_empty(), "{prefix}: expected no touched paths");
            assert_eq!(doc, before, "{prefix}: merge mutated the document");
        }
    }

    #[test]
    fn merge_that_writes_nothing_leaves_an_existing_document_byte_identical() {
        let mut doc = json!({"a": {"b": 1}});
        let before = doc.clone();
        // Same value: apply_obj reports nothing, so nothing is spliced.
        let touched = merge(&mut doc, &p("a"), &json!({"b": 1})).unwrap();
        assert!(touched.is_empty());
        assert_eq!(doc, before);
    }

    #[test]
    fn merge_over_a_scalar_replaces_it_with_a_map() {
        let mut doc = json!({"a": "scalar"});
        let touched = merge(&mut doc, &p("a"), &json!({"x": 1})).unwrap();
        assert_eq!(doc, json!({"a": {"x": 1}}));
        assert_eq!(paths(&touched), vec!["a.x".to_string()]);

        // An *empty* patch over a scalar still empties it -- and says so.
        let mut doc2 = json!({"a": "scalar"});
        let touched2 = merge(&mut doc2, &p("a"), &json!({})).unwrap();
        assert_eq!(doc2, json!({"a": {}}));
        assert_eq!(touched2, vec![Touched { path: p("a"), op: Op::Set }]);
    }

    #[test]
    fn merge_through_array_is_invalid_path() {
        let mut doc = json!({"a": [1, 2, 3]});
        assert!(matches!(merge(&mut doc, &p("a.0.b"), &json!({"x": 1})), Err(Error::InvalidPath(_))));
    }

    #[test]
    fn merge_rejects_invalid_patch() {
        let mut doc = json!({});
        assert!(matches!(merge(&mut doc, &p("a"), &json!([1, 2])), Err(Error::Lint(_))));
        assert!(matches!(merge(&mut doc, &p("a"), &json!({"bad key": 1})), Err(Error::Lint(_))));
    }

    // -- remove ---------------------------------------------------------

    #[test]
    fn remove_deletes_the_key_unlike_replace_with_an_empty_object() {
        let mut a = json!({"traefik": {"spec": {"host": "x"}}, "other": 1});
        let ta = remove(&mut a, &p("traefik")).unwrap();
        assert_eq!(a, json!({"other": 1}));
        assert_eq!(paths(&ta), vec!["traefik.spec".to_string()]);

        let mut b = json!({"traefik": {"spec": {"host": "x"}}, "other": 1});
        replace(&mut b, &p("traefik"), json!({})).unwrap();
        assert_eq!(b, json!({"traefik": {}, "other": 1}));
    }

    #[test]
    fn remove_of_an_existing_empty_map_reports_a_delete() {
        let mut doc = json!({"a": {}, "b": 1});
        let touched = remove(&mut doc, &p("a")).unwrap();
        assert_eq!(doc, json!({"b": 1}));
        assert_eq!(touched, vec![Touched { path: p("a"), op: Op::Delete }]);
    }

    #[test]
    fn remove_of_a_scalar_reports_a_delete() {
        let mut doc = json!({"tags": "prod"});
        let touched = remove(&mut doc, &p("tags")).unwrap();
        assert_eq!(doc, json!({}));
        assert_eq!(touched, vec![Touched { path: p("tags"), op: Op::Delete }]);
    }

    #[test]
    fn remove_at_root_empties_the_document() {
        let mut doc = json!({"a": 1, "b": 2});
        let touched = remove(&mut doc, &Path::root()).unwrap();
        assert_eq!(doc, json!({}));
        assert_eq!(paths(&touched), vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn remove_missing_is_noop() {
        let mut doc = json!({"a": 1});
        assert!(remove(&mut doc, &p("missing")).unwrap().is_empty());
    }

    #[test]
    fn remove_through_array_is_invalid_path() {
        let mut doc = json!({"a": [1, 2, 3]});
        assert!(matches!(remove(&mut doc, &p("a.0")), Err(Error::InvalidPath(_))));
    }

    // -- filter -----------------------------------------------------------

    #[test]
    fn filter_no_readable_prefixes_is_empty() {
        let doc = json!({"a": 1, "b": {"c": 2}});
        assert_eq!(filter(&doc, &[]), json!({}));
    }

    #[test]
    fn filter_root_prefix_returns_whole_doc_unchanged() {
        let doc = json!({"__": "top", "a": 1, "b": {"c": 2}});
        assert_eq!(filter(&doc, &[Path::root()]), doc);
    }

    #[test]
    fn filter_keeps_only_readable_top_level_subtrees() {
        let doc = json!({"a": 1, "b": {"c": 2}, "d": 3});
        assert_eq!(filter(&doc, &[p("a"), p("d")]), json!({"a": 1, "d": 3}));
    }

    #[test]
    fn filter_partial_prefix_recurses() {
        let doc = json!({"a": {"b": {"c": 1}, "d": 2}});
        assert_eq!(filter(&doc, &[p("a.b")]), json!({"a": {"b": {"c": 1}}}));
    }

    #[test]
    fn filter_comment_keys_travel_with_included_sibling() {
        let doc = json!({"a__": "doc a", "a": {"x": 1}, "b": 2});
        assert_eq!(filter(&doc, &[p("a")]), json!({"a__": "doc a", "a": {"x": 1}}));
    }

    #[test]
    fn filter_keeps_comment_key_that_follows_its_own_key() {
        // Review F10: the natural authoring order (`a` then `a__`) used to
        // drop the comment, because the first pass's map was drained as the
        // second pass walked it.
        let doc = json!({"a": 1, "a__": "about a", "b": 2});
        assert_eq!(filter(&doc, &[p("a")]), json!({"a": 1, "a__": "about a"}));
    }

    #[test]
    fn filter_keeps_comment_keys_in_both_orders_for_nested_maps() {
        let doc = json!({"ns": {"x": 1, "x__": "about x", "y__": "about y", "y": 2}});
        assert_eq!(
            filter(&doc, &[p("ns")]),
            json!({"ns": {"x": 1, "x__": "about x", "y__": "about y", "y": 2}})
        );
    }

    #[test]
    fn filter_drops_comment_keys_for_excluded_sibling() {
        let doc = json!({"a__": "doc a", "a": {"x": 1}, "b": 2});
        assert_eq!(filter(&doc, &[p("b")]), json!({"b": 2}));
    }

    #[test]
    fn filter_bare_map_comment_travels_only_when_something_included() {
        let doc = json!({"__": "top doc", "a": 1, "b": 2});
        assert_eq!(filter(&doc, &[p("a")]), json!({"__": "top doc", "a": 1}));
        assert_eq!(filter(&doc, &[]), json!({}));
    }

    #[test]
    fn filter_preserves_document_order() {
        let doc = json!({"z": 1, "a__": "about a", "a": 2, "m": 3});
        let out = filter(&doc, &[p("z"), p("a"), p("m")]);
        let keys: Vec<&str> = out.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["z", "a__", "a", "m"]);
    }

    #[test]
    fn filter_through_array_never_matches() {
        let doc = json!({"a": [1, 2, 3]});
        assert_eq!(filter(&doc, &[p("a.0")]), json!({}));
    }

    #[test]
    fn filter_empty_maps_stay() {
        let doc = json!({"a": {}});
        assert_eq!(filter(&doc, &[p("a")]), json!({"a": {}}));
    }

    // -- render / parse -----------------------------------------------------

    #[test]
    fn render_and_parse_round_trip_non_object_values() {
        let value = json!(["prod", "web"]);
        let text = render(&value, Format::Yaml);
        assert_eq!(parse(&text, Format::Yaml).unwrap(), value);

        let scalar = json!("hello");
        let text2 = render(&scalar, Format::Json);
        assert_eq!(parse(&text2, Format::Json).unwrap(), scalar);
    }

    #[test]
    fn parse_rejects_null_even_for_non_object_top_level() {
        assert!(matches!(parse("[1, null]", Format::Yaml), Err(Error::Lint(_))));
    }

    #[test]
    fn parse_rejects_bad_syntax() {
        assert!(matches!(parse("a: [", Format::Yaml), Err(Error::Parse { .. })));
    }

    #[test]
    fn render_object_round_trips_through_all_formats() {
        let value = json!({"host": "x", "port": 8080});
        for fmt in Format::ALL {
            let text = render(&value, fmt);
            assert_eq!(parse(&text, fmt).unwrap(), value);
        }
    }

    // -- parse_patch --------------------------------------------------------

    #[test]
    fn parse_patch_accepts_null_where_parse_rejects_it() {
        // Review F8: `merge` + `null` deletes must be reachable from the
        // wire, in both JSON and YAML, top-level and nested.
        for (text, fmt) in [
            ("{\"host\": null}", Format::Json),
            ("host: null\n", Format::Yaml),
            ("host: ~\n", Format::Yaml),
            ("{\"spec\": {\"host\": null}}", Format::Json),
            ("spec:\n  host: null\n", Format::Yaml),
        ] {
            assert!(
                parse(text, fmt).is_err(),
                "replace payload must still reject null: {text:?}"
            );
            let patch = parse_patch(text, fmt)
                .unwrap_or_else(|e| panic!("merge payload must accept null: {text:?}: {e}"));
            assert!(patch.is_object());
        }
    }

    #[test]
    fn parse_patch_deletes_end_to_end_through_merge() {
        let mut doc = json!({"traefik": {"spec": {"host": "x", "port": 1}}});
        let patch = parse_patch("host: null\n", Format::Yaml).unwrap();
        let touched = merge(&mut doc, &p("traefik.spec"), &patch).unwrap();
        assert_eq!(doc, json!({"traefik": {"spec": {"port": 1}}}));
        assert_eq!(touched, vec![Touched { path: p("traefik.spec.host"), op: Op::Delete }]);
    }

    #[test]
    fn parse_patch_requires_an_object_and_checks_keys() {
        assert!(matches!(parse_patch("[1, 2]", Format::Json), Err(Error::Lint(_))));
        assert!(matches!(
            parse_patch("{\"bad key\": 1}", Format::Json),
            Err(Error::Lint(_))
        ));
        // A comment key may be deleted, but not set to a non-string.
        assert!(parse_patch("{\"foo__\": null}", Format::Json).is_ok());
        assert!(matches!(
            parse_patch("{\"foo__\": 5}", Format::Json),
            Err(Error::Lint(_))
        ));
    }

    #[test]
    fn parse_patch_rejects_bad_syntax() {
        assert!(matches!(parse_patch("a: [", Format::Yaml), Err(Error::Parse { .. })));
    }
}
