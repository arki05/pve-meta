//! Document identity and access grants.
//!
//! A *view* is a key-path prefix (`docs/DESIGN.md` §2): the empty prefix is the whole
//! document, `traefik` is the subtree under `traefik` with the prefix stripped. Views are
//! also the unit of access — a principal holds `ro`/`rw` on a prefix and then sees and
//! edits only that part of a document (§3), and every write the tree makes is a `PUT` on
//! the view that names one row.
//!
//! Pure module: no `web-sys`/`wasm-bindgen`, so it is unit-tested natively
//! (`cargo test --lib`) without a browser or any wasm-only dependency.

use serde::Deserialize;
use serde::de::Deserializer;
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

    /// True if operator scopes apply to this document.
    ///
    /// They do not to the datacenter document: `docs/DESIGN.md` §3 — "Scopes apply to
    /// **guest documents only**. The datacenter document is governed by ACLs alone." So no
    /// grammar row, and no Access entry, is ever shown for it.
    pub fn scoped(&self) -> bool {
        matches!(self, DocId::Guest(_))
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

impl Mode {
    /// The wire spelling, for display.
    pub fn as_str(&self) -> &'static str {
        match self {
            Mode::Ro => "ro",
            Mode::Rw => "rw",
        }
    }
}

/// One prefix-scoped grant of `GET /meta/access`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Scope {
    /// The key-path prefix this grant covers.
    pub prefix: String,
    /// Read or read-write.
    pub mode: Mode,
}

/// The caller's effective grants **for one document** (`GET /meta/access?vmid=…` or
/// `?dc=1`, `docs/DESIGN.md` §5).
///
/// `read`/`write` are the ACL half — `VM.Audit`/`VM.Config.Options` on that guest, or
/// `Sys.Audit`/`Sys.Modify` on `/` for the datacenter document. They are deliberately two
/// fields: the endpoint used to answer a single audit-derived `full`, which the editor then
/// used as the *write* grant, so a `Sys.Audit`-only auditor got an editable page and learned
/// otherwise from a 403 (`docs/REVIEW-2026-09-07.md` F22).
///
/// `scopes` is the prefix half, already resolved against this guest's selectors by the
/// server. It is what makes a single row editable while its siblings are not.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct Access {
    /// May read the whole document.
    #[serde(default, deserialize_with = "deserialize_flag")]
    pub read: bool,
    /// May write the whole document.
    #[serde(default, deserialize_with = "deserialize_flag")]
    pub write: bool,
    /// Prefix scopes granted to this principal on this document.
    #[serde(default)]
    pub scopes: Vec<Scope>,
}

/// A PVE boolean, in every spelling the wire uses.
///
/// `PVE::RESTHandler` renders booleans as a plain `0`/`1` integer rather than a JSON
/// `true`/`false` (the same quirk `proxmox_serde::perl::deserialize_bool` exists for), and
/// an implementation may just as well answer `"1"` or `"*"`. Accept all of those rather
/// than failing the whole response — and, crucially, resolve anything unrecognised to
/// *false*: a grant the page cannot understand is not a grant.
pub(crate) fn deserialize_flag<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<bool, D::Error> {
    let value = Value::deserialize(deserializer)?;
    Ok(flag_value(&value))
}

/// [`deserialize_flag`] as a plain function over an already-parsed value.
pub(crate) fn flag_value(value: &Value) -> bool {
    match value {
        Value::Bool(flag) => *flag,
        Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.0),
        Value::String(s) => matches!(s.as_str(), "1" | "true" | "yes" | "*"),
        _ => false,
    }
}

impl Access {
    /// True if `prefix` covers `view` — the empty prefix covers everything, and
    /// `traefik` covers `traefik` and `traefik.spec` but not `traefikx`.
    ///
    /// A comment key follows its subject (`docs/DESIGN.md` §3, the *only* comment-key
    /// rule): a scope on `traefik` also covers `traefik__`, the note *about* `traefik`.
    pub fn covers(prefix: &str, view: &str) -> bool {
        covers_exactly(prefix, view) || covers_exactly(prefix, strip_comment_marker(view))
    }

    /// True if the caller may write `view` of this document.
    ///
    /// The server is the final arbiter (a refused write comes back as a 403 with its own
    /// message); this only decides whether a row's Edit/Remove actions are offered.
    pub fn may_write(&self, view: &str) -> bool {
        if view.is_empty() {
            // A write to the root view requires full write (`docs/DESIGN.md` §3); no
            // scope, however wide, grants it.
            return self.write;
        }
        self.write
            || self
                .scopes
                .iter()
                .any(|s| s.mode == Mode::Rw && Self::covers(&s.prefix, view))
    }
}

