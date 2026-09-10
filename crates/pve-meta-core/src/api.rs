//! The API layer: everything `PVE::API2::Ext::Meta` does that is not
//! parameters, PVE ACLs or locking (`docs/DESIGN.md` §5).
//!
//! `crates/pve-meta-perl` exports these functions to Perl as
//! `PVE::RS::Meta::api_*`, adding nothing but a [`crate::store::MetaStore`]
//! rooted at `$PVE_META_ROOT` and the [`crate::registry`] load. They live
//! here, in the platform-independent core, because they are the project's
//! security boundary: the write authorization below must be unit-testable on
//! any machine, without `libperl-dev` and without a PVE cluster.
//!
//! Perl does parameters, PVE ACL checks, the vmlist, the guests' tags and the
//! per-document write lock, and hands this module a [`CallerAcl`]. This
//! module does everything else: resolving the caller's scopes against the
//! registrations, view extraction/replace/merge/remove, touched-path
//! computation, the lint, YAML/JSON rendering, and digesting.
//!
//! ## Authorization
//!
//! Authorization is decided **from the request, never from a diff**
//! (`docs/DESIGN.md` §3):
//!
//! 1. [`Effective::can_write`] must hold for the view before anything is
//!    computed, and a caller without `full_write` may not write the root view
//!    at all;
//! 2. the mutation is planned against a **clone** of the stored document and
//!    every path the plan touches is checked with [`Effective::check_write`];
//! 3. the planned document is linted — once, the same way for every caller
//!    (`docs/DESIGN.md` §4);
//! 4. only then is the planned value written.
//!
//! ## Wire contract
//!
//! Everything crosses the Perl/Rust boundary as **native hashes and arrays**
//! (`docs/DESIGN.md` §5): the caller's ACL and tags in, the guest rows in,
//! the documents and results out — perlmod's `serde`-based conversion renders
//! them as Perl scalars/arrays/hashes directly. The single exception is the
//! client-supplied `data` parameter, which is a JSON **string** because that
//! is what the REST parameter is, decoded once here.
//!
//! Errors are `anyhow::Error`s whose `Display` is `"<NNN>: <message>"` (an
//! HTTP status prefix and a human-readable message); the Perl layer parses
//! that prefix back out and re-raises via `PVE::Exception::raise`. Messages
//! name paths: there is no disclosure filtering (`docs/DESIGN.md` §1).

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::Error as CoreError;
use crate::format::{self, Format};
use crate::model;
use crate::patch::{Op, Touched};
use crate::path::Path as DocPath;
use crate::registry::{self, Permission, PrefixDef};
use crate::scopes::{Effective, Scope};
use crate::store::{DocId, MetaStore, RegistryKind, DISK_FORMAT};
use crate::view;

fn unix_secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Maps a [`CoreError`] to the `"NNN: message"` string the Perl layer
/// expects: `409` digest mismatch, `404` not found, `400`
/// lint/parse/invalid-path/invalid-name/registration/too-large, `500`
/// everything else.
pub fn api_err(err: CoreError) -> anyhow::Error {
    let status: u16 = match &err {
        CoreError::DigestMismatch { .. } => 409,
        CoreError::NotFound(_) => 404,
        CoreError::Lint(_)
        | CoreError::Parse { .. }
        | CoreError::InvalidPath(_)
        | CoreError::InvalidName(_)
        | CoreError::Registry(_)
        | CoreError::TooLarge { .. } => 400,
        CoreError::Io(_) | CoreError::Other(_) => 500,
    };
    // An empty digest is what a *missing* document reports; render it
    // visibly rather than as nothing at all.
    let msg = match &err {
        CoreError::DigestMismatch { expected, actual } => format!(
            "digest mismatch: expected {}, actual {}",
            show_digest(expected),
            show_digest(actual)
        ),
        other => other.to_string(),
    };
    anyhow::anyhow!("{status}: {msg}")
}

fn show_digest(d: &str) -> String {
    if d.is_empty() {
        "<empty>".to_string()
    } else {
        d.to_string()
    }
}

fn bad_request(msg: impl std::fmt::Display) -> anyhow::Error {
    anyhow::anyhow!("400: {msg}")
}

/// A 403 naming the path that was refused (`docs/DESIGN.md` §1: key-name
/// disclosure is out of scope; a message that says nothing is a message
/// nobody can act on).
fn forbidden(path: &DocPath) -> anyhow::Error {
    if path.is_root() {
        return anyhow::anyhow!("403: not permitted: the whole document");
    }
    anyhow::anyhow!("403: not permitted: {path}")
}

/// The caller, as `PVE::API2::Ext::Meta` computes it: their authid, the two
/// ACL answers for the document being addressed, and (for a guest) that
/// guest's PVE tags, which resolve the registrations' selectors.
///
/// This is a native Perl hash on the wire; `read`/`write` arrive as ordinary
/// Perl scalars and are converted by truthiness.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CallerAcl {
    /// The caller's PVE authid (`user@realm`, optionally `!tokenid`).
    #[serde(default)]
    pub authid: String,
    /// `VM.Audit` on `/vms/<vmid>` (or `Sys.Audit` on `/`).
    #[serde(default)]
    pub read: bool,
    /// `VM.Config.Options` on `/vms/<vmid>` (or `Sys.Modify` on `/`).
    #[serde(default)]
    pub write: bool,
    /// The guest's PVE tags. Empty for the datacenter document.
    #[serde(default)]
    pub tags: Vec<String>,
}

/// The caller's effective [`Effective`] on `doc_id`.
///
/// Scopes apply to **guest documents only** (`docs/DESIGN.md` §3); the
/// datacenter document is governed by ACLs alone, which is what keeps the
/// registry from being able to grant access to it.
pub fn effective(permission_files: &[Permission], doc_id: &DocId, acl: &CallerAcl) -> Effective {
    let scopes = match doc_id {
        DocId::Guest(_) => registry::scopes_for(permission_files, &acl.authid, &acl.tags),
        // The datacenter document is governed by ACLs alone -- that is what
        // keeps the registry from being able to grant access to it -- and a
        // registry document for the same reason one turn further: a permission that
        // could reach the permission files would be able to widen itself.
        DocId::Datacenter | DocId::Registry(..) => Vec::new(),
    };
    Effective {
        full_read: acl.read,
        full_write: acl.write,
        scopes,
    }
}

/// Parses an API `id` into a [`DocId`]: a vmid, the literal `"datacenter"`, or
/// a registry document as `prefixes/<name>` / `permissions/<name>`.
///
/// The registry form is the API path it is reached at, so the id a caller sends
/// back is the one it read. `<name>` is the file's name, checked with
/// [`registry::is_valid_file_name`]: dotted, because **the file name is the
/// prefix** and `homelab.docker` is a legitimate prefix, but never a slash,
/// a leading dot or a `..`, so an id can never address a file outside its
/// directory.
pub fn parse_id(id: &str) -> Result<DocId, anyhow::Error> {
    if id == "datacenter" {
        return Ok(DocId::Datacenter);
    }
    if let Some((kind, name)) = id.split_once('/') {
        let kind = match kind {
            "prefixes" => RegistryKind::PrefixDef,
            "permissions" => RegistryKind::Permission,
            other => {
                return Err(bad_request(format!(
                    "invalid id '{id}': unknown registry kind '{other}' \
                     (expected 'prefixes' or 'permissions')"
                )))
            }
        };
        if !registry::is_valid_file_name(name) {
            return Err(bad_request(format!(
                "invalid id '{id}': '{name}' is not a valid {kind} name"
            )));
        }
        return Ok(DocId::Registry(kind, name.to_string()));
    }
    id.parse::<u32>().map(DocId::Guest).map_err(|_| {
        bad_request(format!(
            "invalid id '{id}': must be a vmid, 'datacenter', \
             'prefixes/<name>' or 'permissions/<name>'"
        ))
    })
}

fn id_str(id: &DocId) -> String {
    match id {
        DocId::Guest(vmid) => vmid.to_string(),
        DocId::Datacenter => "datacenter".to_string(),
        DocId::Registry(kind, name) => format!("{kind}/{name}"),
    }
}

/// Parses a `view` parameter (a dotted/slash path, or absent = the whole
/// document). No key is reserved and no segment is special: the one lint on
/// the planned document is what refuses a write that would build something
/// the document model does not allow (`docs/DESIGN.md` §4).
fn parse_view(view: Option<&str>) -> Result<DocPath, anyhow::Error> {
    match view {
        Some(s) => DocPath::parse(s).map_err(api_err),
        None => Ok(DocPath::root()),
    }
}

fn view_out(view: Option<&str>) -> String {
    view.unwrap_or("").to_string()
}

/// Parses a view's wire `format`: `json` or `yaml` (`docs/DESIGN.md` §4).
fn parse_view_format(name: &str) -> Result<Format, anyhow::Error> {
    Format::from_ext(name)
        .ok_or_else(|| bad_request(format!("invalid format '{name}': expected 'json' or 'yaml'")))
}

/// What one document read gave us.
struct Stored {
    /// The stored document — or the **empty document** whenever
    /// [`Stored::unrecoverable`] is set, since nothing else can be recovered.
    value: Value,
    /// The digest of what is actually on disk (`""` for a document that does
    /// not exist), so a repairing write can still carry its
    /// compare-and-swap precondition.
    digest: String,
    /// `Some(reason)` when a file exists but its content could not be
    /// recovered *as a document*. Exactly one condition, with three causes
    /// (`docs/DESIGN.md` §4):
    ///
    /// * it is not valid YAML (or not text at all);
    /// * it is above the store's read cap, so it is never parsed;
    /// * it parses to something that is not a mapping — `null` (an empty or
    ///   comment-only file, the likeliest out-of-band corruption of all), a
    ///   scalar, a list.
    ///
    /// They differ only in the message. Keying anything on the *parse* alone
    /// let the third one through: a scalar- or list-rooted file has no
    /// structure a narrower write could preserve either, so a root `merge`
    /// would have replaced it wholesale while reporting the merge's own
    /// touched paths.
    unrecoverable: Option<String>,
    /// The file's own text, or `None` when the bytes were never read (no
    /// file, or one above the read cap). Never a stand-in: a caller that
    /// renders this is showing the administrator the file they must repair.
    raw: Option<String>,
}

impl Stored {
    /// The empty document: what a caller sees for an id with no file at all
    /// (`docs/DESIGN.md` §2 — a non-existent document is an empty document;
    /// there is no explicit create).
    fn absent() -> Stored {
        Stored {
            value: Value::Object(Map::new()),
            digest: String::new(),
            unrecoverable: None,
            raw: None,
        }
    }
}

/// The empty document plus `reason`, keeping `digest` and `raw` as they came
/// off the disk.
fn unrecoverable(digest: String, raw: Option<String>, reason: String) -> Stored {
    Stored {
        value: Value::Object(Map::new()),
        digest,
        unrecoverable: Some(reason),
        raw,
    }
}

/// Reads `id`'s document without ever failing on the document's own content:
/// the one read every handler uses.
///
/// Content that cannot be recovered is *not* an error — it comes back as the
/// empty document with its real digest and a reason (see
/// [`Stored::unrecoverable`]). Every write handler reads before it plans, so
/// a fatal read would make an out-of-band hand-edit unrepairable through the
/// API; and [`list_guests`] reads every guest in a loop, so one unreadable
/// document would otherwise take the whole listing down for every principal.
fn read_stored(store: &MetaStore, id: &DocId) -> Result<Stored, anyhow::Error> {
    let doc = match store.read(id) {
        Ok(doc) => doc,
        Err(CoreError::NotFound(_)) => return Ok(Stored::absent()),
        // Above the read cap the bytes are never pulled in; the digest still
        // is (`MetaStore::digest_of` caps the same way), so the file stays
        // addressable by a compare-and-swap repair.
        Err(e @ (CoreError::TooLarge { .. } | CoreError::Parse { .. })) => {
            let digest = store.digest_of(id).map_err(api_err)?.unwrap_or_default();
            return Ok(unrecoverable(digest, None, e.to_string()));
        }
        Err(e) => return Err(api_err(e)),
    };
    if let Some(err) = doc.parse_error {
        return Ok(unrecoverable(doc.digest, Some(doc.raw), err));
    }
    if !doc.value.is_object() {
        return Ok(unrecoverable(
            doc.digest,
            Some(doc.raw),
            "the stored document is not a mapping".to_string(),
        ));
    }
    Ok(Stored {
        value: doc.value,
        digest: doc.digest,
        unrecoverable: None,
        raw: Some(doc.raw),
    })
}

