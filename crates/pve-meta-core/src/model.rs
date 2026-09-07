//! The core document data model: the [`Value`] alias, lint rules, and
//! comment-key handling.

use std::fmt;

use crate::path::{is_valid_segment, Path};

/// A document (or any sub-value within one). Built on `serde_json::Value` with
/// the `preserve_order` feature enabled, so `Value::Object` is an
/// insertion-ordered map (order = human order).
pub type Value = serde_json::Value;

/// The suffix that marks an object key as a *comment key*: `foo__` documents
/// the sibling key `foo` (which need not exist); the bare key `__` documents
/// the containing map itself. Comment key values must be strings.
pub const COMMENT_SUFFIX: &str = "__";

/// `true` if `k` is a comment key, i.e. ends with [`COMMENT_SUFFIX`].
pub fn is_comment_key(k: &str) -> bool {
    k.ends_with(COMMENT_SUFFIX)
}

/// `true` if `k` is a syntactically valid object key: `^[A-Za-z0-9_@!-]+$`
/// (see [`crate::path::is_valid_segment`]).
pub(crate) fn is_valid_key(k: &str) -> bool {
    is_valid_segment(k)
}

/// A single lint finding: `path` points at the offending location, `msg`
/// describes the problem in human-readable terms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lint {
    /// The location of the problem.
    pub path: Path,
    /// A human-readable description.
    pub msg: String,
}

impl fmt::Display for Lint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.path.is_root() {
            write!(f, "{}", self.msg)
        } else {
            write!(f, "{}: {}", self.path, self.msg)
        }
    }
}

/// Validates `doc` against the document-model rules:
///
/// 1. the top level must be an object;
/// 2. no `null` anywhere (absent means unset);
/// 3. every object key must match `^[A-Za-z0-9_@!-]+$` (the extra `@`/`!`
///    accommodate PVE authids used as keys, e.g. the datacenter document's
///    `scopes` map);
/// 4. a comment key's value must be a string;
/// 5. numbers must be integers or finite floats (already guaranteed by
///    `serde_json::Value` without the `arbitrary_precision` feature, so this
///    is not checked separately at runtime).
pub fn lint(doc: &Value) -> Vec<Lint> {
    let mut out = Vec::new();
    if !doc.is_object() {
        out.push(Lint {
            path: Path::root(),
            msg: "top level must be an object".to_string(),
        });
    }
    out.extend(lint_relaxed(doc));
    out
}

/// Like [`lint`], but does not require the top level to be an object.
///
/// Used by [`crate::view`] to validate a *view*'s value, which need not
/// itself be an object at its own root (e.g. a view of an array-valued key):
/// only [`lint`]'s recursive rules (no nulls, valid keys, comment key values
/// are strings) apply.
pub fn lint_relaxed(doc: &Value) -> Vec<Lint> {
    let mut out = Vec::new();
    walk(doc, &Path::root(), &mut out);
    out
}

fn walk(v: &Value, path: &Path, out: &mut Vec<Lint>) {
    match v {
        Value::Null => out.push(Lint {
            path: path.clone(),
            msg: "null values are not allowed (omit the key instead)".to_string(),
        }),
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                walk(item, &path.join(i.to_string()), out);
            }
        }
        Value::Object(map) => {
            for (k, val) in map.iter() {
                let child_path = path.join(k.clone());
                if !is_valid_key(k) {
                    out.push(Lint {
                        path: child_path.clone(),
                        msg: format!(
                            "invalid key '{k}': keys must match ^[A-Za-z0-9_@!-]+$ and contain no dots"
                        ),
                    });
                }
                if is_comment_key(k) && !val.is_string() {
                    out.push(Lint {
                        path: child_path.clone(),
                        msg: "comment key value must be a string".to_string(),
                    });
                }
                walk(val, &child_path, out);
            }
        }
        Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

/// Removes all comment keys from `doc`, recursively (including inside
/// arrays).
pub fn strip_comments(doc: &mut Value) {
    strip(doc);
}

fn strip(v: &mut Value) {
    match v {
        Value::Object(map) => {
            map.retain(|k, _| !is_comment_key(k));
            for val in map.values_mut() {
                strip(val);
            }
        }
        Value::Array(items) => {
            for item in items.iter_mut() {
                strip(item);
            }
        }
        _ => {}
    }
}

/// The document's top-level non-comment keys, in document order.
pub fn namespaces(doc: &Value) -> Vec<String> {
    match doc {
        Value::Object(map) => map
            .keys()
            .filter(|k| !is_comment_key(k))
            .cloned()
            .collect(),
        _ => Vec::new(),
    }
}

