//! The **meta-schema**: the prefix file format described in the same dialect
//! a prefix uses to describe a guest's subtree (`docs/DESIGN.md` §6).
//!
//! A prefix file is an ordinary document ([`crate::store::DocId::Registry`]),
//! so the editor can show it as a tree and lint it as it is typed -- but only
//! if something says what shape it has. That is what this module is:
//! `prefix.yaml` written out as a schema, served by `GET /meta/schemas`, and
//! used by the editor exactly the way a prefix's own `schema` is used on a
//! guest document.
//!
//! One rule it cannot express: a selector is **exactly one of** `all` or `tag`
//! (`registry::parse_selector`). The dialect has no "one of these" keyword, so
//! both are declared individually optional and the parser is the only thing that
//! enforces the choice -- which is fine (it is the authority either way), but it
//! means the editor will happily *offer* you both rows. The test below pins that
//! as a known limit rather than leaving it to be rediscovered.
//!
//! It is deliberately **not** the validator. `registry::parse_prefix` decides
//! what is storable, on the way in, in one place
//! (`api::check_registry_shape`); this is the affordance that tells a human
//! what to type before they try. The test at the bottom is what keeps the two
//! from drifting: every property this schema marks as required is one the
//! parser actually refuses to do without.
//!
//! The `schema:` property of a prefix is described as a free-form object on
//! purpose. It is a schema in its own right, in an open-ended dialect, and the
//! editor's honest offer for it is the text editor (a map row opens Monaco on
//! its own subtree, DESIGN §12) rather than a form that would only ever cover
//! the keywords we happened to think of. Free-form here, not on the way in:
//! `registry::check_schema_dialect` refuses a known keyword whose value the
//! checker could not act on, since with `enforce` the schema is a write gate.

use crate::format::{self, Format};
use crate::model::Value;

/// The prefix file format (`docs/DESIGN.md` §3).
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
      prefix says otherwise. Format checks are never enforced. This is the default
      for the whole subtree; any schema node may set its own 'enforce' and every
      node below inherits that instead.
  hidden:
    type: boolean
    optional: 1
    description: >-
      Do not offer this prefix's declared-but-unset keys as rows. For a prefix whose
      schema is a vocabulary rather than a handful of keys, where the useful default
      is to show what a guest actually set. This is the default for the whole
      subtree; any schema node may set its own 'hidden' and every node below
      inherits that instead. It never hides a key that IS set, and never affects
      validation.
  schema:
    type: object
    optional: 1
    description: >-
      What the subtree under this prefix looks like, in the PVE::JSONSchema dialect:
      type, properties, description, default, optional, enum, minimum, maximum,
      format, plus two editor hints of our own -- 'multiline' (this string is a block
      of text) and 'hidden' (do not offer this as a row until it is set). A node may
      also carry 'enforce'. All three are inherited by everything below the node that
      sets them. Free-form, so it is edited as text.
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

/// What `GET /meta/schemas` returns: `{ prefix }`.
pub fn schemas() -> Value {
    let mut map = serde_json::Map::new();
    map.insert("prefix".to_string(), prefix());
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
        // Not a tautology: this is a string constant, and the accessor panics
        // rather than returns a Result, so this test is the thing that stops a
        // typo in it from reaching pvedaemon.
        assert_eq!(schemas().as_object().unwrap().len(), 1);
        assert_eq!(prefix()["type"], "object");
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
    }

    /// The editor renders declared-but-unset rows from `properties`, so a schema
    /// that names a key the parser does not know paints a row that can never be
    /// written. `properties()` alone only looks one level down, so this checks
    /// the nested case too: everything inside `selector`.
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
        assert_eq!(ns, ["description", "selector", "enforce", "hidden", "schema"]);
        // The list is the point: a property here that the parser refuses would be a
        // row the editor paints and a file that then fails to load. The two booleans
        // are the ones added late, so check them against the parser directly.
        for flag in ["enforce", "hidden"] {
            let text = format!("selector: {{all: true}}\n{flag}: true\n");
            assert!(
                registry::parse_prefix("x", &text).is_ok(),
                "the meta-schema offers '{flag}', which the parser refuses"
            );
        }
    }
}
