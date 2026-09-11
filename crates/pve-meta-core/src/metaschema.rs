//! The **meta-schema**: the two registry file formats described in the same
//! dialect a prefix uses to describe a guest's subtree (`docs/DESIGN.md`
//! §3.6).
//!
//! Since revision 6 a prefix or permission file is an ordinary document
//! ([`crate::store::DocId::Registry`]), so the editor can show it as a tree and
//! lint it as it is typed -- but only if something says what shape it has. That
//! is what this module is: `prefix.yaml` and `grant.yaml` written out as
//! schemas, served by `GET /meta/schemas`, and used by the editor exactly the
//! way a prefix's own `schema` is used on a guest document.
//!
//! One rule it cannot express: a selector is **exactly one of** `all` or `tag`
//! (`registry::parse_selector`). The dialect has no "one of these" keyword, so
//! both are declared individually optional and the parser is the only thing that
//! enforces the choice -- which is fine (it is the authority either way), but it
//! means the editor will happily *offer* you both rows. The test below pins that
//! as a known limit rather than leaving it to be rediscovered.
//!
//! It is deliberately **not** the validator. `registry::parse_prefix` and
//! `registry::parse_permission` decide what is storable, on the way in, in one place
//! (`api::check_registry_shape`); this is the affordance that tells a human
//! what to type before they try. The test at the bottom is what keeps the two
//! from drifting: every property this schema marks as required is one the
//! parser actually refuses to do without.
//!
//! The `schema:` property of a prefix is described as a free-form object on
//! purpose. It is a schema in its own right, in an open-ended dialect, and the
//! editor's honest offer for it is the text editor (a map row opens Monaco on
//! its own subtree, DESIGN §8) rather than a form that would only ever cover
//! the keywords we happened to think of.

use crate::format::{self, Format};
use crate::model::Value;

/// The prefix file format (`docs/DESIGN.md` §3.1).
const PREFIX: &str = r#"
type: object
description: >-
  A prefix definition: what a prefix is, and which guests it reaches. Its file name
  is the prefix it declares, so there is no field here for the two to disagree about.
properties:
  description:
    type: string
    optional: 1
    description: What this prefix is for. Shown in the prefix list and on hover.
  selector:
    type: object
    description: >-
      Which guests this prefix reaches. Exactly one of 'all' or 'tag'. This is a
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
  enforce:
    type: boolean
    optional: 1
    description: >-
      Refuse an API write that would leave this prefix's subtree not matching its
      schema, for the paths the write changed; 'force=1' stores it anyway (the
      editor's "Save anyway" tick). Off by default: a schema is advisory unless the
      prefix says otherwise. Format checks are never enforced.
  schema:
    type: object
    optional: 1
    description: >-
      What the subtree under this prefix looks like, in the PVE::JSONSchema dialect:
      type, properties, description, default, optional, enum, minimum, maximum,
      format, plus 'multiline' as an editor hint. Free-form, so it is edited as text.
"#;

/// The permission file format (`docs/DESIGN.md` §3.2).
const PERMISSION: &str = r#"
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
  rules:
    type: array
    optional: 1
    description: >-
      The entries this principal gets: each one a prefix, a mode ('ro' or 'rw') and a
      selector. Permissions accumulate by containment, so an entry on 'homelab' also covers
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

/// The prefix file's schema.
pub fn prefix() -> Value {
    parse(PREFIX, "prefix")
}

/// The permission file's schema.
pub fn permission() -> Value {
    parse(PERMISSION, "permission")
}

/// Both, keyed by kind: what `GET /meta/schemas` returns.
pub fn schemas() -> Value {
    let mut map = serde_json::Map::new();
    map.insert("prefix".to_string(), prefix());
    map.insert("permission".to_string(), permission());
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
        assert_eq!(prefix()["type"], "object");
        assert_eq!(permission()["type"], "object");
    }

    /// The point of the module: what it says is required has to be what the
    /// parser actually requires. Both directions -- a required property the
    /// parser would accept without is a form that asks for too much, and an
    /// optional one it refuses without is a form that lets you write a file the
    /// API will reject.
    #[test]
    fn required_properties_are_the_ones_the_parser_refuses_to_do_without() {
        let ns = "description: d\nselector: {all: true}\nschema: {type: object}\n";
        assert!(registry::parse_prefix("x", ns).is_ok(), "the full example parses");
        for (key, optional) in properties(&prefix()) {
            let value: serde_json::Value = format::parse_raw(Format::Yaml, ns).unwrap();
            let mut without = value.as_object().unwrap().clone();
            without.remove(&key);
            let text = format::dump(Format::Yaml, &Value::Object(without));
            assert_eq!(
                registry::parse_prefix("x", &text).is_ok(),
                optional,
                "prefix: dropping '{key}' (optional: {optional}) does not match the parser",
            );
        }

        let permissions_text = "authid: a@pve!t1\ndescription: d\nrules: []\n";
        assert!(registry::parse_permission("x", permissions_text).is_ok());
        for (key, optional) in properties(&permission()) {
            let value: serde_json::Value = format::parse_raw(Format::Yaml, permissions_text).unwrap();
            let mut without = value.as_object().unwrap().clone();
            without.remove(&key);
            let text = format::dump(Format::Yaml, &Value::Object(without));
            assert_eq!(
                registry::parse_permission("x", &text).is_ok(),
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
        let selector = &prefix()["properties"]["selector"];
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
            registry::parse_prefix("x", &format!("selector: {sel}\n")).is_ok()
        };
        assert!(with("{all: true}"), "one alternative is a selector");
        assert!(with("{tag: web}"), "so is the other");
        assert!(!with("{}"), "neither is not");
        assert!(!with("{all: true, tag: web}"), "and both is not");
        assert!(!with("{all: false}"), "nor is 'all: false', which selects nothing");
    }

    #[test]
    fn no_property_is_invented() {
        let ns: Vec<String> = properties(&prefix()).into_iter().map(|(k, _)| k).collect();
        assert_eq!(ns, ["description", "selector", "enforce", "schema"]);
        let g: Vec<String> = properties(&permission()).into_iter().map(|(k, _)| k).collect();
        assert_eq!(g, ["authid", "description", "rules"]);
    }
}
