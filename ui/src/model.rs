//! Document model shared by the whole UI: `DocId`, the wire `Document` shape, and the
//! comment-key helpers used by the form renderer.
//!
//! Pure module: no `web-sys`/`wasm-bindgen`, safe to unit-test natively.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Which document is loaded: one guest's metadata, or the cluster-wide datacenter one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DocId {
    Guest(u32),
    Datacenter,
}

impl Document {
    /// A synthetic empty document for a guest/datacenter that has no metadata file
    /// yet (`GET` 404s). An empty `digest` means "no expected digest" to the write
    /// endpoints — the first `Apply` creates the file (`docs/API.md`: "Creates the
    /// document ... if it does not exist").
    pub fn empty(id: &DocId) -> Self {
        Self {
            id: id.label(),
            format: "yaml".to_string(),
            digest: String::new(),
            mtime: 0,
            data: Value::Object(Map::new()),
            raw: Some(String::new()),
            touched: Vec::new(),
        }
    }
}

impl DocId {
    /// API base path for this document, e.g. `/meta/guests/105` or `/meta/datacenter`.
    pub fn api_path(&self) -> String {
        match self {
            DocId::Guest(vmid) => format!("/meta/guests/{vmid}"),
            DocId::Datacenter => "/meta/datacenter".to_string(),
        }
    }

    /// A short id, e.g. "105" or "datacenter" (also what `Document::id` carries on the wire).
    pub fn label(&self) -> String {
        match self {
            DocId::Guest(vmid) => vmid.to_string(),
            DocId::Datacenter => "datacenter".to_string(),
        }
    }
}

/// One entry of a write's `touched` list.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Touched {
    pub path: String,
    pub op: String,
}

/// Wire representation of a guest or datacenter document (`docs/API.md` "Documents").
///
/// `touched` is only populated by responses to a write (`PUT .../{vmid}`, `.../raw`,
/// `POST .../convert`); plain `GET`s leave it empty.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Default)]
pub struct Document {
    pub id: String,
    pub format: String,
    pub digest: String,
    #[serde(default)]
    pub mtime: i64,
    #[serde(default)]
    pub data: Value,
    #[serde(default)]
    pub raw: Option<String>,
    #[serde(default)]
    pub touched: Vec<Touched>,
}

/// True if `key` is a pve-meta comment key: either the bare section comment `__`, or a
/// per-key comment `<key>__` (a non-empty key name followed by `__`).
pub fn is_comment_key(key: &str) -> bool {
    key == "__" || (key.len() > 2 && key.ends_with("__"))
}

/// The comment key that documents `key`, e.g. `host` -> `host__`.
pub fn comment_key_for(key: &str) -> String {
    format!("{key}__")
}

/// The plain key a comment key documents, e.g. `host__` -> `Some("host")`, `__` -> `None`.
pub fn key_for_comment(comment_key: &str) -> Option<&str> {
    if comment_key == "__" {
        None
    } else {
        comment_key.strip_suffix("__")
    }
}

/// Recursively strip comment keys from a `Value`, returning a new value. Used before
/// diffing/patching so comment-only edits don't get conflated with data edits when we
/// don't want them (the editor itself keeps comments inline in its working copy).
pub fn strip_comments(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = Map::new();
            for (k, v) in map {
                if is_comment_key(k) {
                    continue;
                }
                out.insert(k.clone(), strip_comments(v));
            }
            Value::Object(out)
        }
        Value::Array(arr) => Value::Array(arr.iter().map(strip_comments).collect()),
        other => other.clone(),
    }
}

/// Look up the comment text for `key` inside object `obj`, if any.
pub fn comment_for<'a>(obj: &'a Value, key: &str) -> Option<&'a str> {
    obj.get(comment_key_for(key)).and_then(Value::as_str)
}

/// The section-level comment (`__`) of an object, if any.
pub fn section_comment(obj: &Value) -> Option<&str> {
    obj.get("__").and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn comment_key_detection() {
        assert!(is_comment_key("__"));
        assert!(is_comment_key("host__"));
        assert!(!is_comment_key("_"));
        assert!(!is_comment_key("host"));
        assert!(!is_comment_key("__host")); // leading, not trailing
    }

    #[test]
    fn comment_key_round_trip() {
        assert_eq!(comment_key_for("host"), "host__");
        assert_eq!(key_for_comment("host__"), Some("host"));
        assert_eq!(key_for_comment("__"), None);
        assert_eq!(key_for_comment("host"), None);
    }

    #[test]
    fn strips_nested_comments() {
        let value = json!({
            "traefik": {
                "__": "section comment",
                "spec": {
                    "host": "wiki.example.com",
                    "host__": "public hostname",
                    "port": 8080
                }
            }
        });
        let stripped = strip_comments(&value);
        assert_eq!(
            stripped,
            json!({
                "traefik": {
                    "spec": {
                        "host": "wiki.example.com",
                        "port": 8080
                    }
                }
            })
        );
    }

    #[test]
    fn comment_lookup() {
        let obj = json!({"host": "wiki.example.com", "host__": "public hostname", "__": "sect"});
        assert_eq!(comment_for(&obj, "host"), Some("public hostname"));
        assert_eq!(comment_for(&obj, "port"), None);
        assert_eq!(section_comment(&obj), Some("sect"));
    }

    #[test]
    fn document_deserializes_without_touched() {
        let doc: Document = serde_json::from_value(json!({
            "id": "105",
            "format": "yaml",
            "digest": "abc123",
            "mtime": 1757265409i64,
            "data": {"traefik": {"spec": {"host": "wiki.example.com"}}}
        }))
        .unwrap();
        assert_eq!(doc.id, "105");
        assert!(doc.touched.is_empty());
        assert!(doc.raw.is_none());
    }
}
