//! Operator registrations: the drop-directory that holds scopes and operator
//! metadata (`docs/DESIGN.md` §3).
//!
//! Access-control data lives **outside** the documents, one file per
//! principal:
//!
//! * `/usr/share/pve-meta/operators/<name>.yaml` — packaged defaults, dropped
//!   in by an operator's own `.deb`;
//! * `/etc/pve/meta.d/operators/<name>.yaml` — cluster-wide overrides on
//!   pmxcfs. A cluster file **overrides the packaged file of the same name**.
//!
//! ```yaml
//! authid: svc@pve!traefik
//! description: Traefik dynamic-configuration provider
//! scopes:
//!   - prefix: traefik
//!     mode: rw
//!     selector: { all: true }   # or { tag: traefik }
//!     grammar:                  # optional, PVE::JSONSchema dialect
//!       type: object
//! ```
//!
//! Files are parsed **strictly and independently**: unknown fields, a bad
//! `authid`, an empty prefix or an unrecognised selector make the file
//! invalid, and an invalid file is skipped with a warning and grants nothing.
//! One operator's typo can never take another's grants away — which is the
//! whole reason the data left `datacenter.yaml`.

use std::collections::BTreeMap;
use std::path::{Path as FsPath, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::format::{self, Format};
use crate::model::Value;
use crate::path::Path;
use crate::scopes::{Mode, Scope};

/// The packaged registration directory.
pub const PACKAGED_DIR: &str = "/usr/share/pve-meta/operators";
/// The cluster-wide registration directory (pmxcfs); overrides
/// [`PACKAGED_DIR`] by file name.
pub const CLUSTER_DIR: &str = "/etc/pve/meta.d/operators";
/// Environment variable overriding both directories with a colon-separated
/// list, lowest precedence first. Used by the tests and by `test/basic.pl`.
pub const DIRS_ENV: &str = "PVE_META_OPERATOR_DIRS";

/// Which guests a scope applies to (`docs/DESIGN.md` §3). Room is left in the
/// format for `{ pool: <name> }`; it is deliberately not implemented.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selector {
    /// Every guest.
    All,
    /// Guests carrying this PVE tag. Adding the tag is the deliberate act of
    /// granting the operator that guest.
    Tag(String),
}

/// Serialized exactly as it is written in the file — `{all: true}` or
/// `{tag: <name>}` — so `GET /meta/operators` hands the UI the same shape an
/// administrator edits, with no second spelling to learn.
impl Serialize for Selector {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut m = s.serialize_map(Some(1))?;
        match self {
            Selector::All => m.serialize_entry("all", &true)?,
            Selector::Tag(t) => m.serialize_entry("tag", t)?,
        }
        m.end()
    }
}

impl Selector {
    /// `true` if this selector matches a guest carrying `tags`.
    pub fn matches(&self, tags: &[String]) -> bool {
        match self {
            Selector::All => true,
            Selector::Tag(t) => tags.iter().any(|have| have == t),
        }
    }
}

/// One `scopes:` entry of a registration.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RegisteredScope {
    /// The key-path prefix, any depth. Non-empty.
    pub prefix: Path,
    /// What it grants.
    pub mode: Mode,
    /// Which guests it applies to.
    pub selector: Selector,
    /// An optional `PVE::JSONSchema`-dialect description of the subtree,
    /// passed through verbatim for the UI.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grammar: Option<Value>,
}

/// One registration file.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Registration {
    /// The file's base name without the extension — the override key.
    pub name: String,
    /// The PVE user or token id this registration is for.
    pub authid: String,
    /// A human description, for the UI.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The prefixes it claims.
    pub scopes: Vec<RegisteredScope>,
}

