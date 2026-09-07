//! The on-disk file store: atomic reads/writes of guest and datacenter
//! documents, snapshots, and cheap change-version polling.
//!
//! Root is `/etc/pve/meta` in production, a tempdir in tests. All writes are
//! atomic (write a `.name.tmp.pid` sibling, then `rename`), which pmxcfs
//! supports.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use regex::Regex;
use sha2::{Digest as _, Sha256};

use crate::digest;
use crate::edit;
use crate::error::{Error, Result};
use crate::format::{self, Format};
use crate::model::Value;
use crate::patch::{self, Touched};

/// Warn threshold for document size (informational only; logged via
/// `tracing`).
pub const WARN_BYTES: u64 = 256 * 1024;
/// Hard limit for document size; exceeding it is [`Error::TooLarge`].
pub const MAX_BYTES: u64 = 512 * 1024;

/// Identifies a top-level document in the store (a guest's metadata, or the
/// datacenter's). Snapshots are addressed separately, by `(vmid, name)`, via
/// the dedicated snapshot methods.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DocId {
    /// A guest's metadata document, named `<vmid>.<ext>`.
    Guest(u32),
    /// The datacenter's metadata document, named `datacenter.<ext>`.
    Datacenter,
}

impl DocId {
    fn base_name(&self) -> String {
        match self {
            DocId::Guest(vmid) => vmid.to_string(),
            DocId::Datacenter => "datacenter".to_string(),
        }
    }

    fn file_name(&self, format: Format) -> String {
        format!("{}.{}", self.base_name(), format.ext())
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

/// The result of locating a document on disk: where it is, and in which
/// format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    /// The document's absolute path.
    pub path: PathBuf,
    /// The format its extension indicates.
    pub format: Format,
}

/// A document read from (or just written to) the store.
#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    /// The document's id.
    pub id: DocId,
    /// Its on-disk format.
    pub format: Format,
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

/// One guest's entry as listed by [`MetaStore::list_guests`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestEntry {
    /// The guest's vmid.
    pub vmid: u32,
    /// Its document's format.
    pub format: Format,
    /// Its document's digest.
    pub digest: String,
    /// Its document's last-modified time.
    pub mtime: SystemTime,
    /// Its document's size, in bytes.
    pub size: u64,
}

/// A cheap, poll-friendly summary of the whole store's state.
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

/// `true` if `name` is a valid snapshot name: `^[A-Za-z][A-Za-z0-9_-]*$`.
/// This can never collide with a format extension, since extensions are a
/// closed, all-lowercase set that a leading-letter-followed-by-anything
/// pattern could match syntactically, but which is excluded explicitly here.
pub fn is_valid_snapshot_name(name: &str) -> bool {
    snapshot_name_regex().is_match(name) && Format::from_ext(name).is_none()
}

/// A cached `(mtime, len, digest)` triple for a single file, used by
/// [`MetaStore::version`] to avoid re-hashing files that have not changed.
type VersionCacheEntry = (SystemTime, u64, String);

/// The on-disk metadata store.
pub struct MetaStore {
    root: PathBuf,
    version_cache: Mutex<HashMap<PathBuf, VersionCacheEntry>>,
}

