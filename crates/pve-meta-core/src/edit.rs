//! Staged edits: the difference between what is stored and what an editor
//! shows, and what one Apply then sends (`docs/DESIGN.md` §12, "Edits are
//! staged").
//!
//! An [`EditSet`] is the one model behind the editor's two views, and it is
//! derived, not accumulated: a row edit is one [`Edit`] applied to the
//! planned document, and the set is [`EditSet::between`] the stored document
//! and the result. The tree renders [`EditSet::apply`] -- the document as it
//! *would* be; switching to text renders that same planned document, and
//! switching back runs `between` on whatever was typed; Apply writes the
//! planned subtree at [`EditSet::write_view`], the narrowest view covering
//! every staged path.
//!
//! The semantics are [`crate::view`]'s: a `set` at a path is
//! [`view::replace`] there and a `delete` is [`view::remove`], the same two
//! operations a `PUT ?view=` and a `DELETE ?view=` perform on the server. So
//! what the editor predicts and what the store does are one function, not
//! two that agree.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::model::Value;
use crate::patch::{self, Op};
use crate::path::Path;
use crate::view;

/// One staged change: set `path` to `value`, or delete it. The root path
/// with `Set` replaces the whole document; with `Delete` it empties it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Edit {
    pub path: Path,
    pub op: Op,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
}

impl Edit {
    pub fn set(path: Path, value: Value) -> Edit {
        Edit { path, op: Op::Set, value: Some(value) }
    }

    pub fn delete(path: Path) -> Edit {
        Edit { path, op: Op::Delete, value: None }
    }
}

/// The staged edits on one document.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EditSet(Vec<Edit>);

