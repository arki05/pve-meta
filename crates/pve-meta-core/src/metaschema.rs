//! The **meta-schema**: the two registry file formats described in the same
//! dialect a namespace uses to describe a guest's subtree (`docs/DESIGN.md`
//! §3.6).
//!
//! Since revision 6 a namespace or grant file is an ordinary document
//! ([`crate::store::DocId::Registry`]), so the editor can show it as a tree and
//! lint it as it is typed -- but only if something says what shape it has. That
//! is what this module is: `namespace.yaml` and `grant.yaml` written out as
//! schemas, served by `GET /meta/schemas`, and used by the editor exactly the
//! way a namespace's own `schema` is used on a guest document.
//!
//! One rule it cannot express: a selector is **exactly one of** `all` or `tag`
//! (`registry::parse_selector`). The dialect has no "one of these" keyword, so
//! both are declared individually optional and the parser is the only thing that
//! enforces the choice -- which is fine (it is the authority either way), but it
//! means the editor will happily *offer* you both rows. The test below pins that
//! as a known limit rather than leaving it to be rediscovered.
//!
//! It is deliberately **not** the validator. `registry::parse_namespace` and
//! `registry::parse_grant` decide what is storable, on the way in, in one place
//! (`api::check_registry_shape`); this is the affordance that tells a human
//! what to type before they try. The test at the bottom is what keeps the two
//! from drifting: every property this schema marks as required is one the
//! parser actually refuses to do without.
//!
//! The `schema:` property of a namespace is described as a free-form object on
//! purpose. It is a schema in its own right, in an open-ended dialect, and the
//! editor's honest offer for it is the text editor (a map row opens Monaco on
//! its own subtree, DESIGN §8) rather than a form that would only ever cover
//! the keywords we happened to think of.

use crate::format::{self, Format};
use crate::model::Value;

/// The namespace file format (`docs/DESIGN.md` §3.1).
const NAMESPACE: &str = r#"
type: object
description: >-
  A namespace: what a prefix is. The file name is the prefix, so there is no
  'prefix' field for it to disagree with.
properties:
  description:
    type: string
    optional: 1
    description: What this prefix is for. Shown in the namespace list and on hover.
  selector:
    type: object
    description: >-
      Which guests this namespace reaches. Exactly one of 'all' or 'tag'. This is a
      selector, not a permission boundary.
    properties:
      all:
        type: boolean
        optional: 1
        description: Every guest in the cluster.
      tag:
        type: string
        optional: 1
        description: >-
          Only guests carrying this PVE tag. Adding the tag to a guest is the
          deliberate act of including it.
  schema:
    type: object
    optional: 1
    description: >-
      What the subtree under this prefix looks like, in the PVE::JSONSchema dialect:
      type, properties, description, default, optional, enum, minimum, maximum,
      format, plus 'multiline' as an editor hint. Free-form, so it is edited as text.
"#;

/// The grant file format (`docs/DESIGN.md` §3.2).
const GRANT: &str = r#"
type: object
description: >-
  A grant: who may touch which prefix. Cluster-only, because an operator's own
  package must never be able to ship one.
properties:
  authid:
    type: string
    description: >-
      The PVE user or token id this grant is for -- user@realm, optionally with
      !tokenid.
  description:
    type: string
    optional: 1
    description: What this principal is, for whoever reads the file next.
  grants:
    type: array
    optional: 1
    description: >-
      The entries this principal gets: each one a prefix, a mode ('ro' or 'rw') and a
      selector. Grants accumulate by containment, so an entry on 'homelab' also covers
      'homelab.docker'. Optional because a file with none is a principal that may touch
      nothing, which is a legitimate (if pointless) state -- and the misspelling that
      would otherwise produce it by accident is already refused, since an unknown key
      anywhere in these files is an error.
"#;

fn parse(text: &str, what: &str) -> Value {
    // A constant this crate ships: it either parses on every call or on none,
    // and `the_meta_schema_parses` below is what makes sure it is the latter.
    format::parse_raw(Format::Yaml, text)
        .unwrap_or_else(|e| panic!("the built-in {what} meta-schema does not parse: {e}"))
}

/// The namespace file's schema.
pub fn namespace() -> Value {
    parse(NAMESPACE, "namespace")
}

/// The grant file's schema.
pub fn grant() -> Value {
    parse(GRANT, "grant")
}

