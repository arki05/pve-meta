//! A small subset of JSON Schema, enough to drive the form generator: `type`,
//! `properties`, `required`, `enum`, `description`, `title`, `minimum`, `maximum`,
//! `items` (for scalar arrays), `default`. Anything else is ignored.
//!
//! Pure module: no `web-sys`/`wasm-bindgen`, safe to unit-test natively.

use serde_json::Value;

/// The shape a property's widget should take.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldKind {
    String,
    Number {
        minimum: Option<f64>,
        maximum: Option<f64>,
    },
    Integer {
        minimum: Option<f64>,
        maximum: Option<f64>,
    },
    Boolean,
    /// `enum` of string choices (a combobox).
    Enum(Vec<String>),
    /// A nested object schema, recursively parsed.
    Object(Vec<FieldSpec>),
    /// An array; `items` is the (scalar) item schema, if known.
    Array { items: Option<Box<FieldSpec>> },
    /// Present in the schema but not one of the constructs we understand.
    Unknown,
}

/// One schema-described property.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldSpec {
    pub key: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub kind: FieldKind,
    pub required: bool,
    pub default: Option<Value>,
}

impl FieldSpec {
    /// Label to show in the UI: `title` if present, else the raw key.
    pub fn label(&self) -> &str {
        self.title.as_deref().unwrap_or(&self.key)
    }
}

/// Parse the `properties`/`required` of an object schema into an ordered list of
/// [`FieldSpec`]s (order follows the JSON object's own key order, requires
/// `serde_json`'s `preserve_order` feature). Returns `[]` if `schema` has no
/// `properties` object (e.g. it isn't an object schema, or is `null`/absent).
pub fn parse_object_schema(schema: &Value) -> Vec<FieldSpec> {
    let properties = match schema.get("properties").and_then(Value::as_object) {
        Some(p) => p,
        None => return Vec::new(),
    };
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();

    properties
        .iter()
        .map(|(key, prop)| parse_field(key, prop, required.contains(&key.as_str())))
        .collect()
}

fn parse_field(key: &str, prop: &Value, required: bool) -> FieldSpec {
    let title = prop.get("title").and_then(Value::as_str).map(String::from);
    let description = prop
        .get("description")
        .and_then(Value::as_str)
        .map(String::from);
    let default = prop.get("default").cloned();
    let kind = parse_kind(prop);

    FieldSpec {
        key: key.to_string(),
        title,
        description,
        kind,
        required,
        default,
    }
}

fn parse_kind(prop: &Value) -> FieldKind {
    if let Some(values) = prop.get("enum").and_then(Value::as_array) {
        return FieldKind::Enum(
            values
                .iter()
                .map(|v| match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .collect(),
        );
    }

    match prop.get("type").and_then(Value::as_str) {
        Some("boolean") => FieldKind::Boolean,
        Some("integer") => FieldKind::Integer {
            minimum: prop.get("minimum").and_then(Value::as_f64),
            maximum: prop.get("maximum").and_then(Value::as_f64),
        },
        Some("number") => FieldKind::Number {
            minimum: prop.get("minimum").and_then(Value::as_f64),
            maximum: prop.get("maximum").and_then(Value::as_f64),
        },
        Some("object") => FieldKind::Object(parse_object_schema(prop)),
        Some("array") => {
            let items = prop
                .get("items")
                .map(|items_schema| Box::new(parse_field("items", items_schema, false)));
            FieldKind::Array { items }
        }
        Some("string") | None => FieldKind::String,
        _ => FieldKind::Unknown,
    }
}

/// Find the schema (from a `GET /meta/schemas/{vmid}` map keyed by namespace prefix)
/// that applies to `namespace`, if any. Prefixes are matched exactly against the
/// top-level namespace key (the registry's `claims[].prefix` is the same string as the
/// document's top-level key in practice).
pub fn schema_for_namespace<'a>(
    schemas: &'a std::collections::HashMap<String, Value>,
    namespace: &str,
) -> Option<&'a Value> {
    schemas.get(namespace)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_basic_scalar_types() {
        let schema = json!({
            "type": "object",
            "properties": {
                "host": {"type": "string", "title": "Host", "description": "public hostname"},
                "port": {"type": "integer", "minimum": 1, "maximum": 65535, "default": 8080},
                "weight": {"type": "number", "minimum": 0.0},
                "enabled": {"type": "boolean", "default": true}
            },
            "required": ["host"]
        });
        let fields = parse_object_schema(&schema);
        assert_eq!(fields.len(), 4);

        let host = &fields[0];
        assert_eq!(host.key, "host");
        assert_eq!(host.label(), "Host");
        assert_eq!(host.description.as_deref(), Some("public hostname"));
        assert!(host.required);
        assert_eq!(host.kind, FieldKind::String);

        let port = &fields[1];
        assert!(!port.required);
        assert_eq!(
            port.kind,
            FieldKind::Integer {
                minimum: Some(1.0),
                maximum: Some(65535.0)
            }
        );
        assert_eq!(port.default, Some(json!(8080)));

        let weight = &fields[2];
        assert_eq!(
            weight.kind,
            FieldKind::Number {
                minimum: Some(0.0),
                maximum: None
            }
        );

        let enabled = &fields[3];
        assert_eq!(enabled.kind, FieldKind::Boolean);
    }

    #[test]
    fn parses_enum_as_combobox_source() {
        let schema = json!({
            "type": "object",
            "properties": {
                "scope": {"type": "string", "enum": ["rw", "ro"]}
            }
        });
        let fields = parse_object_schema(&schema);
        assert_eq!(
            fields[0].kind,
            FieldKind::Enum(vec!["rw".to_string(), "ro".to_string()])
        );
    }

    #[test]
    fn parses_nested_object() {
        let schema = json!({
            "type": "object",
            "properties": {
                "spec": {
                    "type": "object",
                    "properties": {
                        "host": {"type": "string"}
                    },
                    "required": ["host"]
                }
            }
        });
        let fields = parse_object_schema(&schema);
        match &fields[0].kind {
            FieldKind::Object(nested) => {
                assert_eq!(nested.len(), 1);
                assert_eq!(nested[0].key, "host");
                assert!(nested[0].required);
            }
            other => panic!("expected nested object, got {other:?}"),
        }
    }

    #[test]
    fn parses_array_of_scalars() {
        let schema = json!({
            "type": "object",
            "properties": {
                "tags": {"type": "array", "items": {"type": "string"}}
            }
        });
        let fields = parse_object_schema(&schema);
        match &fields[0].kind {
            FieldKind::Array { items: Some(item) } => assert_eq!(item.kind, FieldKind::String),
            other => panic!("expected array of strings, got {other:?}"),
        }
    }

    #[test]
    fn missing_properties_yields_empty_list() {
        assert_eq!(parse_object_schema(&json!({"type": "object"})), Vec::new());
        assert_eq!(parse_object_schema(&Value::Null), Vec::new());
    }

    #[test]
    fn unknown_type_falls_back_gracefully() {
        let schema = json!({
            "type": "object",
            "properties": {
                "weird": {"type": "something-unsupported"}
            }
        });
        let fields = parse_object_schema(&schema);
        assert_eq!(fields[0].kind, FieldKind::Unknown);
    }

    #[test]
    fn preserves_property_order() {
        let schema = json!({
            "type": "object",
            "properties": {
                "z_first": {"type": "string"},
                "a_second": {"type": "string"}
            }
        });
        let fields = parse_object_schema(&schema);
        assert_eq!(fields[0].key, "z_first");
        assert_eq!(fields[1].key, "a_second");
    }
}
