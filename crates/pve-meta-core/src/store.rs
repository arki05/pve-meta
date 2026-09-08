//! The on-disk file store: atomic reads/writes of guest and datacenter
//! documents, snapshots, and change-version polling.
//!
//! Root is `/etc/pve/meta` in production, a tempdir in tests. Documents are
//! always YAML (`docs/DESIGN.md` §2): `<vmid>.yaml`, `datacenter.yaml`, and
//! `<vmid>.<snapname>.yaml` for a guest's snapshot copies. All writes are
//! atomic (write a hidden, node- and call-unique sibling, then `rename`),
//! which pmxcfs supports.
//!
//! Serialising concurrent writers is *not* this layer's job: the API write
//! handlers run the whole read-check-write cycle under
//! `PVE::Cluster::cfs_lock_domain` (`docs/DESIGN.md` §4), and the digest
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
///
/// This is a **backstop, not the operative limit** for an API write: pveproxy
/// rejects a request body of roughly this size before the request ever
/// reaches us (measured on PVE 8: 520 000 bytes through, 530 000 bytes
/// answered "for data too large", HTTP 501). It is the operative limit for a
/// hand-written or replicated file being rewritten, and it is what keeps a
/// single document from eating the pmxcfs size budget.
pub const MAX_BYTES: u64 = 512 * 1024;

/// Hard limit for what [`MetaStore::read`] will read off the disk at all.
///
/// [`MAX_BYTES`] only ever applied to *writes*, so a multi-megabyte file
/// dropped into `/etc/pve/meta` out of band (a bad rsync, a replicated file
/// from a future version, a mistake) was read and SHA-256'd on every request
/// that touched it — including `api::grants`, which reads `datacenter.yaml`
/// on every request that touched it.
///
/// It is deliberately eight times [`MAX_BYTES`]: nothing this store writes
/// can ever reach it, so hitting it always means the file arrived out of
/// band, and the slack means a document that was legally written can always
/// still be read back (and therefore repaired) even if the write limit is
/// lowered later.
pub const MAX_READ_BYTES: u64 = 4 * 1024 * 1024;

