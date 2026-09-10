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
//!
//! ## A file that is not there is never a 500
//!
//! Reads run unlocked, writes hold `pve-meta-<id>` and the GC holds
//! `pve-meta-gc` — three disjoint lock domains, so no reader is ever excluded
//! from a directory a `DELETE` or the GC is working on. Every operation here
//! is therefore written so that a file disappearing between two syscalls is
//! an ordinary outcome, never an `io::ErrorKind::NotFound` propagated as
//! [`Error::Io`] (which the API layer maps to HTTP 500): a read reports
//! [`Error::NotFound`], [`MetaStore::delete`] and [`MetaStore::delete_snapshot`]
//! are idempotent and say whether they removed anything, and the directory
//! walks ([`MetaStore::version`], [`MetaStore::stored_vmids`],
//! [`MetaStore::list_snapshots`]) skip an entry that vanished under them.
//! There is deliberately no `exists()`-then-act pair left in this file.

use std::fmt;
use std::collections::BTreeMap;
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
use crate::patch::{self, Touched};
pub use crate::registry::RegistryKind;
use crate::registry::Registry;

/// Warn threshold for document size (informational only; reported through
/// [`crate::warn`]).
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
/// that touched it.
///
/// It is deliberately eight times [`MAX_BYTES`]: nothing this store writes
/// can ever reach it, so hitting it always means the file arrived out of
/// band, and the slack means a document that was legally written can always
/// still be read back (and therefore repaired) even if the write limit is
/// lowered later.
///
/// The cap is applied by **every** path that would otherwise pull a file's
/// bytes into memory, not just [`MetaStore::read`]: [`MetaStore::digest_of`],
/// the compare-and-swap precondition and [`MetaStore::version`] all go
/// through [`identify`], which substitutes a surrogate identity for a file
/// above the cap rather than hashing megabytes on every 5 s poll and every
/// listing.
pub const MAX_READ_BYTES: u64 = 4 * 1024 * 1024;

/// The one on-disk format (`docs/DESIGN.md` §2: YAML on disk).
pub const DISK_FORMAT: Format = Format::Yaml;

/// Identifies a top-level document in the store: a guest's metadata, the
/// datacenter's, or one registry file. Snapshots are addressed separately, by
/// `(vmid, name)`, via the dedicated snapshot methods.
///
/// A registry file is a document like any other **on purpose**: it is YAML, it
/// is read, replaced and deleted whole or by view, and it wants exactly the
/// machinery the other two already have -- the digest compare-and-swap, the
/// lint, the version poll, and in the editor the tree, the markers and the
/// diff. The alternative was a second write path beside the first, which is
/// the shape of every wrong-result bug this project has had.
///
/// Two things are *not* uniform, and both live outside this type: a registry
/// document is governed by ACLs alone (`api::effective` gives it no scopes -- a
/// permission file that could widen its own grants would be self-registration), and
/// its content must additionally parse as the kind it claims to be, which
/// `api::put_document` checks before writing.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DocId {
    /// A guest's metadata document, named `<vmid>.yaml`.
    Guest(u32),
    /// The datacenter's metadata document, named `datacenter.yaml`.
    Datacenter,
    /// A prefix or permission file, named `<name>.yaml` in its kind's directory.
    /// The name is a single path segment (`crate::path::is_valid_segment`), so
    /// it can never contain a slash or escape that directory.
    Registry(RegistryKind, String),
}

impl DocId {
    fn base_name(&self) -> String {
        match self {
            DocId::Guest(vmid) => vmid.to_string(),
            DocId::Datacenter => "datacenter".to_string(),
            DocId::Registry(_, name) => name.clone(),
        }
    }
}

