//! The two drop directories: **prefixes** (what a prefix is) and **grants**
//! (who may touch one) — `docs/DESIGN.md` §3.
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
//! * `/etc/pve/meta.d/prefixes/<prefix>.yaml` — cluster overrides, by file name.
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
//! two rules cannot live on one object, which is the concrete reason revision 6
//! made this two concepts and not one (`docs/DESIGN.md` §12).

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path as FsPath, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::format::{self, Format};
use crate::model::Value;
use crate::path::Path;
use crate::scopes::{Mode, Scope};

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
    /// Which directory this one was read from. Not part of the file.
    pub origin: Origin,
    /// `true` when a lower-precedence directory holds a file of the same name that
    /// this one displaced. With the two configured directories that is exactly "a
    /// cluster file written over a packaged one"; with three or more (only reachable
    /// through `PVE_META_PREFIX_DIRS`) a packaged file can displace another packaged
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
/// `yaml_files` has already filtered to names [`is_valid_file_name`] accepts
/// before `load_dirs` calls the parser at all, so `name` here is always
/// something `GET /meta/prefixes/{name}` or `GET /meta/permissions/{name}`
/// can open -- never a name a hand-edit made unaddressable in the first place.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RegistryFailure {
    /// The file name without `.yaml`, which for a prefix IS the prefix.
    pub name: String,
    pub origin: Origin,
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
    #[serde(default)]
    enforce: Option<bool>,
    #[serde(default)]
    schema: Option<Value>,
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
    Ok(PrefixDef {
        // A parse knows the text, not the directory: `load_dirs` stamps the real
        // origin over these. The defaults are what a hand-parsed file is -- the
        // cluster's, displacing nothing.
        origin: Origin::Cluster,
        overrides: false,
        prefix,
        description: raw.description,
        selector,
        enforce: raw.enforce.unwrap_or(false),
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

/// Owns the two drop-directory lists. Everything that needs to know where a
/// prefix or permission file lives -- `MetaStore`, the perlmod bindings, a
/// test -- is handed one of these rather than reading
/// `PVE_META_PREFIX_DIRS`/`PVE_META_PERMISSION_DIRS` (or the environment at
/// all) on its own: two callers reading the same variable independently agree
/// only because nothing changes it mid-process, and that was never a
/// guarantee, just an accident of everything running in one process.
#[derive(Debug, Clone)]
pub struct Registry {
    prefix_dirs: Vec<PathBuf>,
    permission_dirs: Vec<PathBuf>,
}

impl Registry {
    /// Reads `PVE_META_PREFIX_DIRS`/`PVE_META_PERMISSION_DIRS` (or the
    /// compiled-in defaults) once. See [`crate::store::MetaStore::new`]'s doc
    /// comment for why that has to happen exactly once per store rather than
    /// wherever a directory list happens to be needed next.
    pub fn from_env() -> Self {
        Registry { prefix_dirs: prefix_dirs(), permission_dirs: permission_dirs() }
    }

    /// Explicit directories, lowest precedence first -- for tests and
    /// sandboxes that must not consult the environment.
    pub fn new(prefix_dirs: Vec<PathBuf>, permission_dirs: Vec<PathBuf>) -> Self {
        Registry { prefix_dirs, permission_dirs }
    }

    /// The directories of `kind`, lowest precedence first.
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

    /// [`load_prefixes`] over this registry's own prefix directories.
    pub fn load_prefixes(&self) -> Vec<PrefixDef> {
        self.list_prefixes().0
    }

    /// [`load_permissions`] over this registry's own permission directories.
    pub fn load_permissions(&self) -> Vec<Permission> {
        self.list_permissions().0
    }

    /// Every prefix, plus every file that did not load, from one walk of this
    /// registry's own prefix directories. `GET /meta/prefixes`'s source: the
    /// one listing where a failure has to survive (see [`RegistryFailure`]).
    pub fn list_prefixes(&self) -> (Vec<PrefixDef>, Vec<RegistryFailure>) {
        prefixes_with_failures(self.dirs(RegistryKind::PrefixDef))
    }

    /// Every permission, plus every file that did not load, likewise --
    /// `GET /meta/permissions`'s source.
    pub fn list_permissions(&self) -> (Vec<Permission>, Vec<RegistryFailure>) {
        permissions_with_failures(self.dirs(RegistryKind::Permission))
    }
}

/// Loads one drop-directory list, later directories overriding earlier by file
/// name, and stamps each survivor with where it came from.
///
/// Which directory is writable -- and so, here, which is `Origin::Cluster`
/// rather than `Origin::Packaged` -- is decided in exactly one place,
/// [`Registry::write_dir`]; this just derives the same last-directory-wins
/// fact for every entry as it folds them in.
///
/// Returns the failures alongside the parsed items -- an unreadable file and
/// an unparseable one both count -- so a caller that needs to show them (`GET
/// /meta/prefixes`, `GET /meta/permissions`) can have both from one walk of
/// the directories, instead of two.
fn load_dirs<T>(
    dirs: &[PathBuf],
    kind: &str,
    parse: impl Fn(&str, &str) -> Result<T>,
    stamp: impl Fn(&mut T, Origin, bool),
) -> (Vec<(String, T)>, Vec<RegistryFailure>) {
    let mut by_name: BTreeMap<String, T> = BTreeMap::new();
    let mut failures = Vec::new();
    let last = dirs.len().saturating_sub(1);
    for (index, dir) in dirs.iter().enumerate() {
        let origin = if index == last { Origin::Cluster } else { Origin::Packaged };
        for (name, path) in yaml_files(dir) {
            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) => {
                    crate::warn_line!(
                        "skipping unreadable {kind} file {}: {e}",
                        path.display()
                    );
                    failures.push(RegistryFailure { name, origin, error: e.to_string() });
                    continue;
                }
            };
            match parse(&name, &text) {
                Ok(mut parsed) => {
                    // File names are unique within a directory, so anything this
                    // displaces necessarily came from a lower-precedence one.
                    let displaced = by_name.contains_key(&name);
                    stamp(&mut parsed, origin, displaced);
                    by_name.insert(name, parsed);
                }
                Err(e) => {
                    crate::warn_line!(
                        "skipping malformed {kind} file {}: {e}",
                        path.display()
                    );
                    failures.push(RegistryFailure { name, origin, error: e.to_string() });
                }
            }
        }
    }
    (by_name.into_iter().collect(), failures)
}

