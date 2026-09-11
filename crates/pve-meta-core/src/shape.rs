//! The shape of one document: which declared prefixes reach it, which of
//! them governs a given path, and what that prefix's schema says about the
//! value there (`docs/DESIGN.md` §3).
//!
//! A [`Shape`] is built once per document from the prefix registry and the
//! guest's tags. Every prefix rule -- does this selector match, sort
//! most-specific first, which prefix governs, walk the schema but stop
//! where a child prefix takes over -- is a method on it, and the editor
//! consults that one `Shape` (through the wasm build of this crate) instead
//! of reimplementing any of them.
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
    /// The server refuses a write that leaves this subtree not matching
    /// `schema` (`crate::registry::PrefixDef::enforce`). The root default for
    /// the per-node `enforce`, which is inherited from here down.
    pub enforce: bool,
    /// The root default for the per-node `hidden`: with it set, this prefix
    /// offers no declared-but-unset rows below its own unless a node asks to
    /// be shown. For a vocabulary large enough that the useful default is
    /// "show what is set, and nothing else". The prefix's own row stays: it is
    /// the declaration that something lives there.
    pub hidden: bool,
}

impl From<&PrefixDef> for Declared {
    fn from(p: &PrefixDef) -> Self {
        Declared {
            prefix: p.prefix.clone(),
            selector: p.selector.clone(),
            description: p.description.clone(),
            schema: p.schema.clone(),
            enforce: p.enforce,
            hidden: p.hidden,
        }
    }
}

/// One path a schema describes: where it is, what it says, and whether the
/// editor should offer it as a row before anything is stored there.
///
/// `hidden` is an editor hint and nothing else, like `multiline` and `format`.
/// It never reaches [`Shape::findings`] or enforcement: a hidden key that *is*
/// set is typed, validated and refused exactly as a shown one, because hiding a
/// declaration must never hide data or excuse it from its own schema.
#[derive(Debug, Clone, Serialize)]
pub struct Described<'a> {
    pub path: Path,
    pub schema: &'a Value,
    pub hidden: bool,
}

/// One thing a schema says is wrong with a value, at a document path.
/// Advisory unless `enforced`: the server's one lint ([`model::lint`])
/// decides what is storable, a schema only describes what was meant
/// (`docs/DESIGN.md` §7) -- except where its prefix says `enforce: true`,
/// and then a write that introduces such a finding is refused without
/// `force` ([`Shape::enforced_findings`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub path: Path,
    pub msg: String,
    /// The governing prefix declares `enforce: true`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub enforced: bool,
}

