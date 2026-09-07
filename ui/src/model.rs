//! Document identity, access grants and views.
//!
//! A *view* is a key-path prefix (`docs/DESIGN.md` §1): the empty prefix is the whole
//! document, `traefik` is the subtree under `traefik` with the prefix stripped. Views are
//! also the unit of access — a principal holds `ro`/`rw` on a prefix and then sees and
//! edits only that part of every document (§2).
//!
//! Pure module: no `web-sys`/`wasm-bindgen`, so it is unit-tested natively
//! (`cargo test --lib`) without a browser or any wasm-only dependency.

use serde::Deserialize;
use serde::de::{self, Deserializer};
use serde_json::Value;

/// Which document is loaded: one guest's, or the cluster-wide datacenter one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DocId {
    Guest(u32),
    Datacenter,
}

impl DocId {
    /// API base path for this document, e.g. `/meta/guests/200` or `/meta/datacenter`.
    pub fn api_path(&self) -> String {
        match self {
            DocId::Guest(vmid) => format!("/meta/guests/{vmid}"),
            DocId::Datacenter => "/meta/datacenter".to_string(),
        }
    }

    /// A short id, e.g. `200` or `datacenter`.
    pub fn label(&self) -> String {
        match self {
            DocId::Guest(vmid) => vmid.to_string(),
            DocId::Datacenter => "datacenter".to_string(),
        }
    }
}

/// Access mode of a grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Read-only.
    Ro,
    /// Read-write.
    Rw,
}

/// One prefix-scoped grant of `GET /meta/access`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Scope {
    /// The key-path prefix this grant covers.
    pub prefix: String,
    /// Read or read-write.
    pub mode: Mode,
}

/// Full (ACL-granted) access, as reported by `GET /meta/access`.
///
/// `docs/DESIGN.md` §3 documents this as `[vmids or "*"]`; PVE renders booleans as a
/// plain `0`/`1` integer rather than a JSON `true`/`false` (the same wire quirk
/// `proxmox_serde::perl::deserialize_bool` exists for), and an implementation may just
/// as well answer `1`, `"*"` or a bare list. Accept all of those shapes rather than
/// failing the whole response over the spelling of one field.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum FullAccess {
    /// No document is fully accessible.
    #[default]
    None,
    /// Every document is.
    All,
    /// Only these guest documents are (the datacenter document is never in this list).
    Guests(Vec<u32>),
}

impl FullAccess {
    /// True if `doc` is covered by this grant.
    pub fn covers(&self, doc: DocId) -> bool {
        match (self, doc) {
            (FullAccess::None, _) => false,
            (FullAccess::All, _) => true,
            (FullAccess::Guests(ids), DocId::Guest(vmid)) => ids.contains(&vmid),
            (FullAccess::Guests(_), DocId::Datacenter) => false,
        }
    }
}

impl<'de> Deserialize<'de> for FullAccess {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        Ok(match value {
            Value::Null => FullAccess::None,
            Value::Bool(true) => FullAccess::All,
            Value::Bool(false) => FullAccess::None,
            Value::Number(n) => match n.as_i64() {
                Some(0) | None => FullAccess::None,
                Some(_) => FullAccess::All,
            },
            Value::String(s) => match s.as_str() {
                "*" | "1" | "all" => FullAccess::All,
                "" | "0" => FullAccess::None,
                other => match other.parse() {
                    Ok(vmid) => FullAccess::Guests(vec![vmid]),
                    Err(_) => FullAccess::None,
                },
            },
            Value::Array(items) => {
                let mut ids = Vec::new();
                for item in items {
                    match item {
                        Value::String(s) if s == "*" => return Ok(FullAccess::All),
                        Value::String(s) => {
                            if let Ok(vmid) = s.parse() {
                                ids.push(vmid);
                            }
                        }
                        Value::Number(n) => {
                            if let Some(vmid) = n.as_u64() {
                                ids.push(vmid as u32);
                            }
                        }
                        _ => {}
                    }
                }
                FullAccess::Guests(ids)
            }
            other => {
                return Err(de::Error::custom(format!(
                    "unexpected 'full' value {other}"
                )));
            }
        })
    }
}

/// The caller's effective grants (`GET /meta/access`).
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct Access {
    #[serde(default)]
    pub full: FullAccess,
    #[serde(default)]
    pub scopes: Vec<Scope>,
}

impl Access {
    /// True if `prefix` covers `view` — the empty prefix covers everything, and
    /// `traefik` covers `traefik` and `traefik.spec` but not `traefikx`.
    pub fn covers(prefix: &str, view: &str) -> bool {
        prefix.is_empty()
            || view == prefix
            || (view.len() > prefix.len()
                && view.starts_with(prefix)
                && view.as_bytes()[prefix.len()] == b'.')
    }

