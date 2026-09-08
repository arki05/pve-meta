//! The row model behind the tree (`docs/DESIGN.md` §8).
//!
//! > "rows are the union of the keys present and the keys the applicable grammars declare
//! > (declared-but-unset rows are greyed with default and description and a "set" action).
//! > Columns: key, value …, owner …. Comment keys are shown as the row description, not as
//! > rows. Editability is per row from `/meta/access`."
//!
//! This module is that sentence, and nothing else: it turns a document (`data`, an
//! unordered JSON object), the operator registrations, the guest's tags and the caller's
//! grants into a tree of [`Row`]s. It performs no I/O and knows nothing about pwt, so it
//! is unit-tested natively (`cargo test --lib`) — the wasm-only page in `editor.rs` only
//! pours the result into a `SlabTree` and renders cells from it.
//!
//! Ordering: `data` is unordered on the wire (§4), so siblings sort alphabetically unless
//! a grammar declares an `order` for that map, whose keys come first in that order.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};

use crate::grammar::{
    Operator, OperatorScope, schema_at, schema_default, schema_description, schema_enum,
    schema_order, schema_properties, schema_type,
};
use crate::model::{Access, Mode, is_comment_key};

/// Deepest nesting the tree renders. A document is administrator-authored data, not a
/// hostile input, but the build is recursive and a cycle-free bound costs nothing.
const MAX_DEPTH: usize = 32;

/// What kind of editor a row's value wants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValueKind {
    /// A nested map: not edited as a value, only through its children or as text.
    Map,
    /// A list. One text leaf (`docs/DESIGN.md` §8), edited as JSON.
    Array,
    /// A free string.
    Text,
    Integer,
    Number,
    Boolean,
    /// A string constrained by a grammar's `enum`.
    Enum(Vec<String>),
}

impl ValueKind {
    /// True if this row has children rather than a value of its own.
    pub fn is_map(&self) -> bool {
        matches!(self, ValueKind::Map)
    }
}

/// The registration a row belongs to: the scope covering it, and the selector that made
/// that scope apply to this guest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Owner {
    /// Registration name (the drop-in file's name).
    pub name: String,
    /// The principal it registers.
    pub authid: String,
    /// The covering scope's prefix.
    pub prefix: String,
    /// What that scope grants its principal.
    pub mode: Mode,
    /// `all`, `tag: traefik`, … — why it applies here.
    pub selector: Option<String>,
}

impl Owner {
    /// `traefik (tag: traefik)` — the owner column's text.
    pub fn label(&self) -> String {
        match &self.selector {
            Some(selector) => format!("{} ({selector})", self.name),
            None => self.name.clone(),
        }
    }

    /// The tooltip: who it is and what the scope grants.
    pub fn detail(&self) -> String {
        format!("{} — {} ({})", self.authid, self.prefix, self.mode.as_str())
    }
}

/// One row of the tree, as stored in the `TreeStore`.
///
/// Children live in the tree structure, not here, so this is the flat record the
/// `DataTable` renders and compares.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    /// Full dotted key path — also the view a write to this row addresses, and the row's
    /// unique key in the store. Empty for the invisible root.
    pub path: String,
    /// Last path segment, shown in the Key column.
    pub key: String,
    pub kind: ValueKind,
    /// The value in the document, or `None` for a row only a grammar declares.
    pub value: Option<Value>,
    /// What the row's note says, shown under the key: the sibling comment key
    /// (`key__`), the map's own `__`, or — when the document carries neither — the
    /// grammar's `description`.
    pub description: Option<String>,
    /// The note the *document* carries, if any. Deliberately not `description`: a
    /// grammar's description is documentation about the key, not data in this document,
    /// and pre-filling an edit dialog with it would write the grammar's own prose into a
    /// comment key on the first save.
    pub note: Option<String>,
    /// Where that note lives (or would live): the sibling `key__`, or `key.__` for a map
    /// documented by its own bare `__` (`docs/DESIGN.md` §2).
    pub note_path: String,
    /// The grammar's `default`, shown on an unset row.
    pub default: Option<Value>,
    /// The registration whose scope covers this row.
    pub owner: Option<Owner>,
    /// Whether `/meta/access` says this row may be written.
    pub writable: bool,
    /// Whether a grammar declares this key.
    pub declared: bool,
}

