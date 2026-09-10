//! Staged edits: what an editor accumulates between a row edit and Apply,
//! and what one Apply then sends (`docs/DESIGN.md` §8, "Edits are staged").
//!
//! An [`EditSet`] is the one model behind the editor's two views. A row edit
//! stages an [`Edit`]; the tree renders [`EditSet::apply`] -- the document as
//! it *would* be; switching to text renders that same planned document, and
//! switching back turns whatever was typed into edits again through
//! [`EditSet::between`]; Apply writes the planned subtree at
//! [`EditSet::write_view`], the narrowest view covering every staged path.
//!
//! The semantics are [`crate::view`]'s: a `set` at a path is
//! [`view::replace`] there and a `delete` is [`view::remove`], the same two
//! operations a `PUT ?view=` and a `DELETE ?view=` perform on the server. So
//! what the editor predicts and what the store does are one function, not
//! two that agree.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::model::{self, Value};
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

/// The staged edits on one document, in the order they were staged.
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

    /// Stages `edit`, dropping every earlier edit at or under its path: a
    /// write of `a` says everything about `a.b`, so an older `a.b` would only
    /// be re-applied on top of the new value. The root replaces everything.
    pub fn stage(&mut self, edit: Edit) {
        self.0.retain(|e| !edit.path.is_prefix_of(&e.path));
        self.0.push(edit);
    }

    /// The edits at or under `path` -- what [`EditSet::stage`] would drop, and
    /// what discarding one row's edits removes. One rule for both.
    pub fn under<'a>(&'a self, path: &'a Path) -> impl Iterator<Item = &'a Edit> + 'a {
        self.0.iter().filter(move |e| path.is_prefix_of(&e.path))
    }

    /// Drops the edits at or under `path`.
    pub fn discard_under(&mut self, path: &Path) {
        self.0.retain(|e| !path.is_prefix_of(&e.path));
    }

    /// The document as it would be once these edits are applied to `stored`:
    /// what the tree renders, what the schema findings are computed from, and
    /// what Apply writes. Key order is kept as [`view::replace`] keeps it.
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
    /// Checks itself: key order is data, and a pure reordering produces no
    /// per-key edits, so the result is replayed and replaced by a single
    /// whole-document set when the replay is not what was typed. A diff that
    /// quietly loses something is worse than no diff at all.
    pub fn between(stored: &Value, edited: &Value) -> EditSet {
        let whole = || EditSet(vec![Edit::set(Path::root(), edited.clone())]);
        let (Value::Object(_), Value::Object(_)) = (stored, edited) else {
            return if model::same_ordered(stored, edited) { EditSet::new() } else { whole() };
        };
        let mut out = Vec::new();
        walk(stored, edited, &Path::root(), &mut out);
        let set = EditSet(out);
        match set.apply(stored) {
            Ok(replayed) if model::same_ordered(&replayed, edited) => set,
            _ => whole(),
        }
    }

    /// The narrowest view covering every staged path: the common prefix of
    /// them all, moved one level up when a delete sits exactly there, since a
    /// key cannot be removed by replacing it. `None` with nothing staged.
    ///
    /// Narrow on purpose: a write that names less can collide with less, and
    /// a scope-only principal cannot name the root view at all
    /// (`docs/DESIGN.md` §3.4). Not narrow for *permission* reasons -- this
    /// consults no scopes; what a write may do is decided by what it changes,
    /// and a plan whose narrowest view is the root is an ordinary write.
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
            Some(wv) if !model::same_ordered(wv, nv) => out.push(Edit::set(at, nv.clone())),
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
/// document touched every path in it. Not the edits needed to get from one
/// to the other; that is [`EditSet::between`], which must be exact and so
/// has a fallback this deliberately lacks.
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
    fn apply_is_view_replace_and_view_remove_in_staging_order() {
        let stored = json!({"zebra": 1, "alpha": 2, "middle": {"z": 1, "a": 2}});
        let mut set = EditSet::new();
        set.stage(Edit::set(p("alpha"), json!(9)));
        set.stage(Edit::set(p("middle.new"), json!(true)));
        set.stage(Edit::delete(p("middle.z")));
        set.stage(Edit::set(p("deep.er.key"), json!("made")));
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
        // And a later root set discards everything staged before it.
        let mut set = EditSet::new();
        set.stage(Edit::set(p("a"), json!(5)));
        set.stage(Edit::set(Path::root(), json!({"fresh": true})));
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn staging_at_a_path_drops_what_was_staged_under_it() {
        let mut set = EditSet::new();
        set.stage(Edit::set(p("a.b"), json!(1)));
        set.stage(Edit::set(p("a.c"), json!(2)));
        set.stage(Edit::set(p("x"), json!(3)));
        set.stage(Edit::set(p("a"), json!({"whole": true})));
        let paths: Vec<String> = set.edits().iter().map(|e| e.path.to_string()).collect();
        assert_eq!(paths, ["x", "a"]);
        assert_eq!(set.under(&p("a")).count(), 1);
        set.discard_under(&p("a"));
        assert_eq!(set.len(), 1);
        // `ab` is not under `a`: the separator makes a child.
        set.stage(Edit::set(p("ab"), json!(1)));
        set.stage(Edit::set(p("a"), json!(2)));
        assert_eq!(set.len(), 3);
    }

    #[test]
    fn between_produces_row_edits_and_replays_exactly() {
        let stored = json!({"a": {"b": 1, "c": 2}, "d": 4, "gone": 1, "list": [1, 2]});
        // `new` appended at the end: a key inserted anywhere else is an order
        // change, and the whole-document fallback below is the right answer.
        let edited = json!({"a": {"b": 10, "c": 2}, "d": 4, "list": [1, 2, 3], "new": 1});
        let set = EditSet::between(&stored, &edited);
        let shown: Vec<String> = set
            .edits()
            .iter()
            .map(|e| format!("{:?} {} {}", e.op, e.path, e.value.as_ref().map(ordered).unwrap_or_default()))
            .collect();
        assert_eq!(shown, ["Set a.b 10", "Set list [1,2,3]", "Set new 1", "Delete gone "]);
        assert!(model::same_ordered(&set.apply(&stored).unwrap(), &edited));
    }

    #[test]
    fn between_falls_back_to_the_whole_document_when_order_is_all_that_changed() {
        // Key order is data: a reordering has no per-key expression, and
        // rather than lose it the diff replaces the document whole.
        let stored = json!({"b": 1, "a": 2});
        let edited = json!({"a": 2, "b": 1});
        let set = EditSet::between(&stored, &edited);
        assert_eq!(set.edits(), &[Edit::set(Path::root(), edited.clone())]);
        assert_eq!(ordered(&set.apply(&stored).unwrap()), ordered(&edited));

        // ... at any depth, and for a key inserted anywhere but the end.
        let stored = json!({"m": {"b": 1, "a": 2}});
        assert_eq!(EditSet::between(&stored, &json!({"m": {"a": 2, "b": 1}})).edits()[0].path, Path::root());
        let stored = json!({"a": 1, "c": 3});
        assert_eq!(EditSet::between(&stored, &json!({"a": 1, "b": 2, "c": 3})).edits()[0].path, Path::root());
        // Appending stays a one-key edit.
        assert_eq!(EditSet::between(&stored, &json!({"a": 1, "c": 3, "d": 4})).edits()[0].path, p("d"));

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
