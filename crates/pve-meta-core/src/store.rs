//! The on-disk file store: atomic guest-document and snapshot I/O, the
//! digest compare-and-swap, and version polling (`docs/DESIGN.md` §1-§2).
//! Root is `/etc/pve/meta` in production, a tempdir in tests.

use std::fmt;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use regex::Regex;
use sha2::{Digest as _, Sha256};

use crate::digest;
use crate::error::{Error, Result};
use crate::format::{self, Format};
use crate::model::Value;
pub use crate::registry::RegistryKind;
use crate::registry::Registry;

/// Size warning threshold, informational only (see [`crate::warn`]).
pub const WARN_BYTES: u64 = 256 * 1024;
/// Hard write-size limit; exceeding it is [`Error::TooLarge`].
pub const MAX_BYTES: u64 = 512 * 1024;
/// Hard read limit; above it [`identify`] uses `(len, mtime)`, never bytes.
pub const MAX_READ_BYTES: u64 = 4 * 1024 * 1024;

/// The on-disk format (`docs/DESIGN.md` §2: YAML).
pub const DISK_FORMAT: Format = Format::Yaml;

/// A document id: a guest document, or a registry file (a document too: `docs/decisions/003-registry-files-are-documents.md`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DocId {
    /// A guest's metadata document, named `<vmid>.yaml`.
    Guest(u32),
    /// A prefix file, named `<name>.yaml` in its kind's directory.
    Registry(RegistryKind, String),
}

impl DocId {
    fn base_name(&self) -> String {
        match self {
            DocId::Guest(vmid) => vmid.to_string(),
            DocId::Registry(_, name) => name.clone(),
        }
    }
}

/// The wire and error-message spelling: `105`, `prefixes/<name>`.
impl fmt::Display for DocId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DocId::Guest(vmid) => write!(f, "{vmid}"),
            DocId::Registry(kind, name) => write!(f, "{kind}/{name}"),
        }
    }
}

/// A document read from (or just written to) the store.
#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    /// The document's id.
    pub id: DocId,
    /// Its absolute path.
    pub path: PathBuf,
    /// The raw file text.
    pub raw: String,
    /// The parsed value, empty when [`Document::parse_error`] is set.
    pub value: Value,
    /// `Some(message)` if the text is not valid YAML (`docs/DESIGN.md` §1 invariant 5).
    pub parse_error: Option<String>,
    /// The lowercase hex SHA-256 digest of the raw file bytes.
    pub digest: String,
    /// The file's last-modified time.
    pub mtime: SystemTime,
}

/// A poll-friendly summary of the whole store's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreVersion {
    /// Changes whenever any file's content changes; equal tokens mean
    /// identical content.
    pub token: String,
}

/// What [`MetaStore::rollback`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RollbackOutcome {
    /// The snapshot existed; the live document now matches it (created if absent).
    Restored,
    /// No such snapshot, but a live document did exist: it was removed.
    RemovedNoSnapshot,
    /// Neither existed; nothing to do.
    NoOp,
}

fn snapshot_name_regex() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[A-Za-z][A-Za-z0-9_-]*$").unwrap())
}

/// `true` for a valid snapshot name: `^[A-Za-z][A-Za-z0-9_-]*$`, excluding a
/// format extension (so a snapshot file can't be mistaken for a live one).
pub fn is_valid_snapshot_name(name: &str) -> bool {
    snapshot_name_regex().is_match(name) && Format::from_ext(name).is_none()
}

