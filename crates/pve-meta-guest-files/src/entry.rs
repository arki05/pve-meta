//! The `guest-files` key of a guest's document: what should be a file inside the
//! guest, and what that file holds.
//!
//! Every entry is judged on its own. An entry that does not validate is
//! **refused** and holds whatever it wrote before: a typo in `mode` must
//! not delete a service's configuration. An entry whose view is not in the
//! document is **absent**, which is the same as the entry having been
//! removed. Comment keys (`swap__`, `path__`) are the document's own notes and
//! are never entries or fields.

use std::collections::BTreeMap;
use std::fmt;

use pve_meta_core::format::Format;
use pve_meta_core::model::{self, Value};
use pve_meta_core::path::{self, Path};
use pve_meta_core::store::{DocId, MetaStore};
use pve_meta_core::{digest, view, Error as StoreError};

use crate::{GUEST_ROOT, MAX_CONTENT_BYTES, PREFIX};

/// Directories no entry may write under: kernel and device filesystems, where
/// a file is not configuration. `/run` is allowed.
pub const FORBIDDEN_ROOTS: [&str; 3] = ["/proc", "/sys", "/dev"];

/// Reserved inside every path segment: the daemon's temp files are named
/// with it, so no written file can be one.
pub const TEMP_MARK: &str = ".pve-meta-guest-files";

/// How a view becomes file content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileFormat {
    /// The store's canonical YAML dump of the view.
    Yaml,
    /// Pretty JSON with a trailing newline.
    Json,
    /// The view is a string and is written verbatim.
    Raw,
}

impl FileFormat {
    fn parse(s: &str) -> Option<FileFormat> {
        match s {
            "yaml" => Some(FileFormat::Yaml),
            "json" => Some(FileFormat::Json),
            "raw" => Some(FileFormat::Raw),
            _ => None,
        }
    }
}

/// What happens to a file that no longer holds what was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalEdits {
    /// Leave it, and do not delete it either.
    Keep,
    /// Replace it with the written content.
    Overwrite,
}

/// A file's owner as seen inside the guest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Owner {
    pub uid: u32,
    pub gid: u32,
}

impl Owner {
    pub const ROOT: Owner = Owner { uid: 0, gid: 0 };

    /// `uid:gid`, both decimal.
    pub fn parse(s: &str) -> Option<Owner> {
        let (u, g) = s.split_once(':')?;
        let num = |t: &str| {
            (!t.is_empty() && t.len() <= 10 && t.bytes().all(|b| b.is_ascii_digit()))
                .then(|| t.parse::<u32>().ok())
                .flatten()
        };
        Some(Owner { uid: num(u)?, gid: num(g)? })
    }
}

impl fmt::Display for Owner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.uid, self.gid)
    }
}

/// `0444` or `444`: permission bits only, no setuid, setgid or sticky bit.
pub fn parse_mode(s: &str) -> Option<u32> {
    let digits = match s.len() {
        4 => s.strip_prefix('0')?,
        3 => s,
        _ => return None,
    };
    if !digits.bytes().all(|b| (b'0'..=b'7').contains(&b)) {
        return None;
    }
    u32::from_str_radix(digits, 8).ok()
}

/// A mode as the document and the manifest spell it: four octal digits.
pub fn mode_text(mode: u32) -> String {
    format!("{mode:04o}")
}

/// A file's absolute path inside the guest, valid by construction.
///
/// An entry's `path` is either absolute or relative to [`GUEST_ROOT`]; both
/// resolve to this. Every segment is of the key charset plus `.`, never empty,
/// `.` or `..`, never carrying [`TEMP_MARK`]. Nothing is normalised: a path that
/// would need it is refused, so the path the document says is the path that is
/// written. Refused as well: `/` itself, anything under [`FORBIDDEN_ROOTS`],
/// and the manifest and every directory it lives in.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GuestPath(String);

impl GuestPath {
    /// Resolves and validates an entry's `path`.
    pub fn parse(s: &str) -> Result<GuestPath, String> {
        if s.is_empty() {
            return Err("path is empty".into());
        }
        let absolute = if s.starts_with('/') { s.to_string() } else { format!("{GUEST_ROOT}/{s}") };
        GuestPath::absolute(&absolute).map_err(|why| format!("path '{s}': {why}"))
    }

