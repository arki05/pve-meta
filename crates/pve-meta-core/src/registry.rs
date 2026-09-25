//! The prefix drop directory: loads `/usr/share/pve-meta/prefixes/*.yaml`
//! (packaged) and `/etc/pve/meta.d/prefixes/*.yaml` (cluster), and resolves
//! which ones reach a guest (`docs/DESIGN.md` §3). The file name is the
//! prefix; precedence between the two directories is by presence
//! ([`effective_file`]), not content.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path as FsPath, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::format::{self, Format};
use crate::model::Value;
use crate::path::Path;
use crate::store::gone_is_none;

/// The packaged prefix directory.
pub const PREFIX_PACKAGED_DIR: &str = "/usr/share/pve-meta/prefixes";
/// The cluster-wide prefix directory (pmxcfs); overrides
/// [`PREFIX_PACKAGED_DIR`] by file name.
pub const PREFIX_CLUSTER_DIR: &str = "/etc/pve/meta.d/prefixes";

/// Environment variable overriding the prefix directories with a
/// colon-separated list, lowest precedence first. Tests and `test/basic.pl`.
pub const PREFIX_DIRS_ENV: &str = "PVE_META_PREFIX_DIRS";

/// Which drop directory a [`crate::store::DocId::Registry`] document lives in.
/// One kind today; the type stays so a document id keeps its `<kind>/<name>`
/// shape if a second registry kind is ever added.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RegistryKind {
    /// A prefix: what a prefix is ([`PrefixDef`]).
    PrefixDef,
}

impl RegistryKind {
    /// The kind's wire name, and the first segment of a registry document's
    /// API id: `prefixes`.
    pub fn as_str(&self) -> &'static str {
        match self {
            RegistryKind::PrefixDef => "prefixes",
        }
    }

    /// The singular noun for one file of this kind, as an error message about
    /// that file spells it: `prefix`.
    pub fn noun(&self) -> &'static str {
        match self {
            RegistryKind::PrefixDef => "prefix",
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

    /// Reads a selector as the API *lists* it (`GET /meta/prefixes`): the
    /// same shape [`Selector`] serializes, tolerating Perl's `true` → `1`.
    /// The rule itself is [`parse_selector`]'s, applied unchanged.
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

/// Most-specific first (longer prefix, then by name): the order
/// [`crate::shape::Shape`] resolves in and `GET /meta/prefixes` lists in --
/// one comparator for both.
pub fn by_specificity(a: &Path, b: &Path) -> std::cmp::Ordering {
    b.segments()
        .len()
        .cmp(&a.segments().len())
        .then_with(|| a.to_string().cmp(&b.to_string()))
}

/// A node's override of a prefix's `schema`/`enforce`/`hidden`
/// (`docs/DESIGN.md` §3): any subset, replacing -- never merging -- the
/// top-level field for a guest on that node.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NodeOverride {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enforce: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hidden: Option<bool>,
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
    /// Refuses a write that leaves this subtree not matching `schema`
    /// (`docs/DESIGN.md` §5). Off by default.
    pub enforce: bool,
    /// The root default for the schema's per-node `hidden`: with it set, this
    /// prefix offers no declared-but-unset rows unless a node asks to be shown.
    #[serde(default)]
    pub hidden: bool,
    /// Per-node overrides, as parsed, so the editor can show them. Empty on a
    /// row [`Registry::prefixes_for_guest`] has already resolved, since the
    /// resolved row's own fields are the effective ones.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub nodes: BTreeMap<NodeName, NodeOverride>,
    /// Which directory this one was read from. Not part of the file.
    pub origin: Origin,
    /// `true` when a lower-precedence directory holds a file of this name
    /// that this one displaced.
    pub overrides: bool,
}

/// Where a loaded file came from, and what it displaced -- a packaged prefix
/// and a cluster override of the same name are otherwise the same row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    /// From a package's directory (`/usr/share/pve-meta/prefixes`): read-only,
    /// and replaced rather than edited -- a write creates the cluster file.
    Packaged,
    /// From the cluster directory (`/etc/pve/meta.d/...`).
    Cluster,
}

