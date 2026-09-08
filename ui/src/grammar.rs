//! Operator registrations and the grammar they declare (`GET /meta/operators`).
//!
//! One registration names a principal (`authid`) and the prefixes it may read or write
//! (`docs/DESIGN.md` §3). Each scope carries a **selector** — `all`, or `tag: <t>` — that
//! decides which guests it applies to, and optionally a **grammar**, a `PVE::JSONSchema`
//! object describing the subtree under the prefix.
//!
//! The tree uses registrations for two things, and neither is an access decision (that is
//! `GET /meta/access`, `crate::model::Access`):
//!
//! * **Rows that ought to exist.** A grammar's `properties` name keys the operator expects;
//!   the tree shows them greyed with their default and description even when the document
//!   has never carried them, plus a "set" action.
//! * **Owner.** The registration whose scope covers a row is the thing that put it there,
//!   and the column says so — with the selector that made it apply to this guest.
//!
//! Pure module: unit-tested natively (`cargo test --lib`).

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::model::{Mode, flag_value};

/// One entry of `GET /meta/operators`.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct Operator {
    /// The registration file's name, e.g. `traefik`.
    #[serde(default)]
    pub name: String,
    /// The PVE user or token id the registration is for.
    #[serde(default)]
    pub authid: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub scopes: Vec<OperatorScope>,
}

/// One scope of a registration.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct OperatorScope {
    /// Dotted key path, any depth.
    pub prefix: String,
    /// What the operator may do there. Unknown spellings are not a grant, so they parse
    /// as `ro` — this is display-only anyway (the caller's own rights come from
    /// `/meta/access`).
    #[serde(default = "read_only")]
    pub mode: Mode,
    /// Which guests the scope applies to.
    #[serde(default)]
    pub selector: Selector,
    /// `PVE::JSONSchema` description of the subtree at `prefix`, if the operator declared
    /// one. Kept as a raw `Value`: the dialect is Perl's, not serde's, and the tree only
    /// ever reads a handful of well-known keys out of it (see the `schema_*` helpers).
    #[serde(default)]
    pub grammar: Option<Value>,
}

fn read_only() -> Mode {
    Mode::Ro
}

/// Which guests a scope applies to (`docs/DESIGN.md` §3).
///
/// Deliberately not an enum: an unknown selector shape (a future `pool:`, a typo) must
/// deserialize successfully and then match *nothing*, rather than fail the whole
/// `/meta/operators` response for every other operator in it.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct Selector {
    #[serde(default)]
    pub all: Option<Value>,
    #[serde(default)]
    pub tag: Option<String>,
}

impl Selector {
    /// True if this selector picks a guest carrying `tags`.
    pub fn matches(&self, tags: &[String]) -> bool {
        if self.all.as_ref().is_some_and(flag_value) {
            return true;
        }
        match &self.tag {
            Some(tag) => tags.iter().any(|t| t == tag),
            None => false,
        }
    }

    /// A short human label for the owner column: `all`, `tag: traefik`, or nothing.
    pub fn label(&self) -> Option<String> {
        if self.all.as_ref().is_some_and(flag_value) {
            return Some("all".to_string());
        }
        self.tag.as_ref().map(|tag| format!("tag: {tag}"))
    }
}

/// The `properties` map of a `PVE::JSONSchema` object schema.
pub fn schema_properties(schema: &Value) -> Option<&Map<String, Value>> {
    schema.get("properties")?.as_object()
}

/// The schema of one property of an object schema.
pub fn schema_property<'a>(schema: &'a Value, key: &str) -> Option<&'a Value> {
    schema_properties(schema)?.get(key)
}

/// Walk `schema` down a relative dotted path through `properties`.
pub fn schema_at<'a>(schema: &'a Value, path: &str) -> Option<&'a Value> {
    let mut node = schema;
    if path.is_empty() {
        return Some(node);
    }
    for segment in path.split('.') {
        node = schema_property(node, segment)?;
    }
    Some(node)
}

/// `description` of a schema node.
pub fn schema_description(schema: &Value) -> Option<String> {
    schema.get("description")?.as_str().map(str::to_string)
}

/// `default` of a schema node.
pub fn schema_default(schema: &Value) -> Option<&Value> {
    schema.get("default")
}