    /// Validates an already absolute path, as the manifest records one.
    pub fn absolute(p: &str) -> Result<GuestPath, String> {
        let Some(rest) = p.strip_prefix('/') else {
            return Err("not absolute".into());
        };
        if rest.is_empty() {
            return Err("'/' is not a file".into());
        }
        for seg in rest.split('/') {
            if seg.is_empty() {
                return Err("empty segment".into());
            }
            if seg == "." || seg == ".." {
                return Err(format!("'{seg}' segment"));
            }
            if let Some(c) =
                seg.chars().find(|&c| c != '.' && path::invalid_char(&c.to_string()).is_some())
            {
                return Err(format!("{c:?} is not allowed"));
            }
            if seg.contains(TEMP_MARK) {
                return Err(format!("'{TEMP_MARK}' is reserved"));
            }
        }
        if let Some(root) = FORBIDDEN_ROOTS.iter().find(|r| is_at_or_under(r, p)) {
            return Err(format!("{root} is not a filesystem files are written to"));
        }
        let manifest = crate::manifest::path();
        if is_at_or_under(p, &manifest) || is_under(&manifest, p) {
            return Err(format!("collides with the manifest {manifest}"));
        }
        Ok(GuestPath(p.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Every directory the file lives under, parent first, `/` excluded.
    pub fn directories(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut cur = String::new();
        let segs: Vec<&str> = self.0[1..].split('/').collect();
        for seg in &segs[..segs.len() - 1] {
            cur = format!("{cur}/{seg}");
            out.push(cur.clone());
        }
        out
    }

    /// The temp file a new version is pushed to: a hidden sibling, so the
    /// rename that replaces the file stays on one filesystem.
    pub fn temp(&self) -> String {
        temp_for(&self.0)
    }

    /// `true` when one of the two is a directory the other lives under: two
    /// such paths cannot both be files.
    pub fn nests_with(&self, other: &GuestPath) -> bool {
        is_under(&self.0, &other.0) || is_under(&other.0, &self.0)
    }
}

impl fmt::Display for GuestPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// `b` is strictly below the directory `a`.
fn is_under(a: &str, b: &str) -> bool {
    b.len() > a.len() && b.starts_with(a) && b.as_bytes()[a.len()] == b'/'
}

fn is_at_or_under(a: &str, b: &str) -> bool {
    a == b || is_under(a, b)
}

/// The temp sibling of an absolute path inside the guest.
pub fn temp_for(absolute: &str) -> String {
    let (dir, base) = absolute.rsplit_once('/').unwrap_or(("", absolute));
    format!("{dir}/.{base}{TEMP_MARK}.tmp")
}

/// One entry, validated.
///
/// `view` is `Some` for entries read from a document and `None` for managed
/// entries built by an operator through [`Entry::managed`]: the content of a
/// managed entry is given, never rendered from a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub view: Option<Path>,
    pub path: GuestPath,
    pub format: FileFormat,
    pub mode: u32,
    pub owner: Owner,
    pub local_edits: LocalEdits,
    /// Who owns this entry: `user/<name>` for document entries,
    /// `managed/<operator>/<name>` for operator-built ones. Recorded in the
    /// manifest, so two writers claiming one path refuse each other by name.
    pub source: String,
}

/// The source of a document entry: its own name under `user/`.
pub fn user_source(name: &str) -> String {
    format!("user/{name}")
}

/// The source of an operator-built entry.
pub fn managed_source(operator: &str, name: &str) -> String {
    format!("managed/{operator}/{name}")
}

/// `true` for a `managed/` source.
pub fn is_managed_source(source: &str) -> bool {
    source.starts_with("managed/")
}

/// Two sources claiming one path refuse each other, except two `user/`
/// entries: renaming an entry keeps its path working as before. A managed
/// source meeting anything but itself is always a fight between two writers.
pub(crate) fn sources_conflict(a: &str, b: &str) -> bool {
    a != b && (is_managed_source(a) || is_managed_source(b))
}

impl Entry {
    /// An operator-built entry: content given through [`Desired::direct`],
    /// never rendered from a document. The same path, mode and owner rules
    /// as a document entry; `format` is accepted and ignored.
    pub fn managed(
        operator: &str,
        name: &str,
        path: &str,
        format: FileFormat,
        mode: &str,
        owner: &str,
        local_edits: LocalEdits,
    ) -> Result<Entry, String> {
        Ok(Entry {
            name: name.to_string(),
            view: None,
            path: GuestPath::parse(path)?,
            format,
            mode: parse_mode(mode)
                .ok_or(format!("mode '{mode}' is not an octal mode such as \"0444\""))?,
            owner: Owner::parse(owner).ok_or(format!("owner '{owner}' is not uid:gid"))?,
            local_edits,
            source: managed_source(operator, name),
        })
    }
}

/// An entry with its content rendered from the document, or handed over
/// by an operator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Desired {
    pub entry: Entry,
    pub content: Vec<u8>,
    pub sha256: String,
}

impl Desired {
    /// Content handed over by an operator for a managed entry: the same size
    /// cap as rendered content, nothing else checked.
    pub fn direct(entry: Entry, content: Vec<u8>) -> Result<Desired, String> {
        check_content_len(&content)?;
        Ok(Desired { sha256: digest::digest(&content), content, entry })
    }
}

/// What one entry of the document asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    /// A file with this content.
    File(Desired),
    /// No file: the view is not in the document.
    Absent { name: String },
    /// Not valid; holds what the entry wrote before.
    Refused { name: String, reason: String },
}