/// A file in a registry directory that did not load: unreadable, or not
/// valid as the kind it lives in. Reported rather than dropped, so a name
/// missing everywhere a caller looks for it is never explained by nothing
/// but a log line. A failed cluster override also shadows the packaged file
/// of its name (precedence is by presence), so this row is then the only one
/// for that name.
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
    #[serde(default, deserialize_with = "de_flag")]
    enforce: Option<bool>,
    #[serde(default, deserialize_with = "de_flag")]
    hidden: Option<bool>,
    #[serde(default)]
    schema: Option<Value>,
    #[serde(default)]
    nodes: BTreeMap<String, RawNodeOverride>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawNodeOverride {
    #[serde(default)]
    schema: Option<Value>,
    #[serde(default, deserialize_with = "de_flag")]
    enforce: Option<bool>,
    #[serde(default, deserialize_with = "de_flag")]
    hidden: Option<bool>,
}

/// A flag value as a `bool`: `true`/`false`, or `1`/`0` as this dialect
/// already spells `optional` and `multiline` and the wire spells a boolean
/// (`docs/decisions/017-data-is-a-native-structure.md`). `None` if `v` is
/// neither. The one reading of it, for the loader and [`crate::shape`] alike.
pub(crate) fn as_flag(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::Number(n) if n.as_i64() == Some(1) => Some(true),
        Value::Number(n) if n.as_i64() == Some(0) => Some(false),
        _ => None,
    }
}