/// Looks up `path` in `doc`. Object keys and array indices are both
/// supported as path segments. Returns `None` if any segment is missing, out
/// of bounds, or addresses through a scalar.
pub fn get_path<'a>(doc: &'a Value, path: &Path) -> Option<&'a Value> {
    let mut cur = doc;
    for seg in path.segments() {
        cur = match cur {
            Value::Object(map) => map.get(seg)?,
            Value::Array(items) => items.get(seg.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(cur)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lint_top_level_must_be_object() {
        let lints = lint(&json!([1, 2, 3]));
        assert_eq!(lints.len(), 1);
        assert!(lints[0].msg.contains("top level must be an object"));
    }

    #[test]
    fn lint_rejects_null_anywhere() {
        let doc = json!({"a": null, "b": {"c": null}, "d": [1, null]});
        let lints = lint(&doc);
        assert_eq!(lints.len(), 3);
        assert!(lints.iter().any(|l| l.path.to_string() == "a"));
        assert!(lints.iter().any(|l| l.path.to_string() == "b.c"));
        assert!(lints.iter().any(|l| l.path.to_string() == "d.1"));
    }

    #[test]
    fn lint_rejects_invalid_keys() {
        let doc = json!({"a.b": 1, "good_key-1": 2, "bad key": 3});
        let lints = lint(&doc);
        assert_eq!(lints.len(), 2);
    }

    #[test]
    fn lint_accepts_pve_authid_shaped_keys() {
        // The datacenter document's `scopes` map is keyed by authid
        // (`user@realm`, or `user@realm!tokenid`) -- see `docs/DESIGN.md` §2.
        let doc = json!({"scopes": {"svc@pve!traefik": [], "scoped@pve": []}});
        assert!(lint(&doc).is_empty());
    }

    #[test]
    fn lint_accepts_comment_keys_with_string_values() {
        let doc = json!({"__": "doc comment", "foo": 1, "foo__": "about foo"});
        assert!(lint(&doc).is_empty());
    }

    #[test]
    fn lint_rejects_non_string_comment_value() {
        let doc = json!({"foo__": 42});
        let lints = lint(&doc);
        assert_eq!(lints.len(), 1);
        assert!(lints[0].msg.contains("comment key value must be a string"));
    }

    #[test]
    fn comment_key_on_sibling_that_does_not_exist_is_fine() {
        let doc = json!({"foo__": "no foo here"});
        assert!(lint(&doc).is_empty());
    }

    #[test]
    fn strip_comments_removes_recursively() {
        let mut doc = json!({
            "__": "top",
            "a": {"a__": "a doc", "x": 1},
            "list": [{"y__": "y doc", "y": 2}],
        });
        strip_comments(&mut doc);
        assert_eq!(doc, json!({"a": {"x": 1}, "list": [{"y": 2}]}));
    }

    #[test]
    fn namespaces_lists_non_comment_top_keys_in_order() {
        let doc = json!({"__": "c", "zeta": 1, "alpha": 2, "alpha__": "cc"});
        assert_eq!(namespaces(&doc), vec!["zeta".to_string(), "alpha".to_string()]);
    }

    #[test]
    fn namespaces_of_non_object_is_empty() {
        assert!(namespaces(&json!([1, 2])).is_empty());
    }

    #[test]
    fn lint_relaxed_allows_non_object_top_level() {
        assert!(lint_relaxed(&json!([1, 2, 3])).is_empty());
        assert!(lint_relaxed(&json!("scalar")).is_empty());
        let lints = lint_relaxed(&json!([1, null]));
        assert_eq!(lints.len(), 1);
        assert!(lints[0].msg.contains("null"));
    }

    #[test]
    fn lint_relaxed_still_checks_recursive_rules() {
        let lints = lint_relaxed(&json!({"bad key": 1}));
        assert_eq!(lints.len(), 1);
        assert!(lint(&json!({"a": 1})).len() == lint_relaxed(&json!({"a": 1})).len());
    }

    #[test]
    fn get_path_traverses_objects_and_arrays() {
        let doc = json!({"a": {"b": [10, 20, {"c": 30}]}});
        let p = Path::parse("a.b.2.c").unwrap();
        assert_eq!(get_path(&doc, &p), Some(&json!(30)));
        assert_eq!(get_path(&doc, &Path::root()), Some(&doc));
        assert_eq!(get_path(&doc, &Path::parse("a.missing").unwrap()), None);
        assert_eq!(get_path(&doc, &Path::parse("a.b.99").unwrap()), None);
    }
}