impl Resolved {
    pub fn name(&self) -> &str {
        match self {
            Resolved::File(d) => &d.entry.name,
            Resolved::Absent { name } | Resolved::Refused { name, .. } => name,
        }
    }
}

/// A guest's files, as its document states it — or as an operator hands
/// them over. The daemon only ever sees the document side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuestFiles {
    /// No document, or no `guest-files` key: nothing should be written.
    Nothing,
    /// The document cannot be read, or `guest-files` is not a map. Everything
    /// written before is held.
    Held(String),
    /// The entries, in document order.
    Entries(Vec<Resolved>),
    /// Operator-handed files with content already rendered: every member is
    /// a file, never absent or refused. Built by [`GuestFiles::managed`],
    /// which refuses colliding paths up front.
    Managed(Vec<Desired>),
}

impl GuestFiles {
    /// Reads the `guest-files` key of a parsed document.
    pub fn of_document(doc: &Value) -> GuestFiles {
        let Some(tree) = doc.get(PREFIX) else {
            return GuestFiles::Nothing;
        };
        let Some(map) = tree.as_object() else {
            return GuestFiles::Held(format!("'{PREFIX}' is not a map"));
        };
        let mut entries: Vec<Resolved> = map
            .iter()
            .filter(|(name, _)| !model::is_comment_key(name))
            .map(|(name, spec)| resolve(doc, name, spec))
            .collect();
        refuse_collisions(&mut entries);
        GuestFiles::Entries(entries)
    }

    /// `true` when the document has a `guest-files` key, whatever it holds: the
    /// guest is one the daemon watches. Managed files count as keyed: there
    /// is described work either way.
    pub fn has_guest_files_key(&self) -> bool {
        !matches!(self, GuestFiles::Nothing)
    }

    /// Operator-handed files: every path must resolve to exactly one file,
    /// so two entries on one path (or one through the other's directory)
    /// are an error naming both, not a silent pick of a winner.
    pub fn managed(items: Vec<Desired>) -> Result<GuestFiles, String> {
        for (i, a) in items.iter().enumerate() {
            for b in &items[i + 1..] {
                if a.entry.path == b.entry.path || a.entry.path.nests_with(&b.entry.path) {
                    return Err(format!(
                        "path '{}' collides with entry '{}'",
                        a.entry.path, b.entry.name
                    ));
                }
            }
        }
        Ok(GuestFiles::Managed(items))
    }

    /// `true` when some entry wants a file.
    pub fn wants_files(&self) -> bool {
        match self {
            GuestFiles::Entries(e) => e.iter().any(|r| matches!(r, Resolved::File(_))),
            GuestFiles::Managed(items) => !items.is_empty(),
            _ => false,
        }
    }
}

/// The store's directory: `PVE_META_ROOT`, or `/etc/pve/meta`, as for every
/// other reader.
pub fn store_root() -> std::path::PathBuf {
    std::env::var_os("PVE_META_ROOT").unwrap_or_else(|| "/etc/pve/meta".into()).into()
}