// -- strict parsing ------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRegistration {
    authid: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    scopes: Vec<RawScope>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawScope {
    prefix: String,
    mode: String,
    #[serde(default)]
    selector: Option<RawSelector>,
    #[serde(default)]
    grammar: Option<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSelector {
    #[serde(default)]
    all: Option<bool>,
    #[serde(default)]
    tag: Option<String>,
}

fn bad(msg: impl std::fmt::Display) -> Error {
    Error::Registration(msg.to_string())
}

/// `true` if `s` is a PVE realm (or token sub-id):
/// `[A-Za-z][A-Za-z0-9.\-_]+` — `PVE::Auth::Plugin`'s `$realm_regex`, which
/// also backs `PVE::AccessControl`'s `$token_subid_regex`.
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
/// `PVE::AccessControl`'s `$userid_or_token_regex` transliterated:
/// `^[^\s:/]+@[A-Za-z][A-Za-z0-9.\-_]+(?:![A-Za-z][A-Za-z0-9.\-_]+)?$`. The
/// user part may itself contain `@`, so the split is tried from the right —
/// the same answer Perl's greedy match gives.
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

/// Parses one registration file's text. `name` is the file's base name (the
/// override key).
///
/// # Errors
/// [`Error::Registration`] describing the first problem; [`Error::Parse`] if
/// the text is not YAML at all.
pub fn parse(name: &str, text: &str) -> Result<Registration> {
    // `parse_raw` also applies the store's YAML safety rules (no anchors,
    // aliases, explicit tags or complex keys).
    let value = format::parse_raw(Format::Yaml, text)?;
    let raw: RawRegistration =
        serde_json::from_value(value).map_err(|e| bad(format!("{name}: {e}")))?;

    if !is_authid(&raw.authid) {
        return Err(bad(format!(
            "{name}: 'authid' ({}) is not a PVE user or token id (user@realm, optionally !tokenid)",
            raw.authid
        )));
    }

    let mut scopes = Vec::with_capacity(raw.scopes.len());
    for (i, s) in raw.scopes.into_iter().enumerate() {
        let where_ = format!("{name}: scopes.{i}");
        let prefix = Path::parse(&s.prefix)
            .map_err(|_| bad(format!("{where_}: invalid prefix '{}'", s.prefix)))?;
        if prefix.is_root() {
            return Err(bad(format!(
                "{where_}: 'prefix' must not be empty (whole-document access comes \
                 from PVE ACLs, never from a scope)"
            )));
        }
        let mode = match s.mode.as_str() {
            "ro" => Mode::Ro,
            "rw" => Mode::Rw,
            other => {
                return Err(bad(format!(
                    "{where_}: invalid mode '{other}' (expected 'ro' or 'rw')"
                )))
            }
        };
        let selector = match s.selector {
            None => {
                return Err(bad(format!(
                    "{where_}: missing 'selector' (use {{ all: true }} or {{ tag: <name> }})"
                )))
            }
            Some(RawSelector { all: Some(true), tag: None }) => Selector::All,
            Some(RawSelector { all: None, tag: Some(t) }) if !t.is_empty() => Selector::Tag(t),
            Some(_) => {
                return Err(bad(format!(
                    "{where_}: invalid 'selector' (exactly one of {{ all: true }} or \
                     {{ tag: <name> }})"
                )))
            }
        };
        scopes.push(RegisteredScope {
            prefix,
            mode,
            selector,
            grammar: s.grammar,
        });
    }

    Ok(Registration {
        name: name.to_string(),
        authid: raw.authid,
        description: raw.description,
        scopes,
    })
}

/// The registration directories to read, lowest precedence first: the
/// colon-separated [`DIRS_ENV`] if set, else [`PACKAGED_DIR`] then
/// [`CLUSTER_DIR`].
pub fn default_dirs() -> Vec<PathBuf> {
    match std::env::var_os(DIRS_ENV) {
        Some(v) => std::env::split_paths(&v).filter(|p| !p.as_os_str().is_empty()).collect(),
        None => vec![PathBuf::from(PACKAGED_DIR), PathBuf::from(CLUSTER_DIR)],
    }
}

/// Reads every `*.yaml` in `dirs` (lowest precedence first), later
/// directories overriding earlier ones **by file name**. Files are sorted by
/// name; a malformed file is skipped with a `warn!` and grants nothing.
///
/// A directory that does not exist is not an error — neither the packaged nor
/// the cluster directory is required to be there.
pub fn load(dirs: &[PathBuf]) -> Vec<Registration> {
    let mut by_name: BTreeMap<String, Registration> = BTreeMap::new();
    for dir in dirs {
        for (name, path) in yaml_files(dir) {
            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!(file = %path.display(), error = %e, "skipping unreadable operator registration");
                    continue;
                }
            };
            match parse(&name, &text) {
                Ok(reg) => {
                    by_name.insert(name, reg);
                }
                Err(e) => {
                    tracing::warn!(file = %path.display(), error = %e, "skipping malformed operator registration");
                }
            }
        }
    }
    by_name.into_values().collect()
}

/// [`load`] over [`default_dirs`].
pub fn load_default() -> Vec<Registration> {
    load(&default_dirs())
}