/// `prefix` covers `view` as a plain key-path prefix, comment keys not considered.
fn covers_exactly(prefix: &str, view: &str) -> bool {
    prefix.is_empty()
        || view == prefix
        || (view.len() > prefix.len()
            && view.starts_with(prefix)
            && view.as_bytes()[prefix.len()] == b'.')
}

/// True if `key` is a comment key: the bare `__` map note, or a `name__` subject note.
/// Mirrors `pve-meta-core`'s `model::is_comment_key` (`k.ends_with("__")`).
pub fn is_comment_key(key: &str) -> bool {
    key.ends_with("__")
}

/// `traefik.host__` → `traefik.host`: the subject a comment key documents. A bare `__`
/// (the note about the map itself) has no subject and is returned unchanged.
fn strip_comment_marker(view: &str) -> &str {
    let last = match view.rfind('.') {
        Some(dot) => &view[dot + 1..],
        None => view,
    };
    match last.len() > 2 && last.ends_with("__") {
        true => &view[..view.len() - 2],
        false => view,
    }
}

/// What the tree knows about the guest the document belongs to: enough to resolve an
/// operator scope's selector (`docs/DESIGN.md` §3) and to name the document.
///
/// Comes from `GET /meta/guests`; `name`/`node`/`tags` are only answered to a caller with
/// `VM.Audit`, so all three are optional and an absent tag list simply matches no
/// `tag:` selector.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GuestInfo {
    pub name: Option<String>,
    pub node: Option<String>,
    pub guest_type: Option<String>,
    pub tags: Vec<String>,
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
        assert!(DocId::Guest(200).scoped());
        assert!(!DocId::Datacenter.scoped());
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
    fn a_scope_covers_the_comment_key_of_its_subject() {
        assert!(Access::covers("traefik", "traefik__"));
        assert!(Access::covers("traefik", "traefik.spec__"));
        assert!(Access::covers("traefik.spec", "traefik.spec__"));
        // A bare `__` documents the map itself, not the subtree of a scope.
        assert!(!Access::covers("traefik", "__"));
        assert!(!Access::covers("traefik", "netbird__"));
    }

    #[test]
    fn read_and_write_are_separate_grants() {
        // F22: an auditor gets a readable, not an editable, document.
        let auditor: Access = serde_json::from_value(json!({"read": 1, "write": 0})).unwrap();
        assert!(auditor.read);
        assert!(!auditor.may_write("traefik"));

        let admin: Access = serde_json::from_value(json!({"read": 1, "write": 1})).unwrap();
        assert!(admin.may_write(""));
        assert!(admin.may_write("traefik.spec.host"));
    }

    #[test]
    fn access_flags_accept_every_wire_spelling() {
        let perl_bool: Access = serde_json::from_value(json!({"read": 1, "write": 1})).unwrap();
        assert!(perl_bool.write);

        let json_bool: Access =
            serde_json::from_value(json!({"read": true, "write": false})).unwrap();
        assert!(json_bool.read);
        assert!(!json_bool.write);

        let strings: Access = serde_json::from_value(json!({"read": "*", "write": "0"})).unwrap();
        assert!(strings.read);
        assert!(!strings.write);

        // Anything unrecognised resolves to no grant at all.
        let unknown: Access = serde_json::from_value(json!({"full": "*"})).unwrap();
        assert_eq!(unknown, Access::default());
        assert!(!unknown.may_write("traefik"));

        let null: Access = serde_json::from_value(json!({"read": null, "write": null})).unwrap();
        assert_eq!(null, Access::default());
    }

    #[test]
    fn write_permission_follows_scopes() {
        let access: Access = serde_json::from_value(json!({
            "read": 0,
            "write": 0,
            "scopes": [{"prefix": "traefik", "mode": "rw"}, {"prefix": "netbird", "mode": "ro"}],
        }))
        .unwrap();

        assert!(access.may_write("traefik"));
        assert!(access.may_write("traefik.spec"));
        assert!(access.may_write("traefik__"));
        assert!(!access.may_write("netbird"));
        // A read-only scope never grants a write, and no scope alone ever grants the
        // root view (that is an ACL grant, `write`, checked above).
        assert!(!access.may_write(""));
    }

    #[test]
    fn comment_keys_are_recognised() {
        assert!(is_comment_key("__"));
        assert!(is_comment_key("host__"));
        assert!(!is_comment_key("host"));
        assert!(!is_comment_key("_host"));
    }
}