impl Node {
    /// The invisible root of the store's tree.
    pub fn root() -> Self {
        Self {
            path: String::new(),
            key: String::new(),
            kind: ValueKind::Map,
            value: None,
            description: None,
            note: None,
            note_path: "__".to_string(),
            default: None,
            owner: None,
            writable: false,
            declared: false,
        }
    }

    /// True if the document actually carries this key.
    pub fn is_set(&self) -> bool {
        self.value.is_some()
    }

    /// The Value column's text for a row that is set. Maps show nothing — their content is
    /// their children.
    pub fn display_value(&self) -> String {
        match &self.value {
            None => String::new(),
            Some(Value::Object(_)) => String::new(),
            Some(value) => render_scalar(value),
        }
    }

    /// The Value column's placeholder for a declared-but-unset row.
    pub fn default_text(&self) -> Option<String> {
        self.default.as_ref().map(render_scalar)
    }

    /// The comment key that documents this row (`traefik.host` → `traefik.host__`).
    pub fn comment_path(&self) -> &str {
        &self.note_path
    }
}

/// A node and its children.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub node: Node,
    pub children: Vec<Row>,
}

/// Everything the build needs besides the document itself.
pub struct BuildContext<'a> {
    /// The caller's grants for this document, deciding per-row editability.
    pub access: &'a Access,
    /// All registrations (`GET /meta/operators`); empty when the endpoint is unavailable.
    pub operators: &'a [Operator],
    /// The guest's PVE tags, for the scope selectors.
    pub tags: &'a [String],
    /// False for the datacenter document, where scopes do not apply at all (§3).
    pub scoped: bool,
}

/// One registration scope that applies to this document.
struct Applicable<'a> {
    operator: &'a Operator,
    scope: &'a OperatorScope,
}

/// The note about the document as a whole: its bare `__` key (`docs/DESIGN.md` §2).
pub fn document_description(data: &Value) -> Option<String> {
    data.get("__")?.as_str().map(str::to_string)
}

/// Build the tree's top-level rows.
pub fn build(data: &Value, ctx: &BuildContext) -> Vec<Row> {
    let applicable: Vec<Applicable> = match ctx.scoped {
        false => Vec::new(),
        true => ctx
            .operators
            .iter()
            .flat_map(|operator| {
                operator
                    .scopes
                    .iter()
                    .filter(|scope| scope.selector.matches(ctx.tags))
                    .map(move |scope| Applicable { operator, scope })
            })
            .collect(),
    };

    build_map("", data.as_object(), ctx, &applicable, 0)
}

/// Find one row by its path.
pub fn find<'a>(rows: &'a [Row], path: &str) -> Option<&'a Node> {
    for row in rows {
        if row.node.path == path {
            return Some(&row.node);
        }
        if let Some(rest) = under(&row.node.path, path) {
            if !rest.is_empty() {
                if let Some(found) = find(&row.children, path) {
                    return Some(found);
                }
            }
        }
    }
    None
}

/// Every node, depth first — the flat order the table shows when all rows are expanded.
pub fn flatten(rows: &[Row]) -> Vec<&Node> {
    let mut out = Vec::new();
    fn walk<'a>(rows: &'a [Row], out: &mut Vec<&'a Node>) {
        for row in rows {
            out.push(&row.node);
            walk(&row.children, out);
        }
    }
    walk(rows, &mut out);
    out
}