/// `type` of a schema node (`string`, `integer`, `number`, `boolean`, `object`, `array`).
pub fn schema_type(schema: &Value) -> Option<&str> {
    schema.get("type")?.as_str()
}

/// `enum` of a schema node, as display strings.
pub fn schema_enum(schema: &Value) -> Option<Vec<String>> {
    let list = schema.get("enum")?.as_array()?;
    Some(list.iter().map(scalar_to_string).collect())
}

/// The key order a grammar declares for an object schema, if any.
///
/// `data` is unordered on the wire and the tree sorts alphabetically (`docs/DESIGN.md`
/// §4/§8); an object schema may override that for its own properties with an `order`
/// array of key names. Keys it lists come first, in that order; everything else follows
/// alphabetically. Keys in `order` that the schema does not declare are ignored.
pub fn schema_order(schema: &Value) -> Vec<String> {
    match schema.get("order").and_then(Value::as_array) {
        Some(list) => list
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        None => Vec::new(),
    }
}

/// A scalar as the string a form field shows.
pub fn scalar_to_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn operators() -> Vec<Operator> {
        serde_json::from_value(json!([{
            "name": "traefik",
            "authid": "svc@pve!traefik",
            "description": "Traefik dynamic-configuration provider",
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
                                "host": { "type": "string", "description": "Public host name" },
                                "port": {
                                    "type": "integer", "minimum": 1, "maximum": 65535,
                                    "optional": 1, "default": 80,
                                },
                            },
                        },
                    },
                },
            }],
        }]))
        .unwrap()
    }

    #[test]
    fn a_registration_parses_whole() {
        let ops = operators();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].authid, "svc@pve!traefik");
        assert_eq!(ops[0].scopes[0].mode, Mode::Rw);
        assert_eq!(ops[0].scopes[0].selector.tag.as_deref(), Some("traefik"));
    }

    #[test]
    fn a_selector_matches_by_tag_or_by_all() {
        let all: Selector = serde_json::from_value(json!({"all": true})).unwrap();
        assert!(all.matches(&[]));
        assert_eq!(all.label().as_deref(), Some("all"));

        // The wire may spell a Perl boolean as 1.
        let perl_all: Selector = serde_json::from_value(json!({"all": 1})).unwrap();
        assert!(perl_all.matches(&[]));

        let tagged: Selector = serde_json::from_value(json!({"tag": "traefik"})).unwrap();
        assert!(tagged.matches(&["traefik".to_string()]));
        assert!(!tagged.matches(&["netbird".to_string()]));
        assert_eq!(tagged.label().as_deref(), Some("tag: traefik"));
    }

    #[test]
    fn an_unknown_selector_parses_and_grants_nothing() {
        // A future `{ pool: name }` must not fail the whole response.
        let future: Selector = serde_json::from_value(json!({"pool": "prod"})).unwrap();
        assert!(!future.matches(&["prod".to_string()]));
        assert_eq!(future.label(), None);
        // `all: 0` is not "all".
        let off: Selector = serde_json::from_value(json!({"all": 0})).unwrap();
        assert!(!off.matches(&[]));
    }

    #[test]
    fn schema_navigation_walks_properties() {
        let ops = operators();
        let grammar = ops[0].scopes[0].grammar.clone().unwrap();

        let host = schema_at(&grammar, "spec.host").unwrap();
        assert_eq!(schema_type(host), Some("string"));
        assert_eq!(
            schema_description(host).as_deref(),
            Some("Public host name"),
        );

        let port = schema_at(&grammar, "spec.port").unwrap();
        assert_eq!(schema_default(port), Some(&json!(80)));
        assert_eq!(
            schema_order(schema_at(&grammar, "spec").unwrap()),
            ["host", "port"]
        );

        assert!(schema_at(&grammar, "spec.missing").is_none());
        assert!(schema_at(&grammar, "").is_some());
    }

    #[test]
    fn enum_values_render_as_strings() {
        let schema = json!({"type": "string", "enum": ["http", "https", 8080]});
        assert_eq!(
            schema_enum(&schema).unwrap(),
            ["http".to_string(), "https".to_string(), "8080".to_string()],
        );
        assert!(schema_enum(&json!({"type": "string"})).is_none());
    }
}