fn yaml_files(dir: &FsPath) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let file_name = entry.file_name().to_string_lossy().into_owned();
        if file_name.starts_with('.') {
            continue;
        }
        let Some(stem) = file_name.strip_suffix(".yaml") else {
            continue;
        };
        if stem.is_empty() || !entry.path().is_file() {
            continue;
        }
        out.push((stem.to_string(), entry.path()));
    }
    out.sort();
    out
}

/// The scopes `authid` holds on a guest carrying `tags`: the union of the
/// scope entries of every registration for that authid whose selector
/// matches (`docs/DESIGN.md` §3).
///
/// Scopes apply to **guest documents only**; the datacenter document is
/// governed by ACLs alone, so this is never called for it.
pub fn scopes_for(regs: &[Registration], authid: &str, tags: &[String]) -> Vec<Scope> {
    let mut out = Vec::new();
    for reg in regs {
        if reg.authid != authid {
            continue;
        }
        for s in &reg.scopes {
            if s.selector.matches(tags) {
                out.push(Scope {
                    prefix: s.prefix.clone(),
                    mode: s.mode,
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const EXAMPLE: &str = "\
authid: svc@pve!traefik
description: Traefik dynamic-configuration provider
scopes:
  - prefix: traefik
    mode: rw
    selector: { all: true }
    grammar:
      type: object
      properties:
        spec:
          type: object
";

    fn p(s: &str) -> Path {
        Path::parse(s).unwrap()
    }

    #[test]
    fn parses_the_design_doc_example() {
        let reg = parse("traefik", EXAMPLE).unwrap();
        assert_eq!(reg.name, "traefik");
        assert_eq!(reg.authid, "svc@pve!traefik");
        assert_eq!(reg.description.as_deref(), Some("Traefik dynamic-configuration provider"));
        assert_eq!(reg.scopes.len(), 1);
        assert_eq!(reg.scopes[0].prefix, p("traefik"));
        assert_eq!(reg.scopes[0].mode, Mode::Rw);
        assert_eq!(reg.scopes[0].selector, Selector::All);
        assert!(reg.scopes[0].grammar.is_some());
    }

    #[test]
    fn nested_prefixes_are_allowed() {
        let text = "authid: a@pve\nscopes:\n  - prefix: traefik.routers\n    mode: ro\n    selector: {tag: web}\n";
        let reg = parse("a", text).unwrap();
        assert_eq!(reg.scopes[0].prefix, p("traefik.routers"));
        assert_eq!(reg.scopes[0].selector, Selector::Tag("web".to_string()));
    }

    #[test]
    fn parsing_is_strict() {
        for (text, expect) in [
            ("authid: not-an-authid\n", "PVE user or token id"),
            ("authid: a@pve\ntypo: 1\n", "unknown field"),
            (
                "authid: a@pve\nscopes:\n  - prefix: ''\n    mode: rw\n    selector: {all: true}\n",
                "must not be empty",
            ),
            (
                "authid: a@pve\nscopes:\n  - prefix: x\n    mode: readwrite\n    selector: {all: true}\n",
                "invalid mode",
            ),
            ("authid: a@pve\nscopes:\n  - prefix: x\n    mode: rw\n", "missing 'selector'"),
            (
                "authid: a@pve\nscopes:\n  - prefix: x\n    mode: rw\n    selector: {}\n",
                "invalid 'selector'",
            ),
            (
                "authid: a@pve\nscopes:\n  - prefix: x\n    mode: rw\n    selector: {all: true, tag: t}\n",
                "invalid 'selector'",
            ),
            (
                "authid: a@pve\nscopes:\n  - prefix: x\n    mode: rw\n    selector: {pool: web}\n",
                "unknown field",
            ),
            ("authid: a@pve\nscopes: [1, 2]\n", "expected struct RawScope"),
        ] {
            let err = parse("a", text).unwrap_err().to_string();
            assert!(err.contains(expect), "{text:?}: {err}");
        }
        // Not YAML at all.
        assert!(matches!(parse("a", "a: 1\n\tb: 2\n"), Err(Error::Parse { .. })));
    }

    #[test]
    fn is_authid_matches_pve_accesscontrols_shape() {
        for good in [
            "root@pam",
            "svc@pve!traefik",
            "john.doe@pve",
            "svc@ldap.corp",
            "first.last@ldap.corp!token-1",
            "weird@name@pve",
        ] {
            assert!(is_authid(good), "{good} should be an authid");
        }
        for bad in [
            "root", "root@", "root@p", "@pve", "root@1pve", "root@pve!", "root@pve!1t",
            "ro ot@pve", "ro:ot@pve", "ro/ot@pve", "",
        ] {
            assert!(!is_authid(bad), "{bad} should not be an authid");
        }
    }

    #[test]
    fn a_cluster_file_overrides_the_packaged_one_of_the_same_name() {
        let packaged = tempfile::tempdir().unwrap();
        let cluster = tempfile::tempdir().unwrap();
        std::fs::write(packaged.path().join("traefik.yaml"), EXAMPLE).unwrap();
        std::fs::write(packaged.path().join("netbird.yaml"),
            "authid: svc@pve!netbird\nscopes:\n  - prefix: netbird\n    mode: ro\n    selector: {all: true}\n").unwrap();
        std::fs::write(
            cluster.path().join("traefik.yaml"),
            "authid: other@pve!t1\nscopes:\n  - prefix: traefik\n    mode: ro\n    selector: {tag: traefik}\n",
        )
        .unwrap();

        let regs = load(&[packaged.path().to_path_buf(), cluster.path().to_path_buf()]);
        assert_eq!(regs.len(), 2, "{regs:?}");
        let traefik = regs.iter().find(|r| r.name == "traefik").unwrap();
        assert_eq!(traefik.authid, "other@pve!t1", "the cluster file wins");
        assert_eq!(traefik.scopes[0].mode, Mode::Ro);
        assert!(regs.iter().any(|r| r.name == "netbird"), "unrelated files survive");
    }

    #[test]
    fn a_malformed_file_is_skipped_and_grants_nothing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("broken.yaml"), "authid: a@pve\n\tnope\n").unwrap();
        std::fs::write(dir.path().join("nonsense.yaml"), "authid: not-an-authid\n").unwrap();
        std::fs::write(dir.path().join("ignored.txt"), "authid: x@pve\n").unwrap();
        std::fs::write(
            dir.path().join("good.yaml"),
            "authid: good@pve!tok\nscopes:\n  - prefix: traefik\n    mode: rw\n    selector: {all: true}\n",
        )
        .unwrap();

        let regs = load(&[dir.path().to_path_buf()]);
        assert_eq!(regs.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), vec!["good"]);
        assert_eq!(scopes_for(&regs, "a@pve", &[]), vec![]);
        assert_eq!(
            scopes_for(&regs, "good@pve!tok", &[]),
            vec![Scope { prefix: p("traefik"), mode: Mode::Rw }]
        );
    }

    #[test]
    fn a_selector_serializes_as_it_is_written() {
        // `GET /meta/operators` hands the UI the same shape an administrator
        // edits: `{all: true}` / `{tag: <name>}`, never a bare string.
        assert_eq!(serde_json::to_value(Selector::All).unwrap(), serde_json::json!({"all": true}));
        assert_eq!(
            serde_json::to_value(Selector::Tag("traefik".into())).unwrap(),
            serde_json::json!({"tag": "traefik"})
        );
    }

    #[test]
    fn a_missing_directory_is_not_an_error() {
        assert!(load(&[PathBuf::from("/nonexistent/pve-meta/operators")]).is_empty());
    }

    #[test]
    fn selectors_are_resolved_against_the_guests_tags() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("scoped.yaml"),
            "authid: scoped@pve!t1\nscopes:\n\
             \x20 - prefix: traefik\n    mode: rw\n    selector: {tag: traefik}\n\
             \x20 - prefix: netbird\n    mode: ro\n    selector: {all: true}\n",
        )
        .unwrap();
        let regs = load(&[dir.path().to_path_buf()]);

        let untagged = scopes_for(&regs, "scoped@pve!t1", &[]);
        assert_eq!(untagged, vec![Scope { prefix: p("netbird"), mode: Mode::Ro }]);

        let tagged = scopes_for(&regs, "scoped@pve!t1", &["traefik".to_string()]);
        assert_eq!(
            tagged,
            vec![
                Scope { prefix: p("traefik"), mode: Mode::Rw },
                Scope { prefix: p("netbird"), mode: Mode::Ro },
            ]
        );

        // Another principal gets nothing from this registration.
        assert!(scopes_for(&regs, "other@pve", &["traefik".to_string()]).is_empty());
    }
}