/// [`load_dirs`] for prefixes, plus the most-specific-first sort the listing
/// promises (`docs/DESIGN.md` §5) -- shared by [`load_prefixes`] (which drops
/// the failures) and [`Registry::list_prefixes`] (which keeps them), so the
/// two can never compute the sort differently.
fn prefixes_with_failures(dirs: &[PathBuf]) -> (Vec<PrefixDef>, Vec<RegistryFailure>) {
    let (parsed, failures) = load_dirs(dirs, "prefix", parse_prefix, |p, origin, over| {
        p.origin = origin;
        p.overrides = over;
    });
    let mut out: Vec<PrefixDef> = parsed.into_iter().map(|(_, ns)| ns).collect();
    out.sort_by(|a, b| by_specificity(&a.prefix, &b.prefix));
    (out, failures)
}

/// Every prefix in `dirs` (lowest precedence first, later directories
/// overriding earlier **by file name**), **sorted most-specific first** — the
/// order `GET /meta/prefixes` lists them in.
pub fn load_prefixes(dirs: &[PathBuf]) -> Vec<PrefixDef> {
    // Drops the failures, and must keep dropping them: a file that did not
    // parse is not a prefix, and it must never reach a `Shape`, which decides
    // what a document's shape *is*. `Registry::list_prefixes` is the one
    // place the same failure survives, for the listing that has to show it.
    prefixes_with_failures(dirs).0
}

