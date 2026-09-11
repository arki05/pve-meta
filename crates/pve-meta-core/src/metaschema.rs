//! The **meta-schema**: the two registry file formats described in the same
//! dialect a prefix uses to describe a guest's subtree (`docs/DESIGN.md`
//! §6).
//!
//! A prefix or permission file is an ordinary document
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
//! its own subtree, DESIGN §12) rather than a form that would only ever cover
//! the keywords we happened to think of.

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
      What the subtree under this prefix looks like, in the PVE::JSONSchema dialect.
      The node's own keywords are described below; everything under 'properties' is
      not, and cannot be -- those keys are whatever an operator names them, and this
      dialect has no way to say "every child here is another schema node". Declare Key
      is how a property is added at any depth.
    properties:
      type:
        type: string
        enum: [object, string, integer, number, boolean, array]
        description: What a value here must be.
      description:
        type: string
        optional: 1
        description: Shown as the tooltip on this key's row.
      properties:
        type: object
        optional: 1
        description: >-
          The keys this node describes, each mapped to another schema node. Add one
          with Declare Key rather than by hand.
      default:
        type: string
        optional: 1
        hidden: 1
        description: >-
          Offered by "Set to default" and pre-filled by the row editor. Never written
          on its own.
      enum:
        type: array
        optional: 1
        hidden: 1
        description: The only accepted values; the row editor becomes a dropdown.
      minimum:
        type: integer
        optional: 1
        hidden: 1
        description: For integer and number keys.
      maximum:
        type: integer
        optional: 1
        hidden: 1
        description: For integer and number keys.
      format:
        type: string
        optional: 1
        hidden: 1
        description: >-
          A PVE format name (dns-name, ipv4, ...). Checked by the editor, which has
          proxmoxlib's validators; never enforced by the server, which does not.
      multiline:
        type: boolean
        optional: 1
        hidden: 1
        description: Edit this string in a text box rather than on one line.
      hidden:
        type: boolean
        optional: 1
        hidden: 1
        description: >-
          Do not offer this key as a row until it is set. Inherited by everything
          below this node unless that node says otherwise.
      enforce:
        type: boolean
        optional: 1
        hidden: 1
        description: >-
          Refuse a write that leaves this subtree not matching its schema. Inherited
          by everything below this node unless that node says otherwise.
"#;

/// The permission file format (`docs/DESIGN.md` §4).
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
    /// that names a key the parser does not know paints a row that can never be
    /// written. `properties()` alone only looks one level down, so this checks
    /// the nested case too: everything inside `selector`, the one sub-object
    /// either schema has.
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

    /// The meta-schema describes a schema node's own keywords, so the prefix
    /// editor offers them as rows. It stops at `properties`, and that is a hard
    /// stop rather than a depth choice: those keys are whatever an operator names
    /// them, and this dialect has no `$ref` with which to say "every child here is
    /// another schema node". Declare Key is how a property is added at any depth.
    #[test]
    fn the_schema_node_is_described_but_its_properties_cannot_be() {
        let schema = &prefix()["properties"]["schema"];
        let props = schema["properties"].as_object().expect("the node's keywords");
        assert!(props.contains_key("type"));
        assert!(props.contains_key("properties"));
        assert!(props.contains_key("hidden") && props.contains_key("enforce"));

        // The common three are offered; the long tail is there and hidden, which is
        // the feature being used on itself.
        for shown in ["type", "description", "properties"] {
            assert!(props[shown].get("hidden").is_none(), "{shown} should be offered");
        }
        for rare in ["default", "enum", "minimum", "maximum", "format", "multiline"] {
            assert_eq!(props[rare]["hidden"], 1, "{rare} should be hidden");
        }

        // The stop: `properties` is an object with nothing described inside it.
        assert_eq!(props["properties"]["type"], "object");
        assert!(props["properties"].get("properties").is_none());
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
        let g: Vec<String> = properties(&permission()).into_iter().map(|(k, _)| k).collect();
        assert_eq!(g, ["authid", "description", "rules"]);
    }
}
