//! The core document data model: the [`Value`] alias, the lint rules, and
//! comment keys.

use std::fmt;

use crate::path::{is_valid_segment, Path};

/// A document (or any sub-value within one). Built on `serde_json::Value` with
/// the `preserve_order` feature enabled, so `Value::Object` is an
/// insertion-ordered map (order = human order).
pub type Value = serde_json::Value;

/// The suffix that marks an object key as a *comment key*: `foo__` documents
/// the sibling key `foo` (which need not exist); the bare key `__` documents
/// the containing map itself. Comment key values must be strings.
///
/// Comment keys are **ordinary data** (`docs/DESIGN.md` §2). They travel with
/// the subtree they sit in, and the only rule about them anywhere else is that
/// a scope on `p` also covers `p__` ([`crate::scopes::covers`]).
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

/// The **one** lint (`docs/DESIGN.md` §4), run on the planned document:
///
/// 1. the top level must be an object;
/// 2. no `null` anywhere (absent means unset);
/// 3. every object key must match `^[A-Za-z0-9_@!-]+$`;
/// 4. a comment key's value must be a string;
/// 5. numbers must be integers or finite floats (already guaranteed by
///    `serde_json::Value` without the `arbitrary_precision` feature, so this
///    is not checked separately at runtime).
///
/// There is deliberately no second variant. Revision 4 grew three
/// (`lint_relaxed`, `lint_relaxed_at`, `lint_at`) plus a before/after
/// finding-set comparison, because the lint had been narrowed by privilege;
/// revision 5 lints the planned document once, for every caller.
pub fn lint(doc: &Value) -> Vec<Lint> {
    let mut out = Vec::new();
    if !doc.is_object() {
        out.push(Lint {
            path: Path::root(),
            msg: "top level must be an object".to_string(),
        });
    }
    walk(doc, &Path::root(), &mut out);
    out
}

/// The key rule for a key `k`. `Some(msg)` describes the violation.
pub(crate) fn key_lint(k: &str) -> Option<String> {
    if is_valid_key(k) {
        return None;
    }
    Some(format!(
        "invalid key '{k}': keys must match ^[A-Za-z0-9_@!-]+$ and contain no dots"
    ))
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
                if let Some(msg) = key_lint(k) {
                    out.push(Lint {
                        path: child_path.clone(),
                        msg,
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

/// `true` if `a` and `b` are the same document **including key order**.
///
/// `Value`'s own `==` compares maps as sets, which is right for "did a value
/// change" (a reordering touches no path, `docs/DESIGN.md` §3.4) and wrong
/// for "is this the document that was typed": key order is data (§2), and a
/// staged edit set that loses a reordering has lost something.
pub fn same_ordered(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter().zip(y.iter()).all(|((ka, va), (kb, vb))| ka == kb && same_ordered(va, vb))
        }
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(va, vb)| same_ordered(va, vb))
        }
        _ => a == b,
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
        assert!(lints.iter().all(|l| l.msg.contains("invalid key")));
    }

    #[test]
    fn lint_names_the_offending_path() {
        // `docs/DESIGN.md` §4: the 400 names the offending path. There is no
        // redaction rule and no caller-dependent message.
        let lints = lint(&json!({"a": {"b": {"bad key": 1}}}));
        assert_eq!(lints.len(), 1);
        assert_eq!(lints[0].to_string(), "a.b.bad key: invalid key 'bad key': keys must match ^[A-Za-z0-9_@!-]+$ and contain no dots");
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
    fn no_key_is_reserved_in_any_document() {
        // `docs/DESIGN.md` §2. Revision 4 reserved `scopes` in every document
        // and relaxed the key rule inside it for PVE authids; access-control
        // data now lives outside documents entirely (§3), so `scopes` is
        // ordinary user data and an authid-shaped key is not special anywhere.
        assert!(lint(&json!({"scopes": {"a": 1}})).is_empty());
        assert_eq!(lint(&json!({"scopes": {"john.doe@pve": 1}})).len(), 1);
        assert!(lint(&json!({"scopes": {"svc@pve!traefik": 1}})).is_empty());
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
