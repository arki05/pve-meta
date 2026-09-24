//! The **meta-schema**: the prefix file format described in the same dialect
//! a prefix uses to describe a guest's subtree (`docs/DESIGN.md` §3), served
//! by `GET /meta/schemas` so the editor can show a prefix file as a tree and
//! lint it as it is typed. Advisory only -- `registry::parse_prefix` is the
//! validator; the contract test below is what keeps the two from drifting.

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
    default: false
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
    default: false
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
      type, properties, items, description, default, optional, enum, minimum,
      maximum, format, plus two editor hints of our own -- 'multiline' (this string is a block
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

    /// An unset optional flag is a row that says "not set (default: ...)" and a
    /// field that opens on the default -- but only if the schema says what the
    /// default is; without it the row read "not set" beside a description that
    /// names one. So every optional boolean or enum declares its `default`, and
    /// that default is what the parser takes when the key is absent.
    #[test]
    fn every_optional_flag_declares_the_default_the_parser_takes() {
        let schema = prefix();
        let absent = registry::parse_prefix("x", "selector: {all: true}\n").unwrap();
        let absent = serde_json::to_value(&absent).unwrap();
        for (key, optional) in properties(&schema) {
            let node = &schema["properties"][&key];
            let flag = node["type"] == "boolean" || node.get("enum").is_some();
            if !optional || !flag {
                continue;
            }
            let default = node.get("default").unwrap_or_else(|| panic!("'{key}' declares no default"));
            assert_eq!(&absent[&key], default, "'{key}': the declared default is not what the parser takes");
        }
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