fn build_map(
    path: &str,
    obj: Option<&Map<String, Value>>,
    ctx: &BuildContext,
    applicable: &[Applicable],
    depth: usize,
) -> Vec<Row> {
    if depth >= MAX_DEPTH {
        return Vec::new();
    }

    let declared = declared_children(path, applicable);

    let mut keys: BTreeSet<String> = BTreeSet::new();
    if let Some(obj) = obj {
        for key in obj.keys() {
            if !is_comment_key(key) {
                keys.insert(key.clone());
            }
        }
    }
    keys.extend(declared.schemas.keys().cloned());

    // A note whose subject is neither present nor declared would otherwise be invisible —
    // comment keys are ordinary data (§2) and the tree is the only view of the document.
    // Surface the subject as an unset row so the note has somewhere to live.
    if let Some(obj) = obj {
        for key in obj.keys() {
            if key == "__" || !is_comment_key(key) {
                continue;
            }
            let subject = &key[..key.len() - 2];
            if !subject.is_empty() {
                keys.insert(subject.to_string());
            }
        }
    }

    let mut sorted: Vec<String> = keys.into_iter().collect();
    order_keys(&mut sorted, &declared.order)
        .into_iter()
        .map(|key| {
            let child_path = join(path, &key);
            let value = obj.and_then(|o| o.get(&key));
            let schema = declared.schemas.get(&key).and_then(Option::as_ref);
            let mut kind = kind_of(value, schema);
            // A scope prefix reaching *through* this key says it is a map, even when no
            // grammar spells that out and the document has never carried it — otherwise
            // the row a registration claims would be an unset leaf with the rest of its
            // declared subtree nowhere to go.
            if value.is_none() && declared.maps.contains(&key) {
                kind = ValueKind::Map;
            }

            let children = match kind.is_map() {
                true => build_map(
                    &child_path,
                    value.and_then(Value::as_object),
                    ctx,
                    applicable,
                    depth + 1,
                ),
                false => Vec::new(),
            };

            let (note, note_path) = note_of(obj, &key, value, &child_path);
            let description = note.clone().or_else(|| schema.and_then(schema_description));

            Row {
                node: Node {
                    description,
                    note,
                    note_path,
                    default: schema.and_then(schema_default).cloned(),
                    owner: owner_for(&child_path, applicable),
                    writable: ctx.access.may_write(&child_path),
                    declared: declared.schemas.contains_key(&key),
                    value: value.cloned(),
                    kind,
                    key,
                    path: child_path,
                },
                children,
            }
        })
        .collect()
}

/// The keys a grammar declares directly under `path`, plus the order it wants them in.
struct Declared {
    /// Key → its schema, if the grammar spelled one out.
    schemas: BTreeMap<String, Option<Value>>,
    /// Keys a scope prefix runs through or ends at without a grammar: those are maps
    /// (a view prefix addresses through maps only, `docs/DESIGN.md` §2).
    maps: BTreeSet<String>,
    order: Vec<String>,
}

fn declared_children(path: &str, applicable: &[Applicable]) -> Declared {
    let mut schemas: BTreeMap<String, Option<Value>> = BTreeMap::new();
    let mut maps: BTreeSet<String> = BTreeSet::new();
    let mut order: Vec<String> = Vec::new();

    for entry in applicable {
        if let Some(relative) = under(&entry.scope.prefix, path) {
            // `path` is at or below this scope's prefix: read the grammar node there.
            let Some(grammar) = &entry.scope.grammar else {
                continue;
            };
            let Some(node) = schema_at(grammar, relative) else {
                continue;
            };
            for key in schema_order(node) {
                if !order.contains(&key) {
                    order.push(key);
                }
            }
            if let Some(properties) = schema_properties(node) {
                for (key, schema) in properties {
                    if is_comment_key(key) {
                        continue;
                    }
                    match schemas.entry(key.clone()) {
                        std::collections::btree_map::Entry::Occupied(mut existing) => {
                            match existing.get_mut() {
                                Some(kept) => merge_schema(kept, schema),
                                empty => *empty = Some(schema.clone()),
                            }
                        }
                        std::collections::btree_map::Entry::Vacant(slot) => {
                            slot.insert(Some(schema.clone()));
                        }
                    }
                }
            }
        } else if let Some(rest) = under(path, &entry.scope.prefix) {
            // The scope's prefix lies below `path`: the operator claims that subtree, so
            // its first segment is a row even in a document that has never had it.
            let Some(segment) = rest.split('.').next() else {
                continue;
            };
            if segment.is_empty() {
                continue;
            }
            // Only when the prefix *ends* here does the grammar describe this very key.
            let schema = match rest.contains('.') {
                true => None,
                false => entry.scope.grammar.clone(),
            };
            if schema.is_none() {
                maps.insert(segment.to_string());
            }
            schemas
                .entry(segment.to_string())
                .and_modify(|existing| {
                    if existing.is_none() {
                        existing.clone_from(&schema);
                    }
                })
                .or_insert(schema);
        }
    }

    Declared {
        schemas,
        maps,
        order,
    }
}

