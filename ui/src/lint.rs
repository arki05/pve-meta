//! Grammar findings for the text editor: what is wrong, and which line to underline.
//!
//! Text mode shows the document as YAML and Monaco can mark ranges in it, so a grammar
//! can say more here than "this row's editor rejects that value" — it can underline the
//! lines that already violate it.
//!
//! Two halves, kept apart on purpose:
//!
//! * [`findings`] answers *what is wrong*, from the **parsed document** the page already
//!   holds (`format=json`) and the grammars that apply to it. No YAML parsing happens
//!   here — the server stays the only thing in this implementation that reads YAML, which
//!   is the property worth protecting.
//! * [`line_index`] answers *where to draw it*, by scanning the YAML text the server
//!   returned. It is a scan, not a parser: the store dumps canonically (block style,
//!   two-space indent, one key per line), so an indent stack is enough.
//!
//! The pairing only holds while the buffer still *is* what the server returned. Once the
//! user types, the findings describe a document that no longer matches the text, so the
//! caller clears the markers rather than pointing them at moved lines.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::grammar::{
    check_format, scalar_to_string, schema_enum, schema_format, schema_maximum, schema_minimum,
    schema_properties, schema_type,
};

/// The grammars that apply to this document: every registration scope whose selector
/// matches, paired with the schema it declares. Scopes without a grammar contribute
/// nothing — an operator that described no shape has made no claim.
///
/// `scoped` is false for the datacenter document, where registration scopes do not apply
/// at all (`docs/DESIGN.md` §3).
pub fn applicable(
    operators: &[crate::grammar::Operator],
    tags: &[String],
    scoped: bool,
) -> Vec<(String, Value)> {
    if !scoped {
        return Vec::new();
    }
    operators
        .iter()
        .flat_map(|op| op.scopes.iter())
        .filter(|scope| scope.selector.matches(tags))
        .filter_map(|scope| {
            scope
                .grammar
                .clone()
                .map(|schema| (scope.prefix.clone(), schema))
        })
        .collect()
}

/// One thing a grammar objects to, at a document path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// Dotted document path, e.g. `traefik.spec.port`.
    pub path: String,
    /// Human-readable, shown in the marker's hover.
    pub message: String,
}

/// Every finding the applicable grammars have about `data`.
///
/// `applicable` pairs a scope prefix with the schema declared for it — exactly what the
/// tree already resolves. A prefix with no data under it contributes nothing; a schema
/// that declares no `properties` for a key stops the walk there, because an operator that
/// did not describe a subtree has not made a claim about it.
pub fn findings(data: &Value, applicable: &[(String, Value)]) -> Vec<Finding> {
    let mut out = Vec::new();
    for (prefix, schema) in applicable {
        let Some(value) = value_at(data, prefix) else {
            continue;
        };
        walk(value, schema, prefix, &mut out);
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out.dedup();
    out
}

fn value_at<'a>(data: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = data;
    if path.is_empty() {
        return Some(cur);
    }
    for segment in path.split('.') {
        cur = cur.as_object()?.get(segment)?;
    }
    Some(cur)
}

fn walk(value: &Value, schema: &Value, path: &str, out: &mut Vec<Finding>) {
    if let Some(message) = check_value(schema, value) {
        out.push(Finding {
            path: path.to_string(),
            message,
        });
        // A value of the wrong shape has nothing useful to say about its children.
        return;
    }
    let (Some(map), Some(properties)) = (value.as_object(), schema_properties(schema)) else {
        return;
    };
    for (key, child) in map {
        if let Some(child_schema) = properties.get(key) {
            let child_path = match path.is_empty() {
                true => key.clone(),
                false => format!("{path}.{key}"),
            };
            walk(child, child_schema, &child_path, out);
        }
    }
}

