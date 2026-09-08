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

/// The caller's effective grants **for one document** (`GET /meta/access?vmid=…` or
/// `?dc=1`, `docs/DESIGN.md` §8).
///
/// `read`/`write` are the ACL half — `VM.Audit`/`VM.Config.Options` on that guest, or
/// `Sys.Audit`/`Sys.Modify` on `/` for the datacenter document. They are deliberately two
/// fields: the endpoint used to answer a single audit-derived `full`, which the editor then
/// used as the *write* grant, so a `Sys.Audit`-only auditor got an editable buffer and an
/// enabled Apply and learned otherwise from a 403 (`docs/REVIEW-2026-09-07.md` F22).
///
/// `scopes` is the prefix half, which applies to every document (§2).
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct Access {
    /// May read the whole document.
    #[serde(default, deserialize_with = "deserialize_flag")]
    pub read: bool,
    /// May write the whole document.
    #[serde(default, deserialize_with = "deserialize_flag")]
    pub write: bool,
    /// Prefix scopes configured for this principal in the datacenter document.
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
fn deserialize_flag<'de, D: Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    let value = Value::deserialize(deserializer)?;
    Ok(match value {
        Value::Bool(flag) => flag,
        Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.0),
        Value::String(s) => matches!(s.as_str(), "1" | "true" | "yes" | "*"),
        _ => false,
    })
}

impl Access {
    /// True if `prefix` covers `view` — the empty prefix covers everything, and
    /// `traefik` covers `traefik` and `traefik.spec` but not `traefikx`.
    ///
    /// A comment key follows its subject (`docs/DESIGN.md` §8): a scope on `traefik` also
    /// covers `traefik__`, the note *about* `traefik`.
    pub fn covers(prefix: &str, view: &str) -> bool {
        covers_exactly(prefix, view) || covers_exactly(prefix, strip_comment_marker(view))
    }

