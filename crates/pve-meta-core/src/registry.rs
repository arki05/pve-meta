//! The two drop directories: **namespaces** (what a prefix is) and **grants**
//! (who may touch one) — `docs/DESIGN.md` §3.
//!
//! Both live outside the documents, one file each, parsed strictly and
//! independently: a malformed file is skipped with a warning and contributes
//! nothing, and never affects another file. That isolation is the whole
//! reason this data left `datacenter.yaml`.
//!
//! # Namespaces
//!
//! * `/usr/share/pve-meta/namespaces/<prefix>.yaml` — packaged defaults, dropped
//!   in by an operator's own `.deb`;
//! * `/etc/pve/meta.d/namespaces/<prefix>.yaml` — cluster overrides, by file name.
//!
//! **The file name is the prefix**, so one namespace is exactly one prefix and
//! there is no `prefix:` field to disagree with it. A prefix segment is
//! `[A-Za-z0-9_@!-]+` and dots are only separators
//! ([`crate::path::is_valid_segment`]), so a namespace file name can never
//! contain a slash, never start with a dot and never escape its directory.
//!
//! ```yaml
//! # namespaces/traefik.yaml
//! description: Traefik dynamic configuration
//! selector: { tag: traefik }
//! schema:                      # optional, PVE::JSONSchema dialect
//!   type: object
//! ```
//!
//! A namespace names no principal: declaring that a prefix exists and has a
//! shape is useful with no operator, no token and no automation anywhere near
//! it.
//!
//! # Grants
//!
//! * `/etc/pve/meta.d/grants/<name>.yaml` — cluster only. **There is
//!   deliberately no packaged grants directory**: an operator's `.deb` may ship
//!   a namespace (a declaration) but must never ship its own grant, which would
//!   be self-registration. dpkg cannot write into pmxcfs, so "an operator
//!   declares what it expects; only an administrator grants it" is enforced by
//!   where files live rather than by a rule.
//!
//! ```yaml
//! # grants/traefik.yaml
//! authid: svc@pve!traefik
//! grants:
//!   - prefix: traefik
//!     mode: rw
//!     selector: { tag: traefik }
//! ```
//!
//! # The two nesting rules are opposites, deliberately
//!
//! [`governing`]: **most-specific wins, schemas never merge.** The namespace
//! with the longest prefix covering a path governs it; no other contributes.
//!
//! [`scopes_for`]: **grants accumulate by containment.** A grant on `homelab`
//! covers `homelab.docker`, because "you may write `homelab`" not implying its
//! subtree would be surprising.
//!
//! Shape has one owner, so it shadows; permission is a union, so it adds. Those
//! two rules cannot live on one object, which is the concrete reason revision 6
//! made this two concepts and not one (`docs/DESIGN.md` §12).

use std::collections::BTreeMap;
use std::path::{Path as FsPath, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::format::{self, Format};
use crate::model::Value;
use crate::path::Path;
use crate::scopes::{Mode, Scope};

/// The packaged namespace directory.
pub const NAMESPACE_PACKAGED_DIR: &str = "/usr/share/pve-meta/namespaces";
/// The cluster-wide namespace directory (pmxcfs); overrides
/// [`NAMESPACE_PACKAGED_DIR`] by file name.
pub const NAMESPACE_CLUSTER_DIR: &str = "/etc/pve/meta.d/namespaces";
/// The grants directory. Cluster only, on purpose — see the module docs.
pub const GRANT_CLUSTER_DIR: &str = "/etc/pve/meta.d/grants";

/// Environment variable overriding the namespace directories with a
/// colon-separated list, lowest precedence first. Tests and `test/basic.pl`.
pub const NAMESPACE_DIRS_ENV: &str = "PVE_META_NAMESPACE_DIRS";
/// Environment variable overriding the grants directories, likewise.
pub const GRANT_DIRS_ENV: &str = "PVE_META_GRANT_DIRS";

/// Which guests something applies to (`docs/DESIGN.md` §3). Room is left in
/// the format for `{ pool: <name> }`; it is deliberately not implemented.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selector {
    /// Every guest.
    All,
    /// Guests carrying this PVE tag. Adding the tag is the deliberate act of
    /// including that guest.
    Tag(String),
}

