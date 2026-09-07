//! The on-disk file store: atomic reads/writes of guest and datacenter
//! documents, snapshots, and change-version polling.
//!
//! Root is `/etc/pve/meta` in production, a tempdir in tests. Documents are
//! always YAML (`docs/DESIGN.md` §8): `<vmid>.yaml`, `datacenter.yaml`, and
//! `<vmid>.<snapname>.yaml` for a guest's snapshot copies. All writes are
//! atomic (write a hidden, node- and call-unique sibling, then `rename`),
//! which pmxcfs supports.
//!
//! Serialising concurrent writers is *not* this layer's job: the API write
//! handlers run the whole read-check-write cycle under
//! `PVE::Cluster::cfs_lock_domain` (`docs/DESIGN.md` §8), and the digest
//! precondition enforced here ([`MetaStore::put_raw`]'s `expected_digest`) is
//! the single owner of the compare-and-swap rule.

use std::fmt;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use regex::Regex;
use sha2::{Digest as _, Sha256};

use crate::digest;
use crate::error::{Error, Result};
use crate::format::{self, Format};
use crate::model::Value;
use crate::patch::{self, Touched};

/// Warn threshold for document size (informational only; logged via
/// `tracing`).
pub const WARN_BYTES: u64 = 256 * 1024;
/// Hard limit for document size; exceeding it is [`Error::TooLarge`].
pub const MAX_BYTES: u64 = 512 * 1024;

/// The one on-disk format (`docs/DESIGN.md` §8: "YAML only on disk").
pub const DISK_FORMAT: Format = Format::Yaml;

/// Identifies a top-level document in the store (a guest's metadata, or the
/// datacenter's). Snapshots are addressed separately, by `(vmid, name)`, via
/// the dedicated snapshot methods.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DocId {
    /// A guest's metadata document, named `<vmid>.yaml`.
    Guest(u32),
    /// The datacenter's metadata document, named `datacenter.yaml`.
    Datacenter,
}

impl DocId {
    fn base_name(&self) -> String {
        match self {
            DocId::Guest(vmid) => vmid.to_string(),
            DocId::Datacenter => "datacenter".to_string(),
        }
    }
}

impl fmt::Display for DocId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DocId::Guest(vmid) => write!(f, "guest {vmid}"),
            DocId::Datacenter => write!(f, "datacenter"),
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
    /// The parsed value. **Unstripped**: comment keys are still present;
    /// stripping them is the caller's (API layer's) job via
    /// [`crate::model::strip_comments`].
    pub value: Value,
    /// The lowercase hex SHA-256 digest of the raw file bytes.
    pub digest: String,
    /// The file's last-modified time.
    pub mtime: SystemTime,
}

/// The result of [`MetaStore::put_raw`].
#[derive(Debug, Clone, PartialEq)]
pub struct PutResult {
    /// The new document.
    pub document: Document,
    /// The paths that changed, relative to the previous document (or an
    /// empty document, if none existed).
    pub touched: Vec<Touched>,
}

/// A poll-friendly summary of the whole store's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreVersion {
    /// A token that changes whenever any file's content changes (added,
    /// removed, or modified). Two stores with the same token have identical
    /// content.
    pub token: String,
    /// The most recent modification time observed among the store's files.
    pub changed: SystemTime,
}

/// What [`MetaStore::rollback`] actually did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RollbackOutcome {
    /// The snapshot existed and the live document was replaced with its
    /// content (creating the live document if it did not already exist).
    Restored,
    /// The snapshot did not exist, but a live document did: the guest had no
    /// metadata at snapshot time, so the live document was removed.
    RemovedNoSnapshot,
    /// Neither the snapshot nor a live document existed; nothing to do.
    NoOp,
}

fn snapshot_name_regex() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[A-Za-z][A-Za-z0-9_-]*$").unwrap())
}

