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

/// The one reserved top-level key, in **every** document
/// (`docs/DESIGN.md` §9): the datacenter document's access-control map
/// (`docs/DESIGN.md` §2). Its own keys are PVE authids rather than document
/// keys, and it is an **opaque leaf** for path addressing — see
/// [`is_authid`], [`crate::scopes::parse_scopes`] and `crate::api`'s view
/// parsing.
///
/// The name is reserved everywhere rather than only in the datacenter
/// document, so that one document's key rule, view addressing, scope-prefix
/// rule and `touched` reporting cannot disagree with another's (review pass 3
/// §5, `api.rs:173`). Only the datacenter document's `scopes` map *means*
/// anything — [`crate::scopes::scopes_for`] reads that one and no other — so
/// only there is a write to it additionally gated on `full_write`
/// (`crate::api`'s `check_scopes_write`). In a guest document `scopes` is
/// ordinary user data that happens to be addressed as a whole and to accept
/// authid-shaped keys.
pub const SCOPES_KEY: &str = "scopes";

/// `true` if `s` is a PVE realm (or token sub-id): `[A-Za-z][A-Za-z0-9.\-_]+`
/// — `PVE::Auth::Plugin`'s `$realm_regex`, which also backs
/// `PVE::AccessControl`'s `$token_subid_regex`.
fn is_realm(s: &str) -> bool {
    let mut chars = s.chars();
    if !chars.next().is_some_and(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    let rest = chars.as_str();
    !rest.is_empty()
        && rest
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// `true` if `s` is a PVE authid: `user@realm`, optionally `!tokenid`.
///
/// This is `PVE::AccessControl`'s `$userid_or_token_regex` transliterated:
/// `^[^\s:/]+@[A-Za-z][A-Za-z0-9.\-_]+(?:![A-Za-z][A-Za-z0-9.\-_]+)?$`. The
/// user part deliberately permits **dots** (and `@`/`!`), which the document
/// key charset cannot ([`crate::path::is_valid_segment`]: `.` is the path
/// separator), so every LDAP/AD-synced `first.last@realm` can hold a scope
/// (review P9). Since the user part may itself contain `@`, the split is
/// tried from the right — the same answer Perl's greedy match gives.
///
/// PVE additionally caps the user part at 64 characters when *creating* a
/// user (`PVE::Auth::Plugin::verify_username`); that is not a shape rule and
/// is not mirrored here.
pub fn is_authid(s: &str) -> bool {
    for (at, _) in s.rmatch_indices('@') {
        let (user, tail) = (&s[..at], &s[at + 1..]);
        if user.is_empty()
            || user
                .chars()
                .any(|c| c.is_whitespace() || c == ':' || c == '/')
        {
            continue;
        }
        // A realm cannot contain `!`, so the first one starts the token id.
        let ok = match tail.split_once('!') {
            Some((realm, subid)) => is_realm(realm) && is_realm(subid),
            None => is_realm(tail),
        };
        if ok {
            return true;
        }
    }
    false
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
///    `scopes` map) — inside the top-level [`SCOPES_KEY`] map a full PVE
///    authid ([`is_authid`], dots included) is accepted as well
///    (`docs/DESIGN.md` §9);
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
    lint_relaxed_at(doc, &Path::root())
}

/// [`lint_relaxed`] for a value that will live at `base` in the document.
///
/// A view's payload is linted before it is spliced in, so it has to be linted
/// in the *document's* coordinate system, not its own: rule 3's `scopes`
/// clause is positional, so `PUT ?view=scopes` with `{"john.doe@pve": …}`
/// must see that key at `scopes.john.doe@pve` — as `lint` will once the write
/// lands — rather than at the payload root (review P9). Reported paths are
/// absolute for the same reason.
pub fn lint_relaxed_at(doc: &Value, base: &Path) -> Vec<Lint> {
    let mut out = Vec::new();
    walk(doc, base, &mut out);
    out
}

/// Lints `value` **as it sits at `path`** in the document: [`lint_relaxed_at`]
/// plus the two rules that belong to `path`'s own last segment, which are
/// checked in the *parent* map and would otherwise be checked nowhere.
///
/// [`lint`] sees every key from its parent, so it applies the key rule and
/// the comment-key rule to `traefik__` as well as to everything below it. A
/// caller linting only the subtree it wrote (`crate::api`'s scoped write
/// path, review pass 3 R6) starts one level too deep: `lint_relaxed_at(5,
/// "traefik__")` walks a number and finds nothing, so `PUT ?view=traefik__`
/// with `5` would store a comment key whose value is not a string. This is
/// the same rule set, anchored at the parent.
///
/// Root has no parent and no key of its own, so it is exactly
/// [`lint_relaxed_at`] there.
pub fn lint_at(value: &Value, path: &Path) -> Vec<Lint> {
    let mut out = Vec::new();
    if let (Some(parent), Some(key)) = (path.parent(), path.last()) {
        if let Some(msg) = key_lint(&parent, key) {
            out.push(Lint {
                path: path.clone(),
                msg,
            });
        }
        if is_comment_key(key) && !value.is_string() {
            out.push(Lint {
                path: path.clone(),
                msg: "comment key value must be a string".to_string(),
            });
        }
    }
    out.extend(lint_relaxed_at(value, path));
    out
}

/// The key rule for a key `k` in the map at `parent`: an ordinary document
/// key, or — directly inside the top-level [`SCOPES_KEY`] map — a PVE authid
/// ([`is_authid`], dots included; `docs/DESIGN.md` §9). `Some(msg)` describes
/// the violation.
///
/// Shared with [`crate::patch::lint_patch_at`], so a merge payload and a
/// replace payload answer the same question the same way.
pub(crate) fn key_lint(parent: &Path, k: &str) -> Option<String> {
    let authid_keys = parent.segments() == [SCOPES_KEY];
    if is_valid_key(k) || (authid_keys && is_authid(k)) {
        return None;
    }
    Some(if authid_keys {
        format!(
            "invalid key '{k}': 'scopes' keys must be PVE authids \
             (user@realm, optionally !tokenid)"
        )
    } else {
        format!("invalid key '{k}': keys must match ^[A-Za-z0-9_@!-]+$ and contain no dots")
    })
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
                if let Some(msg) = key_lint(path, k) {
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

/// The document's top-level non-comment keys, in document order — the
/// `keys` field of the wire API (`docs/DESIGN.md` §3). Deliberately *not*
/// called "namespaces": DESIGN disclaims that word.
pub fn top_level_keys(doc: &Value) -> Vec<String> {
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
    fn lint_accepts_dotted_authids_inside_scopes_and_nowhere_else() {
        // Review P9: `john.doe@pve` and `svc@ldap.corp` are ordinary PVE
        // authids and could never be granted a scope, because the *document*
        // key charset has no dot (it is the path separator).
        let doc = json!({
            "scopes": {
                "john.doe@pve": [],
                "svc@ldap.corp!tok": [],
                "svc@pve!traefik": [],
                "__": "who gets what",
                "john.doe@pve__": "the ldap-synced admin",
            },
        });
        assert!(lint(&doc).is_empty(), "{:?}", lint(&doc));

        // The relaxation is confined to the top-level `scopes` map ...
        let elsewhere = json!({"other": {"john.doe@pve": 1}});
        assert_eq!(lint(&elsewhere).len(), 1);
        let deeper = json!({"scopes": {"a@pve": {"john.doe@pve": 1}}});
        assert_eq!(lint(&deeper).len(), 1);
        // ... and does not accept arbitrary junk even there.
        let junk = json!({"scopes": {"not an authid": []}});
        let lints = lint(&junk);
        assert_eq!(lints.len(), 1);
        assert!(lints[0].msg.contains("PVE authids"), "{}", lints[0].msg);
    }

    #[test]
    fn is_authid_matches_pve_accesscontrols_shape() {
        for good in [
            "root@pam",
            "svc@pve!traefik",
            "john.doe@pve",
            "svc@ldap.corp",
            "first.last@ldap.corp!token-1",
            "a-b_c@pve",
            "weird@name@pve",
        ] {
            assert!(is_authid(good), "{good} should be an authid");
        }
        for bad in [
            "root",           // no realm
            "root@",          // empty realm
            "root@p",         // realm needs at least two characters
            "@pve",           // empty user
            "root@1pve",      // realm must start with a letter
            "root@pve!",      // empty token id
            "root@pve!1t",    // token id must start with a letter
            "ro ot@pve",      // no whitespace
            "ro:ot@pve",      // no colon
            "ro/ot@pve",      // no slash
            "",
        ] {
            assert!(!is_authid(bad), "{bad} should not be an authid");
        }
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
    fn top_level_keys_lists_non_comment_top_keys_in_order() {
        let doc = json!({"__": "c", "zeta": 1, "alpha": 2, "alpha__": "cc"});
        assert_eq!(top_level_keys(&doc), vec!["zeta".to_string(), "alpha".to_string()]);
    }

    #[test]
    fn top_level_keys_of_non_object_is_empty() {
        assert!(top_level_keys(&json!([1, 2])).is_empty());
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