/// A `format:` the schema asks for on a string, which this crate cannot
/// judge: a format is a `PVE::JSONSchema` format name, and the editor checks
/// it with proxmoxlib's own validator for that name (`docs/DESIGN.md` §3)
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
    /// (`docs/DESIGN.md` §4) -- so the caller drops those before building.
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
    /// ([`crate::metaschema`], `docs/DESIGN.md` §6).
    ///
    /// Through [`Shape::new`] rather than building the struct, because the editor
    /// constructs exactly this one-entry listing and hands it to the generic path.
    /// Built by hand here, the two were the same only by coincidence: a change to
    /// how `new` treats an empty-path entry would have broken the editor's rooted
    /// shape while this function's own tests carried on passing. Now it must break
    /// both or neither.
    pub fn rooted(schema: Value) -> Shape {
        Shape::new(
            [Declared {
                prefix: Path::root(),
                selector: Selector::All,
                description: None,
                schema: Some(schema),
                enforce: false,
                hidden: false,
            }],
            &[],
        )
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
    pub fn schema_index(&self) -> Vec<Described<'_>> {
        let mut out = Vec::new();
        for owner in &self.prefixes {
            if let Some(schema) = &owner.schema {
                self.collect(owner, schema, owner.prefix.clone(), owner.hidden, &mut out);
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
        inherited_hidden: bool,
        out: &mut Vec<Described<'a>>,
    ) {
        if self.governing(&path).map(|d| &d.prefix) != Some(&owner.prefix) {
            return;
        }
        // Inherited, with an explicit setting winning at any depth. Hiding a
        // subtree is the common case -- a vocabulary the size of Traefik's is
        // mostly keys nobody sets on a given guest -- and un-hiding one key
        // inside it is how you keep the two or three that matter. The walk
        // always descends, so that override needs no lookahead.
        let hidden = flag(schema, "hidden", inherited_hidden);
        out.push(Described { path: path.clone(), schema, hidden });
        if let Some(props) = schema.get("properties").and_then(Value::as_object) {
            for (k, sub) in props {
                self.collect(owner, sub, path.join(k.clone()), hidden, out);
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
            self.walk(owner, schema, value, owner.prefix.clone(), owner.enforce, &mut out);
        }
        out.sort_by(|a, b| a.path().cmp(b.path()));
        out
    }

    /// The findings a write may be refused for: those under a prefix that
    /// says `enforce: true`. Format checks are never among them, because the
    /// server cannot judge a `format:` (that is proxmoxlib's job in the
    /// editor) and must not refuse on a guess.
    pub fn enforced_findings(&self, doc: &Value) -> Vec<Finding> {
        self.findings(doc)
            .into_iter()
            .filter_map(|r| match r {
                Report::Finding(f) if f.enforced => Some(f),
                _ => None,
            })
            .collect()
    }

    fn walk(
        &self,
        owner: &Declared,
        schema: &Value,
        value: &Value,
        path: Path,
        inherited_enforce: bool,
        out: &mut Vec<Report>,
    ) {
        // Inherited with an explicit setting winning at any depth, the same rule
        // `hidden` follows and the prefix's own `enforce` as the root default.
        //
        // A prefix that enforces almost everything is the case this exists for: a
        // vocabulary with a modelled part worth refusing bad writes into, and a
        // passthrough subtree that by definition has no shape to check. Without a
        // way to say `enforce: false` on that subtree, the escape hatch and the
        // enforcement cannot both exist -- and the escape hatch is what makes a
        // partial schema honest.
        let enforce = flag(schema, "enforce", inherited_enforce);
        if let Some(msg) = check_value(schema, value) {
            out.push(Report::Finding(Finding { path, msg, enforced: enforce }));
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
            self.walk(owner, sub, child, child_path, enforce, out);
        }
    }
}

/// A schema node's `hidden` or `enforce`, or `inherited` when the node does
/// not say. Spelled `true`/`false` or, as this dialect already spells
/// `optional: 1` and `multiline: 1`, as `1`/`0` -- a hand-written
/// `enforce: 0` on a passthrough subtree is exactly the escape hatch these
/// flags exist for, and it must not be read as "not stated" and inherit the
/// `true` it was written to override.
fn flag(schema: &Value, key: &str, inherited: bool) -> bool {
    match schema.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) if n.as_i64() == Some(1) => true,
        Some(Value::Number(n)) if n.as_i64() == Some(0) => false,
        _ => inherited,
    }
}

