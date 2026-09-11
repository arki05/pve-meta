//! The shape of one document: which declared prefixes reach it, which of
//! them governs a given path, and what that prefix's schema says about the
//! value there (`docs/DESIGN.md` §3.1, §8).
//!
//! A [`Shape`] is built once per document from the prefix registry and the
//! guest's tags, and everything that used to be a loose predicate -- "does
//! this selector match", "sort most-specific first", "which prefix governs",
//! "walk the schema but stop where a child prefix takes over" -- is a method
//! on it. The editor used to hold a second copy of each of those in
//! JavaScript; now it holds a `Shape` (through the wasm build of this crate)
//! and asks.
//!
//! **Schemas shadow; they never merge.** The most specific prefix covering a
//! path governs it and no other contributes, so with `homelab` and
//! `homelab.docker` both declared, `homelab`'s own `properties.docker` is
//! never consulted for anything under `homelab.docker`. That is the opposite
//! of how permissions nest ([`crate::scopes::Effective`] accumulates by
//! containment) and the difference is the point: shape has one owner,
//! access is a union. It is also why [`Shape::governing`] uses plain
//! containment ([`Path::is_prefix_of`]) and deliberately not
//! [`crate::scopes::covers`], which aliases the sibling comment key `p__`:
//! that alias is a *permission* rule (whoever may write `p` may write the
//! note about `p`), and with prefixes `a` and `a__` both declared, `covers`
//! would have handed `a__`'s subtree to `a`.
//!
//! A registry document (a prefix or permission file) is shaped by its
//! meta-schema instead, rooted at the document itself ([`Shape::rooted`]):
//! the root prefix is a prefix of every path and the least specific of all,
//! so it governs everything without a special case anywhere below.

use serde::{Deserialize, Serialize};

use crate::model::{self, Value};
use crate::path::Path;
use crate::registry::{by_specificity, PrefixDef, Selector};

/// One declared prefix as a [`Shape`] sees it: where it sits, whom it
/// reaches, and what it says. A [`PrefixDef`] minus where the file came
/// from, which shape does not care about.
#[derive(Debug, Clone, PartialEq)]
pub struct Declared {
    pub prefix: Path,
    pub selector: Selector,
    pub description: Option<String>,
    pub schema: Option<Value>,
}

impl From<&PrefixDef> for Declared {
    fn from(p: &PrefixDef) -> Self {
        Declared {
            prefix: p.prefix.clone(),
            selector: p.selector.clone(),
            description: p.description.clone(),
            schema: p.schema.clone(),
        }
    }
}

/// One thing a schema says is wrong with a value, at a document path.
/// Advisory: the server's one lint ([`model::lint`]) decides what is
/// storable, a schema only describes what was meant (`docs/DESIGN.md` §4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub path: Path,
    pub msg: String,
}

/// A `format:` the schema asks for on a string, which this crate cannot
/// judge: a format is a `PVE::JSONSchema` format name, and the editor checks
/// it with proxmoxlib's own validator for that name (`docs/DESIGN.md` §8)
/// rather than a third implementation of what `ipv4` means. [`Shape::findings`]
/// carries these out, in place, for whoever holds such a validator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormatCheck {
    pub path: Path,
    pub format: String,
    pub value: String,
}

/// One entry of what [`Shape::findings`] reports: a finding, or a format
/// check it could not decide. Both in the one list so there is one ordering
/// -- by path, decided here -- and a caller that resolves the format checks
/// does so in place instead of sorting a second time by its own rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Report {
    Finding(Finding),
    Format(FormatCheck),
}

impl Report {
    pub fn path(&self) -> &Path {
        match self {
            Report::Finding(f) => &f.path,
            Report::Format(f) => &f.path,
        }
    }

    /// The finding, if this is one.
    pub fn finding(&self) -> Option<&Finding> {
        match self {
            Report::Finding(f) => Some(f),
            Report::Format(_) => None,
        }
    }
}

/// The prefixes that reach one document, most-specific first.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Shape {
    prefixes: Vec<Declared>,
}