/// The one on-disk format (`docs/DESIGN.md` §2: YAML on disk).
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
    /// The parsed value, or the **empty document** when [`Document::parse_error`]
    /// is set. **Unstripped**: comment keys are still present; stripping them
    /// is the caller's (API layer's) job via [`crate::model::strip_comments`].
    pub value: Value,
    /// `Some(message)` when the file's text is not valid YAML at all, in
    /// which case [`Document::value`] is the empty document
    /// (`docs/DESIGN.md` §4).
    ///
    /// A *syntax* error used to propagate out of [`MetaStore::read`], which
    /// made a single tab or indentation slip in a hand-edited
    /// `datacenter.yaml` a cluster-wide 400 for every principal, root
    /// included — and unrepairable through the API, because every write
    /// reads the document before planning. The file is still on disk, still
    /// carries its real [`Document::digest`], and can still be replaced;
    /// callers decide what to do with a document they cannot parse. The
    /// digest is over the file's actual bytes either way, so the
    /// compare-and-swap precondition of a repairing write is unaffected.
    pub parse_error: Option<String>,
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

    /// Reads and parses one document file. **Reads never lint, and never
    /// fail on the document's own content** (`docs/DESIGN.md` §4).
    ///
    /// Every API write is already lint-gated, so invalid content can only
    /// arrive out of band (a hand-edited `/etc/pve/meta/*.yaml`, a restored
    /// backup, pmxcfs replication). Linting on the way *in* made one bad key
    /// anywhere in `datacenter.yaml` a cluster-wide outage — `api::grants`
    /// reads that document on every guest request — and, worse, blocked the
    /// administrator's own repair, since they could neither read the document
    /// to see the problem nor write over it. Strict validation belongs to the
    /// content being written, and lives in [`MetaStore::put_raw`]'s parse of
    /// the *new* text.
    ///
    /// Pass 2 moved the *lint* off this path and left the *parse* fatal,
    /// which is the same outage one layer down: a tab, an indentation slip,
    /// an anchor or an explicit tag still 400'd every endpoint for everyone.
    /// A syntax error is now reported per document, in
    /// [`Document::parse_error`], with the empty document as the value — the
    /// caller decides (`api::grants` grants nothing and warns; a read answers
    /// with `parse_error` and no data; a full-write caller may replace the
    /// whole document to repair it).
    ///
    /// # Errors
    /// [`Error::TooLarge`] if the file exceeds [`MAX_READ_BYTES`], and I/O
    /// errors. Never [`Error::Parse`] or [`Error::Lint`].
    fn read_document(&self, id: DocId, path: &std::path::Path) -> Result<Document> {
        let meta = fs::metadata(path)?;
        // Checked from the metadata, before the bytes are read: the point is
        // not to pull a multi-megabyte file into memory (and hash it) on
        // every request that touches this document.
        Self::check_read_size(meta.len())?;
        let mtime = meta.modified()?;
        let bytes = fs::read(path)?;
        let raw = String::from_utf8(bytes.clone())
            .map_err(|e| Error::Other(anyhow::anyhow!("{}: invalid utf-8: {e}", path.display())))?;
        let (value, parse_error) = match format::parse_raw(DISK_FORMAT, &raw) {
            Ok(value) => (value, None),
            Err(e) => {
                tracing::warn!(document = %id, error = %e, "stored document is not valid YAML; reading it as empty");
                (Value::Object(serde_json::Map::new()), Some(e.to_string()))
            }
        };
        let dig = digest::digest(&bytes);
        Ok(Document {
            id,
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

    /// Reads `id`'s document. **Reads never lint and never fail on the
    /// document's own content**: text the YAML parser rejects is reported in
    /// [`Document::parse_error`], with the empty document as the value
    /// (`docs/DESIGN.md` §4).
    ///
    /// # Errors
    /// [`Error::NotFound`] if it does not exist; [`Error::TooLarge`] if the
    /// file is bigger than [`MAX_READ_BYTES`].
    pub fn read(&self, id: DocId) -> Result<Document> {
        let path = self.locate(id)?.ok_or(Error::NotFound(id))?;
        self.read_document(id, &path)
    }

    /// `id`'s current content digest without parsing (or even keeping) the
    /// document, or `None` if it does not exist.
    ///
    /// This is what lets a write repair a document [`MetaStore::read`]
    /// refuses to read — one above [`MAX_READ_BYTES`] — while still carrying
    /// a compare-and-swap precondition: the caller needs the digest, and the
    /// digest is the one thing about such a file that is cheap and safe to
    /// compute.
    pub fn digest_of(&self, id: DocId) -> Result<Option<String>> {
        match fs::read(self.path_for(id)) {
            Ok(bytes) => Ok(Some(digest::digest(&bytes))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// The digest precondition, enforced in exactly one place: `None`
    /// means "no precondition";
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
    /// (`docs/DESIGN.md` §4) without a second implementation
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
    /// The *new* text is parsed and linted: nothing this store writes can
    /// ever fail [`crate::model::lint`]. There is one gate, for every
    /// caller — the privilege-narrowed variants revision 4 grew are gone
    /// with the tower that needed them (`docs/DESIGN.md` §10).
    ///
    /// # Errors
    /// [`Error::Parse`] / [`Error::Lint`] if `text` does not parse as a valid
    /// document; [`Error::DigestMismatch`] if `expected_digest` is given and
    /// does not match (`Some("")` matches a missing document);
    /// [`Error::TooLarge`] if `text` exceeds [`MAX_BYTES`].
    pub fn put_raw(
        &self,
        id: DocId,
        text: &str,
        expected_digest: Option<&str>,
    ) -> Result<PutResult> {
        let path = self.path_for(id);
        let existing = match fs::read(&path) {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        Self::check_digest(existing.as_deref(), expected_digest)?;

        // The *old* content is only read to diff against, so it is parsed
        // leniently: an out-of-band edit that broke it must not stop an
        // administrator from writing the repair (`docs/DESIGN.md` §4).
        // That includes a *syntax* error: it is the last thing that would
        // otherwise stand between a hand-edited document and its repair.
        // Unparseable old content diffs as the empty
        // document: the repair reports everything it writes as newly set,
        // which is exactly true of a document that had no readable structure.
        let old_value = match &existing {
            // Above the read cap the old content is not parsed at all: the
            // point of the cap is that nothing pulls a multi-megabyte
            // document through the YAML parser, and this is the path that
            // replaces such a file.
            Some(bytes) if bytes.len() as u64 <= MAX_READ_BYTES => {
                let old_raw = String::from_utf8(bytes.clone())
                    .map_err(|e| Error::Other(anyhow::anyhow!("invalid utf-8: {e}")))?;
                format::parse_raw(DISK_FORMAT, &old_raw)
                    .unwrap_or_else(|_| Value::Object(serde_json::Map::new()))
            }
            _ => Value::Object(serde_json::Map::new()),
        };

        let normalized = normalize_trailing_newline(text);
        Self::check_size(normalized.len() as u64)?;
        // The write-time gate: the content being stored parses and passes
        // the lint.
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
                // It was just parsed, on the way in.
                parse_error: None,
                digest: dig,
                mtime,
            },
            touched,
        })
    }

    /// Deletes `id`'s document — **only** the current document. Snapshot
    /// copies are owned by the snapshot hooks (`docs/DESIGN.md` §6) and are
    /// removed by [`MetaStore::purge`], never by this.
    ///
    /// # Errors
    /// [`Error::NotFound`] if it does not exist.
    pub fn delete(&self, id: DocId) -> Result<()> {
        let path = self.locate(id)?.ok_or(Error::NotFound(id))?;
        fs::remove_file(&path)?;
        Ok(())
    }

    /// Every vmid the store holds *any* file for — a live document, a
    /// snapshot copy, or both — sorted ascending. The datacenter document and
    /// temp files are not guests.
    ///
    /// This is the store's half of the GC (`docs/DESIGN.md` §6): Perl passes
    /// the vmlist, and every vmid here that is not in it is removed together
    /// with its snapshot copies. There is no orphan concept in the API.
    pub fn stored_vmids(&self) -> Result<Vec<u32>> {
        let mut out = std::collections::BTreeSet::new();
        if !self.root.is_dir() {
            return Ok(Vec::new());
        }
        let suffix = format!(".{}", DISK_FORMAT.ext());
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || !entry.file_type()?.is_file() {
                continue;
            }
            // `<vmid>.yaml` or `<vmid>.<snapname>.yaml`: the vmid is the
            // first dot-separated component either way.
            let Some(stem) = name.strip_suffix(&suffix) else {
                continue;
            };
            let (head, snap) = match stem.split_once('.') {
                Some((head, snap)) => (head, Some(snap)),
                None => (stem, None),
            };
            if snap.is_some_and(|s| !is_valid_snapshot_name(s)) {
                continue;
            }
            if let Ok(vmid) = head.parse::<u32>() {
                out.insert(vmid);
            }
        }
        Ok(out.into_iter().collect())
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
    /// gone. Used only by the GC (`docs/DESIGN.md` §6); the REST API's
    /// `DELETE` uses [`MetaStore::delete`], which never touches snapshots.
    ///
    /// Returns the number of files removed. Idempotent: a missing document is
    /// not an error.
    pub fn purge(&self, vmid: u32) -> Result<usize> {
        let mut removed = 0;
        if self.locate(DocId::Guest(vmid))?.is_some() {
            self.delete(DocId::Guest(vmid))?;
            removed += 1;
        }
        for name in self.list_snapshots(vmid)? {
            self.delete_snapshot(vmid, &name)?;
            removed += 1;
        }
        Ok(removed)
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