/// The id as the API spells it and as `parse_id` reads it back: `105`,
/// `datacenter`, `prefixes/<name>`, `permissions/<name>`. The one string
/// form of a document id, used on the wire and in error messages alike.
impl fmt::Display for DocId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DocId::Guest(vmid) => write!(f, "{vmid}"),
            DocId::Datacenter => write!(f, "datacenter"),
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
    /// The parsed value, or the **empty document** when [`Document::parse_error`]
    /// is set. Comment keys (`foo__`) are ordinary data and stay in it: nothing
    /// strips them, and what a caller may see of one is decided further up by
    /// [`crate::view::filter`], where [`crate::scopes::covers`] makes a scope on
    /// `foo` cover `foo__` as well.
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
    /// Every **document** in the store with its own digest, sorted by id — the
    /// per-document half of the same walk the token is hashed from.
    ///
    /// Snapshot copies are deliberately absent: they are not addressable through
    /// the API, so a caller diffing this list has nothing to do about one. They
    /// still move [`Self::token`], which is the honest answer — something in the
    /// store changed, just nothing this caller can read.
    pub documents: Vec<(DocId, String)>,
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

/// `Ok(None)` for an `io::ErrorKind::NotFound`, `Ok(Some(v))` otherwise.
///
/// The one idiom for "the file was not there (any more)": every caller in
/// this module treats that as an outcome rather than an error, because a
/// concurrent `DELETE` or GC pass can remove a file between any two syscalls
/// (see the module docs).
fn gone_is_none<T>(r: io::Result<T>) -> Result<Option<T>> {
    match r {
        Ok(v) => Ok(Some(v)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// The identity of one file's content: the SHA-256 of its bytes, or — for a
/// file above [`MAX_READ_BYTES`] — a *surrogate* derived from its size and
/// mtime, computed without reading it. `Ok(None)` if the file is not there.
///
/// Everything that needs a file's identity goes through here:
/// [`MetaStore::digest_of`], the compare-and-swap precondition, and
/// [`MetaStore::version`]'s per-file entry. One function means the digest a
/// caller reads out of a `GET` is the same string the precondition of their
/// next `PUT` is compared against — including for a file the store refuses
/// to read, which is exactly the file that has to stay repairable.
///
/// The surrogate is only ever produced above the read cap, i.e. for a file
/// nothing in this store could have written (`MAX_BYTES` is an eighth of the
/// cap) and that pmxcfs itself cannot hold. Such a file's only legal write is
/// a whole-file replace or a `DELETE`, so the surrogate has one job: change
/// when the file changes. Two different oversized contents of identical
/// length written within the same mtime tick would collide; the consequence
/// is a compare-and-swap that accepts a replacement of unreadable content by
/// a document, which is what the caller asked for either way.
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

/// The surrogate identity of a file above [`MAX_READ_BYTES`]: a domain-separated
/// SHA-256 over `(len, mtime)`. Deliberately shaped like a real digest — it is
/// compared, never parsed.
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

/// A process-wide counter making [`MetaStore::write_atomic`]'s temp file name
/// unique per call (the hostname and pid alone are not: two writes from one
/// pvedaemon worker would otherwise collide, and pmxcfs shares one directory
/// across every node).
static WRITE_SEQ: AtomicU64 = AtomicU64::new(0);

/// The on-disk metadata store.
///
/// Two things, not one: `root` holds the guest and datacenter documents and
/// the snapshot copies, and `registry` holds the prefix and permission
/// drop-directory lists ([`DocId::Registry`]). Those lists are ordered lowest
/// precedence first, exactly as [`Registry`] loads them, so the **last**
/// entry of each is the one a write goes to -- the cluster directory, with
/// the packaged one below it staying read-only. Writing a prefix whose name a
/// packaged file already uses therefore creates the cluster override rather
/// than editing the package's file, and deleting it falls back to the
/// packaged one, which is the same rule the loader has always applied.
pub struct MetaStore {
    root: PathBuf,
    registry: Registry,
}

impl MetaStore {
    /// Opens a store rooted at `root` (created on first write; does not need
    /// to exist yet), with the registry directories [`Registry::from_env`]
    /// would load -- the packaged and cluster defaults, or whatever
    /// `PVE_META_PREFIX_DIRS`/`PVE_META_PERMISSION_DIRS` say.
    ///
    /// Reading those two variables here, once, is deliberate: the alternative
    /// is reading them again deeper in the call path, where a test (or a
    /// caller that changed them between constructing a store and using it)
    /// would get a store whose reads and writes disagree about where a file
    /// lives. Tests that want their own directories pass them explicitly with
    /// [`MetaStore::with_registry_dirs`].
    pub fn new(root: impl Into<PathBuf>) -> Self {
        MetaStore { root: root.into(), registry: Registry::from_env() }
    }

    /// [`MetaStore::new`] with the registry directories given explicitly,
    /// lowest precedence first. For tests, and for any caller that already
    /// knows its directories and does not want the environment consulted.
    pub fn with_registry_dirs(
        root: impl Into<PathBuf>,
        prefix_dirs: Vec<PathBuf>,
        permission_dirs: Vec<PathBuf>,
    ) -> Self {
        MetaStore::with_registry(root, Registry::new(prefix_dirs, permission_dirs))
    }

    /// [`MetaStore::new`] with a [`Registry`] given directly, for a caller
    /// that already built or was handed one and would otherwise have to take
    /// it apart into two `Vec<PathBuf>` just to satisfy
    /// [`MetaStore::with_registry_dirs`], which is defined in terms of this
    /// rather than the other way around.
    pub fn with_registry(root: impl Into<PathBuf>, registry: Registry) -> Self {
        MetaStore { root: root.into(), registry }
    }

    /// The [`Registry`] this store reads and writes registry documents
    /// through, for a caller that needs the same directory lists (loading the
    /// prefixes or permissions themselves, say) rather than a second,
    /// independently-read copy.
    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// The one directory of `kind` a write may land in: the highest-precedence
    /// one -- [`Registry::write_dir`]'s rule.
    ///
    /// An empty list -- `PVE_META_*_DIRS` set to nothing, or
    /// [`MetaStore::with_registry_dirs`] given none -- means no directory was
    /// configured, and falls back **inside `root`** rather than to the
    /// compiled-in `/etc/pve` default. A caller that went out of its way to
    /// have no registry directories (a test, a `--root` sandbox) must not have
    /// its writes land in the live cluster because of it. That fallback needs
    /// `root`, which `Registry` does not have, so it stays here.
    fn registry_write_dir(&self, kind: RegistryKind) -> PathBuf {
        self.registry
            .write_dir(kind)
            .unwrap_or_else(|| self.root.join("meta.d").join(kind.as_str()))
    }

    /// Where `id` is **written**, and where it is read from unless a
    /// lower-precedence registry directory is the only one that has it (see
    /// [`MetaStore::read_path_for`]).
    fn path_for(&self, id: &DocId) -> PathBuf {
        let file = format!("{}.{}", id.base_name(), DISK_FORMAT.ext());
        match id {
            DocId::Registry(kind, _) => self.registry_write_dir(*kind).join(file),
            _ => self.root.join(file),
        }
    }

    /// Where `id` is **read** from: the highest-precedence directory that
    /// actually has the file, which for a registry document is the same file
    /// `crate::registry::load_prefixes` would have picked. Falls back to
    /// [`MetaStore::path_for`] when none has it, so a "not found" error names
    /// the place a write would create it.
    fn read_path_for(&self, id: &DocId) -> PathBuf {
        let DocId::Registry(kind, _) = id else {
            return self.path_for(id);
        };
        let file = format!("{}.{}", id.base_name(), DISK_FORMAT.ext());
        for dir in self.registry.dirs(*kind).iter().rev() {
            let candidate = dir.join(&file);
            if candidate.is_file() {
                return candidate;
            }
        }
        self.path_for(id)
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

    /// Writes `bytes` to `path` atomically: a hidden sibling, then `rename`.
    ///
    /// The temp name carries the node's hostname, the pid *and* a per-call
    /// counter, because `/etc/pve/meta` is one pmxcfs directory shared by
    /// every node in the cluster: `.<name>.tmp.<host>.<pid>.<seq>`.
    ///
    /// The sibling is created in **`path`'s own directory**, not in `root`:
    /// `rename` is only atomic within one filesystem, and a registry document
    /// lives in `/etc/pve/meta.d/...`, a directory away from the documents.
    /// Writing the temp file next to its target keeps the rename atomic
    /// wherever the target is, and keeps a failed write's leftovers in the
    /// directory an administrator would look in.
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

    /// Reads and parses one document file. **Reads never lint, and never
    /// fail on the document's own content** (`docs/DESIGN.md` §4).
    ///
    /// Every API write is already lint-gated, so invalid content can only
    /// arrive out of band (a hand-edited `/etc/pve/meta/*.yaml`, a restored
    /// backup, pmxcfs replication). Linting on the way *in* made one bad key
    /// anywhere in `datacenter.yaml` a cluster-wide outage — permissions lived in
    /// that document then, so every guest request read it — and, worse, blocked the
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
    /// caller decides (`api::effective` grants nothing and warns; a read answers
    /// with `parse_error` and no data; a full-write caller may replace the
    /// whole document to repair it).
    ///
    /// # Errors
    /// [`Error::NotFound`] if the file is not there — including the case
    /// where it vanished between this function's own `stat` and its read,
    /// which is a racing `DELETE` or GC pass and must answer 404, not 500
    /// (see the module docs). [`Error::TooLarge`] if the file exceeds
    /// [`MAX_READ_BYTES`], and I/O errors. [`Error::Parse`] only for bytes
    /// that are not UTF-8 at all — a YAML syntax error is
    /// [`Document::parse_error`], not an error. Never [`Error::Lint`].
    fn read_document(&self, id: &DocId, path: &std::path::Path) -> Result<Document> {
        let Some(meta) = gone_is_none(fs::metadata(path))? else {
            return Err(Error::NotFound(id.clone()));
        };
        // Checked from the metadata, before the bytes are read: the point is
        // not to pull a multi-megabyte file into memory (and hash it) on
        // every request that touches this document.
        Self::check_read_size(meta.len())?;
        let mtime = meta.modified()?;
        let Some(bytes) = gone_is_none(fs::read(path))? else {
            return Err(Error::NotFound(id.clone()));
        };
        // Bytes that are not text have no `raw` to report and nothing to
        // parse, so they are the one read failure that is not a per-document
        // `parse_error`: [`Error::Parse`] (a 400/repairable condition), never
        // [`Error::Other`] (a 500 on every GET of the document, and an
        // unrepairable one, since every write reads before it plans).
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

    /// Reads `id`'s document. **Reads never lint and never fail on the
    /// document's own content**: text the YAML parser rejects is reported in
    /// [`Document::parse_error`], with the empty document as the value
    /// (`docs/DESIGN.md` §4).
    ///
    /// # Errors
    /// [`Error::NotFound`] if it does not exist (or ceases to, mid-read);
    /// [`Error::TooLarge`] if the file is bigger than [`MAX_READ_BYTES`].
    pub fn read(&self, id: &DocId) -> Result<Document> {
        // Deliberately no `locate` first: an `is_file()` followed by a read
        // is a check-then-act pair, and the racing loser of that pair used to
        // surface as an `Error::Io` (HTTP 500) instead of a 404.
        self.read_document(id, &self.read_path_for(id))
    }

    /// `id`'s current content identity without parsing (or even keeping) the
    /// document, or `None` if it does not exist. See [`identify`].
    ///
    /// This is what lets a write repair a document [`MetaStore::read`]
    /// refuses to read — one above [`MAX_READ_BYTES`] — while still carrying
    /// a compare-and-swap precondition: the caller needs the digest, and the
    /// digest is the one thing about such a file that is cheap and safe to
    /// compute. Above the read cap it is a surrogate over `(len, mtime)`, so
    /// this never reads a file the store refuses to read.
    pub fn digest_of(&self, id: &DocId) -> Result<Option<String>> {
        identify(&self.read_path_for(id))
    }

    /// The digest precondition, enforced in exactly one place: `None`
    /// means "no precondition";
    /// `Some("")` matches a *missing* document (that is the digest
    /// `GET` reports for one, `docs/DESIGN.md` §5, so the documented
    /// GET-then-PUT create flow works); any other `Some(_)` must equal the
    /// current file's digest.
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

    /// Checks the compare-and-swap precondition for `id` without writing
    /// anything — the same rule [`MetaStore::put_raw`] enforces, exposed so a
    /// `dry_run` can validate exactly what the real write validates
    /// (`docs/DESIGN.md` §4) without a second implementation
    /// of the rule living in the API layer.
    ///
    /// # Errors
    /// [`Error::DigestMismatch`].
    pub fn check_precondition(&self, id: &DocId, expected: Option<&str>) -> Result<()> {
        if expected.is_none() {
            return Ok(());
        }
        Self::check_digest(self.digest_of(id)?.as_deref(), expected)
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
        id: &DocId,
        text: &str,
        expected_digest: Option<&str>,
    ) -> Result<PutResult> {
        // Written where a write goes; compared and diffed against where a read
        // comes from. For a guest or the datacenter those are the same file.
        // For a registry document shadowing a packaged one they are not: the
        // precondition is checked against the content the caller actually saw
        // (the packaged file), and the write creates the cluster override.
        let path = self.path_for(id);
        let read_path = self.read_path_for(id);
        Self::check_digest(self.digest_of(id)?.as_deref(), expected_digest)?;

        // The *old* content is only read to diff against, so it is parsed
        // leniently: an out-of-band edit that broke it must not stop an
        // administrator from writing the repair (`docs/DESIGN.md` §4).
        // That includes a *syntax* error: it is the last thing that would
        // otherwise stand between a hand-edited document and its repair.
        // Unparseable old content — and content that vanished under us, and
        // content above the read cap, which is never pulled through the YAML
        // parser — diffs as the empty document: the repair reports everything
        // it writes as newly set, which is exactly true of a document that had
        // no readable structure.
        let old_value = match self.read_document(id, &read_path) {
            Ok(doc) => doc.value,
            Err(Error::NotFound(_) | Error::TooLarge { .. } | Error::Parse { .. }) => {
                Value::Object(serde_json::Map::new())
            }
            Err(e) => return Err(e),
        };

        let normalized = normalize_trailing_newline(text);
        Self::check_size(normalized.len() as u64)?;
        // The write-time gate: the content being stored parses and passes
        // the lint.
        let new_value = format::parse(DISK_FORMAT, &normalized)?;

        self.write_atomic(&path, normalized.as_bytes())?;

        let touched = patch::diff(&old_value, &new_value);
        let dig = digest::digest(normalized.as_bytes());
        // The file we just wrote can already be gone again (a racing DELETE
        // or GC pass): report the write's own moment rather than 500-ing on
        // a `stat` of something that is no longer there.
        let mtime = match gone_is_none(fs::metadata(&path))? {
            Some(meta) => meta.modified()?,
            None => SystemTime::now(),
        };
        Ok(PutResult {
            document: Document {
                id: id.clone(),
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
    /// **Idempotent**: returns `Ok(false)` if there was nothing to remove.
    /// Deleting a document is a request for it to be gone, and it being
    /// already gone — because a concurrent `DELETE` or the GC won the race —
    /// is that request satisfied, not a failure. The old `locate`-then-remove
    /// pair turned the loser of that race into an
    /// `io::ErrorKind::NotFound` → [`Error::Io`] → HTTP 500.
    /// A registry document is removed from the **write** directory only: a
    /// packaged prefix belongs to its `.deb`, and deleting the cluster file
    /// that shadowed it is a revert to the packaged one, not a removal. When
    /// only the packaged file exists there is nothing of ours to remove and
    /// this returns `Ok(false)`, with the prefix still there afterwards --
    /// `api::delete_document` says so rather than reporting a deletion that
    /// did not happen.
    pub fn delete(&self, id: &DocId) -> Result<bool> {
        Ok(gone_is_none(fs::remove_file(self.path_for(id)))?.is_some())
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
            // An entry that vanished between the `readdir` and the `stat` is
            // simply not there to collect.
            let Some(file_type) = gone_is_none(entry.file_type())? else {
                continue;
            };
            if name.starts_with('.') || !file_type.is_file() {
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
        let Some(bytes) = gone_is_none(fs::read(self.path_for(&DocId::Guest(vmid))))? else {
            return Ok(false);
        };
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

    /// Deletes a guest's snapshot. Idempotent: returns `Ok(false)` if there
    /// was nothing to remove.
    ///
    /// # Errors
    /// [`Error::InvalidName`] if `name` is not a valid snapshot name.
    pub fn delete_snapshot(&self, vmid: u32, name: &str) -> Result<bool> {
        if !is_valid_snapshot_name(name) {
            return Err(Error::InvalidName(name.to_string()));
        }
        Ok(gone_is_none(fs::remove_file(self.snapshot_path(vmid, name)))?.is_some())
    }

    /// Removes `vmid`'s document **and every snapshot copy** — the guest is
    /// gone. Used only by the GC (`docs/DESIGN.md` §6); the REST API's
    /// `DELETE` uses [`MetaStore::delete`], which never touches snapshots.
    ///
    /// Returns the number of files removed. Idempotent: a missing document is
    /// not an error.
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

    /// A summary of the whole store's content, suitable for polling: the
    /// token changes whenever any file's content changes, and is stable
    /// otherwise (including across mere reads).
    ///
    /// The token is a SHA-256 over the sorted list of `(file name, content
    /// identity)`, hashed from the files' actual bytes on every call. There
    /// is deliberately no `(mtime, len)` cache: the bindings build a fresh
    /// `MetaStore` per request (so it could never hit), and pmxcfs's mtime
    /// granularity cannot distinguish two same-length writes within one tick
    /// (so it would be unsound if it did). Documents are tiny.
    ///
    /// "Tiny" is what [`MAX_READ_BYTES`] enforces, and this poll is the
    /// reason it has to: a single multi-megabyte file dropped into
    /// `/etc/pve/meta` out of band would otherwise be read and SHA-256'd by
    /// every open UI's 5 s poll, forever. Above the cap the file contributes
    /// [`identify`]'s surrogate instead, which still changes when it does.
    ///
    /// An entry that disappears mid-walk is skipped: the store is not locked
    /// against a concurrent `DELETE` or GC pass, and a poll must not 500
    /// because a file it had just listed is gone.
    pub fn version(&self) -> Result<StoreVersion> {
        self.version_of(None)
    }

    /// [`MetaStore::version`], restricted to the one document a caller is
    /// actually watching.
    ///
    /// `None` is the whole store. `Some(id)` covers `id`'s own file plus both
    /// registry directories and nothing else, which is exactly what an open
    /// editor needs: its document's content, and the prefixes and permissions
    /// that decide how that content is rendered and who may write it.
    ///
    /// The registry directories are still walked whole, because they are the
    /// unit of *shadowing*: a prefix appearing in the cluster directory
    /// changes the governing schema without any document changing, and the
    /// token has to move for that. They hold tens of files for a cluster, not
    /// one per guest, which is the whole reason this scoping is worth having.
    ///
    /// What a scoped token deliberately does **not** see: other documents, and
    /// `id`'s own snapshot copies. The unscoped token moves for both — every
    /// file in the store root is an entry — so the two tokens are not
    /// comparable, and a caller that changes scope sees its token move once
    /// and reloads once. That is the only cost.
    ///
    /// The cost it removes is per open tab, per tick: the unscoped poll reads
    /// and hashes every document *and every snapshot copy* in the cluster to
    /// answer a question about one guest. At four guests that is invisible; at
    /// three hundred with snapshots it is a thousand pmxcfs round-trips every
    /// five seconds, per open editor.
    pub fn version_of(&self, only: Option<&DocId>) -> Result<StoreVersion> {
        let mut entries: Vec<(String, String)> = Vec::new();
        // A map, not a list: a prefix present in both the packaged and the
        // cluster directory is *one* document, and the effective one is the
        // last write wins here because the directories are walked lowest
        // precedence first -- the same rule `registry::load_prefixes` applies
        // by file name. The shadowed file still contributes to `entries`, so
        // editing it moves the token even though no document's digest changed:
        // over-notifying a poll is a reload, under-notifying it is a stale UI.
        let mut documents: BTreeMap<DocId, String> = BTreeMap::new();
        let mut latest: Option<SystemTime> = None;

        match only {
            // A registry document's own file is one the registry walk below
            // already covers, and no guest document can affect it: the store
            // root is not read at all.
            Some(DocId::Registry(..)) => {}
            Some(id) => {
                let file = format!("{}.{}", id.base_name(), DISK_FORMAT.ext());
                self.add_version_entry(
                    &self.root.join(&file),
                    &file,
                    "",
                    &mut entries,
                    &mut documents,
                    &mut latest,
                    &document_id,
                )?;
            }
            None => self.scan_for_version(
                &self.root.clone(),
                "",
                &mut entries,
                &mut documents,
                &mut latest,
                &document_id,
            )?,
        }
        for kind in [RegistryKind::PrefixDef, RegistryKind::Permission] {
            for dir in self.registry.dirs(kind) {
                // The full directory path, not just the kind: two directories
                // of the same kind hold same-named files on purpose, and the
                // token must be able to tell them apart.
                let prefix = format!("{}/", dir.display());
                self.scan_for_version(
                    &dir.clone(),
                    &prefix,
                    &mut entries,
                    &mut documents,
                    &mut latest,
                    &|name| registry_document_id(kind, name),
                )?;
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
            documents: documents.into_iter().collect(),
        })
    }

    /// One directory's contribution to [`MetaStore::version`]: every file in it
    /// as a `(prefix + name, identity)` entry, and every file that addresses a
    /// document as a `(DocId, identity)` too.
    ///
    /// An entry that disappears mid-walk is skipped rather than failing the
    /// poll, and a directory that does not exist contributes nothing -- the
    /// packaged prefix directory is absent on a node with no operator
    /// package installed, which is not a condition to report.
    fn scan_for_version(
        &self,
        dir: &std::path::Path,
        prefix: &str,
        entries: &mut Vec<(String, String)>,
        documents: &mut BTreeMap<DocId, String>,
        latest: &mut Option<SystemTime>,
        id_of: &dyn Fn(&str) -> Option<DocId>,
    ) -> Result<()> {
        if !dir.is_dir() {
            return Ok(());
        }
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            self.add_version_entry(
                &entry.path(),
                &name,
                prefix,
                entries,
                documents,
                latest,
                id_of,
            )?;
        }
        Ok(())
    }

    /// One file's contribution to a version token: a `(prefix + name,
    /// identity)` entry, plus a `documents` row when the name addresses a
    /// document.
    ///
    /// A path that is not there, is not a regular file, or disappears between
    /// two of the syscalls here contributes nothing rather than failing: a
    /// poll must not 500 because a document was deleted between two ticks.
    /// `symlink_metadata`, not `metadata`, so a symlink is skipped the same
    /// way the directory walk's `file_type()` check skipped it.
    #[allow(clippy::too_many_arguments)]
    fn add_version_entry(
        &self,
        path: &std::path::Path,
        name: &str,
        prefix: &str,
        entries: &mut Vec<(String, String)>,
        documents: &mut BTreeMap<DocId, String>,
        latest: &mut Option<SystemTime>,
        id_of: &dyn Fn(&str) -> Option<DocId>,
    ) -> Result<()> {
        let Some(meta) = gone_is_none(fs::symlink_metadata(path))? else {
            return Ok(());
        };
        if !meta.is_file() {
            return Ok(());
        }
        let mtime = meta.modified()?;
        let Some(dig) = identify(path)? else {
            return Ok(());
        };
        if let Some(id) = id_of(name) {
            documents.insert(id, dig.clone());
        }
        entries.push((format!("{prefix}{name}"), dig));
        *latest = Some(match *latest {
            Some(t) if t >= mtime => t,
            _ => mtime,
        });
        Ok(())
    }
}

/// The [`DocId`] a store file name addresses, or `None` when it addresses no
/// document: a snapshot copy (`<vmid>.<snapname>.yaml`), a temp file, or
/// anything else that happens to be in the directory.
/// The [`DocId`] a registry file name addresses, or `None` for anything that
/// is not a `<name>.yaml` the loader would read: the same filter
/// `registry::yaml_files` applies, so the poll and the loader agree on what a
/// registry document is.
fn registry_document_id(kind: RegistryKind, name: &str) -> Option<DocId> {
    let stem = name.strip_suffix(&format!(".{}", DISK_FORMAT.ext()))?;
    crate::registry::is_valid_file_name(stem).then(|| DocId::Registry(kind, stem.to_string()))
}

fn document_id(name: &str) -> Option<DocId> {
    let stem = name.strip_suffix(&format!(".{}", DISK_FORMAT.ext()))?;
    if stem == "datacenter" {
        return Some(DocId::Datacenter);
    }
    // `<vmid>.<snapname>.yaml` also ends with the suffix; its stem is not a
    // bare number, which is exactly what tells the two apart.
    stem.parse::<u32>().ok().map(DocId::Guest)
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
