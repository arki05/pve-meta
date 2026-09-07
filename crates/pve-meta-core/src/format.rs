//! The three supported serialization formats (YAML, TOML, JSON): canonical
//! parse/dump, with format-specific validation.

use std::fmt;
use std::str::FromStr;

use saphyr_parser::{Event, Parser};
use serde_json::{Map, Number};

use crate::error::Error;
use crate::model::{self, Value};
use crate::path::Path;

/// A supported document serialization format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    /// YAML (block style on dump).
    Yaml,
    /// TOML.
    Toml,
    /// JSON (pretty-printed on dump).
    Json,
}

impl Format {
    /// All supported formats, in a stable order.
    pub const ALL: [Format; 3] = [Format::Yaml, Format::Toml, Format::Json];

    /// The canonical file extension (`yaml`, `toml`, `json`); never `yml`.
    pub fn ext(&self) -> &'static str {
        match self {
            Format::Yaml => "yaml",
            Format::Toml => "toml",
            Format::Json => "json",
        }
    }

    /// Parses a file extension into a `Format`. Accepts `yml` as an alias for
    /// `yaml`. Case-insensitive.
    pub fn from_ext(ext: &str) -> Option<Format> {
        match ext.to_ascii_lowercase().as_str() {
            "yaml" | "yml" => Some(Format::Yaml),
            "toml" => Some(Format::Toml),
            "json" => Some(Format::Json),
            _ => None,
        }
    }
}

impl fmt::Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.ext())
    }
}

impl FromStr for Format {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Error> {
        Format::from_ext(s).ok_or_else(|| Error::InvalidName(format!("unknown format '{s}'")))
    }
}

/// Parses `text` as `format` into a [`Value`], with no [`model::lint`]
/// pass -- used directly by [`parse`] (which adds the full document lint)
/// and by [`crate::view::parse`] (which adds [`model::lint_relaxed`]
/// instead, since a view's value need not be an object at its own root).
///
/// # Errors
/// [`Error::Parse`] on a syntax error (or a format-specific rejection: TOML
/// datetimes; YAML anchors, aliases, explicit tags, or non-string keys).
pub(crate) fn parse_raw(format: Format, text: &str) -> Result<Value, Error> {
    match format {
        Format::Json => {
            serde_json::from_str(text).map_err(|e| Error::Parse { format, msg: e.to_string() })
        }
        Format::Yaml => parse_yaml(text),
        Format::Toml => parse_toml(text),
    }
}

/// Parses `text` as `format`, then runs [`model::lint`] on the result.
///
/// # Errors
/// [`Error::Parse`] on a syntax error (or a format-specific rejection: TOML
/// datetimes; YAML anchors, aliases, explicit tags, or non-string keys).
/// [`Error::Lint`] if the parsed value fails document-model validation.
pub fn parse(format: Format, text: &str) -> Result<Value, Error> {
    let value = parse_raw(format, text)?;
    let lints = model::lint(&value);
    if !lints.is_empty() {
        return Err(Error::Lint(lints));
    }
    Ok(value)
}

/// Dumps `doc` in canonical form for `format`. Always ends with a single
/// trailing newline and preserves key order. `doc` is assumed to already
/// satisfy [`model::lint`] (this function does not itself validate it).
pub fn dump(format: Format, doc: &Value) -> String {
    let text = match format {
        Format::Json => serde_json::to_string_pretty(doc).expect("json dump of a valid document"),
        Format::Yaml => serde_yaml_ng::to_string(doc).expect("yaml dump of a valid document"),
        Format::Toml => dump_toml(doc).expect("toml dump of a valid document"),
    };
    ensure_single_trailing_newline(text)
}

fn ensure_single_trailing_newline(mut s: String) -> String {
    while s.ends_with('\n') {
        s.pop();
    }
    s.push('\n');
    s
}

// ---------------------------------------------------------------------
// YAML
// ---------------------------------------------------------------------

fn parse_yaml(text: &str) -> Result<Value, Error> {
    scan_yaml_safety(text)?;
    serde_yaml_ng::from_str::<Value>(text).map_err(|e| Error::Parse {
        format: Format::Yaml,
        msg: e.to_string(),
    })
}

/// Context used while walking saphyr's event stream to reject anchors,
/// aliases, explicit tags and non-string (complex) mapping keys.
enum Ctx {
    Seq,
    Map { expect_key: bool },
}