/// Fill the gaps in `kept` from `other`.
///
/// Two registrations may declare the same key — the same operator packaged and dropped in,
/// or two operators sharing a prefix — and there is no reason one of them should have to
/// carry every field. The first schema seen wins field by field; anything it leaves out
/// (a `description`, a `default`, an `enum`) is taken from the next one that has it, so a
/// row shows everything the cluster knows about it. `properties` is deliberately included:
/// the recursion re-collects the next level from *all* grammars anyway, so a merged
/// `properties` here is only a starting point, never the whole answer.
fn merge_schema(kept: &mut Value, other: &Value) {
    let (Some(kept), Some(other)) = (kept.as_object_mut(), other.as_object()) else {
        return;
    };
    for (key, value) in other {
        kept.entry(key.clone()).or_insert_with(|| value.clone());
    }
}

/// Grammar-declared keys first, in the declared order; everything else alphabetically.
fn order_keys(keys: &mut [String], order: &[String]) -> Vec<String> {
    keys.sort();
    let mut out: Vec<String> = Vec::with_capacity(keys.len());
    for key in order {
        if keys.contains(key) && !out.contains(key) {
            out.push(key.clone());
        }
    }
    for key in keys.iter() {
        if !out.contains(key) {
            out.push(key.clone());
        }
    }
    out
}

/// The note this row carries, and the key that holds it.
///
/// A sibling `key__` documents `key`; a map's own bare `__` documents the map. Editing
/// the note has to write back to whichever of the two the note came from, so the path
/// travels with it — and when there is no note yet, the sibling is where a new one goes.
fn note_of(
    parent: Option<&Map<String, Value>>,
    key: &str,
    value: Option<&Value>,
    path: &str,
) -> (Option<String>, String) {
    if let Some(note) = parent
        .and_then(|obj| obj.get(&format!("{key}__")))
        .and_then(Value::as_str)
    {
        return (Some(note.to_string()), format!("{path}__"));
    }
    if let Some(note) = value
        .and_then(Value::as_object)
        .and_then(|obj| obj.get("__"))
        .and_then(Value::as_str)
    {
        return (Some(note.to_string()), format!("{path}.__"));
    }
    (None, format!("{path}__"))
}

fn owner_for<'a>(path: &str, applicable: &[Applicable<'a>]) -> Option<Owner> {
    applicable
        .iter()
        .filter(|entry| Access::covers(&entry.scope.prefix, path))
        .max_by_key(|entry| entry.scope.prefix.len())
        .map(|entry| Owner {
            name: entry.operator.name.clone(),
            authid: entry.operator.authid.clone(),
            prefix: entry.scope.prefix.clone(),
            mode: entry.scope.mode,
            selector: entry.scope.selector.label(),
        })
}