impl Shape {
    /// The prefixes among `declared` whose selector matches a guest carrying
    /// `tags`, sorted most-specific first. Anything that did not parse never
    /// gets this far -- a malformed prefix file describes nothing
    /// (`docs/DESIGN.md` §3.3) -- so the caller drops those before building.
    pub fn new(declared: impl IntoIterator<Item = Declared>, tags: &[String]) -> Shape {
        let mut prefixes: Vec<Declared> = declared
            .into_iter()
            .filter(|d| d.selector.matches(tags))
            .collect();
        prefixes.sort_by(|a, b| by_specificity(&a.prefix, &b.prefix));
        Shape { prefixes }
    }

    /// [`Shape::new`] over the registry's own definitions.
    pub fn of_guest(defs: &[PrefixDef], tags: &[String]) -> Shape {
        Shape::new(defs.iter().map(Declared::from), tags)
    }

    /// A shape with one schema rooted at the document itself, which is how a
    /// registry document is described by its meta-schema
    /// ([`crate::metaschema`], `docs/DESIGN.md` §3.6).
    pub fn rooted(schema: Value) -> Shape {
        Shape {
            prefixes: vec![Declared {
                prefix: Path::root(),
                selector: Selector::All,
                description: None,
                schema: Some(schema),
            }],
        }
    }

    /// A document nothing describes: a guest no prefix reaches.
    pub fn empty() -> Shape {
        Shape::default()
    }

    /// The prefixes that reach this document, most-specific first.
    pub fn prefixes(&self) -> &[Declared] {
        &self.prefixes
    }

    /// The prefix governing `path`: the most specific one that contains it.
    /// `None` when no prefix reaches the path, which for the document root
    /// is always the case unless the shape is [`Shape::rooted`].
    pub fn governing(&self, path: &Path) -> Option<&Declared> {
        self.prefixes.iter().find(|d| d.prefix.is_prefix_of(path))
    }

    /// The schema node describing `path`, if the governing prefix declares
    /// one: its own schema for the prefix itself, and `properties` walked
    /// down from there. Never reaches into a schema a more specific prefix
    /// shadows, because [`Shape::governing`] has already chosen.
    pub fn schema_at(&self, path: &Path) -> Option<&Value> {
        let owner = self.governing(path)?;
        let mut node = owner.schema.as_ref()?;
        for seg in &path.segments()[owner.prefix.segments().len()..] {
            node = node.get("properties")?.get(seg)?;
        }
        Some(node)
    }

    /// Every path a schema describes, with its schema node: what the
    /// editor's row builder and its hover index walk. Each prefix's tree is
    /// pruned where another prefix governs, so a path covered by two of them
    /// appears once, under the more specific one.
    pub fn schema_index(&self) -> Vec<(Path, &Value)> {
        let mut out = Vec::new();
        for owner in &self.prefixes {
            if let Some(schema) = &owner.schema {
                self.collect(owner, schema, owner.prefix.clone(), &mut out);
            }
        }
        out
    }

    /// Pre-order: a path comes before anything under it, so a caller
    /// building a tree from the index meets every parent first.
    fn collect<'a>(
        &self,
        owner: &Declared,
        schema: &'a Value,
        path: Path,
        out: &mut Vec<(Path, &'a Value)>,
    ) {
        if self.governing(&path).map(|d| &d.prefix) != Some(&owner.prefix) {
            return;
        }
        out.push((path.clone(), schema));
        if let Some(props) = schema.get("properties").and_then(Value::as_object) {
            for (k, sub) in props {
                self.collect(owner, sub, path.join(k.clone()), out);
            }
        }
    }

    /// Everything in `doc` that does not match what its governing schema
    /// says, plus the `format:` checks this crate leaves to a validator that
    /// knows PVE's formats -- one list, sorted by path (segment-wise, as
    /// [`Path`] orders). Only what a row editor also enforces -- type, enum,
    /// minimum/maximum, format -- so the marker and the field never disagree
    /// about the same value.
    pub fn findings(&self, doc: &Value) -> Vec<Report> {
        let mut out = Vec::new();
        for owner in &self.prefixes {
            let Some(schema) = &owner.schema else { continue };
            let Some(value) = model::get_path(doc, &owner.prefix) else { continue };
            self.walk(owner, schema, value, owner.prefix.clone(), &mut out);
        }
        out.sort_by(|a, b| a.path().cmp(b.path()));
        out
    }