/// Of the findings a planned document has, the ones an edit is answerable
/// for: those the stored document did not already have, plus any on a path
/// the edit changed -- in either direction, so writing a differently-wrong
/// value onto an already-wrong key still warns, and replacing `homelab`
/// answers for a finding beneath it. Editing something else in the same
/// document does not: a tick you pass every time is a tick you stop
/// reading (`docs/DESIGN.md` §12, "And only for what this edit did").
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
/// (`docs/DESIGN.md` §7), and a document written through `format=json` holds
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
        Declared { prefix: p(prefix), selector, description: None, schema, enforce: false, hidden: false }
    }

    fn all(prefix: &str) -> Declared {
        decl(prefix, Selector::All, None)
    }

    fn tagged(prefix: &str, tag: &str) -> Declared {
        decl(prefix, Selector::Tag(tag.into()), None)
    }

    /// The schema-shadowing rule, one case per line. The editor consults
    /// this same table, through `crates/pve-meta-wasm`, rather than keeping
    /// its own copy.
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

    /// `enforce` follows the same rule, with the prefix's own flag as the root
    /// default -- the case being a vocabulary whose modelled part is worth
    /// refusing bad writes into and whose passthrough subtree has no shape to
    /// check. Without the override those two cannot coexist.
    #[test]
    fn enforce_is_inherited_and_overridden_explicitly() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "port": { "type": "integer" },
                "extra": {
                    "type": "object",
                    "enforce": false,
                    "properties": { "n": { "type": "integer" } },
                },
            },
        });
        let strict = Declared { enforce: true, ..decl("t", Selector::All, Some(schema)) };
        let shape = Shape::new([strict], &tags(&[]));
        let doc = serde_json::json!({ "t": { "port": "no", "extra": { "n": "also no" } } });

        let enforced: Vec<String> =
            shape.enforced_findings(&doc).into_iter().map(|f| f.path.to_string()).collect();
        assert_eq!(enforced, ["t.port"], "the opted-out subtree is not enforced");
        // Still reported, just not refused: advisory is the default everywhere else.
        let all: Vec<String> = shape
            .findings(&doc)
            .into_iter()
            .filter_map(|r| match r {
                Report::Finding(f) => Some(f.path.to_string()),
                Report::Format(_) => None,
            })
            .collect();
        assert!(all.contains(&"t.extra.n".to_string()), "{all:?}");
    }

    /// The flags are read as `true`/`false` and as the file's own `1`/`0`
    /// idiom alike; anything else is "not stated" and inherits.
    #[test]
    fn a_node_flag_may_be_spelled_one_or_zero() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "a": { "type": "integer", "enforce": 0 },
                "b": { "type": "integer", "enforce": 1 },
                "c": { "type": "integer", "enforce": "yes" },
                "h": { "type": "integer", "hidden": 1 },
            },
        });
        let strict = Declared { enforce: true, ..decl("t", Selector::All, Some(schema)) };
        let shape = Shape::new([strict], &tags(&[]));
        let doc = serde_json::json!({ "t": { "a": "x", "b": "x", "c": "x" } });
        let enforced: Vec<String> =
            shape.enforced_findings(&doc).into_iter().map(|f| f.path.to_string()).collect();
        assert_eq!(enforced, ["t.b", "t.c"], "0 opts out, 1 opts in, anything else inherits");
        assert!(shape.schema_index().iter().find(|d| d.path.to_string() == "t.h").unwrap().hidden);
    }

    /// `hidden` is inherited, and an explicit setting wins at any depth: a
    /// vocabulary is hidden wholesale and the two keys that matter are named.
    #[test]
    fn hidden_is_inherited_and_overridden_explicitly() {
        let shape = Shape::new(
            [decl(
                "t",
                Selector::All,
                Some(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "host": { "type": "string" },
                        "routers": {
                            "type": "object",
                            "hidden": true,
                            "properties": {
                                "rule": { "type": "string" },
                                "entrypoint": { "type": "string", "hidden": false },
                            },
                        },
                    },
                })),
            )],
            &tags(&[]),
        );
        let hidden_at = |path: &str| {
            shape
                .schema_index()
                .into_iter()
                .find(|d| d.path.to_string() == path)
                .unwrap_or_else(|| panic!("{path} is described"))
                .hidden
        };
        assert!(!hidden_at("t"), "the prefix itself is not hidden");
        assert!(!hidden_at("t.host"));
        assert!(hidden_at("t.routers"), "hidden at the subtree's root");
        assert!(hidden_at("t.routers.rule"), "and inherited below it");
        assert!(!hidden_at("t.routers.entrypoint"), "an explicit setting wins");
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

        let index: Vec<String> = shape.schema_index().iter().map(|d| d.path.to_string()).collect();
        let mut sorted = index.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), index.len(), "every path once: {index:?}");
        assert!(index.contains(&"homelab.docker.compose".to_string()));
        assert!(index.contains(&"homelab.notes".to_string()));
        assert!(!index.iter().any(|p| p.starts_with("homelab.docker") && p != "homelab.docker" && p != "homelab.docker.compose"));
        // `homelab.docker` itself appears once, under the child (whose schema it is).
        let docker = shape.schema_index().into_iter().find(|d| d.path.to_string() == "homelab.docker").unwrap();
        assert_eq!(docker.schema["properties"]["compose"]["type"], "string");
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
        let f = |path: &str, msg: &str| Finding { path: p(path), msg: msg.into(), enforced: false };
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
    fn enforced_findings_are_the_enforcing_prefixes_type_enum_and_range_findings_only() {
        let schema = json!({"type": "object", "properties": {
            "port": {"type": "integer"},
            "host": {"type": "string", "format": "dns-name"},
        }});
        let strict = Declared { enforce: true, ..decl("t", Selector::All, Some(schema.clone())) };
        let lax = decl("u", Selector::All, Some(schema));
        let shape = Shape::new(vec![strict, lax], &[]);
        let doc = json!({"t": {"port": "x", "host": "not a host"}, "u": {"port": "x"}});
        let enforced = shape.enforced_findings(&doc);
        assert_eq!(enforced.len(), 1, "{enforced:?}");
        assert_eq!(enforced[0].path.to_string(), "t.port");
        assert!(enforced[0].enforced);
        // The full report still carries everything, flagged.
        let all: Vec<(String, bool)> = shape
            .findings(&doc)
            .iter()
            .filter_map(|r| r.finding().map(|f| (f.path.to_string(), f.enforced)))
            .collect();
        assert_eq!(all, [("t.port".to_string(), true), ("u.port".to_string(), false)]);
        // A rooted (meta-schema) shape never enforces.
        assert!(Shape::rooted(json!({"type": "object", "properties": {"a": {"type": "integer"}}}))
            .enforced_findings(&json!({"a": "x"}))
            .is_empty());
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