impl MetaStore {
    /// Opens a store rooted at `root` (created on first write; does not need
    /// to exist yet).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        MetaStore {
            root: root.into(),
            version_cache: Mutex::new(HashMap::new()),
        }
    }

    /// The store's root directory.
    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    fn path_for(&self, id: DocId, format: Format) -> PathBuf {
        self.root.join(id.file_name(format))
    }

    /// Finds the single file matching `stem.<ext>` for any known format
    /// extension.
    ///
    /// # Errors
    /// [`Error::Conflict`] if more than one format file exists for `stem`.
    fn locate_stem(&self, stem: &str) -> Result<Option<Located>> {
        let mut found = None;
        for fmt in Format::ALL {
            let path = self.root.join(format!("{stem}.{}", fmt.ext()));
            if path.is_file() {
                if found.is_some() {
                    return Err(Error::Conflict(format!(
                        "multiple format files found for '{stem}'"
                    )));
                }
                found = Some(Located { path, format: fmt });
            }
        }
        Ok(found)
    }

    /// Locates `id`'s document file, if any.
    ///
    /// # Errors
    /// [`Error::Conflict`] if more than one format file exists for `id`.
    pub fn locate(&self, id: DocId) -> Result<Option<Located>> {
        self.locate_stem(&id.base_name())
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

    fn write_atomic(&self, path: &std::path::Path, bytes: &[u8]) -> Result<()> {
        fs::create_dir_all(&self.root)?;
        let file_name = path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| Error::Other(anyhow::anyhow!("path has no file name: {path:?}")))?;
        let tmp_path = self
            .root
            .join(format!(".{file_name}.tmp.{}", std::process::id()));
        fs::write(&tmp_path, bytes)?;
        fs::rename(&tmp_path, path)?;
        Ok(())
    }

    fn read_document(&self, id: DocId, located: &Located) -> Result<Document> {
        let bytes = fs::read(&located.path)?;
        let raw = String::from_utf8(bytes.clone())
            .map_err(|e| Error::Other(anyhow::anyhow!("{}: invalid utf-8: {e}", located.path.display())))?;
        let value = format::parse(located.format, &raw)?;
        let dig = digest::digest(&bytes);
        let mtime = fs::metadata(&located.path)?.modified()?;
        Ok(Document {
            id,
            format: located.format,
            path: located.path.clone(),
            raw,
            value,
            digest: dig,
            mtime,
        })
    }

    /// Reads `id`'s document.
    ///
    /// # Errors
    /// [`Error::NotFound`] if it does not exist; [`Error::Conflict`] if more
    /// than one format file exists.
    pub fn read(&self, id: DocId) -> Result<Document> {
        let located = self.locate(id)?.ok_or(Error::NotFound(id))?;
        self.read_document(id, &located)
    }

    fn check_digest(bytes: &[u8], expected: Option<&str>) -> Result<()> {
        if let Some(expected) = expected {
            let actual = digest::digest(bytes);
            if actual != expected {
                return Err(Error::DigestMismatch {
                    expected: expected.to_string(),
                    actual,
                });
            }
        }
        Ok(())
    }

    /// Applies `patch` (merge-patch semantics) to `id`'s document, creating
    /// it (in [`MetaStore::default_format`]) if it does not exist and the
    /// patch has no top-level deletes.
    ///
    /// # Errors
    /// [`Error::Lint`] if `patch` itself is invalid; [`Error::DigestMismatch`]
    /// if `expected_digest` is given and does not match; [`Error::NotFound`]
    /// if the document does not exist and the patch tries to delete a
    /// top-level key; [`Error::TooLarge`] if the result exceeds
    /// [`MAX_BYTES`].
    pub fn patch(&self, id: DocId, patch_value: &Value, expected_digest: Option<&str>) -> Result<Document> {
        let lints = patch::lint_patch(patch_value);
        if !lints.is_empty() {
            return Err(Error::Lint(lints));
        }
        match self.locate(id)? {
            Some(located) => {
                let bytes = fs::read(&located.path)?;
                Self::check_digest(&bytes, expected_digest)?;
                let raw = String::from_utf8(bytes)
                    .map_err(|e| Error::Other(anyhow::anyhow!("invalid utf-8: {e}")))?;
                let result = edit::apply_patch_text(located.format, &raw, patch_value)?;
                Self::check_size(result.text.len() as u64)?;
                self.write_atomic(&located.path, result.text.as_bytes())?;
                let dig = digest::digest(result.text.as_bytes());
                let mtime = fs::metadata(&located.path)?.modified()?;
                Ok(Document {
                    id,
                    format: located.format,
                    path: located.path,
                    raw: result.text,
                    value: result.value,
                    digest: dig,
                    mtime,
                })
            }
            None => {
                if has_top_level_delete(patch_value) {
                    return Err(Error::NotFound(id));
                }
                if let Some(expected) = expected_digest {
                    return Err(Error::DigestMismatch {
                        expected: expected.to_string(),
                        actual: String::new(),
                    });
                }
                let fmt = self.default_format()?;
                let empty = Value::Object(serde_json::Map::new());
                let empty_text = format::dump(fmt, &empty);
                let result = edit::apply_patch_text(fmt, &empty_text, patch_value)?;
                Self::check_size(result.text.len() as u64)?;
                let path = self.path_for(id, fmt);
                self.write_atomic(&path, result.text.as_bytes())?;
                let dig = digest::digest(result.text.as_bytes());
                let mtime = fs::metadata(&path)?.modified()?;
                Ok(Document {
                    id,
                    format: fmt,
                    path,
                    raw: result.text,
                    value: result.value,
                    digest: dig,
                    mtime,
                })
            }
        }
    }

    /// Replaces `id`'s document with `text` verbatim (only normalized to end
    /// with a single newline). `format` may switch the document's extension
    /// (the new file is written before the old one is removed).
    ///
    /// # Errors
    /// [`Error::Parse`] / [`Error::Lint`] if `text` does not parse as a valid
    /// document; [`Error::DigestMismatch`] if `expected_digest` is given and
    /// does not match; [`Error::TooLarge`] if `text` exceeds [`MAX_BYTES`].
    pub fn put_raw(
        &self,
        id: DocId,
        text: &str,
        format_override: Option<Format>,
        expected_digest: Option<&str>,
    ) -> Result<PutResult> {
        let located = self.locate(id)?;
        let (old_value, old_path, target_format) = match &located {
            Some(l) => {
                let bytes = fs::read(&l.path)?;
                Self::check_digest(&bytes, expected_digest)?;
                let old_raw = String::from_utf8(bytes)
                    .map_err(|e| Error::Other(anyhow::anyhow!("invalid utf-8: {e}")))?;
                let old_value = format::parse(l.format, &old_raw)?;
                (old_value, Some(l.path.clone()), format_override.unwrap_or(l.format))
            }
            None => {
                if let Some(expected) = expected_digest {
                    return Err(Error::DigestMismatch {
                        expected: expected.to_string(),
                        actual: String::new(),
                    });
                }
                (
                    Value::Object(serde_json::Map::new()),
                    None,
                    format_override.unwrap_or(self.default_format()?),
                )
            }
        };

        let normalized = normalize_trailing_newline(text);
        Self::check_size(normalized.len() as u64)?;
        let new_value = format::parse(target_format, &normalized)?;

        let new_path = self.path_for(id, target_format);
        self.write_atomic(&new_path, normalized.as_bytes())?;
        if let Some(op) = &old_path {
            if *op != new_path {
                fs::remove_file(op)?;
            }
        }

        let touched = patch::diff(&old_value, &new_value);
        let dig = digest::digest(normalized.as_bytes());
        let mtime = fs::metadata(&new_path)?.modified()?;
        Ok(PutResult {
            document: Document {
                id,
                format: target_format,
                path: new_path,
                raw: normalized,
                value: new_value,
                digest: dig,
                mtime,
            },
            touched,
        })
    }

    /// Re-dumps `id`'s document in `to`'s canonical form and switches its
    /// extension. Free-form comments in the old text are lost; comment keys
    /// survive (they are ordinary keys in the document model).
    ///
    /// # Errors
    /// [`Error::NotFound`], [`Error::DigestMismatch`], [`Error::TooLarge`].
    pub fn convert(&self, id: DocId, to: Format, expected_digest: Option<&str>) -> Result<Document> {
        let located = self.locate(id)?.ok_or(Error::NotFound(id))?;
        let bytes = fs::read(&located.path)?;
        Self::check_digest(&bytes, expected_digest)?;
        let raw = String::from_utf8(bytes)
            .map_err(|e| Error::Other(anyhow::anyhow!("invalid utf-8: {e}")))?;
        let value = format::parse(located.format, &raw)?;
        let text = format::dump(to, &value);
        Self::check_size(text.len() as u64)?;
        let new_path = self.path_for(id, to);
        self.write_atomic(&new_path, text.as_bytes())?;
        if new_path != located.path {
            fs::remove_file(&located.path)?;
        }
        let dig = digest::digest(text.as_bytes());
        let mtime = fs::metadata(&new_path)?.modified()?;
        Ok(Document {
            id,
            format: to,
            path: new_path,
            raw: text,
            value,
            digest: dig,
            mtime,
        })
    }

    /// Deletes `id`'s document. For a guest, also deletes all of its
    /// snapshots.
    ///
    /// # Errors
    /// [`Error::NotFound`] if it does not exist.
    pub fn delete(&self, id: DocId) -> Result<()> {
        let located = self.locate(id)?.ok_or(Error::NotFound(id))?;
        fs::remove_file(&located.path)?;
        if let DocId::Guest(vmid) = id {
            for name in self.list_snapshots(vmid)? {
                self.delete_snapshot(vmid, &name)?;
            }
        }
        Ok(())
    }

    /// Lists all guest documents, sorted by vmid. Ignores snapshot files,
    /// the datacenter document, and in-progress temp files.
    pub fn list_guests(&self) -> Result<Vec<GuestEntry>> {
        let mut out = Vec::new();
        if !self.root.is_dir() {
            return Ok(out);
        }
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || !entry.file_type()?.is_file() {
                continue;
            }
            let Some((stem, ext)) = name.rsplit_once('.') else {
                continue;
            };
            let Some(fmt) = Format::from_ext(ext) else {
                continue;
            };
            // Snapshot files ("<vmid>.<name>") and "datacenter" both fail to
            // parse as a bare vmid, so this filters them out for free.
            let Ok(vmid) = stem.parse::<u32>() else {
                continue;
            };
            let meta = entry.metadata()?;
            let bytes = fs::read(entry.path())?;
            out.push(GuestEntry {
                vmid,
                format: fmt,
                digest: digest::digest(&bytes),
                mtime: meta.modified()?,
                size: meta.len(),
            });
        }
        out.sort_by_key(|g| g.vmid);
        Ok(out)
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
            if Format::from_ext(ext).is_none() || !is_valid_snapshot_name(snap_name) {
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
        let Some(located) = self.locate(DocId::Guest(vmid))? else {
            return Ok(false);
        };
        let bytes = fs::read(&located.path)?;
        let snap_path = self.root.join(format!("{vmid}.{name}.{}", located.format.ext()));
        for fmt in Format::ALL {
            let p = self.root.join(format!("{vmid}.{name}.{}", fmt.ext()));
            if p != snap_path && p.exists() {
                fs::remove_file(&p)?;
            }
        }
        self.write_atomic(&snap_path, &bytes)?;
        Ok(true)
    }

    /// Rolls `vmid` back to snapshot `name`. See [`RollbackOutcome`] for the
    /// possible outcomes.
    ///
    /// # Errors
    /// [`Error::InvalidName`] if `name` is not a valid snapshot name;
    /// [`Error::Conflict`] if more than one format file exists for the
    /// snapshot.
    pub fn rollback(&self, vmid: u32, name: &str) -> Result<RollbackOutcome> {
        if !is_valid_snapshot_name(name) {
            return Err(Error::InvalidName(name.to_string()));
        }
        let snap = self.locate_stem(&format!("{vmid}.{name}"))?;
        let live = self.locate(DocId::Guest(vmid))?;
        match snap {
            Some(s) => {
                let bytes = fs::read(&s.path)?;
                let target = self.path_for(DocId::Guest(vmid), s.format);
                if let Some(l) = &live {
                    if l.path != target {
                        fs::remove_file(&l.path)?;
                    }
                }
                self.write_atomic(&target, &bytes)?;
                Ok(RollbackOutcome::Restored)
            }
            None => match live {
                Some(l) => {
                    fs::remove_file(&l.path)?;
                    Ok(RollbackOutcome::RemovedNoSnapshot)
                }
                None => Ok(RollbackOutcome::NoOp),
            },
        }
    }

    /// Deletes a guest's snapshot. Idempotent: does nothing if it does not
    /// exist.
    ///
    /// # Errors
    /// [`Error::InvalidName`] if `name` is not a valid snapshot name;
    /// [`Error::Conflict`] if more than one format file exists for it.
    pub fn delete_snapshot(&self, vmid: u32, name: &str) -> Result<()> {
        if !is_valid_snapshot_name(name) {
            return Err(Error::InvalidName(name.to_string()));
        }
        if let Some(s) = self.locate_stem(&format!("{vmid}.{name}"))? {
            fs::remove_file(&s.path)?;
        }
        Ok(())
    }

    /// Clones `vmid`'s document (only) to `newid`. Copies no snapshots.
    ///
    /// # Errors
    /// [`Error::NotFound`] if `vmid` has no document; [`Error::Conflict`] if
    /// `newid` already has one.
    pub fn clone(&self, vmid: u32, newid: u32) -> Result<Document> {
        let src = self
            .locate(DocId::Guest(vmid))?
            .ok_or(Error::NotFound(DocId::Guest(vmid)))?;
        if self.locate(DocId::Guest(newid))?.is_some() {
            return Err(Error::Conflict(format!(
                "guest {newid} already has a document"
            )));
        }
        let bytes = fs::read(&src.path)?;
        let new_path = self.path_for(DocId::Guest(newid), src.format);
        self.write_atomic(&new_path, &bytes)?;
        self.read_document(DocId::Guest(newid), &Located { path: new_path, format: src.format })
    }

    /// Deletes `vmid`'s document (and its snapshots). Equivalent to
    /// `delete(DocId::Guest(vmid))`.
    pub fn destroy(&self, vmid: u32) -> Result<()> {
        self.delete(DocId::Guest(vmid))
    }

    /// The default format for newly created documents: `settings.default_format`
    /// from the datacenter document if present and valid, else [`Format::Yaml`].
    pub fn default_format(&self) -> Result<Format> {
        match self.read(DocId::Datacenter) {
            Ok(doc) => {
                let fmt = doc
                    .value
                    .get("settings")
                    .and_then(|s| s.get("default_format"))
                    .and_then(|v| v.as_str())
                    .and_then(Format::from_ext);
                Ok(fmt.unwrap_or(Format::Yaml))
            }
            Err(Error::NotFound(_)) => Ok(Format::Yaml),
            Err(e) => Err(e),
        }
    }

    /// A cheap summary of the whole store's content, suitable for polling:
    /// the token changes whenever any file's content changes, and is stable
    /// otherwise (including across mere reads).
    ///
    /// The token is a SHA-256 over the sorted list of `(file name, content
    /// digest)`. Per-file digests are cached in memory, keyed by path and
    /// valid only when the cached `(mtime, len)` matches *and* the file's
    /// mtime is more than two seconds old — recently-modified files are
    /// always re-hashed from their actual bytes. This protects against
    /// pmxcfs's one-second mtime granularity, under which two different
    /// writes within the same second could otherwise be indistinguishable by
    /// `(mtime, len)` alone. Files are tiny, so re-hashing is cheap.
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
                let path = entry.path();
                let meta = entry.metadata()?;
                let mtime = meta.modified()?;
                let len = meta.len();
                let dig = self.digest_cached(&path, mtime, len)?;
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

    fn digest_cached(&self, path: &std::path::Path, mtime: SystemTime, len: u64) -> Result<String> {
        let stale_enough = SystemTime::now()
            .duration_since(mtime)
            .map(|age| age >= Duration::from_secs(2))
            .unwrap_or(false);
        if stale_enough {
            let cache = self.version_cache.lock().unwrap();
            if let Some((cached_mtime, cached_len, dig)) = cache.get(path) {
                if *cached_mtime == mtime && *cached_len == len {
                    return Ok(dig.clone());
                }
            }
        }
        let bytes = fs::read(path)?;
        let dig = digest::digest(&bytes);
        let mut cache = self.version_cache.lock().unwrap();
        cache.insert(path.to_path_buf(), (mtime, len, dig.clone()));
        Ok(dig)
    }
}

fn has_top_level_delete(patch: &Value) -> bool {
    patch
        .as_object()
        .is_some_and(|m| m.values().any(Value::is_null))
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
        assert!(!is_valid_snapshot_name("toml"));
        assert!(!is_valid_snapshot_name("json"));
        assert!(!is_valid_snapshot_name("yml"));
    }

    #[test]
    fn has_top_level_delete_detects_null() {
        assert!(has_top_level_delete(&serde_json::json!({"a": null})));
        assert!(!has_top_level_delete(&serde_json::json!({"a": 1})));
        assert!(!has_top_level_delete(&serde_json::json!({"a": {"b": null}})));
    }

    #[test]
    fn normalize_trailing_newline_collapses_multiple() {
        assert_eq!(normalize_trailing_newline("a: 1"), "a: 1\n");
        assert_eq!(normalize_trailing_newline("a: 1\n\n\n"), "a: 1\n");
        assert_eq!(normalize_trailing_newline("a: 1\n"), "a: 1\n");
    }
}