/// What a grammar objects to about one value, or `None` if it is happy.
///
/// Only what the row editor also enforces, so the two never disagree: declared type,
/// `enum`, `minimum`/`maximum`, `format`. A comment key is never checked — it is a note
/// about the key, not the key.
pub fn check_value(schema: &Value, value: &Value) -> Option<String> {
    if let Some(values) = schema_enum(schema) {
        let shown = scalar_to_string(value);
        if !values.iter().any(|v| *v == shown) {
            return Some(format!("expected one of: {}", values.join(", ")));
        }
        return None;
    }
    if let Some(declared) = schema_type(schema) {
        if !type_matches(declared, value) {
            return Some(format!("expected {declared}"));
        }
    }
    if let Some(n) = value.as_f64() {
        if let Some(min) = schema_minimum(schema) {
            if n < min {
                return Some(format!("must be at least {}", number_text(min)));
            }
        }
        if let Some(max) = schema_maximum(schema) {
            if n > max {
                return Some(format!("must be at most {}", number_text(max)));
            }
        }
    }
    if let (Some(text), Some(name)) = (value.as_str(), schema_format(schema)) {
        if let Err(message) = check_format(name, text) {
            return Some(message);
        }
    }
    None
}

/// `integer` accepts a whole number; `number` accepts either. A JSON boolean arriving as
/// `1`/`0` (the PVE wire convention, `docs/DESIGN.md` §4) is why `boolean` also accepts
/// those two integers — the declared type is the operator's statement of intent, and
/// refusing the very encoding the API uses would flag every boolean in the store.
fn type_matches(declared: &str, value: &Value) -> bool {
    match declared {
        "string" => value.is_string(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean() || matches!(value.as_i64(), Some(0) | Some(1)),
        "object" => value.is_object(),
        "array" => value.is_array(),
        _ => true,
    }
}

fn number_text(n: f64) -> String {
    match n.fract() == 0.0 && n.abs() < 1e15 {
        true => format!("{}", n as i64),
        false => format!("{n}"),
    }
}

/// Maps each document path in `yaml` to the 1-based line its key sits on.
///
/// A scan, not a parser. The store dumps canonically — block style, two-space indent, one
/// mapping key per line — so an indent stack resolves the path of every key. Sequence
/// items are not indexed (a view addresses through maps only), and a block scalar's body
/// is skipped so a line of prose that happens to read `foo: bar` is never mistaken for a
/// key.
pub fn line_index(yaml: &str) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    // (indent, key) for each open mapping level.
    let mut stack: Vec<(usize, String)> = Vec::new();
    // While set, lines indented deeper than this are a block scalar's body.
    let mut block_at: Option<usize> = None;

    for (i, raw) in yaml.lines().enumerate() {
        let indent = raw.len() - raw.trim_start().len();
        let line = raw.trim();

        if let Some(open) = block_at {
            if line.is_empty() || indent > open {
                continue;
            }
            block_at = None;
        }
        if line.is_empty() || line.starts_with('#') || line == "---" || line == "..." {
            continue;
        }
        // A sequence item is not a mapping key, and nothing below it is addressable as
        // one either.
        if line.starts_with("- ") || line == "-" {
            continue;
        }
        let Some((key, rest)) = split_key(line) else {
            continue;
        };

        while stack.last().is_some_and(|(open, _)| *open >= indent) {
            stack.pop();
        }
        stack.push((indent, key));

        let path = stack
            .iter()
            .map(|(_, k)| k.as_str())
            .collect::<Vec<_>>()
            .join(".");
        out.insert(path, i + 1);

        let value = rest.trim();
        if value.starts_with('|') || value.starts_with('>') {
            block_at = Some(indent);
        }
    }
    out
}

/// `key: value` split, unquoting a quoted key. `None` when the line opens no mapping key.
fn split_key(line: &str) -> Option<(String, &str)> {
    if let Some(rest) = line.strip_prefix('"') {
        let mut key = String::new();
        let mut chars = rest.char_indices();
        while let Some((i, c)) = chars.next() {
            match c {
                '\\' => {
                    if let Some((_, escaped)) = chars.next() {
                        key.push(escaped);
                    }
                }
                '"' => {
                    let after = &rest[i + 1..];
                    return after.strip_prefix(':').map(|r| (key, r));
                }
                _ => key.push(c),
            }
        }
        return None;
    }
    let (key, rest) = line.split_once(':')?;
    if key.is_empty() || key.contains(' ') && !key.trim_end().eq(key) {
        return None;
    }
    let key = key.trim_end();
    match key.is_empty() {
        true => None,
        false => Some((key.to_string(), rest)),
    }
}