/// The store at [`store_root`].
pub fn open_store() -> MetaStore {
    MetaStore::new(store_root())
}

/// A guest's files as its stored document says it. A document that
/// does not parse, or is above the read cap, holds everything.
pub fn read_guest_files(store: &MetaStore, vmid: u32) -> anyhow::Result<GuestFiles> {
    match store.read(&DocId::Guest(vmid)) {
        Ok(doc) => Ok(match &doc.parse_error {
            Some(e) => GuestFiles::Held(format!("the document does not parse: {e}")),
            None => GuestFiles::of_document(&doc.value),
        }),
        Err(StoreError::NotFound(_)) => Ok(GuestFiles::Nothing),
        Err(e @ StoreError::TooLarge { .. }) => Ok(GuestFiles::Held(e.to_string())),
        Err(e) => Err(anyhow::Error::new(e).context(format!("reading the document of {vmid}"))),
    }
}

/// Two entries naming one path, or one naming a directory the other's path
/// runs through, are both refused: neither is more right than the other.
fn refuse_collisions(entries: &mut [Resolved]) {
    let paths: Vec<Option<GuestPath>> = entries
        .iter()
        .map(|r| match r {
            Resolved::File(d) => Some(d.entry.path.clone()),
            _ => None,
        })
        .collect();
    let mut clash: BTreeMap<usize, String> = BTreeMap::new();
    for (i, a) in paths.iter().enumerate() {
        let Some(a) = a else { continue };
        for (j, b) in paths.iter().enumerate() {
            let Some(b) = b else { continue };
            if i != j && (a == b || a.nests_with(b)) {
                clash.entry(i).or_insert_with(|| {
                    format!("path '{a}' collides with entry '{}'", entries[j].name())
                });
            }
        }
    }
    for (i, reason) in clash {
        let name = entries[i].name().to_string();
        entries[i] = Resolved::Refused { name, reason };
    }
}

fn resolve(doc: &Value, name: &str, spec: &Value) -> Resolved {
    let entry = match parse_entry(name, spec) {
        Ok(e) => e,
        Err(reason) => return Resolved::Refused { name: name.to_string(), reason },
    };
    let Some(view) = &entry.view else {
        return Resolved::Refused {
            name: name.to_string(),
            reason: "a document entry always has a view".into(),
        };
    };
    let Some(value) = view::extract(doc, view) else {
        return Resolved::Absent { name: name.to_string() };
    };
    match render(&value, entry.format) {
        Ok(content) => {
            Resolved::File(Desired { sha256: digest::digest(&content), content, entry })
        }
        Err(reason) => Resolved::Refused { name: name.to_string(), reason },
    }
}

/// The size rule for any content, rendered or handed over: at most the
/// store's own write cap, so a view of a document written out of band and
/// an operator's blob meet the same limit.
pub fn check_content_len(content: &[u8]) -> Result<(), String> {
    if content.len() > MAX_CONTENT_BYTES {
        return Err(format!(
            "content is {} bytes, above the {MAX_CONTENT_BYTES}-byte cap",
            content.len()
        ));
    }
    Ok(())
}

/// The content of a view in `format`, or why it has none. Comment keys are
/// the document's notes, not configuration, and are never written.
pub fn render(value: &Value, format: FileFormat) -> Result<Vec<u8>, String> {
    let content = match format {
        FileFormat::Yaml => view::render(&view::strip_comments(value), Format::Yaml).into_bytes(),
        FileFormat::Json => view::render(&view::strip_comments(value), Format::Json).into_bytes(),
        FileFormat::Raw => match value {
            Value::String(s) => s.clone().into_bytes(),
            _ => return Err("format raw needs the view to be a string".into()),
        },
    };
    check_content_len(&content)?;
    Ok(content)
}