/// `Ok(None)` for not-found (it may just have vanished), `Ok(Some(v))` otherwise, anything else an error.
pub(crate) fn gone_is_none<T>(r: io::Result<T>) -> Result<Option<T>> {
    match r {
        Ok(v) => Ok(Some(v)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// A file's content identity: SHA-256, or above [`MAX_READ_BYTES`] a `(len, mtime)` surrogate.
fn identify(path: &std::path::Path) -> Result<Option<String>> {
    let Some(meta) = gone_is_none(fs::metadata(path))? else {
        return Ok(None);
    };
    if meta.len() > MAX_READ_BYTES {
        return Ok(Some(oversized_identity(meta.len(), meta.modified()?)));
    }
    Ok(gone_is_none(fs::read(path))?
        .as_deref()
        .map(digest::digest))
}

/// A digest-shaped surrogate over `(len, mtime)`; compared, never parsed.
fn oversized_identity(len: u64, mtime: SystemTime) -> String {
    let nanos = mtime
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut hasher = Sha256::new();
    hasher.update(b"pve-meta:above-read-cap:");
    hasher.update(len.to_le_bytes());
    hasher.update(nanos.to_le_bytes());
    hex::encode(hasher.finalize())
}

/// Per-call counter making [`MetaStore::write_atomic`]'s temp name unique.
static WRITE_SEQ: AtomicU64 = AtomicU64::new(0);

/// `root` holds documents and snapshots; `registry` holds the prefix drop-directories (`docs/DESIGN.md` §3).
pub struct MetaStore {
    root: PathBuf,
    registry: Registry,
    marker: Option<PathBuf>,
}

/// pmxcfs's mount point: a store rooted below it is a store over the cluster fs.
pub const CLUSTER_ROOT: &str = "/etc/pve";
/// The `local` symlink pmxcfs provides when mounted.
pub const CLUSTER_MARKER: &str = "/etc/pve/local";
/// Overrides [`CLUSTER_MARKER`] for any root; empty means "check nothing" (a test knob).
pub const CLUSTER_MARKER_ENV: &str = "PVE_META_CLUSTER_MARKER";

impl MetaStore {
    /// Opens a store rooted at `root` (created lazily), with [`Registry::from_env`]'s directories.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        MetaStore::with_registry(root, Registry::from_env())
    }

    /// [`MetaStore::new`] with the registry directories given explicitly, lowest precedence first.
    pub fn with_registry_dirs(root: impl Into<PathBuf>, prefix_dirs: Vec<PathBuf>) -> Self {
        MetaStore::with_registry(root, Registry::new(prefix_dirs))
    }

    /// [`MetaStore::new`] with a [`Registry`] given directly; the cluster marker is still decided from `root`.
    pub fn with_registry(root: impl Into<PathBuf>, registry: Registry) -> Self {
        let root = root.into();
        let marker = match std::env::var_os(CLUSTER_MARKER_ENV) {
            Some(v) if v.is_empty() => None,
            Some(v) => Some(PathBuf::from(v)),
            None => root.starts_with(CLUSTER_ROOT).then(|| PathBuf::from(CLUSTER_MARKER)),
        };
        MetaStore { root, registry, marker }
    }

    /// This store, checking `marker` before every operation instead.
    pub fn with_cluster_marker(mut self, marker: impl Into<PathBuf>) -> Self {
        self.marker = Some(marker.into());
        self
    }

    /// Checked first by every public method: `Ok` unless a cluster marker exists and is not a symlink (`docs/DESIGN.md` §1 invariant 3).
    pub fn check_available(&self) -> Result<()> {
        let Some(marker) = &self.marker else {
            return Ok(());
        };
        match fs::symlink_metadata(marker) {
            Ok(meta) if meta.file_type().is_symlink() => Ok(()),
            Ok(_) => Err(Error::Unavailable(format!("{} is not a symlink; is pmxcfs mounted?", marker.display()))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                Err(Error::Unavailable(format!("{} is not there; is pmxcfs mounted?", marker.display())))
            }
            Err(e) => Err(Error::Unavailable(format!("{}: {e}", marker.display()))),
        }
    }

    /// The [`Registry`] this store reads and writes through.
    pub fn registry(&self) -> Result<&Registry> {
        self.check_available()?;
        Ok(&self.registry)
    }

    /// The highest-precedence directory of `kind` a write lands in, falling back inside `root` if none is configured.
    fn registry_write_dir(&self, kind: RegistryKind) -> PathBuf {
        self.registry
            .write_dir(kind)
            .unwrap_or_else(|| self.root.join("meta.d").join(kind.as_str()))
    }

    /// Where `id` is written (see [`MetaStore::read_path_for`] for reads).
    fn path_for(&self, id: &DocId) -> PathBuf {
        let file = format!("{}.{}", id.base_name(), DISK_FORMAT.ext());
        match id {
            DocId::Registry(kind, _) => self.registry_write_dir(*kind).join(file),
            DocId::Guest(_) => self.root.join(file),
        }
    }

    /// Where `id` is read from: [`Registry::locate`]'s file, or [`MetaStore::path_for`] when none has it.
    fn read_path_for(&self, id: &DocId) -> PathBuf {
        match id {
            DocId::Registry(kind, name) => {
                self.registry.locate(*kind, name).unwrap_or_else(|| self.path_for(id))
            }
            DocId::Guest(_) => self.path_for(id),
        }
    }

    fn snapshot_path(&self, vmid: u32, name: &str) -> PathBuf {
        self.root
            .join(format!("{vmid}.{name}.{}", DISK_FORMAT.ext()))
    }

    fn check_size(size: u64) -> Result<()> {
        if size > MAX_BYTES {
            return Err(Error::TooLarge {
                size,
                max: MAX_BYTES,
            });
        }
        if size > WARN_BYTES {
            crate::warn_line!("document approaching size limit: {size} bytes (warn at {WARN_BYTES})");
        }
        Ok(())
    }

    /// Writes `bytes` to `path` atomically, via a temp sibling and `rename`.
    fn write_atomic(&self, path: &std::path::Path, bytes: &[u8]) -> Result<()> {
        let dir = path.parent().unwrap_or(&self.root).to_path_buf();
        fs::create_dir_all(&dir)?;
        let file_name = path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| Error::Other(anyhow::anyhow!("path has no file name: {path:?}")))?;
        let host = hostname_tag();
        let seq = WRITE_SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp_path = dir.join(format!(
            ".{file_name}.tmp.{host}.{}.{seq}",
            std::process::id()
        ));
        fs::write(&tmp_path, bytes)?;
        if let Err(e) = fs::rename(&tmp_path, path) {
            let _ = fs::remove_file(&tmp_path);
            return Err(e.into());
        }
        Ok(())
    }

    /// Reads and parses one document file; a syntax error becomes [`Document::parse_error`], never an error (`docs/DESIGN.md` §1 invariant 5).
    fn read_document(&self, id: &DocId, path: &std::path::Path) -> Result<Document> {
        let Some(meta) = gone_is_none(fs::metadata(path))? else {
            return Err(Error::NotFound(id.clone()));
        };
        Self::check_read_size(meta.len())?;
        let mtime = meta.modified()?;
        let Some(bytes) = gone_is_none(fs::read(path))? else {
            return Err(Error::NotFound(id.clone()));
        };
        // Not UTF-8 is the one read failure that is not a parse_error: there
        // is no `raw` to report.
        let raw = String::from_utf8(bytes.clone()).map_err(|e| Error::Parse {
            format: DISK_FORMAT,
            msg: format!("invalid utf-8: {e}"),
            at: None,
        })?;
        let (value, parse_error) = match format::parse_raw(DISK_FORMAT, &raw) {
            Ok(value) => (value, None),
            Err(e) => {
                crate::warn_line!(
                    "stored document is not valid YAML; reading it as empty: {id}: {e}"
                );
                (Value::Object(serde_json::Map::new()), Some(e.to_string()))
            }
        };
        let dig = digest::digest(&bytes);
        Ok(Document {
            id: id.clone(),
            path: path.to_path_buf(),
            raw,
            value,
            parse_error,
            digest: dig,
            mtime,
        })
    }

    fn check_read_size(size: u64) -> Result<()> {
        if size > MAX_READ_BYTES {
            return Err(Error::TooLarge {
                size,
                max: MAX_READ_BYTES,
            });
        }
        Ok(())
    }

    /// Reads `id`'s document. See [`MetaStore::read_document`].
    pub fn read(&self, id: &DocId) -> Result<Document> {
        self.check_available()?;
        // No existence check first: that races a concurrent delete into `Io` (500).
        self.read_document(id, &self.read_path_for(id))
    }

    /// `true` if `id` has a file at the path a write lands in: for a registry
    /// document its cluster file, never the packaged file a read falls back to
    /// ([`MetaStore::read_path_for`]) and no write can touch (`docs/DESIGN.md` §3).
    pub fn has_own_file(&self, id: &DocId) -> Result<bool> {
        self.check_available()?;
        Ok(gone_is_none(fs::metadata(self.path_for(id)))?.is_some())
    }

    /// `id`'s current content identity without reading the whole document; see [`identify`].
    pub fn digest_of(&self, id: &DocId) -> Result<Option<String>> {
        self.check_available()?;
        identify(&self.read_path_for(id))
    }

    /// `None` means no precondition; `Some("")` matches a missing document (`docs/DESIGN.md` §5); else must match exactly.
    fn check_digest(current: Option<&str>, expected: Option<&str>) -> Result<()> {
        let Some(expected) = expected else {
            return Ok(());
        };
        let actual = current.unwrap_or_default();
        if actual != expected {
            return Err(Error::DigestMismatch {
                expected: expected.to_string(),
                actual: actual.to_string(),
            });
        }
        Ok(())
    }

    /// Checks the compare-and-swap precondition without writing, for `dry_run` (`docs/DESIGN.md` §5).
    pub fn check_precondition(&self, id: &DocId, expected: Option<&str>) -> Result<()> {
        if expected.is_none() {
            return Ok(());
        }
        Self::check_digest(self.digest_of(id)?.as_deref(), expected)
    }

    /// Replaces `id`'s document with `text` (normalized to one trailing
    /// newline), creating it if absent, and returns what was written. The
    /// text is parsed and linted first; what it replaces is never read, so a
    /// file that does not parse is no obstacle to its repair. What changed is
    /// the caller's to know: the API diffs its own plan.
    pub fn put_raw(
        &self,
        id: &DocId,
        text: &str,
        expected_digest: Option<&str>,
    ) -> Result<Document> {
        self.check_available()?;
        let path = self.path_for(id);
        Self::check_digest(self.digest_of(id)?.as_deref(), expected_digest)?;

        let normalized = normalize_trailing_newline(text);
        Self::check_size(normalized.len() as u64)?;
        let value = format::parse(DISK_FORMAT, &normalized)?;

        self.write_atomic(&path, normalized.as_bytes())?;

        let dig = digest::digest(normalized.as_bytes());
        // The file just written can already be gone again (a race): report
        // this write's own moment rather than a `stat` of nothing.
        let mtime = match gone_is_none(fs::metadata(&path))? {
            Some(meta) => meta.modified()?,
            None => SystemTime::now(),
        };
        Ok(Document {
            id: id.clone(),
            path,
            raw: normalized,
            value,
            parse_error: None,
            digest: dig,
            mtime,
        })
    }

    /// Deletes `id`'s current document only, never its snapshots ([`MetaStore::purge`]); idempotent.
    pub fn delete(&self, id: &DocId) -> Result<bool> {
        self.check_available()?;
        Ok(gone_is_none(fs::remove_file(self.path_for(id)))?.is_some())
    }

    /// Every vmid with any file, sorted; what `pve-meta ls --orphans` subtracts the vmlist from (`docs/DESIGN.md` §7).
    pub fn stored_vmids(&self) -> Result<Vec<u32>> {
        let mut out = std::collections::BTreeSet::new();
        for (_, vmid) in self.classified_root()? {
            if let Some(vmid) = vmid {
                out.insert(vmid);
            }
        }
        Ok(out.into_iter().collect())
    }

    /// Every file in the root this store cannot name -- neither `<vmid>.yaml`
    /// nor `<vmid>.<snapname>.yaml` -- by file name, sorted. [`MetaStore::stored_vmids`]
    /// passes them over and [`MetaStore::version`] hashes them, so without
    /// this a typo'd file name is invisible; `pve-meta ls` lists them, and
    /// nothing removes them or counts them as an orphan.
    pub fn unknown_files(&self) -> Result<Vec<String>> {
        let mut out: Vec<String> = self
            .classified_root()?
            .into_iter()
            .filter(|(_, vmid)| vmid.is_none())
            .map(|(name, _)| name)
            .collect();
        out.sort();
        Ok(out)
    }

    /// Every regular, non-hidden file in the root with the vmid its name
    /// carries, or `None` for a name this store does not give out: the one
    /// classification both listings read.
    fn classified_root(&self) -> Result<Vec<(String, Option<u32>)>> {
        self.check_available()?;
        let mut out = Vec::new();
        let Some(entries) = gone_is_none(fs::read_dir(&self.root))? else {
            return Ok(out);
        };
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(file_type) = gone_is_none(entry.file_type())? else {
                continue;
            };
            // A dot file is this store's own write-in-progress temp name.
            if name.starts_with('.') || !file_type.is_file() {
                continue;
            }
            let vmid = vmid_of_file(&name);
            out.push((name, vmid));
        }
        Ok(out)
    }

    /// Lists a guest's snapshot names, sorted.
    pub fn list_snapshots(&self, vmid: u32) -> Result<Vec<String>> {
        self.check_available()?;
        let mut out = Vec::new();
        let Some(entries) = gone_is_none(fs::read_dir(&self.root))? else {
            return Ok(out);
        };
        let prefix = format!("{vmid}.");
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || !name.starts_with(&prefix) {
                continue;
            }
            let rest = &name[prefix.len()..];
            let Some((snap_name, ext)) = rest.rsplit_once('.') else {
                continue;
            };
            if Format::from_ext(ext) != Some(DISK_FORMAT) || !is_valid_snapshot_name(snap_name) {
                continue;
            }
            out.push(snap_name.to_string());
        }
        out.sort();
        Ok(out)
    }

    /// Snapshots `vmid`'s document under `name`, or `Ok(false)` if it has none.
    pub fn snapshot(&self, vmid: u32, name: &str) -> Result<bool> {
        self.check_available()?;
        if !is_valid_snapshot_name(name) {
            return Err(Error::InvalidName(name.to_string()));
        }
        let Some(bytes) = gone_is_none(fs::read(self.path_for(&DocId::Guest(vmid))))? else {
            return Ok(false);
        };
        self.write_atomic(&self.snapshot_path(vmid, name), &bytes)?;
        Ok(true)
    }

    /// Rolls `vmid` back to snapshot `name`; see [`RollbackOutcome`].
    pub fn rollback(&self, vmid: u32, name: &str) -> Result<RollbackOutcome> {
        self.check_available()?;
        if !is_valid_snapshot_name(name) {
            return Err(Error::InvalidName(name.to_string()));
        }
        let snap = self.snapshot_path(vmid, name);
        let target = self.path_for(&DocId::Guest(vmid));
        if let Some(bytes) = gone_is_none(fs::read(&snap))? {
            self.write_atomic(&target, &bytes)?;
            Ok(RollbackOutcome::Restored)
        } else if gone_is_none(fs::remove_file(&target))?.is_some() {
            Ok(RollbackOutcome::RemovedNoSnapshot)
        } else {
            Ok(RollbackOutcome::NoOp)
        }
    }

    /// Deletes a guest's snapshot, idempotently.
    pub fn delete_snapshot(&self, vmid: u32, name: &str) -> Result<bool> {
        self.check_available()?;
        if !is_valid_snapshot_name(name) {
            return Err(Error::InvalidName(name.to_string()));
        }
        Ok(gone_is_none(fs::remove_file(self.snapshot_path(vmid, name)))?.is_some())
    }

    /// Removes `vmid`'s document and every snapshot (`docs/DESIGN.md` §7); the API's `DELETE` uses [`MetaStore::delete`] instead.
    pub fn purge(&self, vmid: u32) -> Result<usize> {
        let mut removed = 0;
        if self.delete(&DocId::Guest(vmid))? {
            removed += 1;
        }
        for name in self.list_snapshots(vmid)? {
            if self.delete_snapshot(vmid, &name)? {
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// A poll-friendly summary of the store, hashed fresh from every file's bytes each call (no `(mtime, len)` cache: pmxcfs can't tell two same-tick writes apart).
    pub fn version(&self) -> Result<StoreVersion> {
        self.check_available()?;
        let mut entries: Vec<(String, String)> = Vec::new();
        self.scan_for_version(&self.root.clone(), "", &mut entries)?;
        for dir in self.registry.dirs(RegistryKind::PrefixDef) {
            // Full path, not just the kind: two directories of one kind hold
            // same-named files on purpose.
            let prefix = format!("{}/", dir.display());
            self.scan_for_version(&dir.clone(), &prefix, &mut entries)?;
        }

        entries.sort();
        let mut hasher = Sha256::new();
        for (name, dig) in &entries {
            hasher.update((name.len() as u64).to_le_bytes());
            hasher.update(name.as_bytes());
            hasher.update(dig.as_bytes());
        }
        Ok(StoreVersion { token: hex::encode(hasher.finalize()) })
    }

    /// One directory's `(name, identity)` entries for [`MetaStore::version`]; a symlink is skipped, not followed.
    fn scan_for_version(
        &self,
        dir: &std::path::Path,
        prefix: &str,
        entries: &mut Vec<(String, String)>,
    ) -> Result<()> {
        let Some(listing) = gone_is_none(fs::read_dir(dir))? else {
            return Ok(());
        };
        for entry in listing {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let path = entry.path();
            let Some(meta) = gone_is_none(fs::symlink_metadata(&path))? else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            let Some(dig) = identify(&path)? else {
                continue;
            };
            entries.push((format!("{prefix}{name}"), dig));
        }
        Ok(())
    }
}

/// The vmid `name` names: `<vmid>.yaml` or `<vmid>.<snapname>.yaml`, the vmid
/// being the first dot-separated component either way. `None` for anything
/// else under the store's root.
fn vmid_of_file(name: &str) -> Option<u32> {
    let stem = name.strip_suffix(&format!(".{}", DISK_FORMAT.ext()))?;
    let (head, snap) = match stem.split_once('.') {
        Some((head, snap)) => (head, Some(snap)),
        None => (stem, None),
    };
    if snap.is_some_and(|s| !is_valid_snapshot_name(s)) {
        return None;
    }
    head.parse::<u32>().ok()
}

/// A filesystem-safe node tag for temp file names; falls back to `"node"`.
fn hostname_tag() -> String {
    let raw = std::env::var("HOSTNAME")
        .ok()
        .or_else(|| fs::read_to_string("/proc/sys/kernel/hostname").ok())
        .unwrap_or_default();
    let tag: String = raw
        .trim()
        .split('.')
        .next()
        .unwrap_or("")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(32)
        .collect();
    if tag.is_empty() {
        "node".to_string()
    } else {
        tag
    }
}

fn normalize_trailing_newline(text: &str) -> String {
    let trimmed = text.trim_end_matches('\n');
    format!("{trimmed}\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_name_validation() {
        assert!(is_valid_snapshot_name("before-upgrade"));
        assert!(is_valid_snapshot_name("a"));
        assert!(!is_valid_snapshot_name("1abc"));
        assert!(!is_valid_snapshot_name("bad name"));
        assert!(!is_valid_snapshot_name("yaml"));
        assert!(!is_valid_snapshot_name("json"));
        assert!(!is_valid_snapshot_name("yml"));
    }

    #[test]
    fn normalize_trailing_newline_collapses_multiple() {
        assert_eq!(normalize_trailing_newline("a: 1"), "a: 1\n");
        assert_eq!(normalize_trailing_newline("a: 1\n\n\n"), "a: 1\n");
        assert_eq!(normalize_trailing_newline("a: 1\n"), "a: 1\n");
    }

    #[test]
    fn hostname_tag_is_filesystem_safe_and_never_empty() {
        let tag = hostname_tag();
        assert!(!tag.is_empty());
        assert!(tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn write_atomic_temp_names_are_unique_per_call() {
        let a = WRITE_SEQ.fetch_add(1, Ordering::Relaxed);
        let b = WRITE_SEQ.fetch_add(1, Ordering::Relaxed);
        assert_ne!(a, b);
    }
}