fn touched_out(touched: &[Touched]) -> Vec<ApiTouched> {
    touched
        .iter()
        .map(|t| ApiTouched {
            path: t.path.to_string(),
            op: match t.op {
                Op::Set => "set".to_string(),
                Op::Delete => "delete".to_string(),
            },
        })
        .collect()
}

/// `GET /meta/version`.
#[derive(Debug, Clone, Serialize)]
pub struct ApiVersion {
    /// A content hash over the store; poll it.
    pub token: String,
    /// The newest document mtime, as a unix timestamp.
    pub changed: u64,
    /// With `detail`: every document's own digest, sorted by id, so a caller
    /// that saw the token move can tell **which** documents to re-read instead
    /// of re-listing the store. With an `id` as well, just that one document.
    ///
    /// Unfiltered by design. A digest is not sensitive (`docs/DESIGN.md` §1
    /// puts digests and listings out of scope), and filtering would cost a
    /// grant computation per document on the one endpoint whose whole purpose
    /// is to be cheap enough to poll.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub documents: Option<Vec<ApiDocumentDigest>>,
}

/// One row of `GET /meta/version?detail=1`.
#[derive(Debug, Clone, Serialize)]
pub struct ApiDocumentDigest {
    /// A vmid, or `"datacenter"`.
    pub id: String,
    pub digest: String,
}

/// One row of `GET /meta/guests` (`docs/DESIGN.md` §5).
#[derive(Debug, Clone, Serialize)]
pub struct GuestListEntry {
    pub vmid: u32,
    /// Only with `VM.Audit`.
    pub node: Option<String>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    /// Only with `VM.Audit`.
    pub name: Option<String>,
    /// Only with `VM.Audit`.
    pub tags: Option<Vec<String>>,
    /// `""` when the guest has no document.
    pub digest: String,
}

/// One entry of a write's `touched` list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApiTouched {
    pub path: String,
    pub op: String,
}

/// A read's result: exactly one of `data` (`format=json`) or `text`
/// (`format=yaml`) is populated.
#[derive(Debug, Clone, Serialize)]
pub struct ApiViewDocument {
    pub id: String,
    pub view: String,
    pub digest: String,
    /// Present when `format=json`. A native structure, unordered once it is a
    /// Perl hash; key order is not a wire contract (`docs/DESIGN.md` §4).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    /// Present when `format=yaml`: the file's own text for the root view, a
    /// canonical dump for a sub-view — or, alongside `parse_error`, the raw
    /// text of a document that does not parse, so it can be repaired.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Present only when the stored document is not valid YAML
    /// (`docs/DESIGN.md` §4): the parser's message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parse_error: Option<String>,
}

/// A write's result.
#[derive(Debug, Clone, Serialize)]
pub struct ApiPutResult {
    pub id: String,
    pub view: String,
    pub digest: String,
    pub touched: Vec<ApiTouched>,
}

/// `GET /meta/access`: the caller's grants for one document, with selectors
/// already resolved.
#[derive(Debug, Clone, Serialize)]
pub struct ApiAccess {
    pub read: bool,
    pub write: bool,
    pub scopes: Vec<Scope>,
    /// The guest's PVE tags — empty for any other document.
    ///
    /// Here so an editor can resolve `selector: {tag: …}` without a second
    /// request: the alternative was `GET /meta/guests`, which reads, parses
    /// and digests **every** document in the cluster to answer a question
    /// about one guest. The tags come from the same place either way — Perl
    /// computed them for this request's ACL check — so the rule that the
    /// server supplies tags and the client only matches them is unchanged.
    ///
    /// Filtered exactly as `GET /meta/guests` filters the same field: a
    /// caller without `VM.Audit` on the guest gets an empty list, not the
    /// tags (`docs/DESIGN.md` §5).
    pub tags: Vec<String>,
}

/// One row of `GET /meta/guests`' input: the vmlist row Perl already has,
/// plus that guest's ACL answers and tags. Perl owns the vmlist and the guest
/// properties — there is exactly one reader of `/etc/pve/.vmlist` per
/// request, and guest config parsing is not re-implemented here.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct GuestInput {
    pub vmid: u32,
    #[serde(default)]
    pub node: Option<String>,
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// `VM.Audit` on this guest.
    #[serde(default)]
    pub read: bool,
    /// `VM.Config.Options` on this guest. Never consulted by a listing; it is
    /// here so one row shape serves every caller.
    #[serde(default)]
    pub write: bool,
}

/// `api_version()` -> `{ token, changed }`.
///
/// With `id`, the token covers that document plus the registry directories and
/// nothing else ([`MetaStore::version_of`]) — what an open editor is actually
/// watching, at a cost that does not grow with the number of guests in the
/// cluster. A scoped and an unscoped token are not comparable, which is a
/// caller's business: each poller compares a token against its own previous
/// one.
pub fn version(
    store: &MetaStore,
    detail: bool,
    id: Option<&str>,
) -> Result<ApiVersion, anyhow::Error> {
    let doc_id = id.map(parse_id).transpose()?;
    let v = store.version_of(doc_id.as_ref()).map_err(api_err)?;
    Ok(ApiVersion {
        token: v.token,
        changed: unix_secs(v.changed),
        documents: detail.then(|| {
            v.documents
                .into_iter()
                .map(|(id, digest)| ApiDocumentDigest {
                    id: id_str(&id),
                    digest,
                })
                .collect()
        }),
    })
}

/// `GET /meta/schemas`: the two registry file formats as schemas
/// (`crate::metaschema`), so the editor can show a prefix or permission file as a
/// typed tree the way a prefix's own schema does for a guest document.
pub fn schemas() -> Value {
    crate::metaschema::schemas()
}

/// `GET /meta/access`: `{ read, write, scopes, tags }` for one document.
pub fn access(permission_files: &[Permission], doc_id: &DocId, acl: &CallerAcl) -> ApiAccess {
    let g = effective(permission_files, doc_id, acl);
    ApiAccess {
        read: g.full_read,
        write: g.full_write,
        scopes: g.scopes,
        tags: if acl.read { acl.tags.clone() } else { Vec::new() },
    }
}

/// `GET /meta/permissions`: every permission, readable by every authenticated user.
///
/// Not filtered per caller: a permission says who may touch which prefix, which is
/// exactly what the UI's Access column shows for every row, and the threat
/// model puts listings out of scope (`docs/DESIGN.md` §1).
pub fn permissions_list(permission_files: &[Permission]) -> Vec<Permission> {
    permission_files.to_vec()
}

/// `GET /meta/prefixes`: every prefix, most-specific first, readable by
/// every authenticated user.
pub fn prefixes_list(prefixes: &[PrefixDef]) -> Vec<PrefixDef> {
    prefixes.to_vec()
}

/// `GET /meta/guests`: for every guest Perl passed in, the metadata the
/// caller may see.
///
/// A guest the caller can read *nothing* of is omitted entirely, and
/// `node`/`name`/`tags` are returned only to a caller with `VM.Audit` on that
/// guest (`docs/DESIGN.md` §5).
///
/// # Errors
/// `400:` if `has` is not a valid path.
pub fn list_guests(
    store: &MetaStore,
    permission_files: &[Permission],
    authid: &str,
    guests: &[GuestInput],
    has: Option<&str>,
) -> Result<Vec<GuestListEntry>, anyhow::Error> {
    let has_path = has.map(DocPath::parse).transpose().map_err(api_err)?;

    let mut out = Vec::with_capacity(guests.len());
    for guest in guests {
        let acl = CallerAcl {
            authid: authid.to_string(),
            read: guest.read,
            write: guest.write,
            tags: guest.tags.clone(),
        };
        let g = effective(permission_files, &DocId::Guest(guest.vmid), &acl);
        let readable = g.readable_prefixes();
        if readable.is_empty() {
            continue;
        }

        let stored = read_stored(store, &DocId::Guest(guest.vmid))?;
        let visible = view::filter(&stored.value, &readable);

        if let Some(path) = &has_path {
            if model::get_path(&visible, path).is_none() {
                continue;
            }
        }

        out.push(GuestListEntry {
            vmid: guest.vmid,
            node: g.full_read.then(|| guest.node.clone()).flatten(),
            kind: guest.kind.clone(),
            name: g.full_read.then(|| guest.name.clone()).flatten(),
            tags: g.full_read.then(|| guest.tags.clone()),
            digest: stored.digest,
        });
    }
    Ok(out)
}

/// `GET /meta/guests/{vmid}` / `GET /meta/datacenter`.
///
/// With a `view`, requires read access to it ([`Effective::can_read`]); without
/// one, returns the union of the caller's readable subtrees
/// ([`Effective::readable_prefixes`] + [`view::filter`]) — the whole document
/// for a full-read grant. A caller with **no** read grant at all gets a 403,
/// not an empty document with the real digest (which would be a
/// change-detection oracle over content they may not see).
///
/// **A document whose content could not be recovered** (`docs/DESIGN.md` §4
/// and [`Stored::unrecoverable`] — it does not parse, it is above the read
/// cap, or it is not a mapping): `format=yaml` for a full reader answers
/// `200` with the file's raw text plus `parse_error`, so an administrator can
/// see what to repair. Everyone else — `format=json`, any caller without full
/// read, and anyone at all when the bytes were never read — gets `422`
/// naming the condition. It is reported, never rendered as an empty document:
/// the file is there, and the caller has to know that before writing over it.
///
/// # Errors
/// `400:` invalid id/view/format. `403:` no read grant, or a `view` that is
/// not readable. `422:` the stored document's content could not be recovered.
pub fn get_document(
    store: &MetaStore,
    permission_files: &[Permission],
    id: &str,
    view: Option<&str>,
    format_name: &str,
    acl: &CallerAcl,
) -> Result<ApiViewDocument, anyhow::Error> {
    let doc_id = parse_id(id)?;
    let access = effective(permission_files, &doc_id, acl);
    let fmt = parse_view_format(format_name)?;
    let view_path = parse_view(view)?;

    let readable = access.readable_prefixes();
    if readable.is_empty() {
        return Err(forbidden(&view_path));
    }
    if view.is_some() && !access.can_read(&view_path) {
        return Err(forbidden(&view_path));
    }

    let stored = read_stored(store, &doc_id)?;

    if let Some(err) = &stored.unrecoverable {
        if fmt == Format::Yaml && access.full_read {
            if let Some(raw) = &stored.raw {
                return Ok(ApiViewDocument {
                    id: id_str(&doc_id),
                    view: view_out(view),
                    digest: stored.digest,
                    data: None,
                    text: Some(raw.clone()),
                    parse_error: Some(err.clone()),
                });
            }
        }
        return Err(anyhow::anyhow!(
            "422: the stored document cannot be rendered: {err} \
             (replace it with a full document (no 'view', mode=replace) or delete it)"
        ));
    }

    let result_value = if view.is_some() {
        view::extract(&stored.value, &view_path).unwrap_or_else(|| Value::Object(Map::new()))
    } else {
        view::filter(&stored.value, &readable)
    };

    let (data, text) = match fmt {
        Format::Json => (Some(result_value), None),
        // The root view of a full reader renders the file's own text; every
        // other view is a canonical dump of what they may see.
        Format::Yaml => {
            let own_text = stored.raw.as_deref().filter(|raw| !raw.is_empty());
            let text = if let (None, true, Some(raw)) = (view, access.full_read, own_text) {
                raw.to_string()
            } else {
                view::render(&result_value, Format::Yaml)
            };
            (None, Some(text))
        }
    };

    Ok(ApiViewDocument {
        id: id_str(&doc_id),
        view: view_out(view),
        digest: stored.digest,
        data,
        text,
        parse_error: None,
    })
}