/// Serialized exactly as written in the file — `{all: true}` or `{tag: <name>}`
/// — so the API hands the UI the same shape an administrator edits.
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

/// One namespace: a prefix, what it is, and where it applies.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Namespace {
    /// The prefix, taken from the file name. Non-empty.
    pub prefix: Path,
    /// A human description, for the UI.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Which guests it applies to — and therefore where its declared-but-unset
    /// rows appear.
    pub selector: Selector,
    /// An optional `PVE::JSONSchema`-dialect description of the subtree, passed
    /// through verbatim for the UI.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<Value>,
}

/// One `grants:` entry.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GrantEntry {
    /// The key-path prefix, any depth. Non-empty.
    pub prefix: Path,
    /// What it grants.
    pub mode: Mode,
    /// Which guests it applies to.
    pub selector: Selector,
}

/// One grants file: what a principal may touch.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Grant {
    /// The file's base name without the extension — the override key.
    pub name: String,
    /// The PVE user or token id this grant is for.
    pub authid: String,
    /// A human description, for the UI.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// What it grants.
    pub grants: Vec<GrantEntry>,
}

// -- strict parsing ------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawNamespace {
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    selector: Option<RawSelector>,
    #[serde(default)]
    schema: Option<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGrant {
    authid: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    grants: Vec<RawGrantEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGrantEntry {
    prefix: String,
    mode: String,
    #[serde(default)]
    selector: Option<RawSelector>,
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
    Error::Registry(msg.to_string())
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
/// `PVE::AccessControl`'s `$userid_or_token_regex` transliterated. The user
/// part may itself contain `@`, so the split is tried from the right — the
/// same answer Perl's greedy match gives.
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

/// The selector an entry declares. Absent is an error: which guests something
/// applies to is never a default worth guessing.
fn parse_selector(where_: &str, raw: Option<RawSelector>) -> Result<Selector> {
    match raw {
        None => Err(bad(format!(
            "{where_}: missing 'selector' (use {{ all: true }} or {{ tag: <name> }})"
        ))),
        Some(RawSelector { all: Some(true), tag: None }) => Ok(Selector::All),
        Some(RawSelector { all: None, tag: Some(t) }) if !t.is_empty() => Ok(Selector::Tag(t)),
        Some(_) => Err(bad(format!(
            "{where_}: invalid 'selector' (exactly one of {{ all: true }} or {{ tag: <name> }})"
        ))),
    }
}

/// Parses one namespace file. `name` is the file's base name, which **is** the
/// prefix.
///
/// # Errors
/// [`Error::Registry`] describing the first problem; [`Error::Parse`] if
/// the text is not YAML at all.
pub fn parse_namespace(name: &str, text: &str) -> Result<Namespace> {
    // Dotted form only. `Path::parse` also accepts `a/b`, which must never be a
    // namespace name: the name is used as a file name, so accepting a separator
    // that is also the filesystem's would be the one way this identity could
    // reach outside its directory.
    if name.contains('/') {
        return Err(bad(format!("{name}: file name is not a valid prefix")));
    }
    let prefix = Path::parse(name)
        .map_err(|_| bad(format!("{name}: file name is not a valid prefix")))?;
    if prefix.is_root() {
        return Err(bad(format!("{name}: file name is not a valid prefix")));
    }
    // `parse_raw` also applies the store's YAML safety rules (no anchors,
    // aliases, explicit tags or complex keys).
    let value = format::parse_raw(Format::Yaml, text)?;
    let raw: RawNamespace =
        serde_json::from_value(value).map_err(|e| bad(format!("{name}: {e}")))?;
    let selector = parse_selector(name, raw.selector)?;
    Ok(Namespace {
        prefix,
        description: raw.description,
        selector,
        schema: raw.schema,
    })
}

/// Parses one grants file. `name` is the file's base name (the override key).
///
/// # Errors
/// As [`parse_namespace`].
pub fn parse_grant(name: &str, text: &str) -> Result<Grant> {
    let value = format::parse_raw(Format::Yaml, text)?;
    let raw: RawGrant = serde_json::from_value(value).map_err(|e| bad(format!("{name}: {e}")))?;

    if !is_authid(&raw.authid) {
        return Err(bad(format!(
            "{name}: 'authid' ({}) is not a PVE user or token id (user@realm, optionally !tokenid)",
            raw.authid
        )));
    }

    let mut grants = Vec::with_capacity(raw.grants.len());
    for (i, g) in raw.grants.into_iter().enumerate() {
        let where_ = format!("{name}: grants.{i}");
        let prefix = Path::parse(&g.prefix)
            .map_err(|_| bad(format!("{where_}: invalid prefix '{}'", g.prefix)))?;
        if prefix.is_root() {
            return Err(bad(format!(
                "{where_}: 'prefix' must not be empty (whole-document access comes \
                 from PVE ACLs, never from a grant)"
            )));
        }
        let mode = match g.mode.as_str() {
            "ro" => Mode::Ro,
            "rw" => Mode::Rw,
            other => {
                return Err(bad(format!(
                    "{where_}: invalid mode '{other}' (expected 'ro' or 'rw')"
                )))
            }
        };
        let selector = parse_selector(&where_, g.selector)?;
        grants.push(GrantEntry { prefix, mode, selector });
    }

    Ok(Grant {
        name: name.to_string(),
        authid: raw.authid,
        description: raw.description,
        grants,
    })
}

fn dirs_from(env: &str, defaults: &[&str]) -> Vec<PathBuf> {
    match std::env::var_os(env) {
        Some(v) => std::env::split_paths(&v)
            .filter(|p| !p.as_os_str().is_empty())
            .collect(),
        None => defaults.iter().map(PathBuf::from).collect(),
    }
}

/// The namespace directories, lowest precedence first.
pub fn namespace_dirs() -> Vec<PathBuf> {
    dirs_from(NAMESPACE_DIRS_ENV, &[NAMESPACE_PACKAGED_DIR, NAMESPACE_CLUSTER_DIR])
}

/// The grants directories. One, and cluster-only — see the module docs.
pub fn grant_dirs() -> Vec<PathBuf> {
    dirs_from(GRANT_DIRS_ENV, &[GRANT_CLUSTER_DIR])
}

fn load_dirs<T>(dirs: &[PathBuf], kind: &str, parse: impl Fn(&str, &str) -> Result<T>) -> Vec<(String, T)> {
    let mut by_name: BTreeMap<String, T> = BTreeMap::new();
    for dir in dirs {
        for (name, path) in yaml_files(dir) {
            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!(file = %path.display(), error = %e, kind, "skipping unreadable file");
                    continue;
                }
            };
            match parse(&name, &text) {
                Ok(parsed) => {
                    by_name.insert(name, parsed);
                }
                Err(e) => {
                    tracing::warn!(file = %path.display(), error = %e, kind, "skipping malformed file");
                }
            }
        }
    }
    by_name.into_iter().collect()
}