/// A prefix-level flag field, refused (a file is parsed strictly) unless it
/// is [`as_flag`].
fn de_flag<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Option<bool>, D::Error> {
    match Option::<Value>::deserialize(d)? {
        None | Some(Value::Null) => Ok(None),
        Some(other) => as_flag(&other).map(Some).ok_or_else(|| {
            serde::de::Error::custom(format!("expected true/false or 1/0, got {other}"))
        }),
    }
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

/// Refuses a `schema:` whose known keywords would not do what they say (a
/// typo'd `type`, an `enum` member of the wrong type, a range with no
/// meaning) -- since `enforce: true` makes the schema a write gate, and one
/// that silently enforced nothing would defeat the point. An unknown keyword
/// still passes: the dialect is open-ended.
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
            if as_flag(v).is_none() {
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

    if let Some(items) = map.get("items") {
        if let Some(t) = declared.filter(|t| *t != "array") {
            return Err(fail(format!("'items' has no meaning for type {t}")));
        }
        check_schema_dialect(name, &format!("{at}.items"), items)?;
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

/// Whether `name` is a usable registry **file** name (the part before
/// `.yaml`): one or more [`crate::path::is_valid_segment`] segments joined by
/// dots -- exactly a prefix, since the file name IS the prefix -- and at
/// most [`MAX_FILE_NAME_LEN`] long. `api::parse_id`, the loader and the
/// editor (through the wasm build) all need this same rule and must agree on it.
pub fn is_valid_file_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_FILE_NAME_LEN
        && name.split('.').all(crate::path::is_valid_segment)
}

/// The longest registry file name (without `.yaml`) that is one. Bytes
/// (the segment charset is ASCII); Linux caps a file name at 255 bytes, and
/// 128 leaves room for the suffix and anything pmxcfs adds.
pub const MAX_FILE_NAME_LEN: usize = 128;

/// A PVE node name: `PVE::JSONSchema::pve_verify_node_name`'s
/// `[a-zA-Z0-9]([a-zA-Z0-9-]*[a-zA-Z0-9])?`, transliterated. Making or
/// deserializing one is the only check, so any `NodeName` in hand was one --
/// whether it is in the cluster is a question for the caller that holds the
/// nodelist.
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
    let mut nodes = BTreeMap::new();
    for (key, over) in raw.nodes {
        let node = NodeName::new(&key)
            .map_err(|_| bad(format!("{name}: nodes.{key}: not a valid node name")))?;
        if let Some(schema) = &over.schema {
            check_schema_dialect(name, &format!("nodes.{key}.schema"), schema)?;
        }
        nodes.insert(
            node,
            NodeOverride { schema: over.schema, enforce: over.enforce, hidden: over.hidden },
        );
    }
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
        hidden: raw.hidden.unwrap_or(false),
        schema: raw.schema,
        nodes,
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

/// Owns the drop-directory lists. Everything that needs to know where a
/// prefix file lives -- `MetaStore`, the perlmod bindings, a test -- is
/// handed one of these rather than reading `PVE_META_PREFIX_DIRS` on its own.
#[derive(Debug, Clone)]
pub struct Registry {
    prefix_dirs: Vec<PathBuf>,
}

impl Registry {
    /// Reads `PVE_META_PREFIX_DIRS` (or the compiled-in defaults) once; see
    /// [`crate::store::MetaStore::new`] for why that must happen exactly once
    /// per store. Checks nothing about whether the directories are there to
    /// be read -- [`crate::store::MetaStore::registry`] hands one out behind
    /// the store's availability check.
    pub fn from_env() -> Self {
        Registry { prefix_dirs: prefix_dirs() }
    }

    /// Explicit directories, lowest precedence first -- for tests and
    /// sandboxes that must not consult the environment.
    pub fn new(prefix_dirs: Vec<PathBuf>) -> Self {
        Registry { prefix_dirs }
    }

    /// The directories of `kind`, lowest precedence first.
    pub fn dirs(&self, kind: RegistryKind) -> &[PathBuf] {
        match kind {
            RegistryKind::PrefixDef => &self.prefix_dirs,
        }
    }

    /// The one directory of `kind` a write may land in, if any is configured:
    /// the last one. `None` when `dirs(kind)` is empty, so a caller with none
    /// configured decides its own fallback instead of silently writing to the
    /// compiled-in default.
    pub fn write_dir(&self, kind: RegistryKind) -> Option<PathBuf> {
        self.dirs(kind).last().cloned()
    }

    /// The file for `name` of `kind`, if any directory has one: the
    /// highest-precedence one that does ([`effective_file`]). What the store
    /// reads, and what the loader parses -- the same file, by construction.
    pub fn locate(&self, kind: RegistryKind, name: &str) -> Option<PathBuf> {
        effective_file(self.dirs(kind), name).map(|(_, path)| path)
    }

    /// Every prefix file as it is, packaged and cluster resolved by name --
    /// `GET /meta/prefixes` without an `id` -- plus every file that did not
    /// load. A directory that cannot be listed, for any reason but not being
    /// there, is an error for the whole answer.
    pub fn list_prefixes(&self) -> Result<(Vec<PrefixDef>, Vec<RegistryFailure>)> {
        prefixes_with_failures(&cluster_layers(self.dirs(RegistryKind::PrefixDef)))
    }

    /// The prefixes reaching a guest on `node` carrying `tags`, resolved:
    /// selector-matched, node override applied whole, most-specific first
    /// (`docs/DESIGN.md` §3, §6). What both `GET /meta/prefixes?id=` and
    /// `api::put_document`'s enforcement gate use, plus every file that did
    /// not load, same as [`Registry::list_prefixes`].
    pub fn prefixes_for_guest(
        &self,
        node: Option<&NodeName>,
        tags: &[String],
    ) -> Result<(Vec<PrefixDef>, Vec<RegistryFailure>)> {
        let (loaded, failures) = self.list_prefixes()?;
        let resolved = loaded
            .into_iter()
            .filter(|p| p.selector.matches(tags))
            .map(|p| resolve_for_node(p, node))
            .collect();
        Ok((resolved, failures))
    }
}

/// `def` with `node`'s override applied: `nodes.<node>`'s `schema`/`enforce`/
/// `hidden` replace the top-level ones, whole, wherever it sets them
/// (`docs/DESIGN.md` §3); the map itself is spent, since a resolved row
/// carries none of its own.
fn resolve_for_node(mut def: PrefixDef, node: Option<&NodeName>) -> PrefixDef {
    if let Some(over) = node.and_then(|n| def.nodes.get(n)) {
        if let Some(schema) = &over.schema {
            def.schema = Some(schema.clone());
        }
        if let Some(enforce) = over.enforce {
            def.enforce = enforce;
        }
        if let Some(hidden) = over.hidden {
            def.hidden = hidden;
        }
    }
    def.nodes = BTreeMap::new();
    def
}

/// `dirs` (lowest precedence first) as layers, each with the [`Origin`]
/// `load_dirs` stamps on its files: the last one is the cluster's --
/// [`Registry::write_dir`]'s rule -- and every one below it packaged.
fn cluster_layers(dirs: &[PathBuf]) -> Vec<(PathBuf, Origin)> {
    dirs.iter()
        .enumerate()
        .map(|(i, dir)| {
            let origin = if i + 1 == dirs.len() { Origin::Cluster } else { Origin::Packaged };
            (dir.clone(), origin)
        })
        .collect()
}

/// The file for `name` among `dirs` (lowest precedence first): the one in
/// the highest-precedence directory that has it, with that directory's
/// index. Presence decides, not content. A file that cannot even be looked
/// at, for any reason but not being there, counts as present: reading it is
/// what fails, and that failure is reported for it.
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
/// name, and stamps each survivor with the [`Origin`] of its layer. One row
/// per name, loaded or failed: only the effective file ([`effective_file`])
/// is read and parsed, so a file it shadows is neither loaded nor reported.
fn load_dirs<T>(
    layers: &[(PathBuf, Origin)],
    parse: impl Fn(&str, &str) -> Result<T>,
    stamp: impl Fn(&mut T, Origin, bool),
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
        let origin = layers[index].1;
        let fail = |name: String, error: String| RegistryFailure { name, origin, error };
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                failures.push(fail(name, e.to_string()));
                continue;
            }
        };
        match parse(&name, &text) {
            Ok(mut parsed) => {
                stamp(&mut parsed, origin, count > 1);
                parsed_out.push((name, parsed));
            }
            Err(e) => {
                failures.push(fail(name, e.to_string()));
            }
        }
    }
    Ok((parsed_out, failures))
}

