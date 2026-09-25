//! Views: prefix-addressed reads/writes into a document, through maps only
//! (`docs/DESIGN.md` §2). [`replace`]/[`merge`]/[`remove`] report touched
//! paths below the prefix, never vacuously for a write that changes structure.

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
/// result). The root prefix returns the whole document, unchanged.
/// `Ok(None)` if nothing exists there.
///
/// # Errors
/// [`Error::InvalidPath`] if `prefix` runs through an array or a scalar: a
/// view addresses maps only, on the read side as on the write side
/// (`docs/DESIGN.md` §2).
pub fn extract(doc: &Value, prefix: &Path) -> Result<Option<Value>> {
    Ok(descend(doc, prefix)?.cloned())
}

/// Replaces the subtree at `prefix` with `subtree` wholesale (not a merge),
/// creating any missing intermediate maps along the way. An empty-object
/// `subtree` stores an **empty map**, not a delete (`docs/DESIGN.md` §5;
/// deleting a view is [`remove`]). Replacing at the root replaces the whole
/// document.
///
/// Not linted here: the one lint runs on the planned document, in
/// [`crate::api`] (`docs/DESIGN.md` §5).
///
/// # Errors
/// [`Error::InvalidPath`] if `prefix` runs through an array or a scalar.
pub fn replace(doc: &mut Value, prefix: &Path, subtree: Value) -> Result<Vec<Touched>> {
    if prefix.is_root() {
        let old = doc.clone();
        *doc = subtree;
        return Ok(patch::diff(&old, doc));
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
            // even though the content diff is empty: report it, so `touched`
            // is never vacuous for a write that changed the document.
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
/// (`docs/DESIGN.md` §5). A non-object value at `prefix` is
/// merged over as if it were `{}`.
///
/// # Errors
/// [`Error::InvalidPath`] if `prefix` runs through an array or a scalar.
pub fn merge(doc: &mut Value, prefix: &Path, patch_value: &Value) -> Result<Vec<Touched>> {
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
/// root), and the note about it, `<key>__` beside it. A no-op (no touched
/// paths) if neither exists.
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
    // A key's note goes with it (`docs/DESIGN.md` §2), reported like the key.
    let mut touched = Vec::new();
    let note = format!("{key}{}", model::COMMENT_SUFFIX);
    if map.shift_remove(&note).is_some() {
        touched.push(Touched { path: parent_path.join(note), op: Op::Delete });
    }
    let own = match map.shift_remove(&key) {
        Some(Value::Object(old_map)) => {
            // Report *below* `prefix`, matching every other operation's
            // touched-path convention (see the module docs).
            let mut below = Vec::new();
            patch::diff_at(
                &Value::Object(old_map),
                &Value::Object(Map::new()),
                prefix,
                &mut below,
            );
            // Removing an existing (but empty) map is still a change.
            if below.is_empty() {
                below.push(Touched {
                    path: prefix.clone(),
                    op: Op::Delete,
                });
            }
            below
        }
        Some(_) => vec![Touched {
            path: prefix.clone(),
            op: Op::Delete,
        }],
        None => Vec::new(),
    };
    touched.extend(own);
    Ok(touched)
}

/// `value` without its comment keys, at any depth, array members included: what
/// a read answers unless it asks for `comments` (`docs/DESIGN.md` §2). A
/// scalar is itself.
pub fn strip_comments(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(k, _)| !model::is_comment_key(k))
                .map(|(k, v)| (k.clone(), strip_comments(v)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(strip_comments).collect()),
        other => other.clone(),
    }
}

/// `true` if a segment of `path` is a comment key: a path that names a note.
pub fn names_comment(path: &Path) -> bool {
    path.segments().iter().any(|s| model::is_comment_key(s))
}

/// `new` with the stored notes of `old` put back where the map-level rule
/// keeps them (`docs/DESIGN.md` §2): for a map both sides have, a stored
/// `k__` whose `k` is in both survives unless `new` already carries its own
/// `k__`, and a stored bare `__` survives the same way while `new` is still a
/// map. Recurses into a key both sides hold as a map; a list's contents are
/// never touched, and neither is anything below a key that changed shape.
///
/// A kept note goes after the nearest key that preceded it in `old` and is
/// still there, or first, so a stripped read written back is the same bytes:
/// no rewrite, and no version change.
pub fn keep_comments(old: &Value, new: Value) -> Value {
    match (old, new) {
        (Value::Object(om), Value::Object(nm)) => {
            let mut out: Vec<(String, Value)> = nm
                .into_iter()
                .map(|(k, v)| match om.get(&k) {
                    Some(ov) if ov.is_object() && v.is_object() => (k.clone(), keep_comments(ov, v)),
                    _ => (k, v),
                })
                .collect();
            let keys: Vec<&String> = om.keys().collect();
            for (i, (k, v)) in om.iter().enumerate() {
                let Some(subject) = k.strip_suffix(model::COMMENT_SUFFIX) else { continue };
                let alive = subject.is_empty() || (om.contains_key(subject) && out.iter().any(|(n, _)| n == subject));
                if !alive || out.iter().any(|(n, _)| n == k) {
                    continue;
                }
                let at = keys[..i]
                    .iter()
                    .rev()
                    .find_map(|prev| out.iter().position(|(n, _)| n == *prev))
                    .map_or(0, |p| p + 1);
                out.insert(at, (k.clone(), v.clone()));
            }
            Value::Object(out.into_iter().collect())
        }
        (_, new) => new,
    }
}

/// Renders `value` (a view's extracted subtree, or a whole document) as
/// `format`'s canonical text. Unlike a stored document, a view's value need
/// not be an object at the top level (e.g. a view of an array-valued key).
pub fn render(value: &Value, format: Format) -> String {
    format::dump(format, value)
}

/// Parses `text` as `format` into a `mode=replace` payload: like
/// [`format::parse`], but with no lint and no "must be an object" rule (a
/// view's payload can legitimately be an array or a scalar).
///
/// The payload is not validated here because it is not yet a document: the
/// one lint runs on the *planned* document, after the payload has been
/// spliced in, so its findings name real document paths
/// (`docs/DESIGN.md` §5).
///
/// # Errors
/// [`Error::Parse`] on a syntax error.
pub fn parse(text: &str, format: Format) -> Result<Value> {
    format::parse_raw(format, text)
}

/// Parses `text` as `format` into a `mode=merge` payload: an RFC 7386 merge
/// patch, in which `null` is the delete marker *anywhere*
/// (`docs/DESIGN.md` §5: "`merge` with `null` deletes").
///
/// The only structural rule is that a patch is an object — otherwise
/// [`patch::apply_patch`] would silently do nothing. Everything else the
/// patch produces is judged by the one lint on the planned document; a
/// patch key that only ever deletes never reaches it.
///
/// # Errors
/// [`Error::Parse`] on a syntax error. [`Error::Lint`] if the patch is not
/// an object at the top level.
pub fn parse_patch(text: &str, format: Format) -> Result<Value> {
    let value = format::parse_raw(format, text)?;
    if !value.is_object() {
        return Err(Error::Lint(vec![model::Lint {
            path: Path::root(),
            msg: "a merge payload must be an object".to_string(),
        }]));
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
    fn extract_cases() {
        let notes = json!({"__": "doc", "x__": "about x", "x": 1});
        for (doc, prefix, expected) in [
            (json!({"a": 1, "b": {"c": 2}}), "", Some(json!({"a": 1, "b": {"c": 2}}))),
            (json!({"traefik": {"spec": {"host": "x"}}}), "traefik.spec", Some(json!({"host": "x"}))),
            (json!({"a": 1}), "missing", None),
            (json!({"a": {"b": 1}}), "a.missing.deeper", None),
            (json!({"a": notes}), "a", Some(json!({"__": "doc", "x__": "about x", "x": 1}))), // comment keys travel with the subtree
        ] {
            assert_eq!(extract(&doc, &p(prefix)).unwrap(), expected, "{prefix}");
        }
    }

    #[test]
    fn extract_through_array_or_scalar_is_invalid_path() {
        // A read refuses what a write refuses: a view addresses maps only
        // (`docs/DESIGN.md` §2), and saying "nothing there" would be a lie.
        for (doc, prefix) in [
            (json!({"a": [1, 2, 3]}), "a.0"),
            (json!({"a": [1, 2, 3]}), "a.0.b"),
            (json!({"a": "scalar"}), "a.b"),
        ] {
            assert!(matches!(extract(&doc, &p(prefix)), Err(Error::InvalidPath(_))), "{prefix}");
        }
    }

    // -- replace ----------------------------------------------------------

    #[test]
    fn replace_cases() {
        for (doc, prefix, subtree, expected_doc, expected_touched) in [
            (json!({"a": 1, "b": 2}), "", json!({"b": 2, "c": 3}), json!({"b": 2, "c": 3}), vec!["a", "c"]),
            (json!({}), "a.b.c", json!({"x": 1}), json!({"a": {"b": {"c": {"x": 1}}}}), vec!["a.b.c.x"]),
            (
                json!({"traefik": {"spec": {"host": "old", "port": 1}}}),
                "traefik.spec",
                json!({"host": "new"}),
                json!({"traefik": {"spec": {"host": "new"}}}),
                vec!["traefik.spec.host", "traefik.spec.port"],
            ),
        ] {
            let mut doc = doc;
            let touched = replace(&mut doc, &p(prefix), subtree).unwrap();
            assert_eq!(doc, expected_doc, "{prefix}");
            assert_eq!(paths(&touched), expected_touched, "{prefix}");
        }
    }

    #[test]
    fn replace_with_empty_object_cases() {
        // `replace` with `{}` stores an empty map, never a delete
        // (`docs/DESIGN.md` §5); creating structure must never report
        // `touched: []`, or an authorization check on `touched` misses it.
        for (doc, prefix, expected_doc, expected_touched) in [
            (json!({"traefik": {"spec": {"host": "x"}}, "other": 1}), "traefik", json!({"traefik": {}, "other": 1}), vec![Touched { path: p("traefik.spec"), op: Op::Delete }]),
            (json!({"traefik": {}, "other": 1}), "traefik", json!({"traefik": {}, "other": 1}), vec![]),
            (json!({"tags": "prod"}), "tags", json!({"tags": {}}), vec![Touched { path: p("tags"), op: Op::Set }]),
            (json!({"a": 1}), "missing", json!({"a": 1, "missing": {}}), vec![Touched { path: p("missing"), op: Op::Set }]),
            (json!({}), "zzz.deep", json!({"zzz": {"deep": {}}}), vec![Touched { path: p("zzz.deep"), op: Op::Set }]),
        ] {
            let mut doc = doc;
            let touched = replace(&mut doc, &p(prefix), json!({})).unwrap();
            assert_eq!(doc, expected_doc, "{prefix}");
            assert_eq!(touched, expected_touched, "{prefix}");
        }
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
    fn merge_cases() {
        let traefik_host_x = json!({"traefik": {"spec": {"host": "x"}}});
        for (doc, prefix, patch, expected_doc, expected_touched) in [
            (json!({}), "a.b", json!({"x": 1}), json!({"a": {"b": {"x": 1}}}), vec!["a.b.x"]),
            (
                json!({"traefik": {"spec": {"host": "old", "port": 1}}}),
                "traefik.spec",
                json!({"host": "new"}),
                json!({"traefik": {"spec": {"host": "new", "port": 1}}}),
                vec!["traefik.spec.host"],
            ),
            // A delete does not prune the parent it empties.
            (traefik_host_x.clone(), "traefik.spec", json!({"host": null}), json!({"traefik": {"spec": {}}}), vec!["traefik.spec.host"]),
            // Deleting a key that never existed is a no-op, not a literal `null`, and creates nothing.
            (json!({}), "a.b", json!({"gone": null}), json!({}), vec![]),
            // Same value: nothing is spliced.
            (json!({"a": {"b": 1}}), "a", json!({"b": 1}), json!({"a": {"b": 1}}), vec![]),
            // A scalar at `prefix` is merged over as if it were `{}` -- even an empty patch still sets it.
            (json!({"a": "scalar"}), "a", json!({"x": 1}), json!({"a": {"x": 1}}), vec!["a.x"]),
            (json!({"a": "scalar"}), "a", json!({}), json!({"a": {}}), vec!["a"]),
            // An empty merge at an arbitrary or not-yet-existing prefix touches and mutates nothing.
            (traefik_host_x.clone(), "zzz_hacked", json!({}), traefik_host_x.clone(), vec![]),
            (traefik_host_x.clone(), "zzz_hacked.deep", json!({}), traefik_host_x.clone(), vec![]),
            (traefik_host_x.clone(), "traefik.spec.deeper", json!({}), traefik_host_x.clone(), vec![]),
        ] {
            let mut doc = doc;
            let touched = merge(&mut doc, &p(prefix), &patch).unwrap();
            assert_eq!(doc, expected_doc, "{prefix} + {patch}");
            assert_eq!(paths(&touched), expected_touched, "{prefix} + {patch}");
        }
    }

    #[test]
    fn merge_never_stores_a_nested_delete_marker_as_a_literal_null() {
        // A nested delete marker must never be spliced into the document as
        // a literal `null` -- the same rule the top-level merge enforces.
        let mut absent = json!({"traefik": {"host": "x"}});
        let touched = merge(&mut absent, &p("traefik"), &json!({"sub": {"gone": null}})).unwrap();
        assert_eq!(absent, json!({"traefik": {"host": "x"}}));
        assert!(touched.is_empty(), "a merge that touches nothing changes nothing");

        let mut mixed = json!({"traefik": {"host": "x"}});
        let touched2 = merge(&mut mixed, &p("traefik"), &json!({"sub": {"port": 1, "gone": null}})).unwrap();
        assert_eq!(mixed, json!({"traefik": {"host": "x", "sub": {"port": 1}}}));
        assert_eq!(paths(&touched2), vec!["traefik.sub".to_string()]);

        // Whatever a merge produces must pass the document lint, or the API
        // layer rejects a write it accepted as a patch.
        for (prefix, patch) in [
            ("traefik", json!({"sub": {"gone": null}})),
            ("traefik", json!({"host": {"deep": null}})),
            ("nope", json!({"gone": null})),
        ] {
            let mut doc = json!({"traefik": {"host": "x"}});
            merge(&mut doc, &p(prefix), &patch).unwrap();
            assert!(model::lint(&doc).is_empty(), "{prefix} + {patch}: {doc}");
        }
    }

    #[test]
    fn merge_through_array_is_invalid_path() {
        let mut doc = json!({"a": [1, 2, 3]});
        assert!(matches!(merge(&mut doc, &p("a.0.b"), &json!({"x": 1})), Err(Error::InvalidPath(_))));
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
    fn remove_cases() {
        for (doc, prefix, expected_doc, expected_touched) in [
            (json!({"a": {}, "b": 1}), "a", json!({"b": 1}), vec![Touched { path: p("a"), op: Op::Delete }]),
            (json!({"tags": "prod"}), "tags", json!({}), vec![Touched { path: p("tags"), op: Op::Delete }]),
            (json!({"a": 1}), "missing", json!({"a": 1}), vec![]),
        ] {
            let mut doc = doc;
            let touched = remove(&mut doc, &p(prefix)).unwrap();
            assert_eq!(doc, expected_doc, "{prefix}");
            assert_eq!(touched, expected_touched, "{prefix}");
        }
    }

    #[test]
    fn remove_at_root_empties_the_document() {
        let mut doc = json!({"a": 1, "b": 2});
        let touched = remove(&mut doc, &Path::root()).unwrap();
        assert_eq!(doc, json!({}));
        assert_eq!(paths(&touched), vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn remove_through_array_is_invalid_path() {
        let mut doc = json!({"a": [1, 2, 3]});
        assert!(matches!(remove(&mut doc, &p("a.0")), Err(Error::InvalidPath(_))));
    }

    // -- comment keys -------------------------------------------------------

    #[test]
    fn strip_comments_reaches_every_depth_and_leaves_the_rest() {
        let doc = json!({"__": "d", "a__": "n", "a": {"b__": "n", "b": [{"c__": "n", "c": 1}, 2]}, "s": "x__"});
        assert_eq!(strip_comments(&doc), json!({"a": {"b": [{"c": 1}, 2]}, "s": "x__"}));
        assert_eq!(strip_comments(&json!("x")), json!("x"));
    }

    #[test]
    fn keep_comments_follows_the_map_level_rule() {
        // (stored, payload, expected), `docs/DESIGN.md` §2.
        let cases = [
            // A sibling note survives with the key it is about.
            (json!({"a__": "about a", "a": 1}), json!({"a": 2}), json!({"a__": "about a", "a": 2})),
            // Its key dropped, the note goes with it.
            (json!({"a__": "about a", "a": 1}), json!({}), json!({})),
            // A bare `__` is the map's own note, kept while the payload leaves
            // no `__` of its own.
            (
                json!({"__": "about the map", "a": 1}),
                json!({"a": 2}),
                json!({"__": "about the map", "a": 2}),
            ),
            // A nested map keeps its own notes the same way, recursively.
            (json!({"m": {"x__": "x", "x": 1}}), json!({"m": {"x": 2}}), json!({"m": {"x__": "x", "x": 2}})),
            // A list's contents are never touched, whatever they carry.
            (json!({"l": [{"k__": "k", "k": 1}]}), json!({"l": [{"k": 1}]}), json!({"l": [{"k": 1}]})),
            // The payload's own note is never overwritten by the stored one.
            (json!({"a__": "old", "a": 1}), json!({"a__": "new", "a": 2}), json!({"a__": "new", "a": 2})),
        ];
        for (old, new, expected) in cases {
            assert_eq!(keep_comments(&old, new.clone()), expected, "old={old} new={new}");
        }
    }

    #[test]
    fn remove_takes_the_keys_note_with_it() {
        let mut doc = json!({"a": {"x": 1}, "a__": "about a", "b__": "stale"});
        let touched = remove(&mut doc, &p("a")).unwrap();
        assert_eq!(doc, json!({"b__": "stale"}));
        assert_eq!(paths(&touched), vec!["a.x", "a__"]);
        let touched = remove(&mut doc, &p("b")).unwrap();
        assert_eq!((doc, paths(&touched)), (json!({}), vec!["b__".to_string()]));
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
    fn parse_does_not_lint_the_payload() {
        // `docs/DESIGN.md` §5: the one lint runs on the planned *document*,
        // so a payload's `null` is refused there -- naming the path it would
        // land on -- rather than here, at the payload's own root.
        let value = parse("[1, null]", Format::Yaml).unwrap();
        let mut doc = json!({});
        replace(&mut doc, &p("a"), value).unwrap();
        let lints = model::lint(&doc);
        assert_eq!(lints.len(), 1);
        assert_eq!(lints[0].path.to_string(), "a.1");
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
    fn parse_patch_carries_null_delete_markers_through() {
        // `docs/DESIGN.md` §5: `merge` with `null` deletes, in both wire
        // formats, top-level and nested.
        for (text, fmt) in [
            ("{\"host\": null}", Format::Json),
            ("host: null\n", Format::Yaml),
            ("host: ~\n", Format::Yaml),
            ("{\"spec\": {\"host\": null}}", Format::Json),
            ("spec:\n  host: null\n", Format::Yaml),
        ] {
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
    fn parse_patch_requires_an_object_and_nothing_else() {
        // The one structural rule: a non-object patch would apply as a
        // silent no-op. Everything else a patch produces is judged by the
        // lint on the planned document.
        assert!(matches!(parse_patch("[1, 2]", Format::Json), Err(Error::Lint(_))));
        assert!(matches!(parse_patch("5", Format::Json), Err(Error::Lint(_))));
        assert!(parse_patch("{\"bad key\": 1}", Format::Json).is_ok());
        assert!(parse_patch("{\"foo__\": null}", Format::Json).is_ok());
        assert!(parse_patch("{\"foo__\": 5}", Format::Json).is_ok());

        // ... and the document that would result is what the lint refuses.
        let mut doc = json!({});
        merge(&mut doc, &Path::root(), &parse_patch("{\"foo__\": 5}", Format::Json).unwrap()).unwrap();
        assert_eq!(model::lint(&doc).len(), 1);
    }

    #[test]
    fn parse_patch_rejects_bad_syntax() {
        assert!(matches!(parse_patch("a: [", Format::Yaml), Err(Error::Parse { .. })));
    }
}