/// A finding paired with the line to underline, dropping any whose path the text does not
/// carry — a grammar can object to a key a canonical dump renders somewhere this scan
/// does not reach (inside a sequence, say), and a marker on the wrong line is worse than
/// no marker.
pub fn placed(findings: &[Finding], index: &BTreeMap<String, usize>) -> Vec<(usize, String)> {
    findings
        .iter()
        .filter_map(|f| index.get(&f.path).map(|line| (*line, f.message.clone())))
        .collect()
}

/// The hover text for the key at `path`: its declared type, description and default.
/// `None` when no grammar describes it.
pub fn hover_text(schema: &Value) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(t) = schema_type(schema) {
        parts.push(match schema_format(schema) {
            Some(f) => format!("{t} ({f})"),
            None => t.to_string(),
        });
    }
    if let Some(values) = schema_enum(schema) {
        parts.push(format!("one of: {}", values.join(", ")));
    }
    match (schema_minimum(schema), schema_maximum(schema)) {
        (Some(min), Some(max)) => parts.push(format!("{}..{}", number_text(min), number_text(max))),
        (Some(min), None) => parts.push(format!("at least {}", number_text(min))),
        (None, Some(max)) => parts.push(format!("at most {}", number_text(max))),
        (None, None) => {}
    }
    if let Some(d) = schema.get("default") {
        parts.push(format!("default: {}", scalar_to_string(d)));
    }
    if let Some(Value::String(desc)) = schema.get("description") {
        parts.push(desc.clone());
    }
    match parts.is_empty() {
        true => None,
        false => Some(parts.join(" · ")),
    }
}

/// Every schema node the applicable grammars declare, by document path — the hover index.
pub fn schema_index(applicable: &[(String, Value)]) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    for (prefix, schema) in applicable {
        collect(schema, prefix, &mut out);
    }
    out
}