/// Every namespace in `dirs` (lowest precedence first, later directories
/// overriding earlier **by file name**), **sorted most-specific first** — the
/// order [`governing`] relies on.
pub fn load_namespaces(dirs: &[PathBuf]) -> Vec<Namespace> {
    let mut out: Vec<Namespace> = load_dirs(dirs, "namespace", parse_namespace)
        .into_iter()
        .map(|(_, ns)| ns)
        .collect();
    // Longest prefix first; ties by name, so the order is stable and the UI
    // shows something deterministic.
    out.sort_by(|a, b| {
        b.prefix
            .segments()
            .len()
            .cmp(&a.prefix.segments().len())
            .then_with(|| a.prefix.to_string().cmp(&b.prefix.to_string()))
    });
    out
}

/// Every grant in `dirs`, sorted by file name.
pub fn load_grants(dirs: &[PathBuf]) -> Vec<Grant> {
    load_dirs(dirs, "grant", parse_grant)
        .into_iter()
        .map(|(_, g)| g)
        .collect()
}

/// [`load_namespaces`] over [`namespace_dirs`].
pub fn load_namespaces_default() -> Vec<Namespace> {
    load_namespaces(&namespace_dirs())
}

/// [`load_grants`] over [`grant_dirs`].
pub fn load_grants_default() -> Vec<Grant> {
    load_grants(&grant_dirs())
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

/// The namespace governing `path`: the one whose prefix is the **longest** that
/// covers it, among those whose selector matches `tags`.
///
/// Most-specific wins and schemas never merge (`docs/DESIGN.md` §3.1). With
/// both `homelab` and `homelab.docker` declared, `homelab.docker.compose` is
/// governed by `homelab.docker` alone — `homelab`'s own `properties.docker` is
/// shadowed, not combined. Merging two schemas is what `allOf`/`$ref` exist
/// for, and where parent and child have different owners it would mean two
/// owners fighting over one key.
///
/// `namespaces` must be sorted most-specific first ([`load_namespaces`]), so
/// this is the first match.
///
/// **Nothing in this crate calls it**, and that is deliberate rather than an
/// oversight: schema resolution happens in the editor, which is the only
/// consumer that needs it today. It stays because this crate owns the data
/// model, so this is where the rule and its tests belong — and because the
/// moment a second consumer appears (a client library, a hook script, a second
/// UI) the alternative is each of them re-deriving it. If that never happens,
/// delete it rather than letting it drift from the implementation that runs.
pub fn governing<'a>(
    namespaces: &'a [Namespace],
    path: &Path,
    tags: &[String],
) -> Option<&'a Namespace> {
    namespaces
        .iter()
        .find(|ns| ns.selector.matches(tags) && ns.prefix.is_prefix_of(path))
}

