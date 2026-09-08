//! Turning one row edit into one request (`docs/DESIGN.md` §8).
//!
//! > "A row edit is `PUT ?view=<path>&mode=replace` with the scalar; add is the same at a
//! > new path; delete is `DELETE ?view=<path>`. The digest is sent and 409 reloads."
//!
//! Everything here is the *construction* of that request — parsing what the user typed
//! into a JSON value, and building the body the API module sends — so it can be
//! unit-tested natively (`cargo test --lib`) rather than only through the browser.
//!
//! Two things a write must never do, both enforced here rather than left to the server:
//!
//! * send an empty `digest` as an expected one (an empty digest means "no document yet",
//!   and passing it through would turn every first write into a 409);
//! * write a `null`. A replace payload is document content, and a document has no nulls
//!   (`docs/DESIGN.md` §2); the store refuses one with a 400.

use serde_json::{Map, Value, json};

use crate::tree::ValueKind;

/// One request in an edit. A row edit is usually one [`Write::Put`]; editing a row's
/// description alongside its value is two, applied in order against the digest each one
/// returns.
#[derive(Debug, Clone, PartialEq)]
pub enum Write {
    /// `PUT ?view=<view>&mode=replace` with a JSON payload.
    Put { view: String, value: Value },
    /// `PUT ?view=<view>&mode=replace` with a YAML (or JSON — YAML is a superset) text
    /// payload. Only the "Edit as text" dialog produces this.
    PutText { view: String, text: String },
    /// `DELETE ?view=<view>`.
    Delete { view: String },
}

/// The JSON body of a `PUT` carrying a `data` payload.
///
/// `data` is a JSON-*encoded string* parameter (`docs/DESIGN.md` §5), so a scalar row's
/// value goes out as `"\"ct200.example\""`, not as a bare JSON value.
pub fn put_body(view: &str, value: &Value, digest: &str) -> Value {
    let mut body = Map::new();
    body.insert("mode".into(), json!("replace"));
    body.insert("data".into(), json!(value.to_string()));
    add_view(&mut body, view);
    add_digest(&mut body, digest);
    Value::Object(body)
}

/// The JSON body of a `PUT` carrying a `text` payload.
///
/// No `format` key: the endpoint infers the wire format from which payload parameter is
/// sent (`text` is YAML, `data` is JSON) and rejects `format` outright.
pub fn put_text_body(view: &str, text: &str, digest: &str) -> Value {
    let mut body = Map::new();
    body.insert("mode".into(), json!("replace"));
    body.insert("text".into(), json!(text));
    add_view(&mut body, view);
    add_digest(&mut body, digest);
    Value::Object(body)
}

/// The JSON body of a `DELETE`.
pub fn delete_body(view: &str, digest: &str) -> Value {
    let mut body = Map::new();
    add_view(&mut body, view);
    add_digest(&mut body, digest);
    Value::Object(body)
}

fn add_view(body: &mut Map<String, Value>, view: &str) {
    if !view.is_empty() {
        body.insert("view".into(), json!(view));
    }
}

fn add_digest(body: &mut Map<String, Value>, digest: &str) {
    // An empty digest means "no document yet" and must not be sent as an expected one.
    if !digest.is_empty() {
        body.insert("digest".into(), json!(digest));
    }
}

/// Parse what the user typed into the value a row of `kind` should hold.
///
/// The error strings are shown in the edit dialog, so they name what was expected rather
/// than echoing a parser's internals.
pub fn parse_value(kind: &ValueKind, input: &str) -> Result<Value, String> {
    let trimmed = input.trim();
    match kind {
        ValueKind::Text => Ok(Value::String(input.to_string())),
        ValueKind::Enum(values) => match values.iter().any(|v| v == trimmed) {
            true => Ok(Value::String(trimmed.to_string())),
            false => Err(format!("expected one of: {}", values.join(", "))),
        },
        ValueKind::Boolean => match trimmed.to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "on" => Ok(Value::Bool(true)),
            "false" | "0" | "no" | "off" | "" => Ok(Value::Bool(false)),
            _ => Err("expected a boolean".to_string()),
        },
        ValueKind::Integer => trimmed
            .parse::<i64>()
            .map(|number| json!(number))
            .map_err(|_| "expected a whole number".to_string()),
        ValueKind::Number => trimmed
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
            .ok_or_else(|| "expected a number".to_string()),
        ValueKind::Array => {
            let value: Value =
                serde_json::from_str(trimmed).map_err(|err| format!("invalid JSON: {err}"))?;
            match value.is_array() {
                true => reject_nulls(value),
                false => Err("expected a JSON array".to_string()),
            }
        }
        ValueKind::Map => {
            let value: Value =
                serde_json::from_str(trimmed).map_err(|err| format!("invalid JSON: {err}"))?;
            match value.is_object() {
                true => reject_nulls(value),
                false => Err("expected a JSON object".to_string()),
            }
        }
    }
}