fn collect(schema: &Value, path: &str, out: &mut BTreeMap<String, Value>) {
    out.insert(path.to_string(), schema.clone());
    let Some(properties) = schema_properties(schema) else {
        return;
    };
    for (key, child) in properties {
        let child_path = match path.is_empty() {
            true => key.clone(),
            false => format!("{path}.{key}"),
        };
        collect(child, &child_path, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn traefik() -> Vec<(String, Value)> {
        vec![(
            "traefik".to_string(),
            json!({
                "type": "object",
                "properties": {
                    "spec": {
                        "type": "object",
                        "properties": {
                            "host": {"type": "string", "format": "dns-name",
                                     "description": "Public host name"},
                            "port": {"type": "integer", "minimum": 1, "maximum": 65535,
                                     "default": 80},
                            "scheme": {"type": "string", "enum": ["http", "https"]},
                            "enabled": {"type": "boolean"},
                        },
                    },
                },
            }),
        )]
    }

    #[test]
    fn a_clean_document_has_no_findings() {
        let data = json!({"traefik": {"spec": {
            "host": "a.example", "port": 80, "scheme": "https", "enabled": true,
        }}});
        assert!(findings(&data, &traefik()).is_empty());
    }

    #[test]
    fn each_rule_is_reported_at_its_own_path() {
        let data = json!({"traefik": {"spec": {
            "host": "not a host", "port": 70000, "scheme": "ftp", "enabled": "yes",
        }}});
        let got = findings(&data, &traefik());
        let by_path: Vec<(&str, &str)> = got
            .iter()
            .map(|f| (f.path.as_str(), f.message.as_str()))
            .collect();
        assert_eq!(
            by_path,
            vec![
                ("traefik.spec.enabled", "expected boolean"),
                ("traefik.spec.host", "not a valid dns-name"),
                ("traefik.spec.port", "must be at most 65535"),
                ("traefik.spec.scheme", "expected one of: http, https"),
            ]
        );
    }

    #[test]
    fn a_boolean_on_the_wire_as_one_or_zero_is_not_a_finding() {
        // `docs/DESIGN.md` §4: the JSON view renders booleans as 1/0. Flagging those
        // would put a warning on every boolean in the store.
        let data = json!({"traefik": {"spec": {"enabled": 1}}});
        assert!(findings(&data, &traefik()).is_empty());
        let data = json!({"traefik": {"spec": {"enabled": 0}}});
        assert!(findings(&data, &traefik()).is_empty());
        let data = json!({"traefik": {"spec": {"enabled": 2}}});
        assert_eq!(findings(&data, &traefik()).len(), 1);
    }

    #[test]
    fn keys_no_grammar_describes_are_left_alone() {
        let data = json!({
            "traefik": {"spec": {"host": "a.example"}, "extra": {"anything": [1, 2]}},
            "mine": {"whatever": "goes"},
        });
        assert!(findings(&data, &traefik()).is_empty());
    }

    #[test]
    fn a_prefix_with_nothing_under_it_contributes_nothing() {
        assert!(findings(&json!({}), &traefik()).is_empty());
    }

    #[test]
    fn the_line_index_resolves_nested_paths() {
        let yaml = "\
traefik:
  spec:
    host: a.example
    port: 80
  routers:
    - rule: Host(`a`)
netbird:
  groups:
    - lan
";
        let index = line_index(yaml);
        assert_eq!(index.get("traefik"), Some(&1));
        assert_eq!(index.get("traefik.spec"), Some(&2));
        assert_eq!(index.get("traefik.spec.host"), Some(&3));
        assert_eq!(index.get("traefik.spec.port"), Some(&4));
        assert_eq!(index.get("traefik.routers"), Some(&5));
        assert_eq!(index.get("netbird.groups"), Some(&8));
        // A sequence item is not a mapping key and nothing under it is indexed.
        assert_eq!(index.get("traefik.routers.rule"), None);
    }

    #[test]
    fn a_block_scalar_body_is_never_read_as_keys() {
        let yaml = "\
compose:
  file: |
    services:
      web:
        image: nginx
  name: stack
";
        let index = line_index(yaml);
        assert_eq!(index.get("compose.file"), Some(&2));
        assert_eq!(index.get("compose.name"), Some(&6));
        // The body's `services:` / `web:` lines are content, not document keys.
        assert_eq!(index.get("compose.file.services"), None);
        assert_eq!(index.get("compose.services"), None);
    }

    #[test]
    fn comments_markers_and_quoted_keys() {
        let yaml = "\
---
# a comment
host__: the note about host
host: a.example
\"quoted: key\": 1
";
        let index = line_index(yaml);
        assert_eq!(index.get("host__"), Some(&3));
        assert_eq!(index.get("host"), Some(&4));
        assert_eq!(index.get("quoted: key"), Some(&5));
    }

    #[test]
    fn findings_are_placed_on_their_lines_and_unplaceable_ones_dropped() {
        let yaml = "traefik:\n  spec:\n    port: 70000\n";
        let data = json!({"traefik": {"spec": {"port": 70000}}});
        let placed = placed(&findings(&data, &traefik()), &line_index(yaml));
        assert_eq!(placed, vec![(3, "must be at most 65535".to_string())]);

        // The same finding against text that does not carry the key is dropped rather
        // than pointed at the wrong line.
        assert!(placed_empty(&data));
    }

    fn placed_empty(data: &Value) -> bool {
        placed(&findings(data, &traefik()), &line_index("unrelated: 1\n")).is_empty()
    }

    #[test]
    fn hover_text_reads_as_one_line() {
        let index = schema_index(&traefik());
        assert_eq!(
            hover_text(index.get("traefik.spec.port").unwrap()).as_deref(),
            Some("integer · 1..65535 · default: 80"),
        );
        assert_eq!(
            hover_text(index.get("traefik.spec.host").unwrap()).as_deref(),
            Some("string (dns-name) · Public host name"),
        );
        assert_eq!(
            hover_text(index.get("traefik.spec.scheme").unwrap()).as_deref(),
            Some("string · one of: http, https"),
        );
        assert!(index.contains_key("traefik"));
        assert!(!index.contains_key("traefik.spec.nope"));
    }
}