/// `true` if `name` is a valid snapshot name: `^[A-Za-z][A-Za-z0-9_-]*$`,
/// excluding anything that is itself a format extension (so a snapshot file
/// name can never be confused with a live document's).
pub fn is_valid_snapshot_name(name: &str) -> bool {
    snapshot_name_regex().is_match(name) && Format::from_ext(name).is_none()
}

/// A process-wide counter making [`MetaStore::write_atomic`]'s temp file name
/// unique per call (the hostname and pid alone are not: two writes from one
/// pvedaemon worker would otherwise collide, and pmxcfs shares one directory
/// across every node).
static WRITE_SEQ: AtomicU64 = AtomicU64::new(0);

/// The on-disk metadata store.
pub struct MetaStore {
    root: PathBuf,
}

impl MetaStore {
    /// Opens a store rooted at `root` (created on first write; does not need
    /// to exist yet).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        MetaStore { root: root.into() }
    }

    /// The store's root directory.
    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    fn path_for(&self, id: DocId) -> PathBuf {
        self.root.join(format!("{}.{}", id.base_name(), DISK_FORMAT.ext()))
    }

    fn snapshot_path(&self, vmid: u32, name: &str) -> PathBuf {
        self.root
            .join(format!("{vmid}.{name}.{}", DISK_FORMAT.ext()))
    }

    /// Locates `id`'s document file, if it exists.
    pub fn locate(&self, id: DocId) -> Result<Option<PathBuf>> {
        let path = self.path_for(id);
        Ok(path.is_file().then_some(path))
    }

    fn check_size(size: u64) -> Result<()> {
        if size > MAX_BYTES {
            return Err(Error::TooLarge {
                size,
                max: MAX_BYTES,
            });
        }
        if size > WARN_BYTES {
            tracing::warn!(size, warn_bytes = WARN_BYTES, "document approaching size limit");
        }
        Ok(())
    }

    /// Writes `bytes` to `path` atomically: a hidden sibling, then `rename`.
    ///
    /// The temp name carries the node's hostname, the pid *and* a per-call
    /// counter, because `/etc/pve/meta` is one pmxcfs directory shared by
    /// every node in the cluster: `.<name>.tmp.<host>.<pid>.<seq>`.
    fn write_atomic(&self, path: &std::path::Path, bytes: &[u8]) -> Result<()> {
        fs::create_dir_all(&self.root)?;
        let file_name = path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| Error::Other(anyhow::anyhow!("path has no file name: {path:?}")))?;
        let host = hostname_tag();
        let seq = WRITE_SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp_path = self.root.join(format!(
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

    fn read_document(&self, id: DocId, path: &std::path::Path) -> Result<Document> {
        let bytes = fs::read(path)?;
        let raw = String::from_utf8(bytes.clone())
            .map_err(|e| Error::Other(anyhow::anyhow!("{}: invalid utf-8: {e}", path.display())))?;
        let value = format::parse(DISK_FORMAT, &raw)?;
        let dig = digest::digest(&bytes);
        let mtime = fs::metadata(path)?.modified()?;
        Ok(Document {
            id,
            path: path.to_path_buf(),
            raw,
            value,
            digest: dig,
            mtime,
        })
    }

    /// Reads `id`'s document.
    ///
    /// # Errors
    /// [`Error::NotFound`] if it does not exist.
    pub fn read(&self, id: DocId) -> Result<Document> {
        let path = self.locate(id)?.ok_or(Error::NotFound(id))?;
        self.read_document(id, &path)
    }

    /// The digest precondition, enforced in exactly one place
    /// (`docs/REVIEW-2026-09-07.md` F13): `None` means "no precondition";
    /// `Some("")` matches a *missing* document (that is the digest
    /// `GET` reports for one, `docs/DESIGN.md` §2, so the documented
    /// GET-then-PUT create flow works); any other `Some(_)` must equal the
    /// current file's digest.
    fn check_digest(current: Option<&[u8]>, expected: Option<&str>) -> Result<()> {
        let Some(expected) = expected else {
            return Ok(());
        };
        let actual = current.map(digest::digest).unwrap_or_default();
        if actual != expected {
            return Err(Error::DigestMismatch {
                expected: expected.to_string(),
                actual,
            });
        }
        Ok(())
    }

    /// Checks the compare-and-swap precondition for `id` without writing
    /// anything — the same rule [`MetaStore::put_raw`] enforces, exposed so a
    /// `dry_run` can validate exactly what the real write validates
    /// (`docs/DESIGN.md` §8, review F13/F14) without a second implementation
    /// of the rule living in the API layer.
    ///
    /// # Errors
    /// [`Error::DigestMismatch`].
    pub fn check_precondition(&self, id: DocId, expected: Option<&str>) -> Result<()> {
        if expected.is_none() {
            return Ok(());
        }
        let current = match fs::read(self.path_for(id)) {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        Self::check_digest(current.as_deref(), expected)
    }

    /// Replaces `id`'s document with `text` verbatim (only normalized to end
    /// with a single newline), creating it if it does not exist.
    ///
    /// # Errors
    /// [`Error::Parse`] / [`Error::Lint`] if `text` does not parse as a valid
    /// document; [`Error::DigestMismatch`] if `expected_digest` is given and
    /// does not match (`Some("")` matches a missing document);
    /// [`Error::TooLarge`] if `text` exceeds [`MAX_BYTES`].
    pub fn put_raw(&self, id: DocId, text: &str, expected_digest: Option<&str>) -> Result<PutResult> {
        let path = self.path_for(id);
        let existing = match fs::read(&path) {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        Self::check_digest(existing.as_deref(), expected_digest)?;

        let old_value = match &existing {
            Some(bytes) => {
                let old_raw = String::from_utf8(bytes.clone())
                    .map_err(|e| Error::Other(anyhow::anyhow!("invalid utf-8: {e}")))?;
                format::parse(DISK_FORMAT, &old_raw)?
            }
            None => Value::Object(serde_json::Map::new()),
        };

        let normalized = normalize_trailing_newline(text);
        Self::check_size(normalized.len() as u64)?;
        let new_value = format::parse(DISK_FORMAT, &normalized)?;

        self.write_atomic(&path, normalized.as_bytes())?;

        let touched = patch::diff(&old_value, &new_value);
        let dig = digest::digest(normalized.as_bytes());
        let mtime = fs::metadata(&path)?.modified()?;
        Ok(PutResult {
            document: Document {
                id,
                path,
                raw: normalized,
                value: new_value,
                digest: dig,
                mtime,
            },
            touched,
        })
    }

    /// Deletes `id`'s document — **only** the current document. Snapshot
    /// copies are owned by the lifecycle hooks (`docs/DESIGN.md` §8) and are
    /// removed by [`MetaStore::destroy`], never by this.
    ///
    /// # Errors
    /// [`Error::NotFound`] if it does not exist.
    pub fn delete(&self, id: DocId) -> Result<()> {
        let path = self.locate(id)?.ok_or(Error::NotFound(id))?;
        fs::remove_file(&path)?;
        Ok(())
    }

    /// Lists a guest's snapshot names, sorted.
    pub fn list_snapshots(&self, vmid: u32) -> Result<Vec<String>> {
        let mut out = Vec::new();
        if !self.root.is_dir() {
            return Ok(out);
        }
        let prefix = format!("{vmid}.");
        for entry in fs::read_dir(&self.root)? {
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

    /// Snapshots `vmid`'s current document under `name`. A no-op (returns
    /// `Ok(false)`) if the guest has no document.
    ///
    /// # Errors
    /// [`Error::InvalidName`] if `name` is not a valid snapshot name.
    pub fn snapshot(&self, vmid: u32, name: &str) -> Result<bool> {
        if !is_valid_snapshot_name(name) {
            return Err(Error::InvalidName(name.to_string()));
        }
        let Some(path) = self.locate(DocId::Guest(vmid))? else {
            return Ok(false);
        };
        let bytes = fs::read(&path)?;
        self.write_atomic(&self.snapshot_path(vmid, name), &bytes)?;
        Ok(true)
    }

    /// Rolls `vmid` back to snapshot `name`. See [`RollbackOutcome`] for the
    /// possible outcomes.
    ///
    /// # Errors
    /// [`Error::InvalidName`] if `name` is not a valid snapshot name.
    pub fn rollback(&self, vmid: u32, name: &str) -> Result<RollbackOutcome> {
        if !is_valid_snapshot_name(name) {
            return Err(Error::InvalidName(name.to_string()));
        }
        let snap = self.snapshot_path(vmid, name);
        let target = self.path_for(DocId::Guest(vmid));
        if snap.is_file() {
            let bytes = fs::read(&snap)?;
            self.write_atomic(&target, &bytes)?;
            Ok(RollbackOutcome::Restored)
        } else if target.is_file() {
            fs::remove_file(&target)?;
            Ok(RollbackOutcome::RemovedNoSnapshot)
        } else {
            Ok(RollbackOutcome::NoOp)
        }
    }

    /// Deletes a guest's snapshot. Idempotent: does nothing if it does not
    /// exist.
    ///
    /// # Errors
    /// [`Error::InvalidName`] if `name` is not a valid snapshot name.
    pub fn delete_snapshot(&self, vmid: u32, name: &str) -> Result<()> {
        if !is_valid_snapshot_name(name) {
            return Err(Error::InvalidName(name.to_string()));
        }
        let path = self.snapshot_path(vmid, name);
        if path.is_file() {
            fs::remove_file(&path)?;
        }
        Ok(())
    }

    /// Removes `vmid`'s document **and every snapshot copy** — the guest is
    /// gone. Used only by the `on_destroy` lifecycle hook; the REST API's
    /// `DELETE` uses [`MetaStore::delete`], which never touches snapshots
    /// (`docs/DESIGN.md` §8).
    ///
    /// Idempotent: a missing document is not an error.
    pub fn destroy(&self, vmid: u32) -> Result<()> {
        if self.locate(DocId::Guest(vmid))?.is_some() {
            self.delete(DocId::Guest(vmid))?;
        }
        for name in self.list_snapshots(vmid)? {
            self.delete_snapshot(vmid, &name)?;
        }
        Ok(())
    }

    /// A summary of the whole store's content, suitable for polling: the
    /// token changes whenever any file's content changes, and is stable
    /// otherwise (including across mere reads).
    ///
    /// The token is a SHA-256 over the sorted list of `(file name, content
    /// digest)`, hashed from the files' actual bytes on every call. There is
    /// deliberately no `(mtime, len)` cache: the bindings build a fresh
    /// `MetaStore` per request (so it could never hit), and pmxcfs's mtime
    /// granularity cannot distinguish two same-length writes within one tick
    /// (so it would be unsound if it did). Documents are tiny.
    pub fn version(&self) -> Result<StoreVersion> {
        let mut entries: Vec<(String, String)> = Vec::new();
        let mut latest: Option<SystemTime> = None;
        if self.root.is_dir() {
            for entry in fs::read_dir(&self.root)? {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') || !entry.file_type()?.is_file() {
                    continue;
                }
                let mtime = entry.metadata()?.modified()?;
                let dig = digest::digest(&fs::read(entry.path())?);
                entries.push((name, dig));
                latest = Some(match latest {
                    Some(t) if t >= mtime => t,
                    _ => mtime,
                });
            }
        }
        entries.sort();
        let mut hasher = Sha256::new();
        for (name, dig) in &entries {
            hasher.update((name.len() as u64).to_le_bytes());
            hasher.update(name.as_bytes());
            hasher.update(dig.as_bytes());
        }
        let token = hex::encode(hasher.finalize());
        Ok(StoreVersion {
            token,
            changed: latest.unwrap_or(SystemTime::UNIX_EPOCH),
        })
    }
}

/// A filesystem-safe tag identifying this node, for temp file names. Falls
/// back to `"node"` when the hostname is unavailable or unusable.
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
