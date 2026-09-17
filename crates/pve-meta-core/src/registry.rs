//! The two drop directories: **prefixes** (what a prefix is) and **grants**
//! (who may touch one) — `docs/DESIGN.md` §3, §4.
//!
//! Both live outside the documents, one file each, parsed strictly and
//! independently: a malformed file is skipped with a warning and contributes
//! nothing, and never affects another file. That isolation is the whole
//! reason this data left `datacenter.yaml`.
//!
//! # Prefixes
//!
//! * `/usr/share/pve-meta/prefixes/<prefix>.yaml` — packaged defaults, dropped
//!   in by an operator's own `.deb`;
//! * `/etc/pve/meta.d/prefixes/<prefix>.yaml` — cluster overrides, by file name;
//! * `/etc/pve/nodes/<node>/meta.d/prefixes/<prefix>.yaml` — one node's
//!   overrides, which reach only the guests currently on that node
//!   (`docs/decisions/020-node-level-prefixes.md`).
//!
//! **Precedence is by presence.** The highest-precedence directory that has a
//! file of a name is the file for that name, whether or not it parses: a
//! malformed cluster override contributes a failure and nothing else, and the
//! packaged file it shadows stays inert until the override is fixed or
//! removed. The alternative -- fall back to the packaged file when the
//! override is broken -- would let a typo in a deliberate override quietly
//! re-activate the definition it was written to replace. [`effective_file`]
//! is that rule, and it is the one both the loader and the document store
//! ([`crate::store::MetaStore`]) apply, so the file `GET /meta/prefixes/{name}`
//! opens for repair is always the one the loader judged.
//!
//! A node's directory is the third and highest of those directories, and it
//! exists only for a guest on that node: the set a guest sees is
//! [`Registry::load_prefixes`] for its node, the packaged and cluster files
//! with that node's on top, resolved by name as above. A node file shadows the
//! cluster file of its name whole, like a cluster file shadows a packaged one;
//! between different names the one composition there is stays the
//! most-specific-prefix rule, so a cluster `gpu` and a node `gpu.devices` both
//! apply. Another node's files do not exist for that guest at all. There is no
//! node-level permissions directory: access is the cluster's alone.
//!
//! **The file name is the prefix.** `homelab.docker.yaml` declares the prefix
//! `homelab.docker`, so a definition and its prefix are one thing and there is
//! no `prefix:` field for the two to disagree about. A prefix segment is
//! `[A-Za-z0-9_@!-]+` and dots are only separators
//! ([`crate::path::is_valid_segment`]), so a prefix file name can never
//! contain a slash, never start with a dot and never escape its directory.
//!
//! ```yaml
//! # prefixes/traefik.yaml
//! description: Traefik dynamic configuration
//! selector: { tag: traefik }
//! schema:                      # optional, PVE::JSONSchema dialect
//!   type: object
//! ```
//!
//! A definition names no principal: saying that a prefix exists and has a shape
//! is useful with no operator, no token and no automation anywhere near it. It
//! is also **entirely optional** -- a document may hold any key at all; a
//! definition is only the "give this one a bit more structure" piece, for the
//! operators and hook scripts that want it.
//!
//! # Effective
//!
//! * `/etc/pve/meta.d/permissions/<name>.yaml` — cluster only. **There is
//!   deliberately no packaged permissions directory**: an operator's `.deb` may ship
//!   a prefix definition (what it expects) but must never ship its own grant,
//!   which would be self-registration. dpkg cannot write into pmxcfs, so "an operator
//!   declares what it expects; only an administrator grants it" is enforced by
//!   where files live rather than by a rule.
//!
//! ```yaml
//! # permissions/traefik.yaml
//! authid: svc@pve!traefik
//! rules:
//!   - prefix: traefik
//!     mode: rw
//!     selector: { tag: traefik }
//! ```
//!
//! # The two nesting rules are opposites, deliberately
//!
//! [`crate::shape::Shape::governing`]: **most-specific wins, schemas never
//! merge.** The longest declared prefix covering a path governs it; no other
//! contributes.
//!
//! [`scopes_for`]: **permissions accumulate by containment.** A grant on `homelab`
//! covers `homelab.docker`, because "you may write `homelab`" not implying its
//! subtree would be surprising.
//!
//! Shape has one owner, so it shadows; permission is a union, so it adds. Those
//! two rules cannot live on one object, which is why a prefix and a permission
//! are two concepts, not one (see
//! `docs/decisions/001-prefix-and-permission-are-two-concepts.md`).

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path as FsPath, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::format::{self, Format};
use crate::model::Value;
use crate::path::Path;
use crate::scopes::{Mode, Scope};
use crate::store::gone_is_none;

/// The packaged prefix directory.
pub const PREFIX_PACKAGED_DIR: &str = "/usr/share/pve-meta/prefixes";
/// The cluster-wide prefix directory (pmxcfs); overrides
/// [`PREFIX_PACKAGED_DIR`] by file name.
pub const PREFIX_CLUSTER_DIR: &str = "/etc/pve/meta.d/prefixes";
/// The permissions directory. Cluster only, on purpose — see the module docs.
pub const PERMISSION_CLUSTER_DIR: &str = "/etc/pve/meta.d/permissions";

/// Environment variable overriding the prefix directories with a
/// colon-separated list, lowest precedence first. Tests and `test/basic.pl`.
pub const PREFIX_DIRS_ENV: &str = "PVE_META_PREFIX_DIRS";
/// Environment variable overriding the permissions directories, likewise.
pub const PERMISSION_DIRS_ENV: &str = "PVE_META_PERMISSION_DIRS";
/// The cluster's per-node directories (pmxcfs). A node's prefix files are
/// `<this>/<node>/meta.d/prefixes/<prefix>.yaml`, next to the node's guest
/// configs, and pmxcfs replicates them like everything else in `/etc/pve`.
pub const NODES_DIR: &str = "/etc/pve/nodes";
/// Environment variable overriding [`NODES_DIR`]; set to nothing, no node
/// directory is read or written at all. Tests and `test/basic.pl`.
pub const NODES_DIR_ENV: &str = "PVE_META_NODES_DIR";

/// Which of the two drop directories a [`crate::store::DocId::Registry`] document lives in
/// .
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RegistryKind {
    /// A prefix: what a prefix is ([`PrefixDef`]).
    PrefixDef,
    /// A permission file: who may touch one ([`Permission`]).
    Permission,
}

impl RegistryKind {
    /// The kind's wire name, and the first segment of a registry document's
    /// API id: `prefixes` / `permissions`.
    pub fn as_str(&self) -> &'static str {
        match self {
            RegistryKind::PrefixDef => "prefixes",
            RegistryKind::Permission => "permissions",
        }
    }
}

impl fmt::Display for RegistryKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

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

    /// Reads a selector as the API *lists* it (`GET /meta/prefixes`,
    /// `GET /meta/permissions`), which is [`Selector`]'s own serialization
    /// after a trip through Perl -- where `true` becomes `1`. That is the one
    /// tolerance here; the rule ("exactly one of `all: true` or `tag: <name>`")
    /// is [`parse_selector`]'s, applied unchanged, so a file and a listing
    /// cannot mean different things by the same selector.
    ///
    /// # Errors
    /// [`Error::Registry`] as [`parse_selector`].
    pub fn from_wire(v: &Value) -> Result<Selector> {
        let Some(map) = v.as_object() else {
            return Err(bad("selector: not a map"));
        };
        let all = match map.get("all") {
            None => None,
            Some(Value::Bool(b)) => Some(*b),
            Some(Value::Number(n)) => Some(n.as_i64() == Some(1)),
            Some(Value::String(s)) => Some(s == "1"),
            Some(_) => return Err(bad("selector: 'all' is not a boolean")),
        };
        let tag = match map.get("tag") {
            None => None,
            Some(Value::String(s)) => Some(s.clone()),
            Some(_) => return Err(bad("selector: 'tag' is not a string")),
        };
        if map.keys().any(|k| k != "all" && k != "tag") {
            return Err(bad("selector: unknown field"));
        }
        parse_selector("selector", Some(RawSelector { all, tag }))
    }
}

/// Most-specific first: the longer prefix sorts before the shorter, and
/// equal depths by name, so the order is total and the UI shows something
/// deterministic. The order [`crate::shape::Shape`] resolves in, and the
/// order `GET /meta/prefixes` lists in -- one comparator, so the listing a
/// client sees is the order the server would have resolved.
pub fn by_specificity(a: &Path, b: &Path) -> std::cmp::Ordering {
    b.segments()
        .len()
        .cmp(&a.segments().len())
        .then_with(|| a.to_string().cmp(&b.to_string()))
}