/// Refuses every write against a document whose content could not be
/// recovered — see [`Stored::unrecoverable`] for the three causes — except
/// the two that *replace the file whole*: a root `replace` and a root
/// `DELETE` (`docs/DESIGN.md` §4).
///
/// The value planned against is the empty document (nothing else can be
/// recovered), so any narrower write would silently discard everything the
/// file contains.
///
/// One condition, one gate: whatever makes a document unrecoverable, the
/// repair is the same two shapes and nothing narrower — a root `merge` very
/// much included, since it is planned against the empty document too.
///
/// **And it takes `full_write`, which is the one place that rule survives.**
/// Everywhere else a write is authorized by what it changes, because the diff
/// against the stored document can see what that is. Here it cannot: the
/// stored value *is* the empty document, so a scoped principal replacing the
/// root with nothing but its own subtree would produce a touched list entirely
/// inside its own scope — and destroy every other prefix's content in a file
/// nobody can currently read. Content-based authorization needs content to
/// authorize against; when there is none, the coarse rule is the only honest
/// one.
fn check_repairable(
    stored: &Stored,
    access: &Effective,
    view_path: &DocPath,
    is_merge: bool,
) -> Result<(), anyhow::Error> {
    let Some(err) = &stored.unrecoverable else {
        return Ok(());
    };
    if view_path.is_root() && !is_merge {
        if !access.full_write {
            return Err(anyhow::anyhow!(
                "403: not permitted: this document cannot be read back, so repairing it \
                 replaces content that cannot be checked against your permissions; \
                 repairing it as a whole requires full write access"
            ));
        }
        return Ok(());
    }
    Err(bad_request(format!(
        "the stored document cannot be read back and can only be repaired as a whole: \
         replace it with a full document (no 'view', mode=replace) or delete it ({err})"
    )))
}

/// Every write's up-front, request-shaped authorization gate.
///
/// **What authorizes a write is what it changes, not what it is addressed
/// to.** The real gate is `Effective::check_write` in [`plan_write`], which
/// runs over the paths the write actually touches — computed by `patch::diff`
/// against the stored document, so it sees every value that changed, every key
/// that appeared and every key that vanished, wherever the write was aimed.
/// This function only refuses the requests that must not reach that check at
/// all.
///
/// It used to be the other way round: the view had to be inside a writable
/// scope, and the root view demanded `full_write` outright. That is strictly
/// coarser than the question anyone is actually asking, and it refused writes
/// that violate nobody's permissions — a permission file has `rules`, plural,
/// so holding `rw` on two prefixes and editing one key in each is ordinary,
/// and the narrowest view covering both is the document root. Same for a key
/// reordering, which changes no path at all and can only be expressed as a
/// whole-document write. Both were 403s for writes whose every change was
/// permitted (`docs/DESIGN.md` §3.4).
///
/// Two things still have to be refused here, and neither is about the content:
///
/// * **You must be able to read the view you name.** Otherwise the content
///   check becomes a read oracle: replace a key you cannot read with a guess,
///   and `200` versus `403` tells you whether the guess was right. Requiring
///   read access makes the oracle answer a question you could have asked
///   outright. This also keeps a scope-only principal out of the root view —
///   `covers` never covers the root — which is what stops it from replacing a
///   document it can only see part of.
/// * **You must have some write permission on this document.** The content
///   check measures changed *paths*, and key order is not one: a pure
///   reordering touches nothing and would sail through, letting a read-only
///   auditor rewrite a file. `has_any_write` is the floor that keeps the
///   people who may not write here from causing a write at all.
///
/// `full_write` short-circuits both: it is the existing "may write the whole
/// document" answer, and it does not depend on being able to read it (a PVE
/// ACL can grant `VM.Config.Options` without `VM.Audit`, and that principal
/// could write the root before this change).
fn authorize_view_write(access: &Effective, view_path: &DocPath) -> Result<(), anyhow::Error> {
    if access.full_write {
        return Ok(());
    }
    if !access.has_any_write() {
        return Err(anyhow::anyhow!(
            "403: not permitted: no write access to this document"
        ));
    }
    if !access.can_read(view_path) {
        // The root gets its own sentence: `forbidden` names the path it
        // refused, and the root's name is the empty string.
        if view_path.is_root() {
            return Err(anyhow::anyhow!(
                "403: not permitted: writing the whole document requires being able to \
                 read the whole document; name a view inside a prefix you hold"
            ));
        }
        return Err(forbidden(view_path));
    }
    Ok(())
}

/// Runs the planned mutation against `planned` (already a clone of the
/// stored document), checks every touched path against the caller's permissions,
/// and runs **the** lint on the result.
///
/// The lint lives here, before the `dry_run` branch, so a dry run validates
/// exactly what the write validates — and it is the same lint for every
/// caller, on the whole planned document, naming the offending path
/// (`docs/DESIGN.md` §4).
fn plan_write(
    planned: &mut Value,
    access: &Effective,
    mutate: impl FnOnce(&mut Value) -> Result<Vec<Touched>, anyhow::Error>,
) -> Result<Vec<Touched>, anyhow::Error> {
    let touched = mutate(planned)?;

    if let Err(denied) = access.check_write(&touched) {
        return Err(forbidden(&denied));
    }

    let lints = model::lint(planned);
    if !lints.is_empty() {
        let detail = lints
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ");
        return Err(bad_request(format!("document failed validation: {detail}")));
    }
    Ok(touched)
}

/// The extra gate a registry document passes and the other two do not: the
/// text about to be written must parse as the kind it is
/// (`registry::parse_prefix` / `registry::parse_permission`).
///
/// The loader **skips** a malformed file with a warning and carries on -- that
/// isolation is why this data left `datacenter.yaml` -- so without this check
/// the editor's most likely mistake (a typo in `selector:`) would be answered
/// by the prefix silently disappearing from the list, with a 200 on the
/// write that removed it. The rule is the parser itself, not a copy of it, so
/// what the API accepts and what the loader reads back cannot drift.
///
/// Guest and datacenter documents have no shape beyond `model::lint`: they
/// hold whatever an administrator puts in them, which is the point of them.
fn check_registry_shape(doc_id: &DocId, text: &str) -> Result<(), anyhow::Error> {
    let DocId::Registry(kind, name) = doc_id else {
        return Ok(());
    };
    let parsed = match kind {
        RegistryKind::PrefixDef => registry::parse_prefix(name, text).map(|_| ()),
        RegistryKind::Permission => registry::parse_permission(name, text).map(|_| ()),
    };
    parsed.map_err(|e| {
        let kind = match kind {
            RegistryKind::PrefixDef => "prefix",
            RegistryKind::Permission => "permission file",
        };
        bad_request(format!(
            "the result would not be a valid {kind}: {e} \
             (the loader would skip the file, so the write is refused instead)"
        ))
    })
}

/// `PUT /meta/guests/{vmid}` / `PUT /meta/datacenter`.
///
/// `mode` is `"replace"` (default: the view's subtree is replaced by
/// `payload` wholesale — an empty object stores an empty map) or `"merge"`
/// (RFC 7386-style merge-patch relative to the view, where `null` deletes).
///
/// # Errors
/// `400:` invalid id/view/format/mode/payload, or the planned document fails
/// the lint. `409:` digest mismatch. `403:` the view is not writable, or a
/// planned touched path is outside the caller's write permissions.
#[allow(clippy::too_many_arguments)] // matches the PUT endpoint's parameter set 1:1 (docs/DESIGN.md §5)
pub fn put_document(
    store: &MetaStore,
    permission_files: &[Permission],
    id: &str,
    view: Option<&str>,
    format_name: &str,
    payload: &str,
    mode: &str,
    digest: Option<&str>,
    dry_run: bool,
    acl: &CallerAcl,
) -> Result<ApiPutResult, anyhow::Error> {
    let doc_id = parse_id(id)?;
    let access = effective(permission_files, &doc_id, acl);
    let fmt = parse_view_format(format_name)?;
    let view_path = parse_view(view)?;

    // (1) Authorize the *request* before computing anything.
    authorize_view_write(&access, &view_path)?;

    // A merge payload is a patch (`null` deletes); a replace payload is
    // document content.
    let is_merge = match mode {
        "merge" => true,
        "replace" | "" => false,
        other => {
            return Err(bad_request(format!(
                "invalid mode '{other}': expected 'replace' or 'merge'"
            )))
        }
    };
    let payload_value = if is_merge {
        view::parse_patch(payload, fmt).map_err(api_err)?
    } else {
        view::parse(payload, fmt).map_err(api_err)?
    };

    store.check_precondition(&doc_id, digest).map_err(api_err)?;
    let stored = read_stored(store, &doc_id)?;
    check_repairable(&stored, &access, &view_path, is_merge)?;

    // (2) Plan the mutation against a *copy*; the stored document is only
    //     touched once the plan has passed every check.
    let mut planned = stored.value.clone();
    let touched = plan_write(&mut planned, &access, |v| {
        if is_merge {
            view::merge(v, &view_path, &payload_value).map_err(api_err)
        } else {
            view::replace(v, &view_path, payload_value.clone()).map_err(api_err)
        }
    })?;

    let text = format::dump(DISK_FORMAT, &planned);
    check_registry_shape(&doc_id, &text)?;

    // (3) Apply — unless there is nothing to apply. A write that changes no
    //     path *and* would put back the bytes already on disk is skipped
    //     entirely: rewriting the file advances its mtime and so
    //     `version()`'s `changed`, while the content token correctly does not
    //     move, and "a merge that touches nothing changes nothing"
    //     (`docs/DESIGN.md` §4) is not true of a file whose timestamp jumped.
    //     Both halves of the condition are needed: `touched: []` alone still
    //     covers the repair of a document that reads back as empty because it
    //     is unrecoverable, and a byte comparison alone would skip nothing a
    //     canonical dump ever produces.
    let unchanged =
        touched.is_empty() && stored.unrecoverable.is_none() && stored.raw.as_deref() == Some(&text);
    let new_digest = if dry_run || unchanged {
        crate::digest::digest(text.as_bytes())
    } else {
        store
            .put_raw(&doc_id, &text, digest)
            .map_err(api_err)?
            .document
            .digest
    };

    Ok(ApiPutResult {
        id: id_str(&doc_id),
        view: view_out(view),
        digest: new_digest,
        touched: touched_out(&touched),
    })
}

/// `DELETE /meta/guests/{vmid}` / `DELETE /meta/datacenter`: removes the
/// subtree at `view`, or the whole document if `view` is absent.
///
/// Removes **only the current document**: snapshot copies belong to the
/// snapshot hooks and are never touched from the REST API
/// (`docs/DESIGN.md` §6).
///
/// # Errors
/// `400:` invalid id/view. `409:` digest mismatch. `403:` the view is not
/// writable, or a planned touched path is outside the caller's write permissions.
pub fn delete_document(
    store: &MetaStore,
    permission_files: &[Permission],
    id: &str,
    view: Option<&str>,
    digest: Option<&str>,
    acl: &CallerAcl,
) -> Result<ApiPutResult, anyhow::Error> {
    let doc_id = parse_id(id)?;
    let access = effective(permission_files, &doc_id, acl);
    let view_path = parse_view(view)?;

    authorize_view_write(&access, &view_path)?;

    store.check_precondition(&doc_id, digest).map_err(api_err)?;
    let stored = read_stored(store, &doc_id)?;
    // A root DELETE removes the file whole, so it repairs an unrecoverable
    // document exactly like a root replace does.
    check_repairable(&stored, &access, &view_path, false)?;

    let mut planned = stored.value.clone();
    let touched = plan_write(&mut planned, &access, |v| {
        view::remove(v, &view_path).map_err(api_err)
    })?;

    // "Is there a file?" is answered by the read that already happened — a
    // missing document is the only one that reports an empty digest — rather
    // than by a fresh `locate`, whose answer could already be stale by the
    // time it is acted on. `MetaStore::delete` is idempotent for the same
    // reason: losing the race to another `DELETE` or to the GC is this
    // request's own outcome, not a 500.
    let existed = !stored.digest.is_empty();
    let new_digest = if view_path.is_root() {
        store.delete(&doc_id).map_err(api_err)?;
        String::new()
    } else if existed {
        let text = format::dump(DISK_FORMAT, &planned);
        // A partial delete is a write, and a write of a registry document must
        // still leave a file its own loader will read: dropping `authid` from a
        // grant is a `DELETE ?view=authid`, and the same rule has to hold on
        // this path as on `put_document`'s.
        check_registry_shape(&doc_id, &text)?;
        store
            .put_raw(&doc_id, &text, digest)
            .map_err(api_err)?
            .document
            .digest
    } else {
        String::new()
    };

    Ok(ApiPutResult {
        id: id_str(&doc_id),
        view: view_out(view),
        digest: new_digest,
        touched: touched_out(&touched),
    })
}