/// What kind of value this row holds: the document decides for a key that exists, the
/// grammar for one that only ought to.
fn kind_of(value: Option<&Value>, schema: Option<&Value>) -> ValueKind {
    let enumeration = schema.and_then(schema_enum);
    match value {
        Some(Value::Object(_)) => ValueKind::Map,
        Some(Value::Array(_)) => ValueKind::Array,
        Some(Value::Bool(_)) => ValueKind::Boolean,
        Some(Value::Number(number)) => match enumeration {
            Some(values) => ValueKind::Enum(values),
            None => match number.is_f64() {
                true => ValueKind::Number,
                false => ValueKind::Integer,
            },
        },
        Some(_) => match enumeration {
            Some(values) => ValueKind::Enum(values),
            None => ValueKind::Text,
        },
        None => match (enumeration, schema.and_then(schema_type)) {
            (Some(values), _) => ValueKind::Enum(values),
            (None, Some("object")) => ValueKind::Map,
            (None, Some("array")) => ValueKind::Array,
            (None, Some("boolean")) => ValueKind::Boolean,
            (None, Some("integer")) => ValueKind::Integer,
            (None, Some("number")) => ValueKind::Number,
            _ => ValueKind::Text,
        },
    }
}

/// A scalar (or an array) as one line of text.
pub fn render_scalar(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// `traefik` + `spec` → `traefik.spec`; the empty parent is the document root.
pub fn join(parent: &str, key: &str) -> String {
    match parent.is_empty() {
        true => key.to_string(),
        false => format!("{parent}.{key}"),
    }
}

/// The part of `full` below `base`, or `None` if `base` does not cover `full`.
fn under<'a>(base: &str, full: &'a str) -> Option<&'a str> {
    if base.is_empty() {
        return Some(full);
    }
    if full == base {
        return Some("");
    }
    if full.len() > base.len() && full.starts_with(base) && full.as_bytes()[base.len()] == b'.' {
        return Some(&full[base.len() + 1..]);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn access(value: Value) -> Access {
        serde_json::from_value(value).unwrap()
    }

    fn operators() -> Vec<Operator> {
        serde_json::from_value(json!([
            {
                "name": "traefik",
                "authid": "svc@pve!traefik",
                "scopes": [{
                    "prefix": "traefik",
                    "mode": "rw",
                    "selector": { "tag": "traefik" },
                    "grammar": {
                        "type": "object",
                        "properties": {
                            "spec": {
                                "type": "object",
                                "order": ["host", "port"],
                                "properties": {
                                    "port": {
                                        "type": "integer", "default": 80, "optional": 1,
                                        "description": "Backend port",
                                    },
                                    "host": { "type": "string", "description": "Public host name" },
                                },
                            },
                        },
                    },
                }],
            },
            {
                "name": "netbird",
                "authid": "svc@pve!netbird",
                "scopes": [{
                    "prefix": "netbird",
                    "mode": "rw",
                    "selector": { "all": 1 },
                }],
            },
        ]))
        .unwrap()
    }

    fn build_for(data: &Value, grants: Value, tags: &[&str]) -> Vec<Row> {
        let access = access(grants);
        let operators = operators();
        let tags: Vec<String> = tags.iter().map(|t| t.to_string()).collect();
        build(
            data,
            &BuildContext {
                access: &access,
                operators: &operators,
                tags: &tags,
                scoped: true,
            },
        )
    }

    fn paths(rows: &[Row]) -> Vec<String> {
        flatten(rows).iter().map(|n| n.path.clone()).collect()
    }

    #[test]
    fn rows_are_the_union_of_present_and_declared_keys() {
        // `traefik.spec.host` is present; `traefik.spec.port` only exists in the grammar.
        let data = json!({
            "traefik": { "spec": { "host": "ct200.example" } },
            "netbird": { "groups": ["lan"] },
        });
        let rows = build_for(&data, json!({"read": 1, "write": 1}), &["traefik"]);

        assert_eq!(
            paths(&rows),
            [
                "netbird",
                "netbird.groups",
                "traefik",
                "traefik.spec",
                // the grammar's own order for this map: host before port
                "traefik.spec.host",
                "traefik.spec.port",
            ],
        );

        let port = find(&rows, "traefik.spec.port").unwrap();
        assert!(!port.is_set());
        assert!(port.declared);
        assert_eq!(port.kind, ValueKind::Integer);
        assert_eq!(port.default_text().as_deref(), Some("80"));
        assert_eq!(port.description.as_deref(), Some("Backend port"));
    }

    #[test]
    fn siblings_sort_alphabetically_without_a_declared_order() {
        // `data` is unordered on the wire (§4) — the tree, not the server, decides.
        let data = json!({ "zebra": 1, "alpha": 2, "middle": 3 });
        let rows = build_for(&data, json!({"read": 1, "write": 1}), &[]);
        // `netbird` is the row the `all` registration declares; the rest is the document.
        assert_eq!(paths(&rows), ["alpha", "middle", "netbird", "zebra"]);
    }

    #[test]
    fn a_declared_order_comes_first_and_the_rest_stays_alphabetical() {
        let data = json!({ "traefik": { "spec": { "zzz": 1, "port": 8080, "host": "h" } } });
        let rows = build_for(&data, json!({"read": 1, "write": 1}), &["traefik"]);
        assert_eq!(
            paths(&rows),
            [
                "netbird",
                "traefik",
                "traefik.spec",
                "traefik.spec.host",
                "traefik.spec.port",
                "traefik.spec.zzz",
            ],
        );
    }

    #[test]
    fn a_scope_selector_decides_whether_its_grammar_applies() {
        let data = json!({});
        // Without the tag, only the `all` operator's prefix shows up.
        let rows = build_for(&data, json!({"read": 1, "write": 1}), &[]);
        assert_eq!(paths(&rows), ["netbird"]);

        // With it, the traefik grammar contributes its declared subtree too.
        let rows = build_for(&data, json!({"read": 1, "write": 1}), &["traefik"]);
        assert_eq!(
            paths(&rows),
            [
                "netbird",
                "traefik",
                "traefik.spec",
                "traefik.spec.host",
                "traefik.spec.port"
            ],
        );
    }

    #[test]
    fn scopes_do_not_apply_to_the_datacenter_document() {
        // §3: the datacenter document is governed by ACLs alone — no declared rows, no
        // owners.
        let access = access(json!({"read": 1, "write": 1}));
        let operators = operators();
        let rows = build(
            &json!({ "site": "lab" }),
            &BuildContext {
                access: &access,
                operators: &operators,
                tags: &[],
                scoped: false,
            },
        );
        assert_eq!(paths(&rows), ["site"]);
        assert!(find(&rows, "site").unwrap().owner.is_none());
    }

    #[test]
    fn comment_keys_become_descriptions_not_rows() {
        let data = json!({
            "__": "the whole document",
            "netbird": {
                "__": "netbird settings",
                "groups": ["lan"],
                "groups__": "peer groups",
            },
        });
        let rows = build_for(&data, json!({"read": 1, "write": 1}), &[]);

        assert_eq!(paths(&rows), ["netbird", "netbird.groups"]);
        assert_eq!(
            document_description(&data).as_deref(),
            Some("the whole document"),
        );
        assert_eq!(
            find(&rows, "netbird").unwrap().description.as_deref(),
            Some("netbird settings"),
        );
        let groups = find(&rows, "netbird.groups").unwrap();
        assert_eq!(groups.description.as_deref(), Some("peer groups"));
        assert_eq!(groups.kind, ValueKind::Array);
        assert_eq!(groups.display_value(), "[\"lan\"]");
    }

    #[test]
    fn a_sibling_note_wins_over_the_grammars_description() {
        let data = json!({ "traefik": { "spec": { "host": "h", "host__": "our own note" } } });
        let rows = build_for(&data, json!({"read": 1, "write": 1}), &["traefik"]);
        assert_eq!(
            find(&rows, "traefik.spec.host")
                .unwrap()
                .description
                .as_deref(),
            Some("our own note"),
        );
    }

    #[test]
    fn a_grammars_description_is_not_the_documents_note() {
        // The dialog pre-fills the note field from `note`, never from `description`:
        // otherwise the first save would copy the grammar's own prose into a comment key.
        let rows = build_for(&json!({}), json!({"read": 1, "write": 1}), &["traefik"]);
        let port = find(&rows, "traefik.spec.port").unwrap();
        assert_eq!(port.description.as_deref(), Some("Backend port"));
        assert_eq!(port.note, None);
        assert_eq!(port.comment_path(), "traefik.spec.port__");
    }

    #[test]
    fn a_maps_own_note_is_edited_where_it_lives() {
        // A map documented by its own bare `__` must write back to `netbird.__`, not to a
        // second, sibling `netbird__`.
        let data = json!({ "netbird": { "__": "netbird settings", "groups": [] } });
        let rows = build_for(&data, json!({"read": 1, "write": 1}), &[]);
        let netbird = find(&rows, "netbird").unwrap();
        assert_eq!(netbird.note.as_deref(), Some("netbird settings"));
        assert_eq!(netbird.comment_path(), "netbird.__");

        // A sibling note keeps the sibling path.
        let data = json!({ "netbird": {}, "netbird__": "from outside" });
        let rows = build_for(&data, json!({"read": 1, "write": 1}), &[]);
        assert_eq!(find(&rows, "netbird").unwrap().comment_path(), "netbird__");
    }

    #[test]
    fn an_orphan_note_still_gets_a_row_to_hang_on() {
        // Otherwise the note would be invisible in the only view of the document.
        let data = json!({ "gone__": "a note about a key nobody set" });
        let rows = build_for(&data, json!({"read": 1, "write": 1}), &[]);
        let gone = find(&rows, "gone").unwrap();
        assert!(!gone.is_set());
        assert_eq!(
            gone.description.as_deref(),
            Some("a note about a key nobody set"),
        );
    }

    #[test]
    fn editability_is_per_row_and_comes_from_access() {
        let data = json!({ "traefik": { "spec": { "host": "h" } }, "netbird": { "groups": [] } });
        let rows = build_for(
            &data,
            json!({
                "read": 1, "write": 0,
                "scopes": [{"prefix": "traefik", "mode": "rw"}, {"prefix": "netbird", "mode": "ro"}],
            }),
            &["traefik"],
        );

        assert!(find(&rows, "traefik").unwrap().writable);
        assert!(find(&rows, "traefik.spec.host").unwrap().writable);
        assert!(!find(&rows, "netbird").unwrap().writable);
        assert!(!find(&rows, "netbird.groups").unwrap().writable);
    }

    #[test]
    fn the_owner_is_the_registration_with_the_longest_covering_prefix() {
        let data = json!({ "traefik": { "spec": { "host": "h" } } });
        let rows = build_for(&data, json!({"read": 1, "write": 1}), &["traefik"]);

        let owner = find(&rows, "traefik.spec.host")
            .unwrap()
            .owner
            .clone()
            .unwrap();
        assert_eq!(owner.name, "traefik");
        assert_eq!(owner.label(), "traefik (tag: traefik)");
        assert_eq!(owner.detail(), "svc@pve!traefik — traefik (rw)");

        // A key no scope covers has no owner.
        let data = json!({ "notes": "hello" });
        let rows = build_for(&data, json!({"read": 1, "write": 1}), &["traefik"]);
        assert!(find(&rows, "notes").unwrap().owner.is_none());
    }

    #[test]
    fn value_kinds_follow_the_document_then_the_grammar() {
        let data = json!({
            "s": "text", "i": 7, "f": 1.5, "b": true, "a": [1, 2], "m": { "x": 1 },
        });
        let rows = build_for(&data, json!({"read": 1, "write": 1}), &[]);
        assert_eq!(find(&rows, "s").unwrap().kind, ValueKind::Text);
        assert_eq!(find(&rows, "i").unwrap().kind, ValueKind::Integer);
        assert_eq!(find(&rows, "f").unwrap().kind, ValueKind::Number);
        assert_eq!(find(&rows, "b").unwrap().kind, ValueKind::Boolean);
        assert_eq!(find(&rows, "a").unwrap().kind, ValueKind::Array);
        assert_eq!(find(&rows, "m").unwrap().kind, ValueKind::Map);
        // A map row shows no value of its own.
        assert_eq!(find(&rows, "m").unwrap().display_value(), "");
    }

    #[test]
    fn an_enum_in_the_grammar_makes_an_enum_row() {
        let operators: Vec<Operator> = serde_json::from_value(json!([{
            "name": "x", "authid": "u@pve",
            "scopes": [{
                "prefix": "x", "mode": "rw", "selector": {"all": 1},
                "grammar": {
                    "type": "object",
                    "properties": { "scheme": { "type": "string", "enum": ["http", "https"] } },
                },
            }],
        }]))
        .unwrap();
        let access = access(json!({"read": 1, "write": 1}));
        let rows = build(
            &json!({ "x": { "scheme": "https" } }),
            &BuildContext {
                access: &access,
                operators: &operators,
                tags: &[],
                scoped: true,
            },
        );
        assert_eq!(
            find(&rows, "x.scheme").unwrap().kind,
            ValueKind::Enum(vec!["http".to_string(), "https".to_string()]),
        );
    }

    #[test]
    fn two_registrations_declaring_the_same_key_are_merged() {
        // The lab carries exactly this: one packaged registration with the default and no
        // description, another with the description. The row wants both.
        let operators: Vec<Operator> = serde_json::from_value(json!([
            {
                "name": "packaged", "authid": "u@pve",
                "scopes": [{
                    "prefix": "traefik", "mode": "rw", "selector": {"all": 1},
                    "grammar": { "type": "object", "properties": {
                        "port": { "type": "integer", "default": 80 },
                    }},
                }],
            },
            {
                "name": "local", "authid": "u@pve",
                "scopes": [{
                    "prefix": "traefik", "mode": "rw", "selector": {"all": 1},
                    "grammar": { "type": "object", "properties": {
                        "port": { "type": "integer", "description": "Backend port" },
                        "scheme": { "type": "string", "enum": ["http", "https"] },
                    }},
                }],
            },
        ]))
        .unwrap();
        let access = access(json!({"read": 1, "write": 1}));
        let rows = build(
            &json!({}),
            &BuildContext {
                access: &access,
                operators: &operators,
                tags: &[],
                scoped: true,
            },
        );

        let port = find(&rows, "traefik.port").unwrap();
        assert_eq!(port.default_text().as_deref(), Some("80"));
        assert_eq!(port.description.as_deref(), Some("Backend port"));
        assert_eq!(port.kind, ValueKind::Integer);
        // A key only the second registration declares is a row all the same.
        assert!(find(&rows, "traefik.scheme").is_some());
    }

    #[test]
    fn a_declared_prefix_that_is_nested_only_contributes_its_first_segment() {
        let operators: Vec<Operator> = serde_json::from_value(json!([{
            "name": "deep", "authid": "u@pve",
            "scopes": [{ "prefix": "a.b.c", "mode": "rw", "selector": {"all": 1} }],
        }]))
        .unwrap();
        let access = access(json!({"read": 1, "write": 1}));
        let rows = build(
            &json!({}),
            &BuildContext {
                access: &access,
                operators: &operators,
                tags: &[],
                scoped: true,
            },
        );
        // `a` exists as a row; `a.b` under it; `a.b.c` under that. Nothing else.
        assert_eq!(paths(&rows), ["a", "a.b", "a.b.c"]);
        assert!(find(&rows, "a.b.c").unwrap().owner.is_some());
    }

    #[test]
    fn path_joining_and_covering() {
        assert_eq!(join("", "a"), "a");
        assert_eq!(join("a.b", "c"), "a.b.c");
        assert_eq!(under("a", "a.b"), Some("b"));
        assert_eq!(under("a", "a"), Some(""));
        assert_eq!(under("a", "ab"), None);
        assert_eq!(under("", "a.b"), Some("a.b"));
    }
}