impl EditSet {
    pub fn new() -> EditSet {
        EditSet::default()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn edits(&self) -> &[Edit] {
        &self.0
    }

    /// The document as it would be once these edits are applied to `stored`,
    /// in order: what the tree renders, what the schema findings are computed
    /// from, and what Apply writes. Key order is kept as [`view::replace`]
    /// keeps it.
    ///
    /// # Errors
    /// [`Error::InvalidPath`] if an edit addresses through an array or a
    /// scalar (a view addresses through maps only); `Error::Other` for a `set`
    /// with no value. Neither is reachable from a row edit or a text diff,
    /// which is why a caller may treat one as a bug rather than a state.
    pub fn apply(&self, stored: &Value) -> Result<Value> {
        let mut planned = stored.clone();
        for e in &self.0 {
            match e.op {
                Op::Set => {
                    let value = e.value.clone().ok_or_else(|| {
                        Error::Other(anyhow::anyhow!("a set at '{}' carries no value", e.path))
                    })?;
                    view::replace(&mut planned, &e.path, value)?;
                }
                Op::Delete => {
                    view::remove(&mut planned, &e.path)?;
                }
            }
        }
        Ok(planned)
    }

    /// What would have to be staged to turn `stored` into `edited`, as the
    /// same edits a row produces: a key only in `edited` is a set, a key only
    /// in `stored` is a delete, a differing value is a set, and maps recurse
    /// so a one-key change stays a one-key edit. Lists are compared whole,
    /// because their members are not addressable (`docs/DESIGN.md` §2).
    ///
    /// Key order is not a value (`docs/DESIGN.md` §2, `decisions/007`): a
    /// pure reordering stages nothing, and a key inserted in the middle is
    /// one edit on that key, which lands at the end of its map.
    pub fn between(stored: &Value, edited: &Value) -> EditSet {
        let (Value::Object(_), Value::Object(_)) = (stored, edited) else {
            return if stored == edited {
                EditSet::new()
            } else {
                EditSet(vec![Edit::set(Path::root(), edited.clone())])
            };
        };
        let mut out = Vec::new();
        walk(stored, edited, &Path::root(), &mut out);
        EditSet(out)
    }

    /// The narrowest view covering every staged path: the common prefix of
    /// them all, moved one level up when a delete sits exactly there, since a
    /// key cannot be removed by replacing it. `None` with nothing staged.
    ///
    /// Narrow on purpose: a write that names less can collide with less. A
    /// plan whose narrowest view is the root is an ordinary write.
    pub fn write_view(&self) -> Option<Path> {
        let first = self.0.first()?;
        let mut common: Vec<String> = first.path.segments().to_vec();
        for e in &self.0[1..] {
            let shared = common
                .iter()
                .zip(e.path.segments())
                .take_while(|(a, b)| a == b)
                .count();
            common.truncate(shared);
        }
        let mut view = Path::new(common);
        if !view.is_root() && self.0.iter().any(|e| e.op == Op::Delete && e.path == view) {
            view = view.parent().expect("non-root path has a parent");
        }
        Some(view)
    }
}

impl From<Vec<Edit>> for EditSet {
    fn from(edits: Vec<Edit>) -> Self {
        EditSet(edits)
    }
}

fn walk(was: &Value, now: &Value, path: &Path, out: &mut Vec<Edit>) {
    let (Some(wm), Some(nm)) = (was.as_object(), now.as_object()) else { return };
    for (k, nv) in nm {
        let at = path.join(k.clone());
        match wm.get(k) {
            None => out.push(Edit::set(at, nv.clone())),
            Some(wv) if wv.is_object() && nv.is_object() => walk(wv, nv, &at, out),
            Some(wv) if wv != nv => out.push(Edit::set(at, nv.clone())),
            Some(_) => {}
        }
    }
    for k in wm.keys() {
        if !nm.contains_key(k) {
            out.push(Edit::delete(path.join(k.clone())));
        }
    }
}

/// Every path at which two documents differ in *value*: what an edit is
/// answerable for. [`patch::diff`]'s paths, whatever the op -- so a pure key
/// reordering changes nothing, and a caller is never told that reordering a
/// document touched every path in it. Lists are whole, as everywhere: a
/// list that differs is one changed path, never a member of it. Not the
/// edits needed to get from one to the other; that is [`EditSet::between`],
/// which says what to set and delete where this only says where.
pub fn changed_paths(was: &Value, now: &Value) -> Vec<Path> {
    patch::diff(was, now).into_iter().map(|t| t.path).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    fn p(s: &str) -> Path {
        Path::parse(s).unwrap()
    }

    fn ordered(v: &Value) -> String {
        serde_json::to_string(v).unwrap()
    }

    #[test]
    fn apply_is_view_replace_and_view_remove_in_order() {
        let stored = json!({"zebra": 1, "alpha": 2, "middle": {"z": 1, "a": 2}});
        let set = EditSet::from(vec![
            Edit::set(p("alpha"), json!(9)),
            Edit::set(p("middle.new"), json!(true)),
            Edit::delete(p("middle.z")),
            Edit::set(p("deep.er.key"), json!("made")),
        ]);
        let planned = set.apply(&stored).unwrap();
        assert_eq!(
            ordered(&planned),
            r#"{"zebra":1,"alpha":9,"middle":{"a":2,"new":true},"deep":{"er":{"key":"made"}}}"#,
            "order survives, a set keeps its slot, intermediates are created"
        );
        // The stored document is untouched.
        assert_eq!(stored["alpha"], 2);
    }

    #[test]
    fn the_root_path_is_the_document_not_a_key_called_nothing() {
        let stored = json!({"a": 1});
        let set = EditSet::from(vec![Edit::set(Path::root(), json!({"b": 2}))]);
        assert_eq!(set.apply(&stored).unwrap(), json!({"b": 2}));
        let set = EditSet::from(vec![Edit::delete(Path::root())]);
        assert_eq!(set.apply(&stored).unwrap(), json!({}));
    }

    #[test]
    fn between_produces_row_edits_and_replays_exactly() {
        let stored = json!({"a": {"b": 1, "c": 2}, "d": 4, "gone": 1, "list": [1, 2]});
        let edited = json!({"a": {"b": 10, "c": 2}, "d": 4, "list": [1, 2, 3], "new": 1});
        let set = EditSet::between(&stored, &edited);
        let shown: Vec<String> = set
            .edits()
            .iter()
            .map(|e| format!("{:?} {} {}", e.op, e.path, e.value.as_ref().map(ordered).unwrap_or_default()))
            .collect();
        assert_eq!(shown, ["Set a.b 10", "Set list [1,2,3]", "Set new 1", "Delete gone "]);
        assert_eq!(set.apply(&stored).unwrap(), edited);
    }

    #[test]
    fn a_pure_reordering_stages_nothing() {
        // Key order is kept on disk as a courtesy and is not a value
        // (`docs/DESIGN.md` §2): the same keys with the same values in another
        // order are the same document, at any depth.
        let stored = json!({"b": 1, "a": 2});
        assert!(EditSet::between(&stored, &json!({"a": 2, "b": 1})).is_empty());
        let nested = json!({"m": {"b": 1, "a": 2}});
        assert!(EditSet::between(&nested, &json!({"m": {"a": 2, "b": 1}})).is_empty());

        // A key inserted in the middle is one edit on that key, and it lands at
        // the end of its map; nothing else moves.
        let stored = json!({"a": 1, "c": 3});
        let set = EditSet::between(&stored, &json!({"a": 1, "b": 2, "c": 3}));
        assert_eq!(set.edits(), &[Edit::set(p("b"), json!(2))]);
        assert_eq!(ordered(&set.apply(&stored).unwrap()), r#"{"a":1,"c":3,"b":2}"#);

        // Identical documents stage nothing; a non-map on either side is whole or nothing.
        assert!(EditSet::between(&stored, &stored).is_empty());
        assert!(EditSet::between(&json!("x"), &json!("x")).is_empty());
        assert_eq!(EditSet::between(&json!("x"), &json!({"a": 1})).edits()[0].path, Path::root());
    }

    #[test]
    fn write_view_is_the_common_prefix_moved_up_for_a_delete_there() {
        let set = EditSet::from(vec![Edit::set(p("a.b.c"), json!(1)), Edit::set(p("a.b.d"), json!(2))]);
        assert_eq!(set.write_view(), Some(p("a.b")));
        let set = EditSet::from(vec![Edit::set(p("a.b.c"), json!(1)), Edit::set(p("x"), json!(2))]);
        assert_eq!(set.write_view(), Some(Path::root()));
        let set = EditSet::from(vec![Edit::set(p("selector"), json!({"tag": "web"}))]);
        assert_eq!(set.write_view(), Some(p("selector")), "one row is exactly the one-key write");
        // A delete cannot be expressed by replacing the thing being deleted.
        let set = EditSet::from(vec![Edit::delete(p("a.b"))]);
        assert_eq!(set.write_view(), Some(p("a")));
        let set = EditSet::from(vec![Edit::delete(p("a"))]);
        assert_eq!(set.write_view(), Some(Path::root()));
        // A delete *under* the common prefix does not move it.
        let set = EditSet::from(vec![Edit::set(p("a.b"), json!(1)), Edit::delete(p("a.c"))]);
        assert_eq!(set.write_view(), Some(p("a")));
        assert_eq!(EditSet::new().write_view(), None);
    }

    #[test]
    fn changed_paths_are_values_not_order_and_lists_are_whole() {
        let was = json!({"a": {"b": 1, "c": 2}, "list": [1], "gone": 1});
        let now = json!({"a": {"c": 2, "b": 1}, "list": [1, 2], "new": 1});
        let mut paths: Vec<String> = changed_paths(&was, &now).iter().map(ToString::to_string).collect();
        paths.sort();
        assert_eq!(paths, ["gone", "list", "new"]);
        assert!(changed_paths(&json!({"b": 1, "a": 2}), &json!({"a": 2, "b": 1})).is_empty());
    }

    #[test]
    fn edits_serialize_as_the_editor_holds_them() {
        let set = EditSet::from(vec![Edit::set(p("a.b"), json!(1)), Edit::delete(p("c"))]);
        assert_eq!(
            serde_json::to_string(&set).unwrap(),
            r#"[{"path":"a.b","op":"set","value":1},{"path":"c","op":"delete"}]"#
        );
        let back: EditSet = serde_json::from_str(r#"[{"path":"","op":"set","value":{"x":1}}]"#).unwrap();
        assert_eq!(back.edits()[0].path, Path::root());
    }
}