/// The GC (`docs/DESIGN.md` §6): every stored vmid that is **not** in
/// `vmids`, the vmlist Perl passes in — the candidates for purging, in
/// ascending order.
///
/// This is the first half of a two-phase GC, and the phases exist because
/// they are locked differently. Writes serialize under
/// `cfs_lock_domain("pve-meta-<vmid>")` and the GC pass under
/// `cfs_lock_domain("pve-meta-gc")` — disjoint domains, so a whole-sweep GC
/// could (and did) delete a document that was written after its vmlist
/// snapshot was taken: destroy 999500, recreate a guest at 999500, `PUT` its
/// metadata, and a GC pass already in flight purges the fresh document with
/// the `PUT` long since answered `200`. Silent server-side data loss.
///
/// So the caller purges **one vmid at a time**, each under that vmid's own
/// write lock, re-validating liveness inside it: see [`gc_purge`], and
/// `libexec/gc` for the Perl that does it.
///
/// The datacenter document is never a guest and is never a candidate.
pub fn gc_candidates(store: &MetaStore, vmids: &[u32]) -> Result<Vec<u32>, anyhow::Error> {
    let live: std::collections::HashSet<u32> = vmids.iter().copied().collect();
    Ok(store
        .stored_vmids()
        .map_err(api_err)?
        .into_iter()
        .filter(|vmid| !live.contains(vmid))
        .collect())
}

/// The GC's second phase: purge one vmid's document **and every snapshot
/// copy**, having re-checked it against `live` — a vmlist read *inside*
/// `cfs_lock_domain("pve-meta-<vmid>")`, the same lock every write to that
/// document holds. Returns the number of files removed, `0` if the vmid is
/// live again.
///
/// The re-check is the whole point: the candidate list came from a snapshot
/// taken before the lock, and a guest can be created (and its metadata
/// written) in between. Under the lock, `live` is authoritative — a document
/// written after the snapshot belongs to a guest that is now in the vmlist,
/// and the purge does not happen.
///
/// # Errors
/// `500:` `live` is empty. An empty vmlist means "every guest is gone", which
/// is what a fresh process that has not called `PVE::Cluster::cfs_update()`,
/// or one that hit a pmxcfs hiccup, also looks like. `libexec/gc` refuses it
/// too; this is the guard on the destructive operation itself, so no future
/// caller has to remember it.
pub fn gc_purge(store: &MetaStore, vmid: u32, live: &[u32]) -> Result<usize, anyhow::Error> {
    if live.is_empty() {
        return Err(anyhow::anyhow!(
            "500: refusing to garbage-collect {vmid} against an empty vmlist"
        ));
    }
    if live.contains(&vmid) {
        return Ok(0);
    }
    store.purge(vmid).map_err(api_err)
}