fn yaml_err(msg: impl Into<String>) -> Error {
    Error::Parse {
        format: Format::Yaml,
        msg: msg.into(),
    }
}

fn scan_yaml_safety(text: &str) -> Result<(), Error> {
    let parser = Parser::new_from_str(text);
    let mut stack: Vec<Ctx> = Vec::new();

    // Records that a scalar/sequence/mapping is being consumed as a value
    // (toggling the enclosing map's expect_key back to `true`) or as a key
    // (toggling it to `false`, awaiting the value).
    fn note_child(stack: &mut [Ctx]) {
        if let Some(Ctx::Map { expect_key }) = stack.last_mut() {
            *expect_key = !*expect_key;
        }
    }

    fn reject_if_key_position(stack: &[Ctx]) -> Result<(), Error> {
        if let Some(Ctx::Map { expect_key: true }) = stack.last() {
            return Err(yaml_err(
                "non-string (complex) mapping keys are not allowed",
            ));
        }
        Ok(())
    }

    for ev in parser {
        let (event, _span) = ev.map_err(|e| yaml_err(e.to_string()))?;
        match &event {
            Event::Alias(_) => {
                return Err(yaml_err("YAML aliases are not allowed"));
            }
            Event::Scalar(_, _, anchor, tag) => {
                if *anchor != 0 {
                    return Err(yaml_err("YAML anchors are not allowed"));
                }
                if tag.is_some() {
                    return Err(yaml_err("YAML explicit tags are not allowed"));
                }
                note_child(&mut stack);
            }
            Event::SequenceStart(anchor, tag) => {
                if *anchor != 0 {
                    return Err(yaml_err("YAML anchors are not allowed"));
                }
                if tag.is_some() {
                    return Err(yaml_err("YAML explicit tags are not allowed"));
                }
                reject_if_key_position(&stack)?;
                stack.push(Ctx::Seq);
            }
            Event::SequenceEnd => {
                stack.pop();
                note_child(&mut stack);
            }
            Event::MappingStart(anchor, tag) => {
                if *anchor != 0 {
                    return Err(yaml_err("YAML anchors are not allowed"));
                }
                if tag.is_some() {
                    return Err(yaml_err("YAML explicit tags are not allowed"));
                }
                reject_if_key_position(&stack)?;
                stack.push(Ctx::Map { expect_key: true });
            }
            Event::MappingEnd => {
                stack.pop();
                note_child(&mut stack);
            }
            _ => {}
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------
// TOML
// ---------------------------------------------------------------------

fn toml_parse_err(e: impl fmt::Display) -> Error {
    Error::Parse {
        format: Format::Toml,
        msg: e.to_string(),
    }
}

fn parse_toml(text: &str) -> Result<Value, Error> {
    let doc: toml_edit::DocumentMut = text.parse().map_err(toml_parse_err)?;
    toml_table_to_json(doc.as_table(), &Path::root())
}

fn toml_table_to_json(t: &toml_edit::Table, path: &Path) -> Result<Value, Error> {
    let mut map = Map::new();
    for (k, item) in t.iter() {
        let child_path = path.join(k.to_string());
        map.insert(k.to_string(), toml_item_to_json(item, &child_path)?);
    }
    Ok(Value::Object(map))
}

fn toml_item_to_json(item: &toml_edit::Item, path: &Path) -> Result<Value, Error> {
    match item {
        toml_edit::Item::None => Ok(Value::Null),
        toml_edit::Item::Value(v) => toml_value_to_json(v, path),
        toml_edit::Item::Table(t) => toml_table_to_json(t, path),
        toml_edit::Item::ArrayOfTables(aot) => {
            let mut out = Vec::with_capacity(aot.len());
            for (i, t) in aot.iter().enumerate() {
                out.push(toml_table_to_json(t, &path.join(i.to_string()))?);
            }
            Ok(Value::Array(out))
        }
    }
}

fn toml_value_to_json(v: &toml_edit::Value, path: &Path) -> Result<Value, Error> {
    match v {
        toml_edit::Value::String(s) => Ok(Value::String(s.value().clone())),
        toml_edit::Value::Integer(i) => Ok(Value::Number(Number::from(*i.value()))),
        toml_edit::Value::Float(f) => Number::from_f64(*f.value())
            .map(Value::Number)
            .ok_or_else(|| toml_parse_err(format!("{path}: non-finite float"))),
        toml_edit::Value::Boolean(b) => Ok(Value::Bool(*b.value())),
        toml_edit::Value::Datetime(_) => Err(toml_parse_err(format!(
            "{path}: datetime values are not supported"
        ))),
        toml_edit::Value::Array(arr) => {
            let mut out = Vec::with_capacity(arr.len());
            for (i, item) in arr.iter().enumerate() {
                out.push(toml_value_to_json(item, &path.join(i.to_string()))?);
            }
            Ok(Value::Array(out))
        }
        toml_edit::Value::InlineTable(t) => {
            let mut map = Map::new();
            for (k, v) in t.iter() {
                map.insert(
                    k.to_string(),
                    toml_value_to_json(v, &path.join(k.to_string()))?,
                );
            }
            Ok(Value::Object(map))
        }
    }
}

/// Builds a fresh `toml_edit` document from `doc`: nested objects become
/// explicit `[a.b]` tables (never inline tables, never dotted keys); arrays
/// whose elements are all objects become `[[a.b]]` arrays of tables; every
/// other value (scalars, arrays of scalars, or heterogeneous arrays whose
/// object elements fall back to inline tables) is written as `key = value`.
fn dump_toml(doc: &Value) -> Result<String, Error> {
    let map = doc
        .as_object()
        .ok_or_else(|| toml_parse_err("document must be an object"))?;
    let table = build_toml_table(map, &Path::root())?;
    let doc_mut: toml_edit::DocumentMut = table.into();
    Ok(doc_mut.to_string())
}

fn build_toml_table(
    map: &Map<String, Value>,
    path: &Path,
) -> Result<toml_edit::Table, Error> {
    let mut table = toml_edit::Table::new();
    table.set_implicit(false);
    for (k, v) in map.iter() {
        let child_path = path.join(k.clone());
        match v {
            Value::Object(sub) => {
                let sub_table = build_toml_table(sub, &child_path)?;
                table.insert(k, toml_edit::Item::Table(sub_table));
            }
            Value::Array(items) if !items.is_empty() && items.iter().all(Value::is_object) => {
                let mut aot = toml_edit::ArrayOfTables::new();
                for (i, item) in items.iter().enumerate() {
                    let obj = item.as_object().expect("checked all-object above");
                    aot.push(build_toml_table(obj, &child_path.join(i.to_string()))?);
                }
                table.insert(k, toml_edit::Item::ArrayOfTables(aot));
            }
            _ => {
                let tv = json_to_toml_value(v, &child_path)?;
                table.insert(k, toml_edit::Item::Value(tv));
            }
        }
    }
    Ok(table)
}

/// Converts a JSON value that is *not* directly a table-shaped object at the
/// document/table level (scalars, arrays, and inline-table fallbacks for
/// objects nested inside arrays) into a `toml_edit::Value`.
pub(crate) fn json_to_toml_value(v: &Value, path: &Path) -> Result<toml_edit::Value, Error> {
    match v {
        Value::Null => Err(toml_parse_err(format!("{path}: null is not representable in TOML"))),
        Value::Bool(b) => Ok((*b).into()),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(i.into())
            } else if let Some(f) = n.as_f64() {
                Ok(f.into())
            } else {
                Err(toml_parse_err(format!("{path}: number out of range for TOML")))
            }
        }
        Value::String(s) => Ok(s.as_str().into()),
        Value::Array(items) => {
            let mut arr = toml_edit::Array::new();
            for (i, item) in items.iter().enumerate() {
                arr.push_formatted(json_to_toml_value(item, &path.join(i.to_string()))?);
            }
            Ok(toml_edit::Value::Array(arr))
        }
        Value::Object(obj) => {
            let mut t = toml_edit::InlineTable::new();
            for (k, val) in obj.iter() {
                t.insert(k, json_to_toml_value(val, &path.join(k.clone()))?);
            }
            Ok(toml_edit::Value::InlineTable(t))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    #[test]
    fn format_ext_and_from_ext() {
        assert_eq!(Format::Yaml.ext(), "yaml");
        assert_eq!(Format::from_ext("yml"), Some(Format::Yaml));
        assert_eq!(Format::from_ext("YAML"), Some(Format::Yaml));
        assert_eq!(Format::from_ext("toml"), Some(Format::Toml));
        assert_eq!(Format::from_ext("json"), Some(Format::Json));
        assert_eq!(Format::from_ext("ini"), None);
    }

    #[test]
    fn format_display_and_from_str() {
        assert_eq!(Format::Toml.to_string(), "toml");
        assert_eq!("toml".parse::<Format>().unwrap(), Format::Toml);
        assert_eq!("yml".parse::<Format>().unwrap(), Format::Yaml);
        assert!("nope".parse::<Format>().is_err());
    }

    #[test]
    fn json_parse_and_dump() {
        let doc = json!({"b": 1, "a": 2});
        let text = dump(Format::Json, &doc);
        assert!(text.ends_with('\n'));
        assert!(!text.ends_with("\n\n"));
        let back = parse(Format::Json, &text).unwrap();
        assert_eq!(back, doc);
        // order preserved in the dumped text
        assert!(text.find("\"b\"").unwrap() < text.find("\"a\"").unwrap());
    }

    #[test]
    fn json_rejects_comments() {
        let err = parse(Format::Json, "{ \"a\": 1 /* c */ }").unwrap_err();
        assert!(matches!(err, Error::Parse { .. }));
    }

    #[test]
    fn yaml_parse_and_dump_round_trip_and_order() {
        let doc = json!({"z": 1, "a": {"y": 2, "x": 3}, "list": [1, 2, 3]});
        let text = dump(Format::Yaml, &doc);
        assert!(text.ends_with('\n'));
        assert!(!text.ends_with("\n\n"));
        assert!(!text.starts_with("---"));
        let back = parse(Format::Yaml, &text).unwrap();
        assert_eq!(back, doc);
        assert!(text.find("z:").unwrap() < text.find("a:").unwrap());
    }

    #[test]
    fn yaml_1_1_words_stay_strings() {
        let doc = parse(Format::Yaml, "a: yes\nb: no\nc: on\nd: off\ne: true\nf: false\n").unwrap();
        assert_eq!(
            doc,
            json!({"a": "yes", "b": "no", "c": "on", "d": "off", "e": true, "f": false})
        );
    }

    #[test]
    fn yaml_rejects_anchors_and_aliases() {
        let err = parse(Format::Yaml, "a: &anchor 1\nb: *anchor\n").unwrap_err();
        assert!(matches!(err, Error::Parse { .. }));
        assert!(err.to_string().contains("anchor"));
    }

    #[test]
    fn yaml_rejects_explicit_tags() {
        let err = parse(Format::Yaml, "a: !!str 123\n").unwrap_err();
        assert!(matches!(err, Error::Parse { .. }));
        let err2 = parse(Format::Yaml, "a: !Custom 123\n").unwrap_err();
        assert!(matches!(err2, Error::Parse { .. }));
    }

    #[test]
    fn yaml_rejects_complex_keys() {
        let err = parse(Format::Yaml, "? [1, 2]\n: val\n").unwrap_err();
        assert!(matches!(err, Error::Parse { .. }));
    }

    #[test]
    fn yaml_rejects_null_via_lint() {
        let err = parse(Format::Yaml, "a: ~\n").unwrap_err();
        assert!(matches!(err, Error::Lint(_)));
    }

    #[test]
    fn toml_parse_and_dump_round_trip() {
        let doc = json!({
            "z": 1,
            "a": {"y": 2, "x": [1, 2, 3]},
            "items": [{"n": 1}, {"n": 2}],
        });
        let text = dump(Format::Toml, &doc);
        assert!(text.ends_with('\n'));
        assert!(!text.ends_with("\n\n"));
        assert!(text.contains("[a]"));
        assert!(text.contains("[[items]]"));
        assert!(!text.contains("{"));
        let back = parse(Format::Toml, &text).unwrap();
        assert_eq!(back, doc);
    }

    #[test]
    fn toml_rejects_datetime() {
        let err = parse(Format::Toml, "d = 1979-05-27T07:32:00Z\n").unwrap_err();
        match err {
            Error::Parse { format, msg } => {
                assert_eq!(format, Format::Toml);
                assert!(msg.contains('d'), "message should name the path: {msg}");
            }
            other => panic!("expected Parse error, got {other:?}"),
        }
    }

    #[test]
    fn cross_format_preserves_order() {
        let doc = json!({"z": 1, "a": 2, "m": 3});
        for src in Format::ALL {
            let text = dump(src, &doc);
            let back = parse(src, &text).unwrap();
            assert_eq!(back, doc);
            for dst in Format::ALL {
                let converted_text = dump(dst, &back);
                let converted_val = parse(dst, &converted_text).unwrap();
                assert_eq!(converted_val, doc, "round trip {src:?} -> {dst:?} failed");
            }
        }
    }
}