/// The scopes `authid` holds on a guest carrying `tags`: the union of every
/// grant entry for that authid whose selector matches (`docs/DESIGN.md` §3.4).
///
/// Grants **accumulate**: a grant on `homelab` covers `homelab.docker`, because
/// [`crate::scopes::covers`] is prefix containment. That is the opposite of how
/// namespaces nest, and deliberately so — permission is a union, shape is not.
///
/// Grants apply to **guest documents only**; the datacenter document is
/// governed by ACLs alone, so this is never called for it.
pub fn scopes_for(grants: &[Grant], authid: &str, tags: &[String]) -> Vec<Scope> {
    let mut out = Vec::new();
    for g in grants {
        if g.authid != authid {
            continue;
        }
        for e in &g.grants {
            if e.selector.matches(tags) {
                out.push(Scope {
                    prefix: e.prefix.clone(),
                    mode: e.mode,
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NS: &str = "\
description: Traefik dynamic configuration
selector: { tag: traefik }
schema:
  type: object
  properties:
    spec:
      type: object
      properties:
        host: { type: string, description: Public host name }
        port: { type: integer, minimum: 1, maximum: 65535, optional: 1, default: 80 }
";

    const GRANT: &str = "\
authid: svc@pve!traefik
grants:
  - prefix: traefik
    mode: rw
    selector: { tag: traefik }
  - prefix: netbird
    mode: ro
    selector: { all: true }
";

    fn write(dir: &std::path::Path, name: &str, text: &str) {
        std::fs::write(dir.join(name), text).unwrap();
    }

    #[test]
    fn a_namespaces_prefix_is_its_file_name() {
        let ns = parse_namespace("traefik", NS).unwrap();
        assert_eq!(ns.prefix.to_string(), "traefik");
        assert_eq!(ns.selector, Selector::Tag("traefik".into()));
        assert!(ns.schema.is_some());
        // Dotted, any depth -- `homelab.docker.yaml` declares `homelab.docker`.
        let nested = parse_namespace("homelab.docker", "selector: {all: true}\n").unwrap();
        assert_eq!(nested.prefix.to_string(), "homelab.docker");
        assert_eq!(nested.prefix.segments().len(), 2);
    }

    #[test]
    fn a_file_name_that_is_not_a_prefix_is_refused() {
        // A segment is `[A-Za-z0-9_@!-]+`, so none of these can name a namespace --
        // which is also what stops a file name escaping the directory.
        for bad in ["", "a/b", "a b", ".hidden", "a..b", "a."] {
            assert!(
                parse_namespace(bad, "selector: {all: true}\n").is_err(),
                "{bad:?} should not be a valid namespace name",
            );
        }
    }

    #[test]
    fn parsing_is_strict_on_both_kinds() {
        // Unknown fields, a missing selector, a bad authid, an empty prefix.
        assert!(parse_namespace("x", "selector: {all: true}\nnope: 1\n").is_err());
        assert!(parse_namespace("x", "description: no selector\n").is_err());
        assert!(parse_namespace("x", "selector: {all: true, tag: t}\n").is_err());
        assert!(parse_grant("g", "authid: not-an-authid\ngrants: []\n").is_err());
        assert!(parse_grant("g", "authid: a@pve\ngrants: [{prefix: '', mode: rw, selector: {all: true}}]\n").is_err());
        assert!(parse_grant("g", "authid: a@pve\ngrants: [{prefix: p, mode: sideways, selector: {all: true}}]\n").is_err());
        assert!(parse_grant("g", "authid: a@pve\ngrants: [{prefix: p, mode: rw}]\n").is_err());
    }

    #[test]
    fn a_grant_has_no_schema_and_a_namespace_has_no_authid() {
        // The split, asserted: neither file can express the other's job.
        assert!(parse_grant(
            "g",
            "authid: a@pve\ngrants: [{prefix: p, mode: rw, selector: {all: true}, schema: {}}]\n"
        )
        .is_err());
        assert!(parse_namespace("x", "selector: {all: true}\nauthid: a@pve\n").is_err());
    }

    #[test]
    fn a_cluster_file_overrides_the_packaged_one_of_the_same_name() {
        let packaged = tempfile::tempdir().unwrap();
        let cluster = tempfile::tempdir().unwrap();
        write(packaged.path(), "traefik.yaml", NS);
        write(packaged.path(), "netbird.yaml", "selector: {all: true}\n");
        write(cluster.path(), "traefik.yaml", "selector: {all: true}\n");

        let all = load_namespaces(&[packaged.path().into(), cluster.path().into()]);
        assert_eq!(all.len(), 2);
        let traefik = all.iter().find(|n| n.prefix.to_string() == "traefik").unwrap();
        assert_eq!(traefik.selector, Selector::All, "the cluster file wins");
        assert!(traefik.schema.is_none(), "wholesale override, not a merge");
    }

    #[test]
    fn a_malformed_file_is_skipped_and_costs_nobody_else_anything() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "traefik.yaml", NS);
        write(dir.path(), "broken.yaml", "selector: {nonsense: true}\n");
        write(dir.path(), "notyaml.txt", "ignored");
        let all = load_namespaces(&[dir.path().into()]);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].prefix.to_string(), "traefik");
    }

    #[test]
    fn namespaces_load_most_specific_first_and_governing_takes_the_first_match() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "homelab.yaml", "selector: {all: true}\nschema: {type: object}\n");
        write(dir.path(), "homelab.docker.yaml", "selector: {all: true}\nschema: {type: object, properties: {compose: {type: string}}}\n");
        let all = load_namespaces(&[dir.path().into()]);
        assert_eq!(
            all.iter().map(|n| n.prefix.to_string()).collect::<Vec<_>>(),
            vec!["homelab.docker", "homelab"],
            "longest prefix first",
        );

        let p = |s: &str| Path::parse(s).unwrap();
        // Most specific wins; schemas never merge.
        assert_eq!(
            governing(&all, &p("homelab.docker.compose"), &[]).unwrap().prefix.to_string(),
            "homelab.docker",
        );
        assert_eq!(
            governing(&all, &p("homelab.notes"), &[]).unwrap().prefix.to_string(),
            "homelab",
        );
        // The child's own prefix is governed by the child, not the parent.
        assert_eq!(
            governing(&all, &p("homelab.docker"), &[]).unwrap().prefix.to_string(),
            "homelab.docker",
        );
        assert!(governing(&all, &p("unrelated"), &[]).is_none());
    }

    #[test]
    fn governing_respects_the_selector() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.b.yaml", "selector: {tag: deep}\n");
        write(dir.path(), "a.yaml", "selector: {all: true}\n");
        let all = load_namespaces(&[dir.path().into()]);
        let p = Path::parse("a.b.c").unwrap();
        // Without the tag the specific namespace does not apply, so the broad one governs.
        assert_eq!(governing(&all, &p, &[]).unwrap().prefix.to_string(), "a");
        assert_eq!(
            governing(&all, &p, &["deep".to_string()]).unwrap().prefix.to_string(),
            "a.b",
        );
    }

    #[test]
    fn grants_accumulate_by_containment_which_is_the_opposite_of_namespaces() {
        let g = parse_grant("traefik", GRANT).unwrap();
        let scopes = scopes_for(std::slice::from_ref(&g), "svc@pve!traefik", &["traefik".to_string()]);
        assert_eq!(scopes.len(), 2, "both entries, the tag one having matched");

        // A grant on `traefik` covers everything under it -- containment, not
        // most-specific-wins (`docs/DESIGN.md` §3.2).
        let grants = crate::scopes::Grants { full_read: false, full_write: false, scopes };
        assert!(grants.can_write(&Path::parse("traefik.spec.host").unwrap()));
        assert!(grants.can_read(&Path::parse("netbird.groups").unwrap()));
        assert!(!grants.can_write(&Path::parse("netbird.groups").unwrap()), "ro stays ro");

        // The selector still gates: no tag, no traefik scope.
        let untagged = scopes_for(&[g], "svc@pve!traefik", &[]);
        assert_eq!(untagged.len(), 1);
        assert_eq!(untagged[0].prefix.to_string(), "netbird");
    }

    #[test]
    fn a_selector_serializes_as_it_is_written() {
        assert_eq!(serde_json::to_value(Selector::All).unwrap(), json!({"all": true}));
        assert_eq!(
            serde_json::to_value(Selector::Tag("t".into())).unwrap(),
            json!({"tag": "t"}),
        );
    }

    #[test]
    fn a_missing_directory_is_not_an_error() {
        assert!(load_namespaces(&[PathBuf::from("/nonexistent/pve-meta")]).is_empty());
        assert!(load_grants(&[PathBuf::from("/nonexistent/pve-meta")]).is_empty());
    }

    #[test]
    fn is_authid_matches_pve_accesscontrols_shape() {
        for good in ["root@pam", "svc@pve!traefik", "john.doe@pve", "svc@ldap.corp"] {
            assert!(is_authid(good), "{good}");
        }
        for bad in ["root", "root@", "@pve", "root@1pve", "root@pve!", "ro ot@pve", ""] {
            assert!(!is_authid(bad), "{bad}");
        }
    }
}