/// The whole-sweep GC: [`gc_candidates`] followed by an unvalidated
/// [`MetaStore::purge`] of each, against the one `vmids` snapshot.
///
/// This replaces the `on_destroy` hook *and* the whole orphan concept: there
/// is no orphan listing, no orphan grant rule and no orphan delete in the
/// API. A document whose guest is gone — destroyed while its node was down,
/// config removed by hand — stops being replicated by pmxcfs and can no
/// longer be inherited by a future guest created at that vmid.
///
/// **`libexec/gc` does not use this**, and neither should a new caller: it
/// holds no per-vmid lock and re-validates nothing, so a document written
/// after `vmids` was read is deleted without a trace. It stays because it is
/// the exact behaviour the two-phase path has to be tested against.
///
/// An **empty `vmids` is refused**, as it is in [`gc_purge`]. "No guest
/// exists" and "the caller has not run `PVE::Cluster::cfs_update()` yet" are
/// the same input here, and this is the most destructive operation in the
/// system: until now the only thing standing between the two was one line of
/// Perl at one call site (`libexec/gc`), and this function has no production
/// caller at all. A test that wants a whole sweep passes a vmid the store
/// does not have.
///
/// The datacenter document is never a guest and is never touched.
pub fn gc(store: &MetaStore, vmids: &[u32]) -> Result<usize, anyhow::Error> {
    if vmids.is_empty() {
        return Err(anyhow::anyhow!(
            "500: refusing to garbage-collect against an empty vmlist"
        ));
    }
    let mut removed = 0;
    for vmid in gc_candidates(store, vmids)? {
        removed += store.purge(vmid).map_err(api_err)?;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Permission;
    use serde_json::json;

    /// A store over a fresh tempdir, with its registry directories inside it
    /// rather than the machine's real ones (see `tests/store.rs` for why). No
    /// global state: every test owns its own root, so the suite runs in
    /// parallel like the rest of the crate's.
    fn store() -> (tempfile::TempDir, MetaStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = MetaStore::with_registry_dirs(
            dir.path(),
            vec![dir.path().join("registry/prefixes")],
            vec![dir.path().join("registry/grants")],
        );
        (dir, store)
    }

    /// The grants used throughout: `scoped@pve!t1` holds `traefik` rw on
    /// guests tagged `traefik`, and `netbird` ro on every guest.
    fn regs() -> Vec<Permission> {
        vec![registry::parse_permission(
            "scoped",
            "authid: scoped@pve!t1\n\
             rules:\n\
             \x20 - prefix: traefik\n    mode: rw\n    selector: {tag: traefik}\n\
             \x20 - prefix: netbird\n    mode: ro\n    selector: {all: true}\n",
        )
        .unwrap()]
    }

    fn full() -> CallerAcl {
        CallerAcl {
            authid: "root@pam".to_string(),
            read: true,
            write: true,
            tags: vec![],
        }
    }

    fn scoped(tags: &[&str]) -> CallerAcl {
        CallerAcl {
            authid: "scoped@pve!t1".to_string(),
            read: false,
            write: false,
            tags: tags.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn none() -> CallerAcl {
        CallerAcl {
            authid: "nobody@pve".to_string(),
            ..Default::default()
        }
    }

    /// `VM.Audit` and no `VM.Config.Options`, plus whatever scopes the
    /// permission files give it: the principal every one of these rules is
    /// actually about. It can read the whole document, so it can compose a
    /// faithful whole-document write; what it may *change* is its scopes.
    fn auditor(tags: &[&str]) -> CallerAcl {
        CallerAcl {
            authid: "scoped@pve!t1".to_string(),
            read: true,
            write: false,
            tags: tags.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// A permission file with two `rw` rules, which is the ordinary shape the
    /// old view gate could not express a write for: the narrowest view
    /// covering one key in each is the document root.
    fn two_rw() -> Vec<Permission> {
        vec![registry::parse_permission(
            "two",
            "authid: scoped@pve!t1\n\
             rules:\n\
             \x20 - prefix: traefik\n    mode: rw\n    selector: {all: true}\n\
             \x20 - prefix: netbird\n    mode: rw\n    selector: {all: true}\n",
        )
        .unwrap()]
    }

    fn seed(store: &MetaStore, id: &str, text: &str) {
        store.put_raw(&parse_id(id).unwrap(), text, None).unwrap();
    }

    fn read_raw(store: &MetaStore, id: &str) -> Option<String> {
        store.read(&parse_id(id).unwrap()).ok().map(|d| d.raw)
    }

    fn status(err: &anyhow::Error) -> u16 {
        err.to_string()[..3].parse().expect("errors are 'NNN: msg'")
    }

    fn get(
        store: &MetaStore,
        id: &str,
        view: Option<&str>,
        fmt: &str,
        acl: &CallerAcl,
    ) -> Result<ApiViewDocument, anyhow::Error> {
        get_document(store, &regs(), id, view, fmt, acl)
    }

    #[allow(clippy::too_many_arguments)]
    fn put(
        store: &MetaStore,
        id: &str,
        view: Option<&str>,
        fmt: &str,
        payload: &str,
        mode: &str,
        digest: Option<&str>,
        dry_run: bool,
        acl: &CallerAcl,
    ) -> Result<ApiPutResult, anyhow::Error> {
        put_with(store, &regs(), id, view, fmt, payload, mode, digest, dry_run, acl)
    }

    #[allow(clippy::too_many_arguments)]
    fn put_with(
        store: &MetaStore,
        permission_files: &[Permission],
        id: &str,
        view: Option<&str>,
        fmt: &str,
        payload: &str,
        mode: &str,
        digest: Option<&str>,
        dry_run: bool,
        acl: &CallerAcl,
    ) -> Result<ApiPutResult, anyhow::Error> {
        put_document(store, permission_files, id, view, fmt, payload, mode, digest, dry_run, acl)
    }

    fn del(
        store: &MetaStore,
        id: &str,
        view: Option<&str>,
        digest: Option<&str>,
        acl: &CallerAcl,
    ) -> Result<ApiPutResult, anyhow::Error> {
        del_with(store, &regs(), id, view, digest, acl)
    }

    fn del_with(
        store: &MetaStore,
        permission_files: &[Permission],
        id: &str,
        view: Option<&str>,
        digest: Option<&str>,
        acl: &CallerAcl,
    ) -> Result<ApiPutResult, anyhow::Error> {
        delete_document(store, permission_files, id, view, digest, acl)
    }

    // -- grants from registrations ----------------------------------------

    #[test]
fn version_detail_names_the_documents_that_changed() {
        let (_dir, store) = store();
        store.put_raw(&DocId::Guest(100), "a: 1\n", None).unwrap();
        store.put_raw(&DocId::Datacenter, "b: 2\n", None).unwrap();

        // Without `detail` the shape is unchanged: no `documents` on the wire.
        let plain = version(&store, false, None).unwrap();
        assert!(plain.documents.is_none());

        let detailed = version(&store, true, None).unwrap();
        let docs = detailed.documents.expect("detail asked for");
        let ids: Vec<&str> = docs.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, vec!["100", "datacenter"]);
        assert_eq!(docs[0].digest, store.read(&DocId::Guest(100)).unwrap().digest);
        assert_eq!(detailed.token, plain.token, "detail does not change the token");
    }

    #[test]
    fn a_scoped_version_ignores_other_documents_and_snapshots() {
        let (dir, store) = store();
        store.put_raw(&DocId::Guest(100), "a: 1\n", None).unwrap();
        store.put_raw(&DocId::Guest(101), "b: 1\n", None).unwrap();

        let mine = || version(&store, false, Some("100")).unwrap().token;
        let before = mine();

        // Another guest's document: the unscoped token moves, mine does not.
        let whole_before = version(&store, false, None).unwrap().token;
        store.put_raw(&DocId::Guest(101), "b: 2\n", None).unwrap();
        assert_ne!(version(&store, false, None).unwrap().token, whole_before);
        assert_eq!(mine(), before, "another guest is not my document");

        // My own snapshot copy is not my document either -- it is exactly the
        // per-guest fan-out this scoping exists to avoid.
        std::fs::write(dir.path().join("100.snap.yaml"), "a: 9\n").unwrap();
        assert_eq!(mine(), before, "a snapshot copy is not the document");

        // My own document, and only when its content actually changed.
        store.put_raw(&DocId::Guest(100), "a: 1\n", None).unwrap();
        assert_eq!(mine(), before, "a rewrite with the same bytes is not a change");
        store.put_raw(&DocId::Guest(100), "a: 2\n", None).unwrap();
        assert_ne!(mine(), before);
    }

    #[test]
    fn a_scoped_version_still_watches_the_registry() {
        // The registry decides what the document *looks like* and who may
        // write it, so a scoped poll that missed it would leave an open editor
        // rendering against a schema that no longer exists.
        let (dir, store) = store();
        store.put_raw(&DocId::Guest(100), "a: 1\n", None).unwrap();
        let prefixes = dir.path().join("registry/prefixes");
        std::fs::create_dir_all(&prefixes).unwrap();

        for (id, body) in [("100", "selector: {all: true}\n"), ("prefixes/homelab", "selector: {tag: web}\n")] {
            let before = version(&store, false, Some(id)).unwrap().token;
            std::fs::write(prefixes.join("homelab.yaml"), body).unwrap();
            assert_ne!(
                version(&store, false, Some(id)).unwrap().token,
                before,
                "a prefix appearing must move the token for {id}"
            );
        }
    }

    #[test]
    fn a_scoped_detail_lists_exactly_what_the_scoped_token_covers() {
        // `detail` answers "which of the things this token covers changed", so
        // it lists what the token is over and nothing else: this document, and
        // the registry documents -- which the scoped token watches too, because
        // they decide how the document is rendered and who may write it.
        let (dir, store) = store();
        store.put_raw(&DocId::Guest(100), "a: 1\n", None).unwrap();
        store.put_raw(&DocId::Guest(101), "b: 1\n", None).unwrap();
        let prefixes = dir.path().join("registry/prefixes");
        std::fs::create_dir_all(&prefixes).unwrap();
        std::fs::write(prefixes.join("homelab.yaml"), "selector: {all: true}\n").unwrap();

        let docs = version(&store, true, Some("100")).unwrap().documents.unwrap();
        let ids: Vec<&str> = docs.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, vec!["100", "prefixes/homelab"]);
    }

    #[test]
    fn a_scoped_version_refuses_a_garbage_id() {
        // The same parser every other endpoint uses: a 400, not a 500 and not
        // a silent fall back to the whole store.
        let (_dir, store) = store();
        let err = version(&store, false, Some("nope")).unwrap_err().to_string();
        assert!(err.starts_with("400: "), "{err}");
    }

    #[test]
        fn a_selector_resolves_against_the_guests_tags() {
        // `docs/DESIGN.md` §3: adding the tag is the deliberate act of
        // granting the operator that guest.
        let permission_files = regs();
        let untagged = effective(&permission_files, &DocId::Guest(100), &scoped(&[]));
        assert_eq!(untagged.scopes.len(), 1);
        assert_eq!(untagged.scopes[0].prefix.to_string(), "netbird");
        assert!(!untagged.can_write(&DocPath::parse("traefik").unwrap()));

        let tagged = effective(&permission_files, &DocId::Guest(100), &scoped(&["traefik"]));
        assert!(tagged.can_write(&DocPath::parse("traefik.spec").unwrap()));
        assert!(tagged.can_read(&DocPath::parse("netbird").unwrap()));
        assert!(!tagged.can_write(&DocPath::parse("netbird").unwrap()));
    }

    #[test]
    fn scopes_never_apply_to_the_datacenter_document() {
        // `docs/DESIGN.md` §3: the datacenter document is governed by ACLs
        // alone, so no registration can ever reach it.
        let g = effective(&regs(), &DocId::Datacenter, &scoped(&["traefik"]));
        assert!(g.scopes.is_empty());
        assert!(g.readable_prefixes().is_empty());
    }

    #[test]
    fn a_registration_for_another_authid_permissions_nothing() {
        let g = effective(&regs(), &DocId::Guest(100), &none());
        assert!(g.readable_prefixes().is_empty());
    }

    // -- write authorization ------------------------------------------------

    #[test]
    fn zero_permission_token_cannot_create_structure_through_an_empty_merge() {
        // A `PUT ?view=zzz.deep&mode=merge` with `{}` must not write
        // `zzz: {deep: {}}` while reporting `touched: []`: `check_write([])`
        // is vacuously Ok, so the up-front view check is what stops it.
        let (_dir, store) = store();
        seed(&store, "100", "traefik:\n  spec:\n    host: ct100.example\n");
        let before = read_raw(&store, "100").unwrap();

        for acl in [none(), scoped(&["traefik"])] {
            for (view, mode, payload) in [
                (Some("zzz_hacked.deep"), "merge", "{}"),
                (Some("zzz_hacked"), "merge", "{}"),
                (Some("zzz_hacked.deep"), "replace", "{}"),
            ] {
                let err = put(&store, "100", view, "json", payload, mode, None, false, &acl)
                    .expect_err("must be refused");
                assert_eq!(status(&err), 403, "{view:?}/{mode}: {err}");
                assert_eq!(read_raw(&store, "100").unwrap(), before, "{view:?}/{mode} mutated");
            }
        }
    }

    /// Still true after the gate moved to the content, but for two different
    /// reasons: a token with nothing has no write permission at all, and a
    /// scope-only one cannot read the whole document it would be replacing.
    #[test]
    fn a_token_without_full_read_cannot_write_the_root_view() {
        let (_dir, store) = store();
        seed(&store, "100", "traefik:\n  host: x\n");
        let before = read_raw(&store, "100").unwrap();
        for acl in [none(), scoped(&["traefik"])] {
            for (mode, payload) in [("merge", "{}"), ("replace", "{\"a\": 1}")] {
                let err = put(&store, "100", None, "json", payload, mode, None, false, &acl)
                    .expect_err("neither principal can name the root view");
                assert_eq!(status(&err), 403, "{mode}: {err}");
            }
            let err = del(&store, "100", None, None, &acl).expect_err("root delete");
            assert_eq!(status(&err), 403, "{err}");
        }
        assert_eq!(read_raw(&store, "100").unwrap(), before);
    }

    // -- what authorizes a write is what it changes -----------------------
    //
    // `docs/DESIGN.md` §3.4. The gate used to be the *view*: it had to sit
    // inside a writable scope, and the root view took full write access. That
    // refused writes which violated nobody's permissions, so these fix the
    // rule at the level it is actually about.

    #[test]
    fn one_write_may_span_two_granted_prefixes() {
        // A permission file has `rules`, plural. Editing one key in each of
        // two `rw` prefixes is ordinary, and no view but the root covers both.
        let (_dir, store) = store();
        seed(&store, "100", "traefik:\n  host: a\nnetbird:\n  groups:\n  - lan\n");
        let doc = r#"{"traefik":{"host":"b"},"netbird":{"groups":["wan"]}}"#;
        let res = put_with(
            &store, &two_rw(), "100", None, "json", doc, "replace", None, false, &auditor(&[]),
        )
        .expect("every change is inside a granted prefix");
        assert_eq!(
            res.touched.iter().map(|t| t.path.as_str()).collect::<Vec<_>>(),
            vec!["traefik.host", "netbird.groups"]
        );
    }

    #[test]
    fn a_whole_document_write_still_answers_for_every_key_it_changes() {
        // The permissiveness is exactly "every change is permitted" and not one
        // step further: the same root replace that is allowed above is refused
        // the moment it reaches outside the scopes -- by changing a key, and by
        // quietly dropping one, which is the shape that would lose data.
        let (_dir, store) = store();
        let stored = "traefik:\n  host: a\nhomelab:\n  owner: arki\n";
        for (label, doc) in [
            ("changes an outside key", r#"{"traefik":{"host":"b"},"homelab":{"owner":"mallory"}}"#),
            ("drops an outside key", r#"{"traefik":{"host":"b"}}"#),
            ("adds an outside key", r#"{"traefik":{"host":"a"},"homelab":{"owner":"arki"},"new":1}"#),
        ] {
            seed(&store, "100", stored);
            let err = put_with(
                &store, &two_rw(), "100", None, "json", doc, "replace", None, false, &auditor(&[]),
            )
            .unwrap_err();
            assert_eq!(status(&err), 403, "{label}: {err}");
            assert!(err.to_string().contains("homelab") || err.to_string().contains("new"), "{label}: {err}");
            assert_eq!(read_raw(&store, "100").as_deref(), Some(stored), "{label} mutated");
        }
    }

    #[test]
    fn reordering_keys_is_a_write_a_scoped_principal_may_make() {
        // Key order is data -- the model preserves it -- but it is not a path,
        // so a reordering changes nothing the permission rules are written
        // about. It can only be expressed as a whole-document write, which is
        // why it used to be a 403 for anyone without full write access.
        let (_dir, store) = store();
        seed(&store, "100", "traefik:\n  host: a\nnetbird:\n  groups:\n  - lan\n");
        let reordered = r#"{"netbird":{"groups":["lan"]},"traefik":{"host":"a"}}"#;
        let res = put_with(
            &store, &two_rw(), "100", None, "json", reordered, "replace", None, false,
            &auditor(&[]),
        )
        .expect("a reordering changes no path");
        assert!(res.touched.is_empty(), "{:?}", res.touched);
        assert_eq!(
            read_raw(&store, "100").as_deref(),
            Some("netbird:\n  groups:\n  - lan\ntraefik:\n  host: a\n"),
            "the reordering is what was asked for, so it is what is stored"
        );
    }

    #[test]
    fn a_reader_with_no_write_permission_cannot_cause_a_write() {
        // The floor under the content check. A reordering touches no path, so
        // the content check has nothing to refuse -- and without this, an
        // auditor holding nothing but `VM.Audit` could rewrite the file.
        let (_dir, store) = store();
        let stored = "traefik:\n  host: a\nnetbird:\n  groups:\n  - lan\n";
        seed(&store, "100", stored);
        let reordered = r#"{"netbird":{"groups":["lan"]},"traefik":{"host":"a"}}"#;
        let readonly = CallerAcl { authid: "auditor@pve".into(), read: true, ..Default::default() };
        let err = put(&store, "100", None, "json", reordered, "replace", None, false, &readonly)
            .unwrap_err();
        assert_eq!(status(&err), 403, "{err}");
        assert_eq!(read_raw(&store, "100").as_deref(), Some(stored));

        // A read-only *scope* is not write permission either.
        let ro_only = vec![registry::parse_permission(
            "ro",
            "authid: auditor@pve\nrules:\n\x20 - prefix: netbird\n    mode: ro\n    selector: {all: true}\n",
        )
        .unwrap()];
        let err = put_with(
            &store, &ro_only, "100", None, "json", reordered, "replace", None, false, &readonly,
        )
        .unwrap_err();
        assert_eq!(status(&err), 403, "{err}");
        assert_eq!(read_raw(&store, "100").as_deref(), Some(stored));
    }

    #[test]
    fn a_write_you_may_not_read_is_refused_before_it_can_answer_anything() {
        // Without the read requirement the content check is a read oracle:
        // replace a key you cannot read with a guess, and 200-versus-403 tells
        // you whether the guess was right, one guess at a time.
        let (_dir, store) = store();
        seed(&store, "100", "traefik:\n  host: a\nsecret:\n  token: hunter2\n");
        // `two_rw` grants traefik and netbird; `secret` is neither, and this
        // principal has no full read.
        for guess in ["\"hunter2\"", "\"wrong\""] {
            let err = put_with(
                &store, &two_rw(), "100", Some("secret.token"), "json", guess, "replace", None,
                false, &scoped(&[]),
            )
            .unwrap_err();
            assert_eq!(status(&err), 403, "{guess}: {err}");
            // The refusal must not depend on the guess, or it is the oracle again.
            assert_eq!(err.to_string(), "403: not permitted: secret.token", "{guess}: {err}");
        }
    }

    #[test]
    fn an_unreadable_document_is_repaired_only_by_full_write() {
        // The one place the coarse rule survives, because the diff is blind
        // here: the stored value is the empty document, so a scoped principal
        // replacing the root with its own subtree produces a touched list
        // entirely inside its own scope -- while destroying every other
        // prefix's content in a file nobody can currently read.
        // Written past the store, because a document this broken is exactly
        // what `put_raw` refuses to create: it arrived by hand, or from an
        // older writer, or from a half-finished replication.
        let (dir, store) = store();
        let broken = "traefik:\n  host: a\nhomelab: [unclosed\n";
        std::fs::write(dir.path().join("100.yaml"), broken).unwrap();
        let before = std::fs::read_to_string(dir.path().join("100.yaml")).unwrap();
        let err = put_with(
            &store, &two_rw(), "100", None, "json", r#"{"traefik":{"host":"b"}}"#, "replace",
            None, false, &auditor(&[]),
        )
        .unwrap_err();
        assert_eq!(status(&err), 403, "{err}");
        assert!(err.to_string().contains("cannot be read back"), "{err}");
        assert_eq!(std::fs::read_to_string(dir.path().join("100.yaml")).unwrap(), before);

        // Full write access still repairs it, exactly as documented.
        put(&store, "100", None, "json", r#"{"traefik":{"host":"b"}}"#, "replace", None, false, &full())
            .expect("the documented repair path");
    }

    #[test]
    fn a_root_delete_still_answers_for_everything_it_removes() {
        let (_dir, store) = store();
        let stored = "traefik:\n  host: a\nhomelab:\n  owner: arki\n";
        seed(&store, "100", stored);
        let err = del_with(&store, &two_rw(), "100", None, None, &auditor(&[])).unwrap_err();
        assert_eq!(status(&err), 403, "{err}");
        assert!(err.to_string().contains("homelab"), "{err}");
        assert_eq!(read_raw(&store, "100").as_deref(), Some(stored));
    }

    #[test]
    fn a_403_names_the_path_it_refused() {
        // `docs/DESIGN.md` §1: key-name disclosure is out of scope, and a
        // message that says nothing is a message nobody can act on.
        let (_dir, store) = store();
        seed(&store, "100", "traefik:\n  host: x\nnetbird:\n  groups:\n  - lan\n");
        let err = put(
            &store,
            "100",
            Some("netbird.groups"),
            "json",
            "[\"guess\"]",
            "replace",
            None,
            false,
            &scoped(&["traefik"]),
        )
        .unwrap_err();
        assert_eq!(err.to_string(), "403: not permitted: netbird.groups", "{err}");
    }

    #[test]
    fn a_scoped_write_outside_the_view_is_refused_by_the_touched_check() {
        let (_dir, store) = store();
        seed(&store, "100", "traefik:\n  host: x\n");
        // A root merge is refused by the view gate; a *scoped* merge whose
        // patch reaches out of the prefix cannot exist (the patch is applied
        // relative to the view), so the touched check is exercised through a
        // full-read/no-write caller instead.
        let ro = CallerAcl { authid: "ro@pve".into(), read: true, write: false, tags: vec![] };
        let err = put(&store, "100", Some("traefik"), "json", "{\"a\":1}", "replace", None, false, &ro)
            .unwrap_err();
        assert_eq!(status(&err), 403, "{err}");
    }

    // -- the one lint -------------------------------------------------------

    #[test]
    fn one_lint_runs_on_the_planned_document_for_every_caller() {
        // `docs/DESIGN.md` §4. Revision 4 narrowed the lint by privilege and
        // then needed a finding-set subset check to make the narrowing safe;
        // there is one lint now, and it names the offending path.
        let (_dir, store) = store();
        seed(&store, "100", "traefik:\n  host: x\n");
        let before = read_raw(&store, "100").unwrap();

        for acl in [full(), scoped(&["traefik"])] {
            for (view, mode, payload, expect) in [
                (Some("traefik__"), "replace", "5", "comment key value must be a string"),
                (Some("traefik"), "replace", "{\"bad key\": 1}", "invalid key"),
                (Some("traefik"), "replace", "{\"a.b\": 1}", "no dots"),
                (Some("traefik"), "replace", "{\"deep\": {\"bad key\": 1}}", "invalid key"),
                (Some("traefik"), "replace", "{\"list\": [{\"bad key\": 1}]}", "invalid key"),
                (Some("traefik"), "replace", "{\"x__\": 5}", "comment key value must be a string"),
                (Some("traefik"), "merge", "{\"x__\": 5}", "comment key value must be a string"),
                (Some("traefik"), "replace", "{\"nul\": null}", "null values are not allowed"),
                // A view *through* a comment key needs no rule of its own:
                // materialising `q__` as a map is what the lint refuses.
                (Some("traefik.q__.r"), "replace", "1", "comment key value must be a string"),
            ] {
                let err = put(&store, "100", view, "json", payload, mode, None, false, &acl)
                    .map(|ok| panic!("{view:?}/{payload} was accepted: {ok:?}"))
                    .unwrap_err()
                    .to_string();
                assert!(err.starts_with("400: "), "{payload}: {err}");
                assert!(err.contains(expect), "{payload}: {err}");
                assert_eq!(read_raw(&store, "100").unwrap(), before, "{payload} wrote anyway");
            }
        }

        // ... and a legitimate comment-key write still goes through.
        put(&store, "100", Some("traefik__"), "json", "\"the ingress config\"", "replace", None, false, &scoped(&["traefik"]))
            .expect("a string comment value is fine");
    }

    #[test]
    fn the_lint_names_the_offending_path_whoever_asks() {
        // No redaction (`docs/DESIGN.md` §1, §10): an out-of-band bad key
        // blocks the write and is spelled out, for a scoped caller too.
        let (dir, store) = store();
        std::fs::write(
            dir.path().join("100.yaml"),
            "traefik:\n  host: x\nsecret_area:\n  customer name: acme\n",
        )
        .unwrap();

        for acl in [full(), scoped(&["traefik"])] {
            let err = put(&store, "100", Some("traefik"), "json", "{\"host\":\"y\"}", "replace", None, false, &acl)
                .unwrap_err();
            assert_eq!(status(&err), 400, "{err}");
            assert!(err.to_string().contains("customer name"), "{err}");
        }
    }

    #[test]
    fn dry_run_validates_exactly_what_the_write_validates() {
        let (_dir, store) = store();
        seed(&store, "100", "traefik:\n  host: x\n");

        let dry = put(&store, "100", Some("foo__"), "json", "{\"a\": 1}", "replace", None, true, &full());
        let wet = put(&store, "100", Some("foo__"), "json", "{\"a\": 1}", "replace", None, false, &full());
        assert_eq!(status(&dry.unwrap_err()), 400);
        assert_eq!(status(&wet.unwrap_err()), 400);
        assert_eq!(read_raw(&store, "100").unwrap(), "traefik:\n  host: x\n");
    }

    #[test]
    fn dry_run_checks_the_digest_and_never_writes() {
        let (_dir, store) = store();
        seed(&store, "100", "traefik:\n  host: x\n");
        let err = put(&store, "100", Some("traefik"), "json", "{\"host\": \"y\"}", "replace", Some("deadbeef"), true, &full())
            .unwrap_err();
        assert_eq!(status(&err), 409);

        let ok = put(&store, "100", Some("traefik"), "json", "{\"host\": \"y\"}", "replace", None, true, &full())
            .unwrap();
        assert_eq!(ok.touched.len(), 1);
        assert_eq!(read_raw(&store, "100").unwrap(), "traefik:\n  host: x\n");
    }

    // -- reads --------------------------------------------------------------

    #[test]
    fn no_permission_read_is_forbidden_not_an_empty_document_with_a_real_digest() {
        let (_dir, store) = store();
        seed(&store, "100", "traefik:\n  host: x\n");
        let err = get(&store, "100", None, "json", &none()).unwrap_err();
        assert_eq!(status(&err), 403);
        assert_eq!(err.to_string(), "403: not permitted: the whole document");

        // A partial grant still gets the whole-document digest (needed for
        // compare-and-swap PUTs).
        let real = get(&store, "100", None, "json", &full()).unwrap();
        let partial = get(&store, "100", None, "json", &scoped(&["traefik"])).unwrap();
        assert_eq!(partial.digest, real.digest);
    }

    #[test]
    fn a_scoped_read_sees_only_its_own_prefixes() {
        let (_dir, store) = store();
        seed(
            &store,
            "100",
            "__: top level note\ntraefik__: about traefik\ntraefik:\n  host: x\nnetbird:\n  groups:\n  - lan\nother: 1\n",
        );

        let got = get(&store, "100", None, "json", &scoped(&["traefik"])).unwrap();
        assert_eq!(
            got.data.unwrap(),
            json!({"traefik__": "about traefik", "traefik": {"host": "x"},
                   "netbird": {"groups": ["lan"]}})
        );

        // The bare `__` documents the whole document and is not disclosed;
        // the explicit view of it agrees.
        let err = get(&store, "100", Some("__"), "json", &scoped(&["traefik"])).unwrap_err();
        assert_eq!(status(&err), 403, "{err}");

        // Untag the guest and the traefik scope disappears from both.
        let untagged = get(&store, "100", None, "json", &scoped(&[])).unwrap();
        assert_eq!(untagged.data.unwrap(), json!({"netbird": {"groups": ["lan"]}}));
    }

    #[test]
    fn a_full_read_of_the_root_view_returns_the_files_own_text() {
        let (_dir, store) = store();
        let text = "zeta: 1\nalpha__: about alpha\nalpha: 2\n";
        seed(&store, "100", text);
        assert_eq!(get(&store, "100", None, "yaml", &full()).unwrap().text.as_deref(), Some(text));
        // A sub-view is a canonical dump.
        assert_eq!(
            get(&store, "100", Some("alpha"), "yaml", &full()).unwrap().text.as_deref(),
            Some("2\n")
        );
    }

    #[test]
    fn merge_with_null_deletes_end_to_end() {
        let (_dir, store) = store();
        seed(&store, "100", "traefik:\n  spec:\n    host: a\n    port: 1\n");
        let acl = scoped(&["traefik"]);

        let r = put(&store, "100", Some("traefik.spec"), "json", "{\"host\": null}", "merge", None, false, &acl)
            .unwrap();
        assert_eq!(r.touched.len(), 1);
        assert_eq!(r.touched[0].op, "delete");
        assert_eq!(read_raw(&store, "100").unwrap(), "traefik:\n  spec:\n    port: 1\n");

        put(&store, "100", Some("traefik"), "yaml", "spec: null\n", "merge", None, false, &acl).unwrap();
        assert_eq!(read_raw(&store, "100").unwrap(), "traefik: {}\n");
    }

    #[test]
    fn replace_with_an_empty_object_stores_an_empty_map() {
        let (_dir, store) = store();
        seed(&store, "100", "traefik: {}\n");
        let r = put(&store, "100", Some("traefik"), "json", "{}", "replace", None, false, &scoped(&["traefik"]))
            .unwrap();
        assert!(r.touched.is_empty());
        assert_eq!(read_raw(&store, "100").unwrap(), "traefik: {}\n");
    }

    #[test]
    fn get_then_put_with_the_empty_digest_creates_a_document() {
        let (_dir, store) = store();
        let got = get(&store, "999", None, "yaml", &full()).unwrap();
        assert_eq!(got.digest, "");

        let put1 = put(&store, "999", Some("traefik"), "json", "{\"host\": \"new\"}", "replace", Some(""), false, &full())
            .unwrap();
        assert!(!put1.digest.is_empty());
        assert_eq!(read_raw(&store, "999").unwrap(), "traefik:\n  host: new\n");

        let err = put(&store, "999", Some("traefik"), "json", "{\"host\": \"o\"}", "replace", Some(""), false, &full())
            .unwrap_err();
        assert_eq!(status(&err), 409);
        assert!(err.to_string().contains("<empty>"), "{err}");
    }

    #[test]
    fn delete_of_a_view_leaves_the_rest_and_reports_touched() {
        let (_dir, store) = store();
        seed(&store, "100", "traefik:\n  host: x\nnetbird:\n  groups:\n  - lan\n");
        let r = del(&store, "100", Some("traefik"), None, &scoped(&["traefik"])).unwrap();
        assert_eq!(r.touched.len(), 1);
        assert_eq!(r.touched[0].op, "delete");
        assert_eq!(read_raw(&store, "100").unwrap(), "netbird:\n  groups:\n  - lan\n");
    }

    // -- unparseable documents ----------------------------------------------

    #[test]
    fn an_unparseable_document_is_yaml_plus_parse_error_json_422_and_root_repairable() {
        // `docs/DESIGN.md` §4. This is a *per-document* condition: nothing
        // reads `datacenter.yaml` on a guest request any more, so it is never
        // cluster-wide.
        for broken in ["a: 1\n\tb: 2\n", "a: &x 1\nb: *x\n", "a: 1\n  b: 2\n", "a: [\n"] {
            let (dir, store) = store();
            std::fs::write(dir.path().join("100.yaml"), broken).unwrap();

            let got = get(&store, "100", None, "yaml", &full()).unwrap();
            assert_eq!(got.text.as_deref(), Some(broken), "{broken:?}");
            assert!(got.parse_error.is_some());
            assert!(!got.digest.is_empty());

            let err = get(&store, "100", None, "json", &full()).unwrap_err();
            assert_eq!(status(&err), 422, "{broken:?}: {err}");
            // A scoped reader gets 422 either way: there is no structure to
            // filter, and the bytes are not theirs to repair.
            let err = get(&store, "100", None, "yaml", &scoped(&["traefik"])).unwrap_err();
            assert_eq!(status(&err), 422, "{broken:?}: {err}");

            // A narrower write would plan against the empty document and drop
            // the file's content: refused.
            for (view, mode) in [(Some("traefik"), "replace"), (Some("traefik"), "merge"), (None, "merge")] {
                let err = put(&store, "100", view, "json", "{\"host\":\"y\"}", mode, None, false, &full())
                    .expect_err("must be refused");
                assert_eq!(status(&err), 400, "{view:?}/{mode}: {err}");
                assert!(err.to_string().contains("repaired as a whole"), "{err}");
            }
            assert_eq!(std::fs::read_to_string(dir.path().join("100.yaml")).unwrap(), broken);

            // The documented repair, with the compare-and-swap precondition.
            let fixed = "traefik:\n  host: y\n";
            put(&store, "100", None, "yaml", fixed, "replace", Some(&got.digest), false, &full())
                .unwrap_or_else(|e| panic!("{broken:?}: repair refused: {e}"));
            assert_eq!(read_raw(&store, "100").unwrap(), fixed);

            // ... and a root DELETE is the other repair shape.
            std::fs::write(dir.path().join("100.yaml"), broken).unwrap();
            del(&store, "100", None, None, &full()).unwrap();
            assert!(read_raw(&store, "100").is_none());
        }
    }

    #[test]
    fn a_document_above_the_read_cap_is_refused_on_read_and_repairable_on_write() {
        let (dir, store) = store();
        let big = format!("a: \"{}\"\n", "x".repeat(4 * 1024 * 1024));
        std::fs::write(dir.path().join("100.yaml"), &big).unwrap();

        // A GET *reports* the condition rather than rendering the document
        // or 400-ing on the size: the bytes were never read, so there is no
        // `text` to hand back and every caller gets the same 422 naming the
        // two repairs.
        for fmt in ["json", "yaml"] {
            let err = get(&store, "100", None, fmt, &full()).unwrap_err();
            assert_eq!(status(&err), 422, "{fmt}: {err}");
            assert!(err.to_string().contains("too large"), "{err}");
            assert!(err.to_string().contains("mode=replace"), "{err}");
        }

        // One oversized document does not take the listing down.
        seed(&store, "101", "traefik:\n  host: x\n");
        let rows = vec![
            GuestInput { vmid: 100, read: true, ..Default::default() },
            GuestInput { vmid: 101, read: true, ..Default::default() },
        ];
        let listed = list_guests(&store, &regs(), "root@pam", &rows, None).unwrap();
        assert_eq!(listed.iter().map(|g| g.vmid).collect::<Vec<_>>(), vec![100, 101]);
        assert!(!listed[0].digest.is_empty(), "it still reports its real digest");

        // Nothing narrower than a whole-file replace, a root merge included.
        for (view, mode) in [(Some("traefik"), "replace"), (Some("traefik"), "merge"), (None, "merge")] {
            let err = put(&store, "100", view, "json", "{\"a\":1}", mode, None, false, &full())
                .unwrap_err();
            assert_eq!(status(&err), 400, "{view:?}/{mode}: {err}");
            assert!(err.to_string().contains("repaired as a whole"), "{err}");
        }

        let stale = put(&store, "100", None, "yaml", "a: 1\n", "replace", Some("deadbeef"), false, &full())
            .unwrap_err();
        assert_eq!(status(&stale), 409, "{stale}");
        // The digest the listing reported is the one the repair's
        // compare-and-swap accepts, even though the file was never read.
        put(&store, "100", None, "yaml", "a: 1\n", "replace", Some(&listed[0].digest), false, &full())
            .unwrap();
        assert_eq!(read_raw(&store, "100").unwrap(), "a: 1\n");

        // ... and a root DELETE is the other repair shape.
        std::fs::write(dir.path().join("100.yaml"), &big).unwrap();
        del(&store, "100", None, None, &full()).unwrap();
        assert!(!dir.path().join("100.yaml").exists());
    }

    #[test]
    fn a_document_that_parses_to_a_non_mapping_is_repairable_only_as_a_whole() {
        // An empty or comment-only file parses *fine* — to `null` — so a
        // repair path keyed on the parse alone let a narrower write through
        // against a document with no structure to preserve. A root `merge`
        // in particular would have replaced the file wholesale while
        // reporting only the merge's own touched paths.
        for text in ["", "# just a comment\n", "- a\n- secret\n", "just a scalar\n"] {
            let (dir, store) = store();
            std::fs::write(dir.path().join("100.yaml"), text).unwrap();

            // A full reader still sees exactly what is on disk, and is told
            // why it is not a document.
            let got = get(&store, "100", None, "yaml", &full()).unwrap();
            assert_eq!(got.text.as_deref(), Some(text), "{text:?}");
            assert!(got.parse_error.is_some(), "{text:?}");
            assert!(!got.digest.is_empty(), "{text:?}");

            // Nobody else gets it rendered as an empty document ...
            for acl in [full(), scoped(&["traefik"])] {
                let err = get(&store, "100", None, "json", &acl).unwrap_err();
                assert_eq!(status(&err), 422, "{text:?}: {err}");
                // ... and no content of it leaks in the message.
                assert!(!err.to_string().contains("secret"), "{err}");
            }

            // `?has=` cannot be used as an oracle over it either.
            let rows = vec![GuestInput { vmid: 100, ..Default::default() }];
            assert!(list_guests(&store, &regs(), "scoped@pve!t1", &rows, Some("traefik"))
                .unwrap()
                .is_empty());

            for (view, mode) in [(Some("traefik"), "replace"), (Some("traefik"), "merge"), (None, "merge")] {
                let err = put(&store, "100", view, "json", "{\"host\":\"y\"}", mode, None, false, &full())
                    .expect_err("must be refused");
                assert_eq!(status(&err), 400, "{text:?} {view:?}/{mode}: {err}");
                assert!(err.to_string().contains("repaired as a whole"), "{err}");
            }
            assert_eq!(std::fs::read_to_string(dir.path().join("100.yaml")).unwrap(), text);

            // The two repairs, with the compare-and-swap precondition.
            put(&store, "100", None, "yaml", "traefik:\n  host: y\n", "replace", Some(&got.digest), false, &full())
                .unwrap_or_else(|e| panic!("{text:?}: repair refused: {e}"));
            assert_eq!(read_raw(&store, "100").unwrap(), "traefik:\n  host: y\n");

            std::fs::write(dir.path().join("100.yaml"), text).unwrap();
            del(&store, "100", None, None, &full()).unwrap();
            assert!(!dir.path().join("100.yaml").exists(), "{text:?}");
        }
    }

    #[test]
    fn a_write_that_changes_nothing_does_not_rewrite_the_file() {
        // The `touched: []` corner: `version()`'s `token` correctly does not
        // move for a no-op write, so `changed` must not either.
        let (dir, store) = store();
        seed(&store, "100", "traefik:\n  spec:\n    host: x\n");
        let path = dir.path().join("100.yaml");
        let before = std::fs::metadata(&path).unwrap().modified().unwrap();

        for (view, mode, payload) in [
            (Some("traefik.spec"), "merge", "{}"),
            (Some("traefik.spec"), "merge", "{\"host\": \"x\"}"),
            (Some("traefik.spec"), "replace", "{\"host\": \"x\"}"),
            (Some("traefik.spec.gone"), "merge", "{\"nope\": null}"),
        ] {
            let r = put(&store, "100", view, "json", payload, mode, None, false, &full())
                .unwrap_or_else(|e| panic!("{view:?}/{mode}/{payload}: {e}"));
            assert!(r.touched.is_empty(), "{view:?}/{mode}/{payload}");
            assert_eq!(
                std::fs::metadata(&path).unwrap().modified().unwrap(),
                before,
                "{view:?}/{mode}/{payload} rewrote the file"
            );
            assert_eq!(r.digest, store.digest_of(&DocId::Guest(100)).unwrap().unwrap());
        }
        assert_eq!(read_raw(&store, "100").unwrap(), "traefik:\n  spec:\n    host: x\n");

        // A no-op write against a file whose *bytes* are not what we would
        // write still rewrites it: skipping is only ever a byte-for-byte
        // no-op, never a silently declined canonicalisation.
        std::fs::write(&path, "traefik:\n    spec:\n        host: x\n").unwrap();
        put(&store, "100", Some("traefik.spec"), "json", "{}", "merge", None, false, &full()).unwrap();
        assert_eq!(read_raw(&store, "100").unwrap(), "traefik:\n  spec:\n    host: x\n");
    }

    #[test]
    fn a_document_that_is_not_a_map_can_never_be_written_back() {
        // The read side of the same condition is
        // `a_document_that_parses_to_a_non_mapping_is_repairable_only_as_a_whole`;
        // this is the write gate that keeps one from being *stored*.
        let (dir, store) = store();
        for text in ["- a\n- secret\n", "just a scalar\n"] {
            std::fs::write(dir.path().join("100.yaml"), text).unwrap();
            let err = put(&store, "100", None, "yaml", text, "replace", None, false, &full()).unwrap_err();
            assert_eq!(status(&err), 400, "{text:?}: {err}");
            assert!(err.to_string().contains("top level must be an object"), "{err}");
            assert_eq!(std::fs::read_to_string(dir.path().join("100.yaml")).unwrap(), text);
        }
    }

    // -- listing, access, operators, gc -------------------------------------

    #[test]
    fn list_guests_uses_the_rows_perl_passes_and_gates_node_name_and_tags() {
        let (_dir, store) = store();
        seed(&store, "100", "traefik:\n  host: x\n");
        seed(&store, "200", "other:\n  k: v\n");
        let rows = vec![
            GuestInput {
                vmid: 100,
                node: Some("node1".into()),
                kind: Some("lxc".into()),
                name: Some("web".into()),
                tags: vec!["traefik".into()],
                ..Default::default()
            },
            GuestInput {
                vmid: 200,
                node: Some("node1".into()),
                kind: Some("lxc".into()),
                name: Some("db".into()),
                ..Default::default()
            },
            GuestInput { vmid: 300, node: Some("node1".into()), ..Default::default() },
        ];

        // The scoped caller: 100 and 200 both carry the `netbird` all-guests
        // scope, so both are listed; 300 too (the scope applies to it as
        // well, it just has no document).
        let list = list_guests(&store, &regs(), "scoped@pve!t1", &rows, None).unwrap();
        assert_eq!(list.iter().map(|g| g.vmid).collect::<Vec<_>>(), vec![100, 200, 300]);
        assert!(list[0].node.is_none() && list[0].name.is_none() && list[0].tags.is_none());

        // A principal with no ACL and no registration sees nothing.
        assert!(list_guests(&store, &regs(), "nobody@pve", &rows, None).unwrap().is_empty());

        // With VM.Audit, node, name and tags come through.
        let audited: Vec<GuestInput> =
            rows.iter().cloned().map(|mut r| { r.read = true; r }).collect();
        let list2 = list_guests(&store, &regs(), "root@pam", &audited, None).unwrap();
        assert_eq!(list2[0].node.as_deref(), Some("node1"));
        assert_eq!(list2[0].name.as_deref(), Some("web"));
        assert_eq!(list2[0].tags.as_deref(), Some(&["traefik".to_string()][..]));
        assert_eq!(list2[2].digest, "", "a guest with no document reports an empty digest");

        // `has` filters on the *visible* data.
        let filtered = list_guests(&store, &regs(), "root@pam", &audited, Some("traefik")).unwrap();
        assert_eq!(filtered.iter().map(|g| g.vmid).collect::<Vec<_>>(), vec![100]);
        // ... and cannot see through a caller's own missing scope.
        assert!(list_guests(&store, &regs(), "scoped@pve!t1", &rows, Some("other"))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn access_reports_resolved_scopes() {
        let permission_files = regs();
        let tagged = access(&permission_files, &DocId::Guest(100), &scoped(&["traefik"]));
        assert!(!tagged.read && !tagged.write);
        assert_eq!(
            tagged.scopes.iter().map(|s| s.prefix.to_string()).collect::<Vec<_>>(),
            vec!["traefik", "netbird"]
        );
        let untagged = access(&permission_files, &DocId::Guest(100), &scoped(&[]));
        assert_eq!(untagged.scopes.len(), 1);
        let dc = access(&permission_files, &DocId::Datacenter, &full());
        assert!(dc.read && dc.write && dc.scopes.is_empty());
    }

    #[test]
    fn access_returns_the_tags_only_to_a_caller_who_may_read_the_guest() {
        // This field exists so the editor stops calling `GET /meta/guests`
        // just to learn one guest's tags. It must therefore be filtered the
        // way that endpoint filters the same field, or moving it would have
        // widened who can see a guest's tags.
        let permission_files = regs();

        let mut auditor = full();
        auditor.tags = vec!["traefik".to_string()];
        let seen = access(&permission_files, &DocId::Guest(100), &auditor);
        assert_eq!(seen.tags, vec!["traefik".to_string()]);

        // A scope-only principal has no VM.Audit: it gets the scopes its
        // permission file grants -- resolved against those very tags -- but
        // not the tags themselves.
        let hidden = access(&permission_files, &DocId::Guest(100), &scoped(&["traefik"]));
        assert!(hidden.tags.is_empty(), "no VM.Audit, no tags");
        assert_eq!(hidden.scopes.len(), 2, "the selector still resolved server-side");

        assert!(access(&permission_files, &DocId::Datacenter, &full()).tags.is_empty());
    }

    #[test]
    fn permissions_list_returns_every_permission() {
        let gs = permissions_list(&regs());
        assert_eq!(gs.len(), 1);
        assert_eq!(gs[0].authid, "scoped@pve!t1");
        assert_eq!(gs[0].rules.len(), 2);
    }

    #[test]
    fn gc_removes_documents_and_snapshots_whose_vmid_is_gone() {
        // `docs/DESIGN.md` §6: this replaces the destroy hook and the whole
        // orphan concept.
        let (_dir, store) = store();
        seed(&store, "100", "traefik:\n  host: x\n");
        seed(&store, "999500", "traefik:\n  host: gone\n");
        seed(&store, "datacenter", "a: 1\n");
        store.snapshot(100, "keep").unwrap();
        store.snapshot(999500, "snapA").unwrap();
        store.snapshot(999500, "snapB").unwrap();

        assert_eq!(gc(&store, &[100]).unwrap(), 3, "one document plus two snapshots");
        assert!(read_raw(&store, "100").is_some());
        assert!(read_raw(&store, "999500").is_none());
        assert_eq!(store.list_snapshots(100).unwrap(), vec!["keep".to_string()]);
        assert!(store.list_snapshots(999500).unwrap().is_empty());
        // The datacenter document is never a guest.
        assert!(read_raw(&store, "datacenter").is_some());

        // Idempotent, and a full vmlist removes nothing.
        assert_eq!(gc(&store, &[100]).unwrap(), 0);
        assert_eq!(gc(&store, &[100, 999500]).unwrap(), 0);
        // A whole sweep is expressed with a vmid the store does not have, not
        // with an empty list: an empty vmlist is refused (it is what an
        // un-refreshed pmxcfs cache looks like).
        assert!(gc(&store, &[]).is_err(), "an empty vmlist must be refused");
        assert_eq!(gc(&store, &[999_999]).unwrap(), 2);
        assert!(read_raw(&store, "100").is_none());
        assert!(read_raw(&store, "datacenter").is_some());
    }

    #[test]
    fn gc_re_validates_liveness_inside_the_per_vmid_lock() {
        // The reported race, as an in-crate reproduction: the vmlist snapshot
        // the GC pass started from predates a `PUT` that has already been
        // answered `200`, and the whole-sweep `gc()` deletes that fresh
        // document without a trace. `libexec/gc` takes
        // `cfs_lock_domain("pve-meta-<vmid>")` per candidate and re-reads the
        // vmlist inside it; `gc_purge` is what that re-read is checked by.
        let (_dir, store) = store();
        seed(&store, "100", "traefik:\n  host: live\n");

        // The snapshot Perl read before taking any lock. 999500 does not
        // exist yet, so it is not in it.
        let snapshot = [100u32];
        assert!(gc_candidates(&store, &snapshot).unwrap().is_empty());

        // ... then a guest is created at 999500 and its metadata written.
        seed(&store, "999500", "traefik:\n  host: fresh\n");
        store.snapshot(999500, "s1").unwrap();
        assert_eq!(gc_candidates(&store, &snapshot).unwrap(), vec![999500]);

        // Under the per-vmid lock, the *fresh* vmlist has it: nothing is
        // purged, and the document the PUT stored survives.
        let fresh = [100u32, 999500];
        assert_eq!(gc_purge(&store, 999500, &fresh).unwrap(), 0);
        assert_eq!(read_raw(&store, "999500").as_deref(), Some("traefik:\n  host: fresh\n"));
        assert_eq!(store.list_snapshots(999500).unwrap(), vec!["s1".to_string()]);

        // An empty fresh vmlist is refused rather than read as "every guest
        // is gone" — the guard lives on the destructive call itself, not
        // only in the one Perl caller.
        let err = gc_purge(&store, 999500, &[]).unwrap_err();
        assert_eq!(status(&err), 500, "{err}");
        assert!(read_raw(&store, "999500").is_some());

        // A vmid that really is gone is still purged, with its snapshots.
        assert_eq!(gc_purge(&store, 999500, &[100]).unwrap(), 2);
        assert!(read_raw(&store, "999500").is_none());
        assert!(store.list_snapshots(999500).unwrap().is_empty());
        // Idempotent: the loser of a race against another node's GC pass.
        assert_eq!(gc_purge(&store, 999500, &[100]).unwrap(), 0);

        // For contrast, the unvalidated whole sweep is what the report
        // reproduced — it deletes against the stale snapshot alone.
        seed(&store, "999500", "traefik:\n  host: fresh\n");
        assert_eq!(gc(&store, &snapshot).unwrap(), 1);
        assert!(read_raw(&store, "999500").is_none());
    }

    // -- a file that vanishes under a request is never a 500 ----------------

    #[test]
    fn a_document_that_vanishes_mid_request_is_404_or_absent_never_500() {
        // Reads run unlocked while writes hold `pve-meta-<id>` and the GC
        // holds `pve-meta-gc`, so a file can disappear between any two
        // syscalls. Every one of these used to be an `Error::Io` → 500.
        let (dir, store) = store();
        seed(&store, "100", "traefik:\n  host: x\n");
        let digest = get(&store, "100", None, "json", &full()).unwrap().digest;
        std::fs::remove_file(dir.path().join("100.yaml")).unwrap();

        // A read of a document that is no longer there is the empty document.
        let got = get(&store, "100", None, "json", &full()).unwrap();
        assert_eq!(got.data.unwrap(), json!({}));
        assert_eq!(got.digest, "");

        // A listing does not 500 for the whole cluster because of one of them.
        let rows = vec![GuestInput { vmid: 100, read: true, ..Default::default() }];
        let listed = list_guests(&store, &regs(), "root@pam", &rows, None).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].digest, "");

        // The version poll skips it rather than failing.
        assert!(version(&store, false, None).is_ok());
        assert!(version(&store, true, None).is_ok());

        // A DELETE of a document another caller already removed is that
        // caller's request satisfied.
        let r = del(&store, "100", None, None, &full()).unwrap();
        assert_eq!(r.digest, "");
        assert!(r.touched.is_empty());

        // ... and the stale digest of the vanished document is still a
        // precondition failure, not a 500.
        let err = put(&store, "100", None, "yaml", "a: 1\n", "replace", Some(&digest), false, &full())
            .unwrap_err();
        assert_eq!(status(&err), 409, "{err}");
    }

    // -- registry documents ------------------------------------------------

    #[test]
    fn parse_id_reads_a_registry_id_and_refuses_anything_that_could_leave_the_directory() {
        assert_eq!(
            parse_id("prefixes/traefik").unwrap(),
            DocId::Registry(RegistryKind::PrefixDef, "traefik".to_string()),
        );
        assert_eq!(
            parse_id("permissions/scoped").unwrap(),
            DocId::Registry(RegistryKind::Permission, "scoped".to_string()),
        );
        // The file name *is* the prefix, so a nested prefix is a dotted file
        // name and has to be addressable: `homelab.docker.yaml` declares
        // `homelab.docker`, and refusing that id would put every nested
        // prefix out of the editor's reach.
        assert_eq!(
            parse_id("prefixes/homelab.docker").unwrap(),
            DocId::Registry(RegistryKind::PrefixDef, "homelab.docker".to_string()),
        );
        for bad in [
            "prefixes/../../etc/passwd",
            "prefixes/a/b",
            "prefixes/.hidden",
            "prefixes/",
            "prefixes/a..b",
            "prefixes/a b",
            "operators/traefik",
        ] {
            let err = parse_id(bad).unwrap_err();
            assert_eq!(status(&err), 400, "{bad} was accepted: {err}");
        }
    }

    #[test]
    fn a_prefix_is_read_and_written_like_any_other_document() {
        let (_dir, store) = store();
        let created = put(
            &store,
            "prefixes/homelab",
            None,
            "yaml",
            "selector:\n  all: true\ndescription: Home\n",
            "replace",
            Some(""),
            false,
            &full(),
        )
        .unwrap();
        assert_eq!(created.id, "prefixes/homelab");

        // A view write reaches into it like into any document, with the same
        // digest compare-and-swap.
        let doc = get(&store, "prefixes/homelab", None, "json", &full()).unwrap();
        put(
            &store,
            "prefixes/homelab",
            Some("schema.type"),
            "json",
            "\"object\"",
            "replace",
            Some(&doc.digest),
            false,
            &full(),
        )
        .unwrap();

        // And what came back out is what the loader parses -- the check that
        // matters, since a file it rejects is a file it silently skips.
        let raw = read_raw(&store, "prefixes/homelab").unwrap();
        let ns = registry::parse_prefix("homelab", &raw).unwrap();
        assert_eq!(ns.prefix.to_string(), "homelab");
        assert_eq!(ns.description.as_deref(), Some("Home"));
        assert_eq!(ns.schema.unwrap()["type"], json!("object"));
    }

    #[test]
    fn a_write_that_would_leave_the_loader_nothing_to_read_is_refused() {
        let (_dir, store) = store();
        // No selector: `parse_prefix` refuses it, so the loader would skip
        // the file and the prefix would vanish on a 200.
        let err = put(
            &store,
            "prefixes/homelab",
            None,
            "yaml",
            "description: Home\n",
            "replace",
            Some(""),
            false,
            &full(),
        )
        .unwrap_err();
        assert_eq!(status(&err), 400, "{err}");
        assert!(format!("{err}").contains("not be a valid prefix"), "{err}");
        assert_eq!(read_raw(&store, "prefixes/homelab"), None, "nothing was written");

        // A dry run is refused for the same reason, and by the same check.
        let err = put(
            &store,
            "prefixes/homelab",
            None,
            "yaml",
            "description: Home\n",
            "replace",
            Some(""),
            true,
            &full(),
        )
        .unwrap_err();
        assert_eq!(status(&err), 400, "{err}");

        // An authid that is not an authid is refused on the permission side.
        let err = put(
            &store,
            "permissions/ops",
            None,
            "yaml",
            "authid: not-an-authid\nrules: []\n",
            "replace",
            Some(""),
            false,
            &full(),
        )
        .unwrap_err();
        assert_eq!(status(&err), 400, "{err}");
        assert!(format!("{err}").contains("not be a valid permission file"), "{err}");
    }

    #[test]
    fn a_partial_delete_that_would_break_a_permission_file_is_refused() {
        let (_dir, store) = store();
        put(
            &store,
            "permissions/ops",
            None,
            "yaml",
            "authid: ops@pve!t1\nrules:\n  - prefix: homelab\n    mode: rw\n    selector: {all: true}\n",
            "replace",
            Some(""),
            false,
            &full(),
        )
        .unwrap();

        let err = del(&store, "permissions/ops", Some("authid"), None, &full()).unwrap_err();
        assert_eq!(status(&err), 400, "{err}");
        let raw = read_raw(&store, "permissions/ops").unwrap();
        assert!(registry::parse_permission("ops", &raw).is_ok(), "the file still loads");

        // Removing the file whole is fine: that is an administrator revoking a
        // grant, not a half-written one.
        del(&store, "permissions/ops", None, None, &full()).unwrap();
        assert_eq!(read_raw(&store, "permissions/ops"), None);
    }

    #[test]
    fn a_permission_never_reaches_the_registry_documents() {
        let (_dir, store) = store();
        put(
            &store,
            "prefixes/traefik",
            None,
            "yaml",
            "selector: {all: true}\n",
            "replace",
            Some(""),
            false,
            &full(),
        )
        .unwrap();

        // `scoped@pve!t1` holds `traefik` rw -- on *guests*. A registry
        // document gets no scopes at all, so this is the same 403 the
        // datacenter document gives it.
        let acl = scoped(&["traefik"]);
        let id = parse_id("prefixes/traefik").unwrap();
        assert!(effective(&regs(), &id, &acl).scopes.is_empty());

        // `access` passes the ACL answers through untouched for these documents --
        // which is the point: the two bits differ from the datacenter document's
        // (a registry file is readable by every authenticated user, while writing
        // one is Sys.Modify), so the caller has to say which document it is asking
        // about. `GET /meta/access?id=` is that question; asking `?dc=1` instead
        // answered `read: 0` for a file the caller could certainly read.
        let admin = CallerAcl {
            authid: "writer@pve".to_string(),
            read: true,
            write: true,
            tags: vec![],
        };
        let a = access(&regs(), &id, &admin);
        assert!(a.read && a.write && a.scopes.is_empty());
        let nobody = access(&regs(), &id, &none());
        assert!(!nobody.read && !nobody.write);
        let err = get(&store, "prefixes/traefik", None, "yaml", &acl).unwrap_err();
        assert_eq!(status(&err), 403, "{err}");
        let err = put(
            &store,
            "prefixes/traefik",
            Some("traefik"),
            "json",
            "1",
            "replace",
            None,
            false,
            &acl,
        )
        .unwrap_err();
        assert_eq!(status(&err), 403, "{err}");
    }
}