/// Both, keyed by kind: what `GET /meta/schemas` returns.
pub fn schemas() -> Value {
    let mut map = serde_json::Map::new();
    map.insert("namespace".to_string(), namespace());
    map.insert("grant".to_string(), grant());
    Value::Object(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry;

    /// Every key the schema declares, with whether it is optional.
    fn properties(schema: &Value) -> Vec<(String, bool)> {
        schema["properties"]
            .as_object()
            .expect("properties is a map")
            .iter()
            .map(|(k, v)| (k.clone(), v.get("optional").is_some()))
            .collect()
    }

    #[test]
    fn the_meta_schema_parses() {
        // Not a tautology: these are string constants, and the accessors panic
        // rather than return a Result, so this test is the thing that stops a
        // typo in them from reaching pvedaemon.
        assert_eq!(schemas().as_object().unwrap().len(), 2);
        assert_eq!(namespace()["type"], "object");
        assert_eq!(grant()["type"], "object");
    }

    /// The point of the module: what it says is required has to be what the
    /// parser actually requires. Both directions -- a required property the
    /// parser would accept without is a form that asks for too much, and an
    /// optional one it refuses without is a form that lets you write a file the
    /// API will reject.
    #[test]
    fn required_properties_are_the_ones_the_parser_refuses_to_do_without() {
        let ns = "description: d\nselector: {all: true}\nschema: {type: object}\n";
        assert!(registry::parse_namespace("x", ns).is_ok(), "the full example parses");
        for (key, optional) in properties(&namespace()) {
            let value: serde_json::Value = format::parse_raw(Format::Yaml, ns).unwrap();
            let mut without = value.as_object().unwrap().clone();
            without.remove(&key);
            let text = format::dump(Format::Yaml, &Value::Object(without));
            assert_eq!(
                registry::parse_namespace("x", &text).is_ok(),
                optional,
                "namespace: dropping '{key}' (optional: {optional}) does not match the parser",
            );
        }

        let grant_text = "authid: a@pve!t1\ndescription: d\ngrants: []\n";
        assert!(registry::parse_grant("x", grant_text).is_ok());
        for (key, optional) in properties(&grant()) {
            let value: serde_json::Value = format::parse_raw(Format::Yaml, grant_text).unwrap();
            let mut without = value.as_object().unwrap().clone();
            without.remove(&key);
            let text = format::dump(Format::Yaml, &Value::Object(without));
            assert_eq!(
                registry::parse_grant("x", &text).is_ok(),
                optional,
                "grant: dropping '{key}' (optional: {optional}) does not match the parser",
            );
        }
    }

    /// The editor renders declared-but-unset rows from `properties`, so a schema
    /// that named a key the parser does not know would paint a row that can
    /// never be written.
    /// The nested half of the same question. `properties()` only ever looked one
    /// level down, so everything inside `selector` -- the one sub-object either
    /// schema has -- was unchecked: a typo in `tag`, or a claim that `all` is
    /// required, would have passed silently.
    #[test]
    fn the_selector_is_described_the_way_the_parser_reads_it() {
        let selector = &namespace()["properties"]["selector"];
        assert_eq!(selector["type"], "object");
        let inner = properties(selector);
        assert_eq!(
            inner,
            vec![("all".to_string(), true), ("tag".to_string(), true)],
            "both alternatives, both individually optional",
        );

        // Individually optional is all the dialect can say. "Exactly one" is the
        // actual rule and only the parser has it -- so it is asserted here, against
        // the parser, rather than described in a schema that cannot hold it.
        let with = |sel: &str| {
            registry::parse_namespace("x", &format!("selector: {sel}\n")).is_ok()
        };
        assert!(with("{all: true}"), "one alternative is a selector");
        assert!(with("{tag: web}"), "so is the other");
        assert!(!with("{}"), "neither is not");
        assert!(!with("{all: true, tag: web}"), "and both is not");
        assert!(!with("{all: false}"), "nor is 'all: false', which selects nothing");
    }

    #[test]
    fn no_property_is_invented() {
        let ns: Vec<String> = properties(&namespace()).into_iter().map(|(k, _)| k).collect();
        assert_eq!(ns, ["description", "selector", "schema"]);
        let g: Vec<String> = properties(&grant()).into_iter().map(|(k, _)| k).collect();
        assert_eq!(g, ["authid", "description", "grants"]);
    }
}