/// [`load_dirs`] for prefixes, plus the most-specific-first sort the listing
/// promises (`docs/DESIGN.md` §6) -- shared by [`load_prefixes`] (which drops
/// the failures) and the [`Registry`] listings (which keep them), so the
/// two can never compute the sort differently.
fn prefixes_with_failures(layers: &[(PathBuf, Origin)]) -> Result<(Vec<PrefixDef>, Vec<RegistryFailure>)> {
    let (parsed, failures) = load_dirs(layers, parse_prefix, |p, origin, over| {
        p.origin = origin;
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
        // Same rule as `api::parse_id`: a file nothing could address must
        // not load either. An entry that cannot be
        // looked at is still listed -- reading it fails, and that is a
        // failure row for its name, not a silently shorter set.
        if !is_valid_file_name(stem) || !is_file_or_unreadable(&entry.path()) {
            continue;
        }
        out.push((stem.to_string(), entry.path()));
    }
    out.sort();
    Ok(out)
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
    fn parsing_is_strict() {
        // Unknown fields, a missing selector, both selector alternatives at once.
        assert!(parse_prefix("x", "selector: {all: true}\nnope: 1\n").is_err());
        assert!(parse_prefix("x", "description: no selector\n").is_err());
        assert!(parse_prefix("x", "selector: {all: true, tag: t}\n").is_err());
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
            ("schema: {type: string, items: {}}\n", "schema: 'items' has no meaning for type string", "a string has no members"),
            ("schema: {type: array, items: {type: nope}}\n", "schema.items: 'type' must be one of", "a member's schema is checked like any node"),
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
            "schema: {type: array, items: {type: integer, maximum: 5}}\n",
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

    /// "Precedence is by presence" (module docs), one row per shape the two
    /// directories can give a name: valid, wholesale-overridden, shadowed by
    /// a malformed override rather than falling back, or simply broken --
    /// all through the walk [`Registry::list_prefixes`] itself calls.
    #[test]
    fn precedence_is_by_presence_valid_or_not() {
        struct Case {
            why: &'static str,
            packaged: Option<&'static str>,
            cluster: Option<&'static str>,
            loaded: Option<(Origin, bool)>,
            failure: Option<Origin>,
        }
        let bad = "selector: {nonsense: true}\n";
        let cases = [
            Case {
                why: "a valid cluster file replaces the packaged one wholesale, not merged",
                packaged: Some(NS),
                cluster: Some("selector: {all: true}\n"),
                loaded: Some((Origin::Cluster, true)),
                failure: None,
            },
            Case {
                why: "a malformed override shadows the packaged file rather than falling back to it",
                packaged: Some(NS),
                cluster: Some(bad),
                loaded: None,
                failure: Some(Origin::Cluster),
            },
            Case {
                why: "a malformed packaged file with nothing to override it is just broken",
                packaged: Some(bad),
                cluster: None,
                loaded: None,
                failure: Some(Origin::Packaged),
            },
            Case {
                why: "a malformed file is skipped and costs nobody else anything",
                packaged: None,
                cluster: Some(bad),
                loaded: None,
                failure: Some(Origin::Cluster),
            },
        ];
        for Case { why, packaged, cluster, loaded, failure } in cases {
            let pkg_dir = tempfile::tempdir().unwrap();
            let cluster_dir = tempfile::tempdir().unwrap();
            if let Some(t) = packaged {
                write(pkg_dir.path(), "x.yaml", t);
            }
            if let Some(t) = cluster {
                write(cluster_dir.path(), "x.yaml", t);
            }
            let dirs = [pkg_dir.path().to_path_buf(), cluster_dir.path().to_path_buf()];
            let (found, failures) = prefixes_with_failures(&cluster_layers(&dirs)).unwrap();
            match loaded {
                Some((origin, overrides)) => {
                    assert_eq!(found.len(), 1, "{why}");
                    assert_eq!(found[0].origin, origin, "{why}");
                    assert_eq!(found[0].overrides, overrides, "{why}");
                    assert!(found[0].schema.is_none(), "{why}: wholesale, not merged");
                }
                None => assert!(found.is_empty(), "{why}"),
            }
            match failure {
                Some(origin) => {
                    assert_eq!(failures.len(), 1, "{why}");
                    assert_eq!(failures[0].name, "x", "{why}");
                    assert_eq!(failures[0].origin, origin, "{why}");
                    assert!(!failures[0].error.is_empty(), "{why}");
                }
                None => assert!(failures.is_empty(), "{why}"),
            }
        }

        // `Registry::list_prefixes` and `prefixes_for_guest` walk the same
        // directories through the same function, so a broken file is
        // reported by both and never reaches the resolved set.
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "traefik.yaml", NS);
        write(dir.path(), "broken.yaml", bad);
        let reg = Registry::new(vec![dir.path().to_path_buf()]);
        let (parsed, failures) = reg.list_prefixes().unwrap();
        assert_eq!(parsed.iter().map(|p| p.prefix.to_string()).collect::<Vec<_>>(), vec!["traefik"]);
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].name, "broken");
        let (resolved, _) = reg.prefixes_for_guest(None, &["traefik".to_string()]).unwrap();
        assert_eq!(resolved, parsed);
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
        let shape = crate::shape::Shape::of_guest(&all);
        assert_eq!(shape.governing(&p("homelab.docker.compose")).unwrap().prefix.to_string(), "homelab.docker");
        assert_eq!(shape.governing(&p("homelab.notes")).unwrap().prefix.to_string(), "homelab");
        assert!(shape.governing(&p("unrelated")).is_none());
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
    }

    // -- node overrides (docs/DESIGN.md §3, §6) ------------------------------

    /// One line per case: the override rule is "for a guest on node `n`, the
    /// file's `nodes.n` fields replace the top-level ones, whole -- any
    /// subset of schema/enforce/hidden, and never merged".
    #[test]
    fn a_nodes_override_replaces_whole_never_merges() {
        let text = "\
selector: {all: true}
enforce: false
hidden: false
schema: {type: object, properties: {a: {type: string}}}
nodes:
  pve1: {enforce: true}
  pve2: {schema: {type: object, properties: {b: {type: integer}}}}
  pve3: {schema: {type: object}, enforce: true, hidden: true}
";
        let def = parse_prefix("t", text).unwrap();
        assert_eq!(def.nodes.len(), 3);

        // (node, expect enforce, expect hidden, expect schema has property)
        let cases: Vec<(Option<&str>, bool, bool, &str)> = vec![
            (None, false, false, "a"),
            (Some("pve1"), true, false, "a"),
            (Some("pve2"), false, false, "b"),
            (Some("pve3"), true, true, "?"),
            (Some("pve4"), false, false, "a"),
        ];
        for (node, enforce, hidden, prop) in cases {
            let n = node.map(|s| NodeName::new(s).unwrap());
            let resolved = resolve_for_node(def.clone(), n.as_ref());
            assert_eq!(resolved.enforce, enforce, "{node:?}: enforce");
            assert_eq!(resolved.hidden, hidden, "{node:?}: hidden");
            assert!(resolved.nodes.is_empty(), "{node:?}: a resolved row carries no nodes map");
            if prop != "?" {
                assert!(
                    resolved.schema.as_ref().unwrap()["properties"].get(prop).is_some(),
                    "{node:?}: schema should have '{prop}'"
                );
            }
        }
        // pve3's override replaces the schema whole: the parent's `a` is gone.
        let pve3 = resolve_for_node(def.clone(), Some(&NodeName::new("pve3").unwrap()));
        assert!(pve3.schema.as_ref().unwrap().get("properties").is_none());
    }

    #[test]
    fn an_invalid_node_key_fails_the_whole_file() {
        for bad in ["../pve1", "a b", "", "pve.1"] {
            let text = format!("selector: {{all: true}}\nnodes:\n  \"{bad}\": {{enforce: true}}\n");
            assert!(parse_prefix("t", &text).is_err(), "{bad:?} should not be a node name");
        }
        // A per-node schema is checked the same way the top-level one is.
        let text = "selector: {all: true}\nnodes:\n  pve1: {schema: {type: nope}}\n";
        let err = parse_prefix("t", text).unwrap_err().to_string();
        assert!(err.contains("nodes.pve1.schema"), "{err}");
    }

    // -- the resolved-for-guest listing --------------------------------------

    /// `Registry::prefixes_for_guest`: selector-matched and node-overridden,
    /// most-specific first, with the failures carried alongside.
    #[test]
    fn prefixes_for_guest_resolves_selector_and_node_together() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "homelab.yaml", "selector: {all: true}\nschema: {type: object}\n");
        write(
            dir.path(),
            "traefik.yaml",
            "selector: {tag: web}\nschema: {type: object}\nnodes:\n  pve1: {enforce: true}\n",
        );
        write(dir.path(), "broken.yaml", "selector: {nonsense: true}\n");
        let reg = Registry::new(vec![dir.path().to_path_buf()]);
        let pve1 = NodeName::new("pve1").unwrap();

        let (resolved, failures) = reg.prefixes_for_guest(Some(&pve1), &["web".to_string()]).unwrap();
        assert_eq!(
            resolved.iter().map(|p| p.prefix.to_string()).collect::<Vec<_>>(),
            vec!["homelab", "traefik"],
            "most-specific first among those the selector reaches",
        );
        let traefik = resolved.iter().find(|p| p.prefix.to_string() == "traefik").unwrap();
        assert!(traefik.enforce, "pve1's override applied");
        assert!(traefik.nodes.is_empty(), "no nodes map on a resolved row");
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].name, "broken");

        // Without the tag, traefik does not reach this guest at all.
        let (resolved, _) = reg.prefixes_for_guest(Some(&pve1), &[]).unwrap();
        assert_eq!(resolved.iter().map(|p| p.prefix.to_string()).collect::<Vec<_>>(), vec!["homelab"]);

        // On another node, traefik reaches the guest but without the override.
        let pve2 = NodeName::new("pve2").unwrap();
        let (resolved, _) = reg.prefixes_for_guest(Some(&pve2), &["web".to_string()]).unwrap();
        let traefik = resolved.iter().find(|p| p.prefix.to_string() == "traefik").unwrap();
        assert!(!traefik.enforce, "pve1's override does not apply to pve2");
    }
}