/// [`load_dirs`] for permissions, split out for the same reason as
/// [`prefixes_with_failures`].
fn permissions_with_failures(dirs: &[PathBuf]) -> (Vec<Permission>, Vec<RegistryFailure>) {
    let (parsed, failures) = load_dirs(dirs, "permission", parse_permission, |g, origin, over| {
        g.origin = origin;
        g.overrides = over;
    });
    (parsed.into_iter().map(|(_, g)| g).collect(), failures)
}

/// Every grant in `dirs`, sorted by file name.
pub fn load_permissions(dirs: &[PathBuf]) -> Vec<Permission> {
    // Same drop, the other direction, and just as deliberate: a file that did
    // not parse is not a permission, and it must never reach `scopes_for` --
    // a malformed permission file must grant nothing, not "grant nothing
    // until someone reads the log". `Registry::list_permissions` is where the
    // failure survives instead.
    permissions_with_failures(dirs).0
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
        // The same rule `api::parse_id` and `store::registry_document_id` apply. It
        // was missing here, which meant a hand-created `my file.yaml` *loaded* -- it
        // granted scopes and appeared in the listing -- while `GET
        // /meta/permissions/my file` was a 400, so nothing could open or repair it.
        // `is_valid_file_name`'s own doc says three places need it and must agree;
        // this was the third.
        if !is_valid_file_name(stem) || !entry.path().is_file() {
            continue;
        }
        out.push((stem.to_string(), entry.path()));
    }
    out.sort();
    out
}

/// The scopes `authid` holds on a guest carrying `tags`: the union of every
/// rule for that authid whose selector matches (`docs/DESIGN.md` §3.4).
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
        let loaded: Vec<String> = load_prefixes(&[dir.path().to_path_buf()])
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

        let loaded = load_prefixes(&[packaged, cluster]);
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
        let only = load_prefixes(&[dir.path().join("cluster")]);
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

    #[test]
    fn enforce_is_off_unless_the_prefix_says_so() {
        assert!(!parse_prefix("x", "selector: {all: true}\n").unwrap().enforce);
        assert!(parse_prefix("x", "selector: {all: true}\nenforce: true\n").unwrap().enforce);
        assert!(!parse_prefix("x", "selector: {all: true}\nenforce: false\n").unwrap().enforce);
        assert!(parse_prefix("x", "selector: {all: true}\nenforce: nope\n").is_err());
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

        let all = load_prefixes(&[packaged.path().into(), cluster.path().into()]);
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
        let all = load_prefixes(&[dir.path().into()]);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].prefix.to_string(), "traefik");
    }

    #[test]
    fn prefixes_load_most_specific_first_and_shape_takes_the_first_match() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "homelab.yaml", "selector: {all: true}\nschema: {type: object}\n");
        write(dir.path(), "homelab.docker.yaml", "selector: {all: true}\nschema: {type: object, properties: {compose: {type: string}}}\n");
        let all = load_prefixes(&[dir.path().into()]);
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
        // most-specific-wins (`docs/DESIGN.md` §3.2).
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
        assert!(load_prefixes(&[PathBuf::from("/nonexistent/pve-meta")]).is_empty());
        assert!(load_permissions(&[PathBuf::from("/nonexistent/pve-meta")]).is_empty());
    }

    #[test]
    fn a_malformed_prefix_file_is_listed_named_but_never_loaded() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "traefik.yaml", NS);
        write(dir.path(), "broken.yaml", "selector: {nonsense: true}\n");
        let reg = Registry::new(vec![dir.path().to_path_buf()], vec![]);

        let (parsed, failures) = reg.list_prefixes();
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
        assert_eq!(reg.load_prefixes(), parsed);
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

        let (parsed, failures) = reg.list_permissions();
        assert_eq!(parsed.len(), 1, "only the good file becomes a permission");
        assert_eq!(parsed[0].name, "good");
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].name, "broken");
        assert!(!failures[0].error.is_empty());
        assert_eq!(failures[0].origin, Origin::Cluster);

        // What `access`/`effective` actually consult must never see it.
        assert_eq!(reg.load_permissions(), parsed);

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