/// One prefix: a prefix, what it is, and where it applies.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PrefixDef {
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
    /// `true` when the server refuses an API write that would leave this
    /// prefix's subtree not matching `schema`, for the paths that write
    /// changed (`api::put_document`; `force=1` stores it anyway). Off by
    /// default: a schema is advisory unless the prefix says otherwise.
    pub enforce: bool,
    /// The root default for the schema's per-node `hidden` (`shape::Described`):
    /// with it set this prefix contributes no declared-but-unset rows unless a
    /// node asks to be shown. A prefix with a vocabulary rather than a handful of
    /// keys wants this; one with five keys does not.
    #[serde(default)]
    pub hidden: bool,
    /// Which directory this one was read from. Not part of the file.
    pub origin: Origin,
    /// The node whose directory it was read from, for [`Origin::Node`] and
    /// only then. Not part of the file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<NodeName>,
    /// `true` when a lower-precedence directory holds a file of the same name that
    /// this one displaced. With the configured directories that is exactly "a
    /// cluster file written over a packaged one", or "a node file written over
    /// either"; with more prefix directories (only reachable through
    /// `PVE_META_PREFIX_DIRS`) a packaged file can displace another packaged
    /// file and this is true of it too. Nothing keys off the combination -- the UI
    /// and the write path both ask [`Origin`] -- so it stays the plain fact it says.
    pub overrides: bool,
}

/// Where a loaded file came from, and what it displaced.
///
/// The loader has always known this -- it walks the directories in precedence
/// order -- and always thrown it away. A list is where it matters: a packaged
/// prefix definition and a cluster override of the same name are the same row
/// in every other respect, and "who owns this file, and is a package's copy
/// underneath it?" is the first question an administrator asks about one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    /// From a package's directory (`/usr/share/pve-meta/prefixes`): read-only,
    /// and replaced rather than edited -- a write creates the cluster file.
    Packaged,
    /// From the cluster directory (`/etc/pve/meta.d/...`).
    Cluster,
    /// From one node's directory (`/etc/pve/nodes/<node>/meta.d/prefixes`):
    /// in effect only for the guests on that node. Prefixes only.
    Node,
}

/// One `rules:` entry.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Rule {
    /// The key-path prefix, any depth. Non-empty.
    pub prefix: Path,
    /// What it grants.
    pub mode: Mode,
    /// Which guests it applies to.
    pub selector: Selector,
}

/// One grants file: what a principal may touch.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Permission {
    /// The file's base name without the extension — the override key.
    pub name: String,
    /// The PVE user or token id this grant is for.
    pub authid: String,
    /// A human description, for the UI.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// What it grants.
    pub rules: Vec<Rule>,
    /// Which directory this one was read from. Always `Cluster` today: there is
    /// no packaged permissions directory, deliberately (see the module docs).
    pub origin: Origin,
    /// `true` when it displaced a same-named file from a lower-precedence
    /// directory. Always `false` while there is only one permissions directory.
    pub overrides: bool,
}

/// A file in a registry directory that did not load: unreadable, or not
/// valid as the kind it lives in (a bad `selector`, an unknown field, text
/// that is not YAML at all). Silently dropping this -- what `load_dirs`
/// always did -- is the one failure mode in the project with no observable
/// symptom (see the module docs): the name is missing everywhere a caller
/// would look for it, and only a log line ever said so.
///
/// A failed cluster override also shadows the packaged file of its name
/// (precedence is by presence), so this row is then the *only* row for that
/// name: the packaged definition is not in effect and is not listed.
///
/// `yaml_files` has already filtered to names [`is_valid_file_name`] accepts
/// before `load_dirs` calls the parser at all, so `name` here is always
/// something `GET /meta/prefixes/{name}` or `GET /meta/permissions/{name}`
/// can open -- never a name a hand-edit made unaddressable in the first place.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RegistryFailure {
    /// The file name without `.yaml`, which for a prefix IS the prefix.
    pub name: String,
    pub origin: Origin,
    /// The node whose directory holds it, for [`Origin::Node`] and only then.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<NodeName>,
    /// Why it did not load, as the parser or the filesystem put it.
    pub error: String,
}

// -- strict parsing ------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPrefixDef {
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    selector: Option<RawSelector>,
    #[serde(default, deserialize_with = "de_flag")]
    enforce: Option<bool>,
    #[serde(default, deserialize_with = "de_flag")]
    hidden: Option<bool>,
    #[serde(default)]
    schema: Option<Value>,
}