/// Validates one entry's fields. Unknown fields are refused, so a misspelt
/// `local_edit` does not silently mean `keep`.
pub fn parse_entry(name: &str, spec: &Value) -> Result<Entry, String> {
    let Some(map) = spec.as_object() else {
        return Err("an entry must be a map".into());
    };
    let text = |key: &str| -> Result<Option<&str>, String> {
        match map.get(key) {
            None => Ok(None),
            Some(Value::String(s)) => Ok(Some(s)),
            Some(_) => Err(format!("'{key}' must be a string")),
        }
    };
    for key in map.keys() {
        if !model::is_comment_key(key)
            && !["view", "path", "format", "mode", "owner", "local_edits"].contains(&key.as_str())
        {
            return Err(format!("unknown field '{key}'"));
        }
    }
    // `source` is deliberately absent above: provenance is assigned by the
    // reader, and a document claiming one is refused as an unknown field.
    let view_text = text("view")?.ok_or("'view' is required")?;
    if view_text.is_empty() || view_text.contains('/') {
        return Err(format!("view '{view_text}' is not a dotted key path"));
    }
    let view = Path::parse(view_text)
        .map_err(|_| format!("view '{view_text}' is not a dotted key path"))?;
    let path = GuestPath::parse(text("path")?.ok_or("'path' is required")?)?;
    let format = match text("format")? {
        None => FileFormat::Yaml,
        Some(f) => FileFormat::parse(f).ok_or(format!("format '{f}' is not yaml, json or raw"))?,
    };
    let mode = match text("mode")? {
        None => 0o444,
        Some(m) => {
            parse_mode(m).ok_or(format!("mode '{m}' is not an octal mode such as \"0444\""))?
        }
    };
    let owner = match text("owner")? {
        None => Owner::ROOT,
        Some(o) => Owner::parse(o).ok_or(format!("owner '{o}' is not uid:gid"))?,
    };
    let local_edits = match text("local_edits")? {
        None | Some("keep") => LocalEdits::Keep,
        Some("overwrite") => LocalEdits::Overwrite,
        Some(o) => return Err(format!("local_edits '{o}' is not keep or overwrite")),
    };
    Ok(Entry {
        name: name.to_string(),
        view: Some(view),
        path,
        format,
        mode,
        owner,
        local_edits,
        source: user_source(name),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pve_meta_core::format;

    fn doc(yaml: &str) -> Value {
        format::parse(Format::Yaml, yaml).unwrap()
    }

    fn entries(yaml: &str) -> Vec<Resolved> {
        match GuestFiles::of_document(&doc(yaml)) {
            GuestFiles::Entries(entries) => entries,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn paths() {
        for (given, resolved) in [
            ("a", "/etc/pve-meta/a"),
            ("llm/llama-swap.yaml", "/etc/pve-meta/llm/llama-swap.yaml"),
            (".env", "/etc/pve-meta/.env"),
            ("x@y!z/_-.json", "/etc/pve-meta/x@y!z/_-.json"),
            ("/etc/traefik/dynamic.yaml", "/etc/traefik/dynamic.yaml"),
            ("/root/somefile.yaml", "/root/somefile.yaml"),
            ("/run/app/conf", "/run/app/conf"),
            ("/devices", "/devices"),
            ("a/.guest-files", "/etc/pve-meta/a/.guest-files"),
        ] {
            assert_eq!(GuestPath::parse(given).unwrap().as_str(), resolved, "{given}");
        }
        for bad in [
            "",
            "/",
            "//",
            "a//b",
            "a/",
            "/etc/",
            "../x",
            "a/../b",
            "/etc/./x",
            "./a",
            "a b",
            "a\\b",
            "a*",
            "é",
            ".guest-files",
            ".guest-files/x",
            "/etc/pve-meta/.guest-files",
            "/etc/pve-meta",
            "/etc",
            "a/.x.pve-meta-guest-files.tmp",
            "/proc/sys/x",
            "/sys",
            "/dev/null",
        ] {
            assert!(GuestPath::parse(bad).is_err(), "{bad}");
        }
        assert!(GuestPath::absolute("relative").is_err());
    }

    #[test]
    fn directories_and_temp() {
        let p = GuestPath::parse("llm/conf/swap.yaml").unwrap();
        assert_eq!(
            p.directories(),
            ["/etc", "/etc/pve-meta", "/etc/pve-meta/llm", "/etc/pve-meta/llm/conf"]
        );
        assert_eq!(p.temp(), "/etc/pve-meta/llm/conf/.swap.yaml.pve-meta-guest-files.tmp");
        assert_eq!(GuestPath::parse("/root/x").unwrap().directories(), ["/root"]);
        assert!(GuestPath::parse("/x").unwrap().directories().is_empty());
        assert_eq!(GuestPath::parse("/x").unwrap().temp(), "/.x.pve-meta-guest-files.tmp");
        let a = GuestPath::parse("/a").unwrap();
        assert!(a.nests_with(&GuestPath::parse("/a/b").unwrap()));
        assert!(!a.nests_with(&GuestPath::parse("/ab/c").unwrap()));
        assert!(!a.nests_with(&a));
    }

    #[test]
    fn modes_and_owners() {
        assert_eq!(parse_mode("0444"), Some(0o444));
        assert_eq!(parse_mode("640"), Some(0o640));
        for bad in ["4755", "0888", "44", "00444", "", "0x44"] {
            assert_eq!(parse_mode(bad), None, "{bad}");
        }
        assert_eq!(mode_text(0o44), "0044");
        assert_eq!(Owner::parse("1000:100"), Some(Owner { uid: 1000, gid: 100 }));
        for bad in ["root:root", "1000", ":1", "1:", "-1:0", "99999999999:0"] {
            assert_eq!(Owner::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn rendering_per_format() {
        let v = doc("b: 1\na: [x, y]\n");
        assert_eq!(render(&v, FileFormat::Yaml).unwrap(), b"b: 1\na:\n- x\n- y\n");
        assert_eq!(
            String::from_utf8(render(&v, FileFormat::Json).unwrap()).unwrap(),
            "{\n  \"b\": 1,\n  \"a\": [\n    \"x\",\n    \"y\"\n  ]\n}\n"
        );
        assert!(render(&v, FileFormat::Raw).is_err());
        let noted = doc("a: 1\na__: a note\n__: about\nb: {c: 2, c__: note}\n");
        assert_eq!(render(&noted, FileFormat::Yaml).unwrap(), b"a: 1\nb:\n  c: 2\n");
        assert!(!String::from_utf8(render(&noted, FileFormat::Json).unwrap())
            .unwrap()
            .contains("__"));
        let s = Value::String("verbatim\nno newline added".into());
        assert_eq!(render(&s, FileFormat::Raw).unwrap(), b"verbatim\nno newline added");
        // The cap is the store's write cap: a view of a document written out
        // of band reaches it, and so does JSON's own rendering of one that is
        // just inside it.
        assert_eq!(MAX_CONTENT_BYTES, pve_meta_core::store::MAX_BYTES as usize);
        let big = Value::String("x".repeat(MAX_CONTENT_BYTES + 1));
        assert!(render(&big, FileFormat::Raw).unwrap_err().contains("cap"));
        // YAML writes "- 1\n" per member and JSON "  1,\n": a view just inside
        // the cap as the store holds it is above it as JSON.
        let wide = Value::Array((0..MAX_CONTENT_BYTES * 2 / 9).map(|_| Value::from(1)).collect());
        assert!(view::render(&wide, Format::Yaml).len() < MAX_CONTENT_BYTES);
        assert!(render(&wide, FileFormat::Yaml).is_ok());
        assert!(render(&wide, FileFormat::Json).is_err());
    }

    #[test]
    fn defaults_and_rendered_content() {
        let e = entries(
            "llm:\n  swap: {port: 8080}\nguest-files:\n  swap:\n    view: llm.swap\n    path: llm/swap.yaml\n",
        );
        let Resolved::File(d) = &e[0] else { panic!("{e:?}") };
        assert_eq!(d.entry.mode, 0o444);
        assert_eq!(d.entry.owner, Owner::ROOT);
        assert_eq!(d.entry.format, FileFormat::Yaml);
        assert_eq!(d.entry.local_edits, LocalEdits::Keep);
        assert_eq!(d.content, b"port: 8080\n");
        assert_eq!(d.sha256, digest::digest(b"port: 8080\n"));
    }

    #[test]
    fn missing_view_is_absent_and_bad_fields_are_refused() {
        let e = entries(
            "a: {x: 1}\nguest-files:\n  gone: {view: a.y, path: g}\n  typo: {view: a, path: t, local_edit: keep}\n  \
             mode: {view: a, path: m, mode: \"4755\"}\n  raw: {view: a, path: r, format: raw}\n  \
             dev: {view: a, path: /dev/sda}\n  noview: {path: n}\n  gone__: a comment key\n",
        );
        assert_eq!(e.len(), 6);
        assert_eq!(e[0], Resolved::Absent { name: "gone".into() });
        for r in &e[1..] {
            assert!(matches!(r, Resolved::Refused { .. }), "{r:?}");
        }
    }

    #[test]
    fn comment_keys_inside_an_entry_are_not_fields() {
        let e =
            entries("a: 1\nguest-files:\n  x: {view: a, path: x, path__: where the service reads}\n");
        assert!(matches!(e[0], Resolved::File(_)), "{e:?}");
    }

    #[test]
    fn colliding_paths_refuse_both() {
        let e = entries(
            "a: 1\nguest-files:\n  one: {view: a, path: x}\n  two: {view: a, path: x}\n  \
             dir: {view: a, path: d}\n  under: {view: a, path: d/e}\n  \
             rel: {view: a, path: s}\n  abs: {view: a, path: /etc/pve-meta/s}\n  fine: {view: a, path: f}\n",
        );
        assert!(e[..6].iter().all(|r| matches!(r, Resolved::Refused { .. })), "{e:?}");
        assert!(matches!(e[6], Resolved::File(_)));
    }

    #[test]
    fn document_shapes() {
        assert_eq!(GuestFiles::of_document(&doc("a: 1\n")), GuestFiles::Nothing);
        assert!(matches!(GuestFiles::of_document(&doc("guest-files: 3\n")), GuestFiles::Held(_)));
        assert!(GuestFiles::of_document(&doc("guest-files: {}\n")).has_guest_files_key());
    }

    fn managed_entry(operator: &str, name: &str, path: &str) -> Desired {
        let entry =
            Entry::managed(operator, name, path, FileFormat::Yaml, "0444", "0:0", LocalEdits::Keep)
                .unwrap();
        Desired::direct(entry, b"content\n".to_vec()).unwrap()
    }

    #[test]
    fn managed_entries_validate_like_document_ones() {
        assert!(Entry::managed("compose", "s", "/opt/stack/compose.yaml", FileFormat::Yaml, "0444", "0:0", LocalEdits::Keep).is_ok());
        for bad in [
            Entry::managed("compose", "s", "/dev/null", FileFormat::Yaml, "0444", "0:0", LocalEdits::Keep),
            Entry::managed("compose", "s", "/x", FileFormat::Yaml, "4755", "0:0", LocalEdits::Keep),
            Entry::managed("compose", "s", "/x", FileFormat::Yaml, "0444", "root", LocalEdits::Keep),
        ] {
            assert!(bad.is_err(), "{bad:?}");
        }
        let entry = Entry::managed("compose", "stack", "/opt/stack/compose.yaml", FileFormat::Yaml, "0644", "0:0", LocalEdits::Overwrite).unwrap();
        assert_eq!(entry.source, "managed/compose/stack");
        assert_eq!(entry.view, None);
        assert!(Desired::direct(entry, vec![b'x'; MAX_CONTENT_BYTES + 1]).is_err());
    }

    #[test]
    fn managed_lists_refuse_colliding_paths() {
        let a = managed_entry("compose", "one", "/opt/stack/a.yaml");
        let b = managed_entry("compose", "two", "/opt/stack/a.yaml");
        let err = GuestFiles::managed(vec![a, b]).unwrap_err();
        assert!(err.contains("/opt/stack/a.yaml") && err.contains("two"), "{err}");
        let ok = GuestFiles::managed(vec![
            managed_entry("compose", "one", "/opt/stack/a.yaml"),
            managed_entry("compose", "two", "/opt/stack/b.yaml"),
        ])
        .unwrap();
        assert!(ok.wants_files() && ok.has_guest_files_key());
    }

    #[test]
    fn sources_conflict_only_across_writers() {
        assert!(!sources_conflict("user/a", "user/a"));
        assert!(!sources_conflict("user/a", "user/b"));
        assert!(sources_conflict("user/a", "managed/compose/stack"));
        assert!(sources_conflict("managed/compose/stack", "user/a"));
        assert!(sources_conflict("managed/a/x", "managed/b/y"));
        assert!(!sources_conflict("managed/compose/stack", "managed/compose/stack"));
    }
}