    /// True if the caller may write `view` of this document.
    ///
    /// The server is the final arbiter (a refused write comes back as a 403 with its own
    /// message); this only decides whether the editor is writable up front.
    pub fn may_write(&self, view: &str) -> bool {
        self.write
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

/// What a load answering a selected view should do once the caller's grants and the
/// document's *current* top-level keys are both known.
///
/// Pulled out of `editor.rs`'s `Msg::Loaded` handling as a pure function so the fallback
/// invariant is unit-tested natively rather than only through the wasm32-only state
/// machine that calls it: a selected view that is no longer an option (deleted, or no
/// longer covered by a scope) always ends up on the whole document, and never silently
/// drops an unapplied edit to get there (`docs/REVIEW-2026-09-08-pass3.md` R3 — a
/// regression from folding the key list into the single document GET, `P10`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewOutcome {
    /// `view` is still among `access.view_options(keys)`: show what was loaded for it.
    Keep,
    /// `view` is gone and the buffer holds no unapplied edits: fall back to the whole
    /// document right away.
    FallBack,
    /// `view` is gone, but the buffer holds unapplied edits: ask before discarding them,
    /// the same way a manual view switch does (`docs/DESIGN.md`'s F5 discipline).
    ConfirmFallBack,
}

/// Decide what a load of `view` should do, given the caller's grants, the document's
/// current top-level `keys`, and whether the buffer is `dirty`.
pub fn view_outcome(access: &Access, keys: &[String], view: &str, dirty: bool) -> ViewOutcome {
    if access
        .view_options(keys)
        .iter()
        .any(|option| option == view)
    {
        return ViewOutcome::Keep;
    }
    match dirty {
        true => ViewOutcome::ConfirmFallBack,
        false => ViewOutcome::FallBack,
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

/// The top-level keys of a document, in document order, comment keys included (they are
/// ordinary data — `docs/DESIGN.md` §1).
///
/// Parsed out of `format=yaml` **text**, never out of `format=json` data: key order is
/// guaranteed in the YAML rendering only, because the Perl API module round-trips the JSON
/// through a plain hash (`docs/DESIGN.md` §8, `docs/REVIEW-2026-09-07.md` F23). The input
/// is the store's own canonical dump — block style, two-space indent, one key per line —
/// so this is a scan for unindented `key:` lines rather than a YAML parser:
///
/// * indented lines are nested data, and so is every line of a block scalar (its content
///   must be indented deeper than the key that introduced it);
/// * `#` comments, `---`/`...` document markers and `- ` sequence items are skipped;
/// * a quoted key (`"a: b":`) is unquoted, with `\\`/`\"` unescaped.
pub fn top_level_keys_from_yaml(text: &str) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();

    for line in text.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() || line.starts_with([' ', '\t']) {
            continue;
        }
        if line.starts_with('#') || line.starts_with("---") || line.starts_with("...") {
            continue;
        }
        // A top-level sequence has no keys at all; `-` cannot start one.
        if line.starts_with('-') {
            continue;
        }

        if let Some(key) = parse_key(line) {
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
    }

    keys
}

/// The mapping key `line` opens, if it opens one.
fn parse_key(line: &str) -> Option<String> {
    let mut chars = line.char_indices();
    let (_, first) = chars.next()?;

    if first == '"' || first == '\'' {
        let mut key = String::new();
        let mut escaped = false;
        for (index, c) in chars {
            if escaped {
                key.push(c);
                escaped = false;
                continue;
            }
            match c {
                '\\' if first == '"' => escaped = true,
                c if c == first => {
                    // Only a quoted scalar that is followed by `:` is a key.
                    return line[index + 1..]
                        .trim_start()
                        .starts_with(':')
                        .then_some(key);
                }
                c => key.push(c),
            }
        }
        return None;
    }

    // A plain key ends at the first `:` that is followed by a space or the end of line —
    // `a:b` is the scalar `a:b`, not a key (YAML 1.2 §7.3.3).
    let bytes = line.as_bytes();
    let end = (0..bytes.len()).find(|&i| {
        bytes[i] == b':' && bytes.get(i + 1).is_none_or(|c| *c == b' ' || *c == b'\t')
    })?;
    let key = line[..end].trim_end();
    (!key.is_empty()).then(|| key.to_string())
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
    fn a_scope_covers_the_comment_key_of_its_subject() {
        // docs/DESIGN.md §8: comment keys follow their subject.
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
        assert!(!auditor.may_write(""));

        let admin: Access = serde_json::from_value(json!({"read": 1, "write": 1})).unwrap();
        assert!(admin.may_write(""));
        assert!(admin.may_write("scopes"));
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

        // Anything unrecognised — including the old audit-only `full` field, which is not
        // a write grant — resolves to no grant at all.
        let unknown: Access = serde_json::from_value(json!({"full": "*"})).unwrap();
        assert_eq!(unknown, Access::default());
        assert!(!unknown.may_write(""));

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
        // whole document (that's an ACL grant, `read`/`write`, checked above).
        assert!(!access.may_write(""));
    }

    #[test]
    fn view_options_start_with_the_whole_document() {
        let access: Access = serde_json::from_value(json!({
            "read": 1,
            "write": 1,
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
    fn view_outcome_keeps_a_view_still_listed_in_fresh_keys() {
        let access: Access = serde_json::from_value(json!({"read": 1, "write": 1})).unwrap();
        let keys = vec!["traefik".to_string(), "netbird".to_string()];

        assert_eq!(
            view_outcome(&access, &keys, "netbird", false),
            ViewOutcome::Keep,
        );
        // A dirty buffer changes nothing while the view is still valid.
        assert_eq!(
            view_outcome(&access, &keys, "netbird", true),
            ViewOutcome::Keep,
        );
    }

    #[test]
    fn view_outcome_falls_back_when_a_clean_views_key_is_gone() {
        // R3: a key deleted out-of-band (or its scope revoked) between one load and the
        // next, no unapplied edits to lose.
        let access: Access = serde_json::from_value(json!({"read": 1, "write": 1})).unwrap();
        let keys = vec!["traefik".to_string()];

        assert_eq!(
            view_outcome(&access, &keys, "netbird", false),
            ViewOutcome::FallBack,
        );
    }

    #[test]
    fn view_outcome_asks_before_falling_back_over_unapplied_edits() {
        // R3's live repro: the selected view disappears while the buffer is dirty — the
        // fallback must not silently discard the draft (`docs/DESIGN.md`'s F5
        // discipline, the same one a manual view switch already honours).
        let access: Access = serde_json::from_value(json!({"read": 1, "write": 1})).unwrap();
        let keys = vec!["traefik".to_string()];

        assert_eq!(
            view_outcome(&access, &keys, "netbird", true),
            ViewOutcome::ConfirmFallBack,
        );
    }

    #[test]
    fn view_outcome_keeps_a_scope_prefix_even_when_the_document_has_no_such_key_yet() {
        // Scopes are granted on every document (§2: no per-vmid scoping), so a document
        // that simply never had this key is not the same as one that lost it — the
        // scope alone keeps the view a valid option.
        let access: Access = serde_json::from_value(json!({
            "read": 0,
            "write": 0,
            "scopes": [{"prefix": "netbird", "mode": "rw"}],
        }))
        .unwrap();

        assert_eq!(
            view_outcome(&access, &[], "netbird", false),
            ViewOutcome::Keep,
        );
    }

    #[test]
    fn view_outcome_keeps_the_whole_document_view() {
        // The empty view is always an option (`view_options` always starts with it), so
        // it can never trigger a fallback loop against itself.
        let access = Access::default();

        assert_eq!(view_outcome(&access, &[], "", false), ViewOutcome::Keep);
    }

    #[test]
    fn top_level_keys_keep_document_order() {
        let text = "traefik:\n  spec:\n    host: ct200.example\n__: a note\nnotes: ''\n";
        assert_eq!(
            top_level_keys_from_yaml(text),
            vec!["traefik", "__", "notes"],
        );
    }

    #[test]
    fn top_level_keys_ignore_nesting_and_noise() {
        let text = "\
# a comment
---
traefik:
  # an indented comment
  spec:
    host: ct200.example
  ports:
    - 80
    - 443
netbird:
  groups: [lan]
";
        assert_eq!(top_level_keys_from_yaml(text), vec!["traefik", "netbird"]);
    }

    #[test]
    fn top_level_keys_ignore_block_scalar_content() {
        // The body of a block scalar is indented deeper than its key, so `readme:` is the
        // only key here — `not: a key` inside the text must not be picked up.
        let text = "readme: |\n  not: a key\n  host: nope\nnetbird: {}\n";
        assert_eq!(top_level_keys_from_yaml(text), vec!["readme", "netbird"]);
    }

    #[test]
    fn top_level_keys_unquote_quoted_keys() {
        let text = "\"a: b\": 1\n'plain': 2\n\"with \\\"quotes\\\"\": 3\n";
        assert_eq!(
            top_level_keys_from_yaml(text),
            vec!["a: b", "plain", "with \"quotes\""],
        );
    }

    #[test]
    fn top_level_keys_of_an_empty_or_scalar_document() {
        assert!(top_level_keys_from_yaml("").is_empty());
        assert!(top_level_keys_from_yaml("--- {}\n").is_empty());
        assert!(top_level_keys_from_yaml("- one\n- two\n").is_empty());
        // A plain scalar line is not a mapping key.
        assert!(top_level_keys_from_yaml("just text\n").is_empty());
    }

    #[test]
    fn top_level_keys_are_reported_once() {
        let text = "traefik:\n  a: 1\ntraefik:\n  b: 2\n";
        assert_eq!(top_level_keys_from_yaml(text), vec!["traefik"]);
    }
}