    fn walk(&self, owner: &Declared, schema: &Value, value: &Value, path: Path, out: &mut Vec<Report>) {
        if let Some(msg) = check_value(schema, value) {
            out.push(Report::Finding(Finding { path, msg }));
            return; // a value of the wrong shape says nothing useful about its children
        }
        if let (Value::String(s), Some(format)) = (value, schema.get("format").and_then(Value::as_str))
        {
            out.push(Report::Format(FormatCheck {
                path: path.clone(),
                format: format.to_string(),
                value: s.clone(),
            }));
        }
        let (Some(props), Value::Object(map)) =
            (schema.get("properties").and_then(Value::as_object), value)
        else {
            return;
        };
        for (key, child) in map {
            let Some(sub) = props.get(key) else { continue };
            let child_path = path.join(key.clone());
            if self.governing(&child_path).map(|d| &d.prefix) != Some(&owner.prefix) {
                continue; // a more specific prefix owns this subtree
            }
            self.walk(owner, sub, child, child_path, out);
        }
    }
}

/// Of the findings a planned document has, the ones an edit is answerable
/// for: those the stored document did not already have, plus any on a path
/// the edit changed -- in either direction, so writing a differently-wrong
/// value onto an already-wrong key still warns, and replacing `homelab`
/// answers for a finding beneath it. Editing something else in the same
/// document does not: a tick you pass every time is a tick you stop
/// reading (`docs/DESIGN.md` §8, "And only for what this edit did").
pub fn introduced(before: &[Finding], after: &[Finding], changed: &[Path]) -> Vec<Finding> {
    let touched = |path: &Path| {
        changed
            .iter()
            .any(|c| c.is_prefix_of(path) || path.is_prefix_of(c))
    };
    after
        .iter()
        .filter(|f| !before.contains(f) || touched(&f.path))
        .cloned()
        .collect()
}

/// A scalar as the string a schema's `enum` is compared against and a hover
/// shows: a string as itself, anything else as its JSON.
pub fn scalar_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Why `value` does not fit `schema`, in the words the row marker uses.
fn check_value(schema: &Value, value: &Value) -> Option<String> {
    if let Some(allowed) = schema.get("enum").and_then(Value::as_array) {
        let allowed: Vec<String> = allowed.iter().map(scalar_text).collect();
        let shown = scalar_text(value);
        return (!allowed.contains(&shown)).then(|| format!("expected one of: {}", allowed.join(", ")));
    }
    if let Some(declared) = schema.get("type").and_then(Value::as_str) {
        if !type_matches(declared, value) {
            return Some(format!("expected {declared}"));
        }
    }
    if let Some(n) = value.as_f64() {
        if let Some(min) = schema.get("minimum").and_then(Value::as_f64) {
            if n < min {
                return Some(format!("must be at least {}", schema["minimum"]));
            }
        }
        if let Some(max) = schema.get("maximum").and_then(Value::as_f64) {
            if n > max {
                return Some(format!("must be at most {}", schema["maximum"]));
            }
        }
    }
    None
}