    /// True if the caller may write `view` of `doc`.
    ///
    /// The server is the final arbiter (a refused write comes back as a 403 with its own
    /// message); this only decides whether the editor is writable up front.
    pub fn may_write(&self, doc: DocId, view: &str) -> bool {
        self.full.covers(doc)
            || self
                .scopes
                .iter()
                .any(|s| s.mode == Mode::Rw && Self::covers(&s.prefix, view))
    }

    /// The prefixes the "View as" selector offers, whole document (the empty prefix)
    /// first, then the document's own top-level keys, then any scope prefix not already
    /// listed.
    pub fn view_options(&self, keys: &[String]) -> Vec<String> {
        let mut options = vec![String::new()];

        for key in keys {
            if !key.is_empty() && !options.contains(key) {
                options.push(key.clone());
            }
        }

        // Scopes are granted on every document (§2: no per-vmid scoping), so they are
        // offered even when the document itself has no such key yet.
        for scope in &self.scopes {
            if scope.prefix.is_empty() || options.contains(&scope.prefix) {
                continue;
            }
            options.push(scope.prefix.clone());
        }

        options
    }
}

/// The top-level keys of a document, in document order, comment keys included (they are
/// ordinary data — `docs/DESIGN.md` §1).
pub fn top_level_keys(data: &Value) -> Vec<String> {
    match data {
        Value::Object(map) => map.keys().cloned().collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn doc_paths() {
        assert_eq!(DocId::Guest(200).api_path(), "/meta/guests/200");
        assert_eq!(DocId::Datacenter.api_path(), "/meta/datacenter");
        assert_eq!(DocId::Datacenter.label(), "datacenter");
    }

    #[test]
    fn prefix_covers_view() {
        assert!(Access::covers("", "traefik"));
        assert!(Access::covers("traefik", "traefik"));
        assert!(Access::covers("traefik", "traefik.spec"));
        assert!(!Access::covers("traefik", "traefikx"));
        assert!(!Access::covers("traefik.spec", "traefik"));
    }

    #[test]
    fn full_access_accepts_every_wire_spelling() {
        let all: Access = serde_json::from_value(json!({"full": "*"})).unwrap();
        assert!(all.full.covers(DocId::Datacenter));

        let one: Access = serde_json::from_value(json!({"full": [200, "201"]})).unwrap();
        assert!(one.full.covers(DocId::Guest(200)));
        assert!(one.full.covers(DocId::Guest(201)));
        assert!(!one.full.covers(DocId::Guest(202)));
        assert!(!one.full.covers(DocId::Datacenter));

        let perl_bool: Access = serde_json::from_value(json!({"full": 1})).unwrap();
        assert!(perl_bool.full.covers(DocId::Guest(200)));

        let none: Access = serde_json::from_value(json!({"full": 0})).unwrap();
        assert!(!none.full.covers(DocId::Guest(200)));

        let missing: Access = serde_json::from_value(json!({})).unwrap();
        assert_eq!(missing, Access::default());
    }

    #[test]
    fn write_permission_follows_scopes() {
        let access: Access = serde_json::from_value(json!({
            "full": [],
            "scopes": [{"prefix": "traefik", "mode": "rw"}, {"prefix": "netbird", "mode": "ro"}],
        }))
        .unwrap();

        assert!(access.may_write(DocId::Guest(200), "traefik"));
        assert!(access.may_write(DocId::Guest(200), "traefik.spec"));
        assert!(!access.may_write(DocId::Guest(200), "netbird"));
        // The whole document is readable (the union of the readable subtrees) but never
        // writable through a scope alone.
        assert!(!access.may_write(DocId::Guest(200), ""));
    }

    #[test]
    fn full_access_writes_everything() {
        let access: Access = serde_json::from_value(json!({"full": "*"})).unwrap();
        assert!(access.may_write(DocId::Guest(200), ""));
        assert!(access.may_write(DocId::Datacenter, "scopes"));
    }

    #[test]
    fn view_options_start_with_the_whole_document() {
        let access: Access = serde_json::from_value(json!({
            "full": "*",
            "scopes": [{"prefix": "netbird", "mode": "ro"}, {"prefix": "traefik", "mode": "rw"}],
        }))
        .unwrap();
        let keys = vec!["traefik".to_string(), "notes".to_string()];

        assert_eq!(
            access.view_options(&keys),
            vec!["", "traefik", "notes", "netbird"],
        );
    }

    #[test]
    fn top_level_keys_keep_document_order() {
        let data = json!({"traefik": {}, "__": "note", "notes": ""});
        assert_eq!(top_level_keys(&data), vec!["traefik", "__", "notes"]);
    }
}