/// A document has no nulls (`docs/DESIGN.md` §2); refuse one here, where the message can
/// still point at the field, instead of collecting a 400 from the store's lint.
fn reject_nulls(value: Value) -> Result<Value, String> {
    fn has_null(value: &Value) -> bool {
        match value {
            Value::Null => true,
            Value::Array(items) => items.iter().any(has_null),
            Value::Object(map) => map.values().any(has_null),
            _ => false,
        }
    }
    match has_null(&value) {
        true => Err("a document cannot contain null".to_string()),
        false => Ok(value),
    }
}

/// The text a field starts with when editing an existing value.
pub fn field_text(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
    }
}

/// Validate a key typed into the Add dialog.
///
/// `.` is the path separator (`docs/DESIGN.md` §2), so it cannot appear in one segment;
/// everything else — including a comment key — is ordinary data and allowed.
pub fn validate_key(key: &str) -> Result<(), String> {
    if key.is_empty() {
        return Err("the key must not be empty".to_string());
    }
    if key.contains('.') {
        return Err("'.' separates path segments and cannot be part of a key".to_string());
    }
    if key.trim() != key {
        return Err("the key must not start or end with whitespace".to_string());
    }
    Ok(())
}

/// The writes one row edit performs: the value, and — when it changed — the comment key
/// that documents it (`comment`: the sibling `key__`, or a map's own `key.__`).
///
/// A cleared description removes the comment key rather than storing an empty string; a
/// description on a row that is only being *set* is written after the value, so the
/// intermediate document is never a note about a key that does not exist yet.
pub fn row_writes(
    path: &str,
    comment: &str,
    value: Option<Value>,
    description: Option<&str>,
    had_description: bool,
) -> Vec<Write> {
    let mut writes = Vec::new();
    if let Some(value) = value {
        writes.push(Write::Put {
            view: path.to_string(),
            value,
        });
    }

    let comment = comment.to_string();
    match description.map(str::trim) {
        Some(text) if !text.is_empty() => writes.push(Write::Put {
            view: comment,
            value: Value::String(text.to_string()),
        }),
        Some(_) if had_description => writes.push(Write::Delete { view: comment }),
        _ => {}
    }

    writes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scalar_edit_is_one_replace_carrying_the_digest() {
        let body = put_body("traefik.spec.host", &json!("ct200.example"), "f6179c2f7e17");
        assert_eq!(
            body,
            json!({
                "mode": "replace",
                "view": "traefik.spec.host",
                // `data` is a JSON-encoded *string* parameter (§5).
                "data": "\"ct200.example\"",
                "digest": "f6179c2f7e17",
            }),
        );
    }

    #[test]
    fn an_integer_edit_sends_a_json_number_not_a_string() {
        let body = put_body("traefik.spec.port", &json!(8080), "abc");
        assert_eq!(body["data"], json!("8080"));
    }

    #[test]
    fn an_empty_digest_is_never_sent_as_an_expected_one() {
        // A document that does not exist yet has an empty digest; sending it would 409.
        let body = put_body("notes", &json!("hi"), "");
        assert_eq!(body.get("digest"), None);
        assert_eq!(delete_body("notes", "").get("digest"), None);
    }

    #[test]
    fn the_root_view_is_omitted_rather_than_sent_empty() {
        let body = put_text_body("", "a: 1\n", "abc");
        assert_eq!(body.get("view"), None);
        assert_eq!(body["text"], json!("a: 1\n"));
        assert_eq!(body["mode"], json!("replace"));
        // No `format`: the endpoint infers it from `text` vs `data`.
        assert_eq!(body.get("format"), None);
    }

    #[test]
    fn delete_carries_only_the_view_and_the_digest() {
        assert_eq!(
            delete_body("netbird.groups", "abc"),
            json!({"view": "netbird.groups", "digest": "abc"}),
        );
    }

    #[test]
    fn values_parse_by_row_kind() {
        assert_eq!(parse_value(&ValueKind::Text, " hi "), Ok(json!(" hi ")));
        assert_eq!(parse_value(&ValueKind::Integer, " 42 "), Ok(json!(42)));
        assert!(parse_value(&ValueKind::Integer, "4.2").is_err());
        assert_eq!(parse_value(&ValueKind::Number, "1.5"), Ok(json!(1.5)));
        assert_eq!(parse_value(&ValueKind::Boolean, "TRUE"), Ok(json!(true)));
        assert_eq!(parse_value(&ValueKind::Boolean, "0"), Ok(json!(false)));
        assert!(parse_value(&ValueKind::Boolean, "maybe").is_err());
        assert_eq!(
            parse_value(&ValueKind::Array, "[\"lan\", \"wan\"]"),
            Ok(json!(["lan", "wan"])),
        );
        assert!(parse_value(&ValueKind::Array, "{}").is_err());
        assert!(parse_value(&ValueKind::Array, "[").is_err());
        assert_eq!(
            parse_value(&ValueKind::Map, "{\"a\": 1}"),
            Ok(json!({"a": 1}))
        );
    }

    #[test]
    fn an_enum_only_accepts_a_declared_value() {
        let kind = ValueKind::Enum(vec!["http".to_string(), "https".to_string()]);
        assert_eq!(parse_value(&kind, "https"), Ok(json!("https")));
        assert_eq!(
            parse_value(&kind, "ftp"),
            Err("expected one of: http, https".to_string()),
        );
    }

    #[test]
    fn null_never_reaches_the_wire() {
        // The store's lint would refuse it with a 400; say so where the field still is.
        assert!(parse_value(&ValueKind::Array, "[1, null]").is_err());
        assert!(parse_value(&ValueKind::Map, "{\"a\": null}").is_err());
    }

    #[test]
    fn keys_are_single_path_segments() {
        assert!(validate_key("host").is_ok());
        // A comment key is ordinary data (§2) and may be typed.
        assert!(validate_key("host__").is_ok());
        assert!(validate_key("").is_err());
        assert!(validate_key("a.b").is_err());
        assert!(validate_key(" a").is_err());
    }

    #[test]
    fn a_row_edit_writes_the_value_then_its_note() {
        let writes = row_writes(
            "traefik.spec.host",
            "traefik.spec.host__",
            Some(json!("h")),
            Some("public name"),
            false,
        );
        assert_eq!(
            writes,
            vec![
                Write::Put {
                    view: "traefik.spec.host".into(),
                    value: json!("h")
                },
                Write::Put {
                    view: "traefik.spec.host__".into(),
                    value: json!("public name"),
                },
            ],
        );
    }

    #[test]
    fn clearing_a_note_removes_the_comment_key() {
        let writes = row_writes("netbird.groups", "netbird.groups__", None, Some("  "), true);
        assert_eq!(
            writes,
            vec![Write::Delete {
                view: "netbird.groups__".into()
            }],
        );
        // Nothing to remove when there was no note to begin with.
        assert!(row_writes("netbird.groups", "netbird.groups__", None, Some(""), false).is_empty());
    }

    #[test]
    fn a_maps_own_note_is_written_back_where_it_came_from() {
        // A map documented by its own bare `__` must not grow a second, sibling note.
        let writes = row_writes("traefik", "traefik.__", None, Some("the router"), true);
        assert_eq!(
            writes,
            vec![Write::Put {
                view: "traefik.__".into(),
                value: json!("the router"),
            }],
        );
    }

    #[test]
    fn field_text_shows_a_string_unquoted_and_everything_else_as_json() {
        assert_eq!(field_text(Some(&json!("hi"))), "hi");
        assert_eq!(field_text(Some(&json!(7))), "7");
        assert_eq!(field_text(Some(&json!(["a"]))), "[\"a\"]");
        assert_eq!(field_text(None), "");
    }
}