/// A prefix-level flag: `true`/`false`, or `1`/`0` as this dialect already
/// spells `optional` and `multiline`, and as the same flag reads on a schema
/// node (`shape`). Anything else is refused, since a file is parsed strictly.
fn de_flag<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Option<bool>, D::Error> {
    let v = Option::<Value>::deserialize(d)?;
    Ok(match v {
        None | Some(Value::Null) => None,
        Some(Value::Bool(b)) => Some(b),
        Some(Value::Number(n)) if n.as_i64() == Some(1) => Some(true),
        Some(Value::Number(n)) if n.as_i64() == Some(0) => Some(false),
        Some(other) => {
            return Err(serde::de::Error::custom(format!(
                "expected true/false or 1/0, got {other}"
            )))
        }
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPermission {
    authid: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    rules: Vec<RawRule>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRule {
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

/// The types a schema may declare: the ones [`crate::shape`] can check.
const SCHEMA_TYPES: &[&str] = &["string", "integer", "number", "boolean", "object", "array"];

/// Refuses a `schema:` whose known keywords would not do what they say.
///
/// The dialect is open-ended (PVE::JSONSchema has more keywords than the
/// editor reads, and a future one must not make today's files unloadable),
/// so an unknown keyword passes. A *known* keyword with a value the checker
/// cannot act on does not: with `enforce: true` the schema is a write gate,
/// and `type: interger` silently constraining nothing, or `enforce: yes`
/// silently inheriting, is exactly the failure a strict parser exists to
/// turn into a loud one. Every other field of the file is already refused
/// on a typo; the schema was the one part that was not.
///
/// `at` is the dotted place in the file an error names, `schema` for the
/// root and `schema.properties.<key>` below it.
fn check_schema_dialect(name: &str, at: &str, node: &Value) -> Result<()> {
    let fail = |msg: String| bad(format!("{name}: {at}: {msg}"));
    let Some(map) = node.as_object() else {
        return Err(fail("a schema node must be a map".into()));
    };

    let declared = match map.get("type") {
        None => None,
        Some(Value::String(t)) if SCHEMA_TYPES.contains(&t.as_str()) => Some(t.as_str()),
        Some(other) => {
            return Err(fail(format!(
                "'type' must be one of {} (got {})",
                SCHEMA_TYPES.join(", "),
                crate::shape::scalar_text(other)
            )))
        }
    };
    let numeric = matches!(declared, Some("integer" | "number"));

    for key in ["description", "format"] {
        if let Some(v) = map.get(key) {
            if !v.is_string() {
                return Err(fail(format!("'{key}' must be a string")));
            }
        }
    }
    for key in ["hidden", "enforce", "multiline", "optional"] {
        if let Some(v) = map.get(key) {
            let ok = v.is_boolean() || matches!(v.as_i64(), Some(0) | Some(1));
            if !ok {
                return Err(fail(format!("'{key}' must be true/false or 1/0")));
            }
        }
    }

    if let Some(members) = map.get("enum") {
        let Some(members) = members.as_array() else {
            return Err(fail("'enum' must be a list".into()));
        };
        if members.is_empty() {
            return Err(fail("'enum' must not be empty".into()));
        }
        for m in members {
            if !(m.is_string() || m.is_number() || m.is_boolean()) {
                return Err(fail("'enum' members must be scalars".into()));
            }
            if let Some(t) = declared {
                if !crate::shape::type_matches(t, m) {
                    return Err(fail(format!(
                        "'enum' member {} is not of type {t}",
                        crate::shape::scalar_text(m)
                    )));
                }
            }
        }
    }

    let bound = |key: &str| -> Result<Option<f64>> {
        match map.get(key) {
            None => Ok(None),
            Some(v) => match v.as_f64() {
                Some(n) if declared.is_none() || numeric => Ok(Some(n)),
                Some(_) => Err(fail(format!("'{key}' has no meaning for type {}", declared.unwrap_or("")))),
                None => Err(fail(format!("'{key}' must be a number"))),
            },
        }
    };
    if let (Some(min), Some(max)) = (bound("minimum")?, bound("maximum")?) {
        if min > max {
            return Err(fail(format!("'minimum' ({min}) is above 'maximum' ({max})")));
        }
    }

    if let (Some(d), Some(t)) = (map.get("default"), declared) {
        if !crate::shape::type_matches(t, d) {
            return Err(fail(format!("'default' is not of type {t}")));
        }
    }

    if let Some(props) = map.get("properties") {
        let Some(props) = props.as_object() else {
            return Err(fail("'properties' must be a map".into()));
        };
        if let Some(t) = declared.filter(|t| *t != "object") {
            return Err(fail(format!("'properties' has no meaning for type {t}")));
        }
        for (key, sub) in props {
            if !crate::path::is_valid_segment(key) {
                return Err(fail(format!("property '{key}' is not a valid key")));
            }
            check_schema_dialect(name, &format!("{at}.properties.{key}"), sub)?;
        }
    }
    Ok(())
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

/// Whether `name` is a usable registry **file** name (the part before `.yaml`).
///
/// One rule, because three places need it and they must agree: `parse_id`
/// turns an API id into a file name, `MetaStore::version` turns a file name
/// back into a document id, and the loader decides which files it reads at all.
///
/// A name is one or more [`crate::path::is_valid_segment`] segments joined by
/// dots -- which is exactly a prefix, since **the file name is the
/// prefix**: `homelab.docker.yaml` declares `homelab.docker`. That admits the
/// dots a prefix needs while still refusing everything that could address
/// another directory (`/`, `..`, a leading dot) or another file type -- and
/// it is at most [`MAX_FILE_NAME_LEN`] long, because the name becomes
/// `<name>.yaml` on disk.
///
/// The API's schema for the `{name}` parameter (`perl/PVE/API2/Ext/Meta.pm`,
/// `pattern` + `maxLength`) mirrors both halves for a friendly 400 before the
/// request reaches Rust; this is the rule.
pub fn is_valid_file_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_FILE_NAME_LEN
        && name.split('.').all(crate::path::is_valid_segment)
}

/// The longest registry file name (without `.yaml`) that is one. Bytes, which
/// for the segment charset (ASCII) is characters. Linux caps a file name at
/// 255 bytes; 128 leaves room for the suffix, a temp-name tag and anything
/// pmxcfs adds, and matches what the API has always accepted -- a longer name
/// could never have been written through it, so no file that loaded before
/// stops loading because of this bound.
pub const MAX_FILE_NAME_LEN: usize = 128;

/// A PVE node name: `PVE::JSONSchema::pve_verify_node_name`'s
/// `[a-zA-Z0-9]([a-zA-Z0-9-]*[a-zA-Z0-9])?`, transliterated, checked when one is
/// made.
///
/// It is the one check between a request and a directory name under
/// [`NODES_DIR`], so nothing that could name another directory -- a dot, a
/// slash, an empty string -- is one. Making or deserializing one is the check,
/// so a node name that reaches a path, a prefix set or a version token was one,
/// and there is no path by which a name that is not quietly means "no node".
/// Whether the node is in the cluster is a question for the caller that holds
/// the nodelist (`PVE::API2::Ext::Meta`, `pve-meta`); this is the shape only.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct NodeName(String);

impl NodeName {
    /// # Errors
    /// [`Error::InvalidName`] when `s` is not a node name.
    pub fn new(s: &str) -> Result<NodeName> {
        let bytes = s.as_bytes();
        let ok = match (bytes.first(), bytes.last()) {
            (Some(first), Some(last)) => {
                first.is_ascii_alphanumeric()
                    && last.is_ascii_alphanumeric()
                    && bytes.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'-')
            }
            _ => false,
        };
        if !ok {
            return Err(Error::InvalidName(s.to_string()));
        }
        Ok(NodeName(s.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for NodeName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for NodeName {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        NodeName::new(&s).map_err(serde::de::Error::custom)
    }
}

/// Parses one prefix file. `name` is the file's base name, which **is** the
/// prefix.
///
/// # Errors
/// [`Error::Registry`] describing the first problem; [`Error::Parse`] if the
/// text is not YAML at all.
pub fn parse_prefix(name: &str, text: &str) -> Result<PrefixDef> {
    // Dotted form only. `Path::parse` also accepts `a/b`, which must never be a
    // prefix name: the name is used as a file name, so accepting a separator
    // that is also the filesystem's would be the one way this identity could
    // reach outside its directory.
    if !is_valid_file_name(name) {
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
    let raw: RawPrefixDef =
        serde_json::from_value(value).map_err(|e| bad(format!("{name}: {e}")))?;
    let selector = parse_selector(name, raw.selector)?;
    if let Some(schema) = &raw.schema {
        check_schema_dialect(name, "schema", schema)?;
    }
    Ok(PrefixDef {
        // A parse knows the text, not the directory: `load_dirs` stamps the real
        // origin over these. The defaults are what a hand-parsed file is -- the
        // cluster's, displacing nothing.
        origin: Origin::Cluster,
        node: None,
        overrides: false,
        prefix,
        description: raw.description,
        selector,
        enforce: raw.enforce.unwrap_or(false),
        hidden: raw.hidden.unwrap_or(false),
        schema: raw.schema,
    })
}

/// Parses one grants file. `name` is the file's base name (the override key).
///
/// # Errors
/// As [`parse_prefix`].
pub fn parse_permission(name: &str, text: &str) -> Result<Permission> {
    let value = format::parse_raw(Format::Yaml, text)?;
    let raw: RawPermission = serde_json::from_value(value).map_err(|e| bad(format!("{name}: {e}")))?;

    if !is_authid(&raw.authid) {
        return Err(bad(format!(
            "{name}: 'authid' ({}) is not a PVE user or token id (user@realm, optionally !tokenid)",
            raw.authid
        )));
    }

    let mut rules = Vec::with_capacity(raw.rules.len());
    for (i, g) in raw.rules.into_iter().enumerate() {
        let where_ = format!("{name}: rules.{i}");
        let prefix = Path::parse(&g.prefix)
            .map_err(|_| bad(format!("{where_}: invalid prefix '{}'", g.prefix)))?;
        if prefix.is_root() {
            return Err(bad(format!(
                "{where_}: 'prefix' must not be empty (whole-document access comes \
                 from PVE ACLs, never from a permission)"
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
        rules.push(Rule { prefix, mode, selector });
    }

    Ok(Permission {
        origin: Origin::Cluster,
        overrides: false,
        name: name.to_string(),
        authid: raw.authid,
        description: raw.description,
        rules,
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

/// The prefix directories, lowest precedence first.
pub fn prefix_dirs() -> Vec<PathBuf> {
    dirs_from(PREFIX_DIRS_ENV, &[PREFIX_PACKAGED_DIR, PREFIX_CLUSTER_DIR])
}

/// The permissions directories. One, and cluster-only — see the module docs.
pub fn permission_dirs() -> Vec<PathBuf> {
    dirs_from(PERMISSION_DIRS_ENV, &[PERMISSION_CLUSTER_DIR])
}

/// The nodes directory, from [`NODES_DIR_ENV`] or [`NODES_DIR`]. `None` when
/// the variable is set to nothing: no node directory is read.
pub fn nodes_dir() -> Option<PathBuf> {
    match std::env::var_os(NODES_DIR_ENV) {
        Some(v) if v.is_empty() => None,
        Some(v) => Some(PathBuf::from(v)),
        None => Some(PathBuf::from(NODES_DIR)),
    }
}

/// Owns the drop-directory lists. Everything that needs to know where a
/// prefix or permission file lives -- `MetaStore`, the perlmod bindings, a
/// test -- is handed one of these rather than reading
/// `PVE_META_PREFIX_DIRS`/`PVE_META_PERMISSION_DIRS`/`PVE_META_NODES_DIR` (or
/// the environment at all) on its own: two callers reading the same variable
/// independently agree only because nothing changes it mid-process, and that
/// was never a guarantee, just an accident of everything running in one process.
#[derive(Debug, Clone)]
pub struct Registry {
    prefix_dirs: Vec<PathBuf>,
    permission_dirs: Vec<PathBuf>,
    nodes_dir: Option<PathBuf>,
}

impl Registry {
    /// Reads `PVE_META_PREFIX_DIRS`/`PVE_META_PERMISSION_DIRS`/`PVE_META_NODES_DIR`
    /// (or the compiled-in defaults) once. See [`crate::store::MetaStore::new`]'s
    /// doc comment for why that has to happen exactly once per store rather than
    /// wherever a directory list happens to be needed next.
    ///
    /// A `Registry` is a building block: it checks nothing about whether its
    /// directories are there to be read, and neither do the free loaders below.
    /// [`crate::store::MetaStore::registry`] hands one out behind the store's
    /// availability check.
    pub fn from_env() -> Self {
        Registry {
            prefix_dirs: prefix_dirs(),
            permission_dirs: permission_dirs(),
            nodes_dir: nodes_dir(),
        }
    }

    /// Explicit directories, lowest precedence first, and no nodes directory --
    /// for tests and sandboxes that must not consult the environment.
    pub fn new(prefix_dirs: Vec<PathBuf>, permission_dirs: Vec<PathBuf>) -> Self {
        Registry { prefix_dirs, permission_dirs, nodes_dir: None }
    }

    /// This registry with `dir` as its nodes directory ([`NODES_DIR`]'s place).
    pub fn with_nodes_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.nodes_dir = Some(dir.into());
        self
    }

    /// The directories of `kind`, lowest precedence first. Cluster-wide ones
    /// only; a node's prefix directory is [`Registry::node_prefix_dir`].
    pub fn dirs(&self, kind: RegistryKind) -> &[PathBuf] {
        match kind {
            RegistryKind::PrefixDef => &self.prefix_dirs,
            RegistryKind::Permission => &self.permission_dirs,
        }
    }

    /// The one directory of `kind` a write may land in, if any is configured.
    ///
    /// **The last directory is the writable one.** This is the one place that
    /// says so; `load_dirs` derives its origin stamping from the same fact,
    /// and `MetaStore::registry_write_dir` calls this rather than restating
    /// it. `None` when `dirs(kind)` is empty, so a caller with no directories
    /// configured -- `PVE_META_*_DIRS` set to nothing, or
    /// [`Registry::new`] given none -- decides its own fallback instead of
    /// silently writing to the compiled-in default.
    pub fn write_dir(&self, kind: RegistryKind) -> Option<PathBuf> {
        self.dirs(kind).last().cloned()
    }

    /// `node`'s prefix directory, `<nodes dir>/<node>/meta.d/prefixes`: where
    /// its files are read from and written to, the one directory of its kind.
    /// `None` when no nodes directory is configured.
    pub fn node_prefix_dir(&self, node: &NodeName) -> Option<PathBuf> {
        Some(self.nodes_dir.as_ref()?.join(node.as_str()).join("meta.d").join("prefixes"))
    }

    /// Every node that has a directory under the nodes directory, sorted. A
    /// directory whose name is not a node name is not a node. What the
    /// every-file listing and the unscoped version token walk; which nodes are
    /// in the cluster is Perl's question, and a removed node's leftover
    /// directory is listed as the files it still holds.
    ///
    /// # Errors
    /// An I/O error other than *not found* listing the nodes directory. An entry
    /// that cannot be looked at is skipped, with a warning.
    pub fn nodes(&self) -> Result<Vec<NodeName>> {
        let Some(dir) = &self.nodes_dir else {
            return Ok(Vec::new());
        };
        let Some(entries) = gone_is_none(std::fs::read_dir(dir))? else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for entry in entries {
            let entry = entry?;
            let Ok(node) = NodeName::new(&entry.file_name().to_string_lossy()) else { continue };
            match gone_is_none(std::fs::metadata(entry.path())) {
                Ok(Some(meta)) if meta.is_dir() => out.push(node),
                Ok(_) => {}
                // One node's entry that cannot be looked at costs that node, not
                // the answer for every other.
                Err(e) => crate::warn_line!("skipping node directory {}: {e}", entry.path().display()),
            }
        }
        out.sort();
        Ok(out)
    }

    /// The file for `name` of `kind`, if any directory has one: the
    /// highest-precedence one that does ([`effective_file`]). What the store
    /// reads, and what the loader parses -- the same file, by construction.
    pub fn locate(&self, kind: RegistryKind, name: &str) -> Option<PathBuf> {
        effective_file(self.dirs(kind), name).map(|(_, path)| path)
    }

    /// The prefixes in effect for a guest on `node` -- or, without one, the
    /// cluster-wide set: the packaged and cluster files with `node`'s on top,
    /// one per name, most-specific first. What decides `enforce` for that guest.
    pub fn load_prefixes(&self, node: Option<&NodeName>) -> Result<Vec<PrefixDef>> {
        // Drops the failures, as the free `load_prefixes` does and for its reason.
        Ok(self.list_prefixes(node.map_or(PrefixSet::Cluster, PrefixSet::Node))?.0)
    }

    /// [`load_permissions`] over this registry's own permission directories.
    pub fn load_permissions(&self) -> Result<Vec<Permission>> {
        Ok(self.list_permissions()?.0)
    }

    /// The prefixes of `set`, plus every file in it that did not load: `GET
    /// /meta/prefixes`' source. In a resolved set `overrides` is decided within
    /// that set, so a node file says whether it displaced a cluster or packaged
    /// file of its name. [`PrefixSet::All`] is not a set any guest sees -- two
    /// rows may name the same prefix -- and a node row's `overrides` there is
    /// what it would be in that node's set.
    ///
    /// A file that cannot be read is a failure like one that does not parse; a
    /// directory that cannot be listed, for any reason but not being there, is an
    /// error for the whole answer, never a set without its files. The same holds
    /// for every walk below.
    pub fn list_prefixes(&self, set: PrefixSet<'_>) -> Result<(Vec<PrefixDef>, Vec<RegistryFailure>)> {
        let node = match set {
            PrefixSet::Cluster => None,
            PrefixSet::Node(node) => Some(node),
            PrefixSet::All => return self.list_every_file(),
        };
        let mut layers = cluster_layers(self.dirs(RegistryKind::PrefixDef));
        if let Some((node, dir)) = node.and_then(|n| Some((n, self.node_prefix_dir(n)?))) {
            layers.push((dir, Source::Node(node.clone())));
        }
        prefixes_with_failures(&layers)
    }

    fn list_every_file(&self) -> Result<(Vec<PrefixDef>, Vec<RegistryFailure>)> {
        let base = self.dirs(RegistryKind::PrefixDef);
        let (mut loaded, mut failures) = self.list_prefixes(PrefixSet::Cluster)?;
        for node in self.nodes()? {
            let Some(dir) = self.node_prefix_dir(&node) else { continue };
            // The node's directory alone: re-reading the cluster-wide files once
            // per node would parse, and log, every broken one of them again.
            let (mut own, own_failures) = prefixes_with_failures(&[(dir, Source::Node(node))])?;
            for p in &mut own {
                p.overrides = effective_file(base, &p.prefix.to_string()).is_some();
            }
            loaded.extend(own);
            failures.extend(own_failures);
        }
        loaded.sort_by(|a, b| by_specificity(&a.prefix, &b.prefix).then_with(|| a.node.cmp(&b.node)));
        Ok((loaded, failures))
    }

    /// Every permission, plus every file that did not load, likewise --
    /// `GET /meta/permissions`'s source.
    pub fn list_permissions(&self) -> Result<(Vec<Permission>, Vec<RegistryFailure>)> {
        permissions_with_failures(self.dirs(RegistryKind::Permission))
    }
}

/// Which prefix files [`Registry::list_prefixes`] is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrefixSet<'a> {
    /// The packaged and cluster files, one per name.
    Cluster,
    /// The same with one node's files on top: the set for a guest on that node.
    Node(&'a NodeName),
    /// Every file -- packaged, cluster and every node's -- each where it came from.
    All,
}

/// Where one directory's files come from: what `load_dirs` stamps on each.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Source {
    Packaged,
    Cluster,
    Node(NodeName),
}

impl Source {
    fn origin(&self) -> Origin {
        match self {
            Source::Packaged => Origin::Packaged,
            Source::Cluster => Origin::Cluster,
            Source::Node(_) => Origin::Node,
        }
    }

    fn node(&self) -> Option<NodeName> {
        match self {
            Source::Node(node) => Some(node.clone()),
            _ => None,
        }
    }
}

/// `dirs` (lowest precedence first) as layers: the last one is the cluster's --
/// [`Registry::write_dir`]'s rule -- and every one below it packaged.
fn cluster_layers(dirs: &[PathBuf]) -> Vec<(PathBuf, Source)> {
    dirs.iter()
        .enumerate()
        .map(|(i, dir)| {
            let source = if i + 1 == dirs.len() { Source::Cluster } else { Source::Packaged };
            (dir.clone(), source)
        })
        .collect()
}

/// The file for `name` among `dirs` (lowest precedence first): the one in
/// the highest-precedence directory that has it, with that directory's
/// index. Presence decides, not content -- see the module docs.
///
/// `name` is a bare file stem; the caller has already checked it with
/// [`is_valid_file_name`], so it cannot carry a separator out of `dirs`.
///
/// A file that cannot even be looked at, for any reason but not being there, is
/// present: reading it is what fails, and that failure is reported for it.
pub fn effective_file(dirs: &[PathBuf], name: &str) -> Option<(usize, PathBuf)> {
    let file = format!("{name}.yaml");
    dirs.iter()
        .enumerate()
        .rev()
        .map(|(index, dir)| (index, dir.join(&file)))
        .find(|(_, path)| is_file_or_unreadable(path))
}

/// `true` for a regular file, and for a path whose `stat` failed with anything
/// but *not found* -- an entry that is there and cannot be looked at, which the
/// read that follows reports.
fn is_file_or_unreadable(path: &FsPath) -> bool {
    match std::fs::metadata(path) {
        Ok(meta) => meta.is_file(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => true,
    }
}

/// What [`load_dirs`] returns: every parsed file by name, and every failure.
type Loaded<T> = (Vec<(String, T)>, Vec<RegistryFailure>);

/// Loads one drop-directory list, later layers overriding earlier by file
/// name, and stamps each survivor with the [`Source`] of its layer.
///
/// One row per name, loaded or failed: every name any directory holds is
/// resolved to its one effective file ([`effective_file`]), and only that
/// file is read and parsed. A file it shadows is not looked at -- not loaded,
/// and not reported either, since nothing it says is in effect.
///
/// Returns the failures alongside the parsed items -- an unreadable file and
/// an unparseable one both count -- so a caller that needs to show them (`GET
/// /meta/prefixes`, `GET /meta/permissions`) can have both from one walk of
/// the directories, instead of two.
fn load_dirs<T>(
    layers: &[(PathBuf, Source)],
    kind: &str,
    parse: impl Fn(&str, &str) -> Result<T>,
    stamp: impl Fn(&mut T, &Source, bool),
) -> Result<Loaded<T>> {
    let dirs: Vec<PathBuf> = layers.iter().map(|(dir, _)| dir.clone()).collect();
    // Every name, and how many directories hold it -- the `overrides` fact.
    let mut holders: BTreeMap<String, usize> = BTreeMap::new();
    for dir in &dirs {
        for (name, _) in yaml_files(dir)? {
            *holders.entry(name).or_insert(0) += 1;
        }
    }
    let mut parsed_out = Vec::new();
    let mut failures = Vec::new();
    for (name, count) in holders {
        // The file `yaml_files` just listed can have vanished since; then the
        // name is simply not there to load.
        let Some((index, path)) = effective_file(&dirs, &name) else { continue };
        let source = &layers[index].1;
        let fail = |name: String, error: String| RegistryFailure {
            name,
            origin: source.origin(),
            node: source.node(),
            error,
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                crate::warn_line!("skipping unreadable {kind} file {}: {e}", path.display());
                failures.push(fail(name, e.to_string()));
                continue;
            }
        };
        match parse(&name, &text) {
            Ok(mut parsed) => {
                stamp(&mut parsed, source, count > 1);
                parsed_out.push((name, parsed));
            }
            Err(e) => {
                crate::warn_line!("skipping malformed {kind} file {}: {e}", path.display());
                failures.push(fail(name, e.to_string()));
            }
        }
    }
    Ok((parsed_out, failures))
}

/// [`load_dirs`] for prefixes, plus the most-specific-first sort the listing
/// promises (`docs/DESIGN.md` §8) -- shared by [`load_prefixes`] (which drops
/// the failures) and the [`Registry`] listings (which keep them), so the
/// two can never compute the sort differently.
fn prefixes_with_failures(layers: &[(PathBuf, Source)]) -> Result<(Vec<PrefixDef>, Vec<RegistryFailure>)> {
    let (parsed, failures) = load_dirs(layers, "prefix", parse_prefix, |p, source, over| {
        p.origin = source.origin();
        p.node = source.node();
        p.overrides = over;
    })?;
    let mut out: Vec<PrefixDef> = parsed.into_iter().map(|(_, ns)| ns).collect();
    out.sort_by(|a, b| by_specificity(&a.prefix, &b.prefix));
    Ok((out, failures))
}

/// Every prefix in `dirs` (lowest precedence first, later directories
/// overriding earlier **by file name**), **sorted most-specific first** — the
/// order `GET /meta/prefixes` lists them in.
pub fn load_prefixes(dirs: &[PathBuf]) -> Result<Vec<PrefixDef>> {
    // Drops the failures, and must keep dropping them: a file that did not
    // parse is not a prefix, and it must never reach a `Shape`, which decides
    // what a document's shape *is*. `Registry::list_prefixes` is the one
    // place the same failure survives, for the listing that has to show it.
    Ok(prefixes_with_failures(&cluster_layers(dirs))?.0)
}

/// [`load_dirs`] for permissions, split out for the same reason as
/// [`prefixes_with_failures`].
fn permissions_with_failures(dirs: &[PathBuf]) -> Result<(Vec<Permission>, Vec<RegistryFailure>)> {
    let (parsed, failures) =
        load_dirs(&cluster_layers(dirs), "permission", parse_permission, |g, source, over| {
            g.origin = source.origin();
            g.overrides = over;
        })?;
    Ok((parsed.into_iter().map(|(_, g)| g).collect(), failures))
}

/// Every grant in `dirs`, sorted by file name.
pub fn load_permissions(dirs: &[PathBuf]) -> Result<Vec<Permission>> {
    // Same drop, the other direction, and just as deliberate: a file that did
    // not parse is not a permission, and it must never reach `scopes_for` --
    // a malformed permission file must grant nothing, not "grant nothing
    // until someone reads the log". `Registry::list_permissions` is where the
    // failure survives instead.
    Ok(permissions_with_failures(dirs)?.0)
}

fn yaml_files(dir: &FsPath) -> Result<Vec<(String, PathBuf)>> {
    let Some(entries) = gone_is_none(std::fs::read_dir(dir))? else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry?;
        let file_name = entry.file_name().to_string_lossy().into_owned();
        if file_name.starts_with('.') {
            continue;
        }
        let Some(stem) = file_name.strip_suffix(".yaml") else {
            continue;
        };
        // The same rule `api::parse_id` and `store::registry_document_id` apply. It
        // was missing here, which meant a hand-created `my file.yaml` *loaded* -- it
        // granted scopes and appeared in the listing -- while `GET
        // /meta/permissions/my file` was a 400, so nothing could open or repair it.
        // `is_valid_file_name`'s own doc says three places need it and must agree;
        // this was the third.
        // An entry that cannot be looked at is listed: reading it fails, and that
        // is a failure row for its name, not a silently shorter set.
        if !is_valid_file_name(stem) || !is_file_or_unreadable(&entry.path()) {
            continue;
        }
        out.push((stem.to_string(), entry.path()));
    }
    out.sort();
    Ok(out)
}

/// The scopes `authid` holds on a guest carrying `tags`: the union of every
/// rule for that authid whose selector matches (`docs/DESIGN.md` §5).
///
/// Effective **accumulate**: a rule on `homelab` covers `homelab.docker`, because
/// [`crate::scopes::covers`] is prefix containment. That is the opposite of how
/// prefixes nest, and deliberately so — permission is a union, shape is not.
///
/// Permissions apply to **guest documents only**; a registry document is
/// governed by ACLs alone, so this is never called for one.
pub fn scopes_for(files: &[Permission], authid: &str, tags: &[String]) -> Vec<Scope> {
    rules_reaching(files, tags)
        .filter(|(file, _)| file.authid == authid)
        .map(|(_, rule)| Scope { prefix: rule.prefix.clone(), mode: rule.mode })
        .collect()
}

/// Every rule, from every permission file, whose selector matches a guest
/// carrying `tags` -- with the file it came from. [`scopes_for`] is this
/// narrowed to one principal; the editor's Access column is this for all of
/// them ("who may touch this row"), and the two must not decide reach
/// differently.
pub fn rules_reaching<'a>(
    files: &'a [Permission],
    tags: &'a [String],
) -> impl Iterator<Item = (&'a Permission, &'a Rule)> + 'a {
    files
        .iter()
        .flat_map(|file| file.rules.iter().map(move |rule| (file, rule)))
        .filter(move |(_, rule)| rule.selector.matches(tags))
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

    const PERMISSION_FILE: &str = "\
authid: svc@pve!traefik
rules:
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
    fn a_file_the_api_could_not_address_is_not_loaded_either() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ok.yaml"), "selector: {all: true}\n").unwrap();
        std::fs::write(dir.path().join("homelab.docker.yaml"), "selector: {all: true}\n").unwrap();
        // Names no id can name: `GET /meta/prefixes/<name>` refuses all three, so
        // loading them would put a prefix in the listing that nothing can open.
        for bad in ["my file", "a/b", "a..b", ".hidden"] {
            let _ = std::fs::write(dir.path().join(format!("{bad}.yaml")), "selector: {all: true}\n");
        }
        let loaded: Vec<String> = load_prefixes(&[dir.path().to_path_buf()]).unwrap()
            .into_iter()
            .map(|p| p.prefix.to_string())
            .collect();
        assert_eq!(loaded, ["homelab.docker", "ok"], "only the addressable ones");
    }

    #[test]
    fn a_loaded_file_says_which_directory_it_came_from() {
        let dir = tempfile::tempdir().unwrap();
        let packaged = dir.path().join("packaged");
        let cluster = dir.path().join("cluster");
        std::fs::create_dir_all(&packaged).unwrap();
        std::fs::create_dir_all(&cluster).unwrap();
        std::fs::write(packaged.join("traefik.yaml"), "selector: {all: true}\n").unwrap();
        std::fs::write(packaged.join("onlypkg.yaml"), "selector: {all: true}\n").unwrap();
        std::fs::write(
            cluster.join("traefik.yaml"),
            "description: overridden\nselector: {tag: traefik}\n",
        )
        .unwrap();
        std::fs::write(cluster.join("mine.yaml"), "selector: {all: true}\n").unwrap();

        let loaded = load_prefixes(&[packaged, cluster]).unwrap();
        let by = |name: &str| {
            loaded
                .iter()
                .find(|p| p.prefix.to_string() == name)
                .unwrap_or_else(|| panic!("{name} was not loaded"))
        };

        // The one an administrator wrote over a package's copy: theirs, and it
        // displaced something -- the two facts a list has to show separately,
        // since "cluster" alone cannot tell you a delete would not remove it.
        assert_eq!(by("traefik").origin, Origin::Cluster);
        assert!(by("traefik").overrides);
        assert_eq!(by("traefik").description.as_deref(), Some("overridden"));

        assert_eq!(by("onlypkg").origin, Origin::Packaged);
        assert!(!by("onlypkg").overrides);
        assert_eq!(by("mine").origin, Origin::Cluster);
        assert!(!by("mine").overrides);

        // Same rule as `Registry::write_dir` -- with a single directory
        // nothing is packaged.
        let only = load_prefixes(&[dir.path().join("cluster")]).unwrap();
        assert!(only.iter().all(|p| p.origin == Origin::Cluster));
    }

    #[test]
    fn a_wire_selector_is_the_file_selector_after_perls_booleans() {
        // Perl renders `true` as `1`; the rule is still `parse_selector`'s.
        assert_eq!(Selector::from_wire(&json!({"all": true})).unwrap(), Selector::All);
        assert_eq!(Selector::from_wire(&json!({"all": 1})).unwrap(), Selector::All);
        assert_eq!(Selector::from_wire(&json!({"tag": "t"})).unwrap(), Selector::Tag("t".into()));
        for bad in [json!({}), json!({"all": 0}), json!({"all": true, "tag": "t"}), json!({"tag": ""}), json!({"pool": "p"}), json!("all")] {
            assert!(Selector::from_wire(&bad).is_err(), "{bad} should not be a selector");
        }
    }

    #[test]
    fn a_registry_file_name_is_a_dotted_prefix_and_never_a_path() {
        for good in ["traefik", "homelab.docker", "a-b_c", "svc@pve!t1"] {
            assert!(is_valid_file_name(good), "{good} was refused");
        }
        for bad in ["", ".", "..", ".hidden", "a/b", "a b", "a..b", "a.", ".a"] {
            assert!(!is_valid_file_name(bad), "{bad} was accepted");
        }
        // The length bound the API schema has always carried (`maxLength => 128`)
        // is part of the rule, so the editor's New dialog refuses what the server
        // would refuse.
        assert!(is_valid_file_name(&"a".repeat(MAX_FILE_NAME_LEN)));
        assert!(!is_valid_file_name(&"a".repeat(MAX_FILE_NAME_LEN + 1)));
        assert!(parse_prefix(&"a".repeat(MAX_FILE_NAME_LEN + 1), "selector: {all: true}\n").is_err());
        // The two halves of "the file name is the prefix" have to agree: a name
        // this accepts must parse as a prefix, and one it refuses must not.
        assert_eq!(
            parse_prefix("homelab.docker", "selector: {all: true}\n")
                .unwrap()
                .prefix
                .to_string(),
            "homelab.docker",
        );
        assert!(parse_prefix("a b", "selector: {all: true}\n").is_err());
    }

    #[test]
    fn a_definitions_prefix_is_its_file_name() {
        let ns = parse_prefix("traefik", NS).unwrap();
        assert_eq!(ns.prefix.to_string(), "traefik");
        assert_eq!(ns.selector, Selector::Tag("traefik".into()));
        assert!(ns.schema.is_some());
        // Dotted, any depth -- `homelab.docker.yaml` declares `homelab.docker`.
        let nested = parse_prefix("homelab.docker", "selector: {all: true}\n").unwrap();
        assert_eq!(nested.prefix.to_string(), "homelab.docker");
        assert_eq!(nested.prefix.segments().len(), 2);
    }

    #[test]
    fn a_file_name_that_is_not_a_prefix_is_refused() {
        // A segment is `[A-Za-z0-9_@!-]+`, so none of these can name a prefix --
        // which is also what stops a file name escaping the directory.
        for bad in ["", "a/b", "a b", ".hidden", "a..b", "a."] {
            assert!(
                parse_prefix(bad, "selector: {all: true}\n").is_err(),
                "{bad:?} should not be a valid prefix name",
            );
        }
    }

    #[test]
    fn parsing_is_strict_on_both_kinds() {
        // Unknown fields, a missing selector, a bad authid, an empty prefix.
        assert!(parse_prefix("x", "selector: {all: true}\nnope: 1\n").is_err());
        assert!(parse_prefix("x", "description: no selector\n").is_err());
        assert!(parse_prefix("x", "selector: {all: true, tag: t}\n").is_err());
        assert!(parse_permission("g", "authid: not-an-authid\nrules: []\n").is_err());
        assert!(parse_permission("g", "authid: a@pve\nrules: [{prefix: '', mode: rw, selector: {all: true}}]\n").is_err());
        assert!(parse_permission("g", "authid: a@pve\nrules: [{prefix: p, mode: sideways, selector: {all: true}}]\n").is_err());
        assert!(parse_permission("g", "authid: a@pve\nrules: [{prefix: p, mode: rw}]\n").is_err());
    }

    /// One line per rule of the dialect check: what it refuses, and the
    /// place in the file the error names. The last lines are what it must
    /// keep accepting -- an unknown keyword, the `1`/`0` flag spelling, and
    /// the wire's `1` as a boolean default.
    #[test]
    fn a_schema_with_a_known_keyword_it_cannot_act_on_is_refused() {
        let refused: Vec<(&str, &str, &str)> = vec![
            ("schema: 1\n", "schema: a schema node must be a map", "not a map at all"),
            ("schema: {type: interger}\n", "schema: 'type' must be one of", "the typo that would enforce nothing"),
            ("schema: {type: [string]}\n", "schema: 'type' must be one of", "a list is not a type"),
            ("schema: {type: object, properties: {p: {type: integer, enum: ['1']}}}\n",
             "schema.properties.p: 'enum' member 1 is not of type integer", "an enum the value check could never admit"),
            ("schema: {enum: []}\n", "schema: 'enum' must not be empty", "nothing would ever match"),
            ("schema: {enum: x}\n", "schema: 'enum' must be a list", "not a list"),
            ("schema: {enum: [{a: 1}]}\n", "schema: 'enum' members must be scalars", "a map is not a member"),
            ("schema: {type: integer, minimum: 10, maximum: 3}\n", "schema: 'minimum' (10) is above 'maximum' (3)", "an empty range"),
            ("schema: {minimum: x}\n", "schema: 'minimum' must be a number", "not a number"),
            ("schema: {type: string, maximum: 3}\n", "schema: 'maximum' has no meaning for type string", "a range on a string"),
            ("schema: {type: integer, default: x}\n", "schema: 'default' is not of type integer", "a default the schema itself refuses"),
            ("schema: {enforce: yes}\n", "schema: 'enforce' must be true/false or 1/0", "would silently inherit"),
            ("schema: {hidden: 2}\n", "schema: 'hidden' must be true/false or 1/0", "... 2 is not a flag"),
            ("schema: {description: [x]}\n", "schema: 'description' must be a string", "a description is text"),
            ("schema: {format: 1}\n", "schema: 'format' must be a string", "a format is a name"),
            ("schema: {properties: [a]}\n", "schema: 'properties' must be a map", "properties are keyed"),
            ("schema: {type: string, properties: {a: {}}}\n", "schema: 'properties' has no meaning for type string", "a string has no keys"),
            ("schema: {properties: {'a.b': {}}}\n", "schema: property 'a.b' is not a valid key", "a dotted key is two keys"),
            ("schema: {properties: {a: {properties: {b: {type: nope}}}}}\n",
             "schema.properties.a.properties.b: 'type' must be one of", "the error names the nested place"),
        ];
        for (text, want, why) in refused {
            let text = format!("selector: {{all: true}}\n{text}");
            let err = parse_prefix("t", &text).unwrap_err().to_string();
            assert!(err.contains(want), "{why}: {text:?} gave {err:?}, wanted {want:?}");
        }

        let accepted = [
            "schema: {type: object, properties: {port: {type: integer, minimum: 1, maximum: 65535, default: 80, enum: [80, 443]}}}\n",
            "schema: {type: object, properties: {name: {type: string, format: dns-name, multiline: 0, optional: 1, title: T}}}\n",
            "schema: {type: object, properties: {on: {type: boolean, default: 1, enum: [true, false]}}}\n",
            "schema: {type: object, hidden: 1, enforce: 0, properties: {x: {pattern: '^a', typetext: xy}}}\n",
            "schema: {minimum: 1, maximum: 2}\n",
            "schema: {}\n",
        ];
        for text in accepted {
            let text = format!("selector: {{all: true}}\n{text}");
            parse_prefix("t", &text).unwrap_or_else(|e| panic!("{text:?} should load: {e}"));
        }
    }

    #[test]
    fn enforce_is_off_unless_the_prefix_says_so() {
        assert!(!parse_prefix("x", "selector: {all: true}\n").unwrap().enforce);
        assert!(parse_prefix("x", "selector: {all: true}\nenforce: true\n").unwrap().enforce);
        assert!(!parse_prefix("x", "selector: {all: true}\nenforce: false\n").unwrap().enforce);
        assert!(parse_prefix("x", "selector: {all: true}\nenforce: nope\n").is_err());
        // The file's own idiom for a flag, as `optional: 1` and `multiline: 1`.
        assert!(parse_prefix("x", "selector: {all: true}\nenforce: 1\n").unwrap().enforce);
        assert!(!parse_prefix("x", "selector: {all: true}\nhidden: 0\n").unwrap().hidden);
        assert!(parse_prefix("x", "selector: {all: true}\nenforce: 2\n").is_err());
    }

    #[test]
    fn a_permission_has_no_schema_and_a_prefix_has_no_authid() {
        // The split, asserted: neither file can express the other's job.
        assert!(parse_permission(
            "g",
            "authid: a@pve\nrules: [{prefix: p, mode: rw, selector: {all: true}, schema: {}}]\n"
        )
        .is_err());
        assert!(parse_prefix("x", "selector: {all: true}\nauthid: a@pve\n").is_err());
    }

    #[test]
    fn a_cluster_file_overrides_the_packaged_one_of_the_same_name() {
        let packaged = tempfile::tempdir().unwrap();
        let cluster = tempfile::tempdir().unwrap();
        write(packaged.path(), "traefik.yaml", NS);
        write(packaged.path(), "netbird.yaml", "selector: {all: true}\n");
        write(cluster.path(), "traefik.yaml", "selector: {all: true}\n");

        let all = load_prefixes(&[packaged.path().into(), cluster.path().into()]).unwrap();
        assert_eq!(all.len(), 2);
        let traefik = all.iter().find(|n| n.prefix.to_string() == "traefik").unwrap();
        assert_eq!(traefik.selector, Selector::All, "the cluster file wins");
        assert!(traefik.schema.is_none(), "wholesale override, not a merge");
    }

    /// Precedence is by presence: the override is the file for its name even
    /// when it does not parse, so a typo in a deliberate override never
    /// quietly puts the packaged definition back in charge.
    #[test]
    fn a_malformed_cluster_override_shadows_the_packaged_file_it_replaces() {
        let packaged = tempfile::tempdir().unwrap();
        let cluster = tempfile::tempdir().unwrap();
        write(packaged.path(), "traefik.yaml", NS);
        write(packaged.path(), "broken.yaml", "selector: {nonsense: true}\n");
        write(cluster.path(), "traefik.yaml", "selector: {nonsense: true}\n");
        write(cluster.path(), "broken.yaml", "selector: {all: true}\n");
        let dirs = [packaged.path().to_path_buf(), cluster.path().to_path_buf()];

        let (loaded, failures) = prefixes_with_failures(&cluster_layers(&dirs)).unwrap();
        assert_eq!(
            loaded.iter().map(|p| p.prefix.to_string()).collect::<Vec<_>>(),
            vec!["broken"],
            "the valid override loads; the broken one shadows its packaged file rather than falling back to it",
        );
        assert!(loaded[0].overrides, "a valid override still says what it displaced");
        assert_eq!(failures.len(), 1, "one row per name: the shadowed packaged files are not reported");
        assert_eq!(failures[0].name, "traefik");
        assert_eq!(failures[0].origin, Origin::Cluster, "the row names the file to repair");

        // The file the store would open for repair is the same one.
        assert_eq!(effective_file(&dirs, "traefik").unwrap().1, cluster.path().join("traefik.yaml"));
        assert_eq!(effective_file(&dirs, "netbird"), None);
    }

    /// Packaged, cluster and one node's directory, as `Registry::from_env`
    /// lays them out, under one tempdir.
    fn three_layers() -> (tempfile::TempDir, Registry) {
        let dir = tempfile::tempdir().unwrap();
        for sub in ["packaged", "cluster", "nodes/pve1/meta.d/prefixes", "nodes/pve2/meta.d/prefixes"] {
            std::fs::create_dir_all(dir.path().join(sub)).unwrap();
        }
        let reg = Registry::new(vec![dir.path().join("packaged"), dir.path().join("cluster")], vec![])
            .with_nodes_dir(dir.path().join("nodes"));
        (dir, reg)
    }

    fn node_dir(dir: &tempfile::TempDir, node: &str) -> PathBuf {
        dir.path().join("nodes").join(node).join("meta.d/prefixes")
    }

    /// Node over cluster over packaged, whole file by name, precedence by
    /// presence -- the rule the two cluster-wide layers already follow, one
    /// layer higher, and only for the guests on that node.
    #[test]
    fn a_node_file_shadows_the_cluster_file_which_shadows_the_packaged_one() {
        let (dir, reg) = three_layers();
        write(&dir.path().join("packaged"), "gpu.yaml", "description: packaged\nselector: {all: true}\n");
        write(&dir.path().join("cluster"), "gpu.yaml", "description: cluster\nselector: {all: true}\n");
        write(&node_dir(&dir, "pve1"), "gpu.yaml", "description: pve1\nselector: {all: true}\n");
        write(&dir.path().join("packaged"), "onlypkg.yaml", "selector: {all: true}\n");
        write(&node_dir(&dir, "pve1"), "local.yaml", "selector: {all: true}\n");

        let n = |s: &str| NodeName::new(s).unwrap();
        let on = |node: Option<&str>| reg.load_prefixes(node.map(n).as_ref()).unwrap();
        let by = |set: &[PrefixDef], name: &str| set.iter().find(|p| p.prefix.to_string() == name).cloned();

        let pve1 = on(Some("pve1"));
        let gpu = by(&pve1, "gpu").unwrap();
        assert_eq!(gpu.description.as_deref(), Some("pve1"), "the node file wins");
        assert_eq!((gpu.origin, gpu.node, gpu.overrides), (Origin::Node, Some(n("pve1")), true));
        assert_eq!(by(&pve1, "onlypkg").unwrap().origin, Origin::Packaged, "the lower layers still apply");
        let local = by(&pve1, "local").unwrap();
        assert!(!local.overrides, "a node file with no namesake below displaces nothing");

        // pve2 has no files of its own: the cluster file is in effect, and
        // pve1's files do not exist for a guest there.
        let pve2 = on(Some("pve2"));
        assert_eq!(by(&pve2, "gpu").unwrap().description.as_deref(), Some("cluster"));
        assert_eq!(by(&pve2, "gpu").unwrap().node, None);
        assert!(by(&pve2, "local").is_none(), "another node's file is invisible");
        assert_eq!(on(None), pve2, "no node is the cluster-wide set");

        // A malformed node file shadows the cluster file of its name and is
        // the failure listed for it, naming its node -- it never falls back.
        write(&node_dir(&dir, "pve1"), "gpu.yaml", "selector: {nonsense: true}\n");
        let (loaded, failures) = reg.list_prefixes(PrefixSet::Node(&n("pve1"))).unwrap();
        assert!(loaded.iter().all(|p| p.prefix.to_string() != "gpu"), "{loaded:?}");
        assert_eq!(failures.len(), 1);
        assert_eq!(
            (failures[0].name.as_str(), failures[0].origin, failures[0].node.as_ref().map(NodeName::as_str)),
            ("gpu", Origin::Node, Some("pve1")),
        );
        assert_eq!(by(&on(Some("pve2")), "gpu").unwrap().description.as_deref(), Some("cluster"));
    }

    /// The `all` listing: the cluster-wide set, resolved as ever -- a packaged
    /// file a cluster file shadows is not a row of its own, the cluster row says
    /// `overrides` -- and each node's own files beside it, where they came from.
    #[test]
    fn the_all_listing_is_the_cluster_wide_set_plus_every_nodes_files() {
        let (dir, reg) = three_layers();
        write(&dir.path().join("packaged"), "gpu.yaml", "selector: {all: true}\n");
        write(&dir.path().join("cluster"), "gpu.yaml", "selector: {all: true}\n");
        write(&node_dir(&dir, "pve2"), "gpu.yaml", "selector: {all: true}\n");
        write(&node_dir(&dir, "pve1"), "gpu.yaml", "selector: {all: true}\n");
        write(&node_dir(&dir, "pve1"), "gpu.devices.yaml", "selector: {all: true}\n");
        write(&node_dir(&dir, "pve1"), "broken.yaml", "selector: {nonsense: true}\n");
        std::fs::create_dir_all(dir.path().join("nodes/not_a_node/meta.d/prefixes")).unwrap();
        write(&dir.path().join("nodes/not_a_node/meta.d/prefixes"), "x.yaml", "selector: {all: true}\n");

        let names: Vec<String> = reg.nodes().unwrap().iter().map(NodeName::to_string).collect();
        assert_eq!(names, ["pve1", "pve2"], "a directory that is not a node name is not a node");
        let (loaded, failures) = reg.list_prefixes(PrefixSet::All).unwrap();
        let rows: Vec<(String, Origin, Option<&str>, bool)> = loaded
            .iter()
            .map(|p| (p.prefix.to_string(), p.origin, p.node.as_ref().map(NodeName::as_str), p.overrides))
            .collect();
        assert_eq!(
            rows,
            [
                ("gpu.devices".to_string(), Origin::Node, Some("pve1"), false),
                // Packaged and cluster `gpu`: one row, the cluster's, overriding.
                ("gpu".to_string(), Origin::Cluster, None, true),
                ("gpu".to_string(), Origin::Node, Some("pve1"), true),
                ("gpu".to_string(), Origin::Node, Some("pve2"), true),
            ],
            "most-specific first, the cluster-wide row before the nodes' of the same name",
        );
        assert_eq!(failures.len(), 1);
        assert_eq!((failures[0].origin, failures[0].node.as_ref().map(NodeName::as_str)), (Origin::Node, Some("pve1")));
    }

    #[test]
    fn a_node_name_is_pves_node_name_and_never_a_path() {
        for good in ["pve1", "PVE-node-3", "a", "1"] {
            assert!(NodeName::new(good).is_ok(), "{good} was refused");
        }
        for bad in ["", ".", "..", "../pve1", "pve1/..", "a.b", "-a", "a-", "a b", "a_b", "pve1\0"] {
            assert!(NodeName::new(bad).is_err(), "{bad:?} was accepted");
            // Deserializing is the same check: a wire value is never an unchecked name.
            assert!(serde_json::from_value::<NodeName>(json!(bad)).is_err(), "{bad:?} deserialized");
        }
        let pve1 = NodeName::new("pve1").unwrap();
        let reg = Registry::new(vec![], vec![]).with_nodes_dir("/nodes");
        assert_eq!(reg.node_prefix_dir(&pve1), Some(PathBuf::from("/nodes/pve1/meta.d/prefixes")));
        assert_eq!(Registry::new(vec![], vec![]).node_prefix_dir(&pve1), None, "no nodes directory, no node files");
    }

    #[test]
    fn a_malformed_file_is_skipped_and_costs_nobody_else_anything() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "traefik.yaml", NS);
        write(dir.path(), "broken.yaml", "selector: {nonsense: true}\n");
        write(dir.path(), "notyaml.txt", "ignored");
        let all = load_prefixes(&[dir.path().into()]).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].prefix.to_string(), "traefik");
    }

    #[test]
    fn prefixes_load_most_specific_first_and_shape_takes_the_first_match() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "homelab.yaml", "selector: {all: true}\nschema: {type: object}\n");
        write(dir.path(), "homelab.docker.yaml", "selector: {all: true}\nschema: {type: object, properties: {compose: {type: string}}}\n");
        let all = load_prefixes(&[dir.path().into()]).unwrap();
        assert_eq!(
            all.iter().map(|n| n.prefix.to_string()).collect::<Vec<_>>(),
            vec!["homelab.docker", "homelab"],
            "longest prefix first",
        );

        // Through real files, because the file name *is* the prefix; the
        // rule itself is `shape::Shape`'s and tested there.
        let p = |s: &str| Path::parse(s).unwrap();
        let shape = crate::shape::Shape::of_guest(&all, &[]);
        assert_eq!(shape.governing(&p("homelab.docker.compose")).unwrap().prefix.to_string(), "homelab.docker");
        assert_eq!(shape.governing(&p("homelab.notes")).unwrap().prefix.to_string(), "homelab");
        assert!(shape.governing(&p("unrelated")).is_none());
    }

    #[test]
    fn permissions_accumulate_by_containment_which_is_the_opposite_of_prefixes() {
        let g = parse_permission("traefik", PERMISSION_FILE).unwrap();
        let scopes = scopes_for(std::slice::from_ref(&g), "svc@pve!traefik", &["traefik".to_string()]);
        assert_eq!(scopes.len(), 2, "both entries, the tag one having matched");

        // A rule on `traefik` covers everything under it -- containment, not
        // most-specific-wins (`docs/DESIGN.md` §4).
        let access = crate::scopes::Effective { full_read: false, full_write: false, scopes };
        assert!(access.can_write(&Path::parse("traefik.spec.host").unwrap()));
        assert!(access.can_read(&Path::parse("netbird.groups").unwrap()));
        assert!(!access.can_write(&Path::parse("netbird.groups").unwrap()), "ro stays ro");

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
        assert!(load_prefixes(&[PathBuf::from("/nonexistent/pve-meta")]).unwrap().is_empty());
        assert!(load_permissions(&[PathBuf::from("/nonexistent/pve-meta")]).unwrap().is_empty());
    }

    #[test]
    fn a_malformed_prefix_file_is_listed_named_but_never_loaded() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "traefik.yaml", NS);
        write(dir.path(), "broken.yaml", "selector: {nonsense: true}\n");
        let reg = Registry::new(vec![dir.path().to_path_buf()], vec![]);

        let (parsed, failures) = reg.list_prefixes(PrefixSet::Cluster).unwrap();
        assert_eq!(
            parsed.iter().map(|p| p.prefix.to_string()).collect::<Vec<_>>(),
            vec!["traefik"],
            "only the good file becomes a prefix",
        );
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].name, "broken", "the file name, not anything inside it");
        assert!(!failures[0].error.is_empty());
        // Same rule as `Registry::write_dir` -- one directory, nothing packaged.
        assert_eq!(failures[0].origin, Origin::Cluster);

        // What `governing`/the write path actually consult must never see it.
        assert_eq!(reg.load_prefixes(None).unwrap(), parsed);
    }

    #[test]
    fn a_malformed_permission_file_is_listed_named_and_grants_nothing() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "good.yaml", PERMISSION_FILE);
        // Well-formed enough to name a real authid -- the point is that a
        // parse failure refuses it regardless of who it claims to be for.
        write(
            dir.path(),
            "broken.yaml",
            "authid: svc@pve!traefik\nrules:\n  - prefix: x\n    mode: sideways\n    selector: {all: true}\n",
        );
        let reg = Registry::new(vec![], vec![dir.path().to_path_buf()]);

        let (parsed, failures) = reg.list_permissions().unwrap();
        assert_eq!(parsed.len(), 1, "only the good file becomes a permission");
        assert_eq!(parsed[0].name, "good");
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].name, "broken");
        assert!(!failures[0].error.is_empty());
        assert_eq!(failures[0].origin, Origin::Cluster);

        // What `access`/`effective` actually consult must never see it.
        assert_eq!(reg.load_permissions().unwrap(), parsed);

        // The important check: even though the malformed file's own `authid`
        // matches exactly, a file that did not parse grants nothing.
        let scopes = scopes_for(&parsed, "svc@pve!traefik", &["traefik".to_string()]);
        assert_eq!(scopes.len(), 2, "only the good file's two rules -- nothing from `broken`");
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