/// Whether `value` is of the declared `PVE::JSONSchema` type. A boolean
/// stored as `1`/`0` passes: it is the API's own wire convention
/// (`docs/DESIGN.md` §4), and a document written through `format=json` holds
/// exactly that. An unknown type constrains nothing.
fn type_matches(declared: &str, value: &Value) -> bool {
    match declared {
        "string" => value.is_string(),
        "integer" => value.is_i64() || value.is_u64(),
        "number" => value.is_number(),
        "boolean" => {
            value.is_boolean() || matches!(value.as_i64(), Some(0) | Some(1))
        }
        "object" => value.is_object(),
        "array" => value.is_array(),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    fn p(s: &str) -> Path {
        Path::parse(s).unwrap()
    }

    fn tags(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn decl(prefix: &str, selector: Selector, schema: Option<Value>) -> Declared {
        Declared { prefix: p(prefix), selector, description: None, schema }
    }

    fn all(prefix: &str) -> Declared {
        decl(prefix, Selector::All, None)
    }

    fn tagged(prefix: &str, tag: &str) -> Declared {
        decl(prefix, Selector::Tag(tag.into()), None)
    }

    /// The schema-shadowing rule, one case per line. This table used to be
    /// `testdata/governing-cases.json`, read by this crate and by the
    /// editor's JavaScript suite, because both had an implementation to keep
    /// honest. The editor now asks this one, so the table lives with it.
    ///
    /// Each case hands over the declared prefixes in arbitrary order with
    /// their selectors, so the whole chain runs -- selector, sort, containment
    /// -- and not just the last predicate.
    /// (declared prefixes, the guest's tags, the path, who governs it, why)
    type Case = (Vec<Declared>, Vec<&'static str>, &'static str, Option<&'static str>, &'static str);

    #[test]
    fn governing_is_the_most_specific_prefix_whose_selector_matches() {
        let cases: Vec<Case> = vec![
            (vec![all("homelab"), all("homelab.docker")], vec![], "homelab.docker.compose", Some("homelab.docker"),
             "most specific wins; the parent's own `properties.docker` is shadowed, never merged"),
            (vec![all("homelab.docker"), all("homelab")], vec![], "homelab.notes", Some("homelab"),
             "a sibling of the nested prefix falls back to the parent"),
            (vec![all("homelab")], vec![], "homelab", Some("homelab"), "the prefix governs its own key"),
            (vec![all("homelab")], vec![], "netbird.groups", None, "no prefix covers it, so nothing describes its shape"),
            (vec![all("homelab")], vec![], "", None, "the document root is above every prefix, not under one"),
            (vec![all("homelab")], vec![], "homelabx.key", None, "a longer name is not a child -- the separator is what makes one"),
            (vec![all("homelab"), all("homelab.docker")], vec![], "homelab.docker", Some("homelab.docker"),
             "the child's own key is the child's, not the parent's"),
            (vec![all("a"), all("a__")], vec![], "a__.note", Some("a__"),
             "plain containment: `a__` is its own prefix here, not `a`'s comment key -- that alias belongs to permissions"),
            (vec![all("a")], vec![], "a__", None,
             "... and without `a__` declared, `a` does not reach the comment key either"),
            (vec![tagged("traefik", "traefik"), all("homelab")], vec![], "traefik.spec", None,
             "a prefix whose selector does not match the guest reaches nothing"),
            (vec![tagged("traefik", "traefik"), all("homelab")], vec!["traefik"], "traefik.spec", Some("traefik"),
             "... and reaches it once the guest carries the tag"),
            (vec![tagged("homelab.docker", "docker"), all("homelab")], vec![], "homelab.docker.compose", Some("homelab"),
             "a more specific prefix that does not reach this guest does not shadow: the parent governs"),
            (vec![tagged("homelab.docker", "docker"), all("homelab")], vec!["docker"], "homelab.docker.compose", Some("homelab.docker"),
             "... until it does"),
            (vec![tagged("traefik", "traefik"), tagged("traefik.spec", "web")], vec!["traefik"], "traefik.spec.host", Some("traefik"),
             "two independently tagged levels: the more specific prefix's selector missed, so the one that does reach this guest governs -- shadowing is decided among the prefixes that apply, not among all of them"),
            (vec![tagged("traefik", "traefik"), tagged("traefik.spec", "web")], vec!["traefik", "web"], "traefik.spec.host", Some("traefik.spec"),
             "... and once its selector matches too, the more specific one takes over"),
            (vec![tagged("traefik", "traefik")], vec![], "traefik.spec", None,
             "an untagged guest is not reached by a tag selector at all"),
            (vec![all("a"), all("a__")], vec![], "a__", Some("a__"),
             "the comment-key prefix queried directly: its own, not `a`'s -- `covers` would have said `a`"),
            (vec![all("a")], vec![], "a.b__", Some("a"),
             "a comment key *inside* the subtree is part of it, on both rules"),
            (vec![all("b"), all("a"), all("a.b.c"), all("a.b")], vec![], "a.b.c.d", Some("a.b.c"),
             "three deep, given in a scrambled order"),
            (vec![all("x.y.z"), all("x"), all("x.y")], vec![], "x.y.other", Some("x.y"),
             "... and the answer changes with the path, not with the input order"),
        ];
        for (declared, guest_tags, path, want, why) in cases {
            let shape = Shape::new(declared, &tags(&guest_tags));
            let got = shape.governing(&p(path)).map(|d| d.prefix.to_string());
            assert_eq!(got.as_deref(), want, "governing({path:?}) with tags {guest_tags:?}: {why}");
        }
    }

    #[test]
    fn prefixes_are_sorted_most_specific_first_then_by_name() {
        let shape = Shape::new(
            vec![all("zeta"), all("alpha.b"), all("alpha"), all("beta.a")],
            &[],
        );
        let order: Vec<String> = shape.prefixes().iter().map(|d| d.prefix.to_string()).collect();
        assert_eq!(order, ["alpha.b", "beta.a", "alpha", "zeta"]);
    }

    #[test]
    fn a_rooted_shape_governs_everything_including_the_root() {
        let shape = Shape::rooted(json!({"type": "object", "properties": {"authid": {"type": "string"}}}));
        assert_eq!(shape.governing(&Path::root()).unwrap().prefix, Path::root());
        assert_eq!(shape.governing(&p("rules.0")).unwrap().prefix, Path::root());
        assert_eq!(shape.schema_at(&p("authid")), Some(&json!({"type": "string"})));
        assert_eq!(shape.schema_at(&p("nope")), None);
    }

    #[test]
    fn schema_at_walks_properties_from_the_governing_prefix_and_never_across_one() {
        let parent = json!({"type": "object", "properties": {
            "notes": {"type": "string"},
            "docker": {"type": "object", "properties": {"compose": {"type": "integer"}}},
        }});
        let child = json!({"type": "object", "properties": {"compose": {"type": "string"}}});
        let shape = Shape::new(
            vec![
                decl("homelab", Selector::All, Some(parent)),
                decl("homelab.docker", Selector::All, Some(child)),
            ],
            &[],
        );
        assert_eq!(shape.schema_at(&p("homelab.notes")), Some(&json!({"type": "string"})));
        // The parent declared `docker.compose` too; the child's wins, whole.
        assert_eq!(shape.schema_at(&p("homelab.docker.compose")), Some(&json!({"type": "string"})));
        assert_eq!(shape.schema_at(&p("homelab.other")), None);

        let index: Vec<String> = shape.schema_index().iter().map(|(p, _)| p.to_string()).collect();
        let mut sorted = index.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), index.len(), "every path once: {index:?}");
        assert!(index.contains(&"homelab.docker.compose".to_string()));
        assert!(index.contains(&"homelab.notes".to_string()));
        assert!(!index.iter().any(|p| p.starts_with("homelab.docker") && p != "homelab.docker" && p != "homelab.docker.compose"));
        // `homelab.docker` itself appears once, under the child (whose schema it is).
        let docker = shape.schema_index().into_iter().find(|(p, _)| p.to_string() == "homelab.docker").unwrap();
        assert_eq!(docker.1["properties"]["compose"]["type"], "string");
    }

    #[test]
    fn findings_check_only_what_the_row_editor_enforces() {
        let schema = json!({"type": "object", "properties": {
            "port": {"type": "integer", "minimum": 1, "maximum": 65535},
            "host": {"type": "string", "format": "dns-name"},
            "mode": {"enum": ["a", "b"]},
            "on": {"type": "boolean"},
            "sub": {"type": "object", "properties": {"x": {"type": "number"}}},
        }});
        let shape = Shape::new(vec![decl("t", Selector::All, Some(schema))], &[]);

        let doc = json!({"t": {"port": 70000, "host": "web.example", "mode": "c", "on": 1, "sub": "not a map"}});
        let got = shape.findings(&doc);
        let shown: Vec<String> = got
            .iter()
            .map(|r| match r {
                Report::Finding(f) => format!("{}: {}", f.path, f.msg),
                Report::Format(f) => format!("{}: {}? {}", f.path, f.format, f.value),
            })
            .collect();
        // One list, one order: the format check sits among the findings at its
        // path, and `on: 1` passed (a boolean as 1/0 is the wire convention).
        assert_eq!(
            shown,
            [
                "t.host: dns-name? web.example",
                "t.mode: expected one of: a, b",
                "t.port: must be at most 65535",
                "t.sub: expected object",
            ]
        );

        let fine = json!({"t": {"port": 80, "host": "x", "mode": "a", "on": true, "sub": {"x": 1.5}}});
        assert!(shape.findings(&fine).iter().all(|r| r.finding().is_none()));
        // A wrong-shaped container says nothing about its children.
        let wrong = json!({"t": {"port": "eighty", "sub": {"x": "one"}}});
        let paths: Vec<String> = shape.findings(&wrong).iter().map(|r| r.path().to_string()).collect();
        assert_eq!(paths, ["t.port", "t.sub.x"]);
        // An absent prefix has no findings; a document nothing describes has none.
        assert!(shape.findings(&json!({})).is_empty());
        assert!(Shape::empty().findings(&doc).is_empty());

        // The order is `Path`'s, segment by segment -- `a.b` before `a-c`, because
        // `a` is a shorter segment than `a-c` -- not the string order a `.` and a
        // `-` would give. There is exactly one sort, here.
        let flat = json!({"type": "object", "properties": {"a-c": {"type": "integer"}, "a": {"type": "object", "properties": {"b": {"type": "integer"}}}}});
        let shape = Shape::new(vec![decl("t", Selector::All, Some(flat))], &[]);
        let paths: Vec<String> = shape.findings(&json!({"t": {"a-c": "x", "a": {"b": "y"}}})).iter().map(|r| r.path().to_string()).collect();
        assert_eq!(paths, ["t.a.b", "t.a-c"]);
        assert!(serde_json::to_string(&shape.findings(&json!({"t": {"a-c": "x"}}))).unwrap().starts_with(r#"[{"path":"t.a-c","msg":"#));
    }

    #[test]
    fn an_edit_answers_for_what_it_introduced_or_touched() {
        let f = |path: &str, msg: &str| Finding { path: p(path), msg: msg.into() };
        let before = vec![f("a.old", "expected integer"), f("b", "expected string")];
        let after = vec![
            f("a.old", "expected integer"), // pre-existing, untouched: not ours
            f("b", "expected boolean"),     // a differently-wrong value: ours
            f("c", "expected string"),      // new: ours
        ];
        let changed = vec![p("b"), p("c")];
        let got: Vec<String> = introduced(&before, &after, &changed).iter().map(|x| x.path.to_string()).collect();
        assert_eq!(got, ["b", "c"]);
        // Replacing a parent answers for a finding beneath it, and vice versa.
        assert_eq!(introduced(&before, &before[..1], &[p("a")]).len(), 1);
        assert_eq!(introduced(&before, &before[..1], &[p("a.old.deeper")]).len(), 1);
        // A pure reordering changes nothing, so it introduces nothing.
        assert!(introduced(&before, &before, &[]).is_empty());
    }

    #[test]
    fn findings_stop_where_a_more_specific_prefix_governs() {
        // The parent says `docker.compose` is an integer; the child says a
        // string. The value is a string: the child is right and the parent's
        // opinion never reaches the path. Schemas shadow, they do not merge.
        let parent = json!({"type": "object", "properties": {
            "docker": {"type": "object", "properties": {"compose": {"type": "integer"}}},
        }});
        let child = json!({"type": "object", "properties": {"compose": {"type": "string"}}});
        let doc = json!({"homelab": {"docker": {"compose": "services: {}"}}});

        let both = Shape::new(
            vec![decl("homelab", Selector::All, Some(parent.clone())), decl("homelab.docker", Selector::All, Some(child))],
            &[],
        );
        assert!(both.findings(&doc).is_empty());

        // A schema-less child prefix still shadows: it governs its subtree and
        // says nothing about it, which is not the same as letting the parent say something.
        let silent_child = Shape::new(
            vec![decl("homelab", Selector::All, Some(parent.clone())), decl("homelab.docker", Selector::All, None)],
            &[],
        );
        assert!(silent_child.findings(&doc).is_empty());

        // Without the child, the parent's opinion counts.
        let parent_only = Shape::new(vec![decl("homelab", Selector::All, Some(parent))], &[]);
        assert_eq!(parent_only.findings(&doc).len(), 1);
        assert_eq!(parent_only.findings(&doc)[0].path().to_string(), "homelab.docker.compose");
    }
}
