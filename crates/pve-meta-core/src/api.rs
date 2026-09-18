//! The API layer: everything `PVE::API2::Ext::Meta` does that is not
//! parameters, PVE ACLs or locking (`docs/DESIGN.md` §8).
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
//! module does everything else: view extraction/replace/merge/remove,
//! touched-path computation, the lint, YAML/JSON rendering, and digesting.
//!
//! ## Authorization
//!
//! PVE's ACLs and nothing else (`docs/DESIGN.md` §4): a caller's read/write
//! access to a document is the two booleans on [`CallerAcl`], computed by
//! Perl. Read without `read` is a 403; write without `write` is a 403. There
//! is no partial access, no per-path authorization and no other check on
//! *what* a write changes -- only whether the caller may write the document
//! at all. The mutation is still planned against a **clone** of the stored
//! document, and the planned document is linted once, the same way for every
//! caller (`docs/DESIGN.md` §7), before it is written.
//!
//! ## Wire contract
//!
//! Everything crosses the Perl/Rust boundary as **native hashes and arrays**
//! (`docs/DESIGN.md` §8): the caller's ACL and tags in, the guest rows in,
//! the documents and results out — perlmod's `serde`-based conversion renders
//! them as Perl scalars/arrays/hashes directly. The single exception is the
//! client-supplied `data` parameter, which is a JSON **string** because that
//! is what the REST parameter is, decoded once here.
//!
//! Errors are [`ApiError`]s whose `Display` is `"<NNN>: <message>"` (an HTTP
//! status prefix and a human-readable message); the Perl layer parses that
//! prefix back out and re-raises via `PVE::Exception::raise`. Messages name
//! paths: there is no disclosure filtering (`docs/DESIGN.md` §1).
//!
//! `ApiError` exists so that status is structural rather than a convention
//! every `?` has to honour by accident: a plain `anyhow::Error` let any new
//! fallible call propagate an unprefixed message straight into a bare PVE
//! 500, with nothing short of reading every call site to notice.

use serde_json::{Map, Value};

use crate::error::Error as CoreError;
use crate::format::{self, Format};
use crate::model;
use crate::patch::{Op, Touched};
use crate::path::Path as DocPath;
use crate::registry::{self, NodeName, PrefixDef, Registry, RegistryFailure, RegistryKind};
use crate::shape::{self, Shape};
use crate::store::{DocId, MetaStore, DISK_FORMAT};
use crate::view;

mod types;
pub use types::*;

/// An HTTP status plus a message, `Display`ed as exactly `"{status}: {msg}"`
/// — the string every `api::*` function returns on its `Err` side, which the
/// Perl layer parses back out to re-raise via `PVE::Exception::raise`. The
/// status is a field, not a string prefix, so a bare `?` cannot produce an
/// error with no status at all: its `From<CoreError>` impl and this module's
/// own constructors (`bad_request`, `forbidden`) are the only ways to make one.
#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: u16,
    pub msg: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.status, self.msg)
    }
}

impl std::error::Error for ApiError {}

/// Maps a [`CoreError`] to its status: `409` digest mismatch, `404` not
/// found, `400` lint/parse/invalid-path/invalid-name/registration/too-large,
/// `503` the cluster filesystem is not there, `500` everything else.
impl From<CoreError> for ApiError {
    fn from(err: CoreError) -> Self {
        let status: u16 = match &err {
            CoreError::DigestMismatch { .. } => 409,
            CoreError::NotFound(_) => 404,
            CoreError::Lint(_)
            | CoreError::Parse { .. }
            | CoreError::InvalidPath(_)
            | CoreError::InvalidName(_)
            | CoreError::Registry(_)
            | CoreError::TooLarge { .. } => 400,
            CoreError::Unavailable(_) => 503,
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
        ApiError { status, msg }
    }
}

fn show_digest(d: &str) -> String {
    if d.is_empty() {
        "<empty>".to_string()
    } else {
        d.to_string()
    }
}

fn bad_request(msg: impl std::fmt::Display) -> ApiError {
    ApiError { status: 400, msg: msg.to_string() }
}

/// A 403 naming the path that was refused (`docs/DESIGN.md` §1: key-name
/// disclosure is out of scope; a message that says nothing is a message
/// nobody can act on).
fn forbidden(path: &DocPath) -> ApiError {
    if path.is_root() {
        return ApiError { status: 403, msg: "not permitted: the whole document".to_string() };
    }
    ApiError { status: 403, msg: format!("not permitted: {path}") }
}

/// Parses an API `id` into a [`DocId`]: a vmid, or a registry document as
/// `prefixes/<name>`.
///
/// The registry form is the API path it is reached at, so the id a caller sends
/// back is the one it read. `<name>` is the file's name, checked with
/// [`registry::is_valid_file_name`]: dotted, because **the file name is the
/// prefix** and `homelab.docker` is a legitimate prefix, but never a slash,
/// a leading dot or a `..`, so an id can never address a file outside its
/// directory.
pub fn parse_id(id: &str) -> Result<DocId, ApiError> {
    if let Some((kind, name)) = id.split_once('/') {
        let kind = match kind {
            "prefixes" => RegistryKind::PrefixDef,
            other => {
                return Err(bad_request(format!(
                    "invalid id '{id}': unknown registry kind '{other}' (expected 'prefixes')"
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
    id.parse::<u32>()
        .map(DocId::Guest)
        .map_err(|_| bad_request(format!("invalid id '{id}': must be a vmid or 'prefixes/<name>'")))
}

/// `node` as a [`NodeName`], or a 400.
fn node_name(node: &str) -> Result<NodeName, ApiError> {
    NodeName::new(node).map_err(|_| bad_request(format!("invalid node name '{node}'")))
}

/// Parses a `view` parameter (a dotted/slash path, or absent = the whole
/// document). No key is reserved and no segment is special: the one lint on
/// the planned document is what refuses a write that would build something
/// the document model does not allow (`docs/DESIGN.md` §7).
fn parse_view(view: Option<&str>) -> Result<DocPath, ApiError> {
    match view {
        Some(s) => DocPath::parse(s).map_err(ApiError::from),
        None => Ok(DocPath::root()),
    }
}

fn view_out(view: Option<&str>) -> String {
    view.unwrap_or("").to_string()
}

/// Parses a view's wire `format`: `json` or `yaml` (`docs/DESIGN.md` §7).
fn parse_view_format(name: &str) -> Result<Format, ApiError> {
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
    /// (`docs/DESIGN.md` §7):
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
fn read_stored(store: &MetaStore, id: &DocId) -> Result<Stored, ApiError> {
    let doc = match store.read(id) {
        Ok(doc) => doc,
        Err(CoreError::NotFound(_)) => return Ok(Stored::absent()),
        // Above the read cap the bytes are never pulled in; the digest still
        // is (`MetaStore::digest_of` caps the same way), so the file stays
        // addressable by a compare-and-swap repair.
        Err(e @ (CoreError::TooLarge { .. } | CoreError::Parse { .. })) => {
            let digest = store.digest_of(id)?.unwrap_or_default();
            return Ok(unrecoverable(digest, None, e.to_string()));
        }
        Err(e) => return Err(e.into()),
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

/// `GET /meta/version` -> `{ token }`: one hash over every document and
/// every prefix file, unscoped (`docs/DESIGN.md` §6).
///
/// # Errors
/// `503:` the cluster filesystem is not available.
pub fn version(store: &MetaStore) -> Result<ApiVersion, ApiError> {
    Ok(ApiVersion { token: store.version()?.token })
}

/// `GET /meta/schemas`: the prefix file format as a schema (`crate::metaschema`),
/// so the editor can show a prefix file as a typed tree the way a prefix's own
/// schema does for a guest document.
pub fn schemas() -> Value {
    crate::metaschema::schemas()
}

/// `GET /meta/access`: `{ read, write }` for one document -- exactly `acl`'s
/// own two ACL answers (`docs/DESIGN.md` §4). With no `id` this is the
/// registry as a whole, which the caller computes the same `acl` for.
///
/// # Errors
/// `503:` the cluster filesystem is not available (`docs/DESIGN.md` §1: no
/// pmxcfs, no answer, not even this one).
pub fn access(store: &MetaStore, acl: &CallerAcl) -> Result<ApiAccess, ApiError> {
    store.check_available()?;
    Ok(ApiAccess { read: acl.read, write: acl.write })
}

/// `GET /meta/prefixes`: without `id`, every prefix file as it is -- packaged
/// and cluster resolved by name, rows carrying `nodes` as parsed
/// ([`Registry::list_prefixes`]); with `id` (a guest), the prefixes reaching
/// that guest, resolved against its `tags` and `node`
/// ([`Registry::prefixes_for_guest`]) -- no `nodes` map, `enforce`/`hidden`/
/// `schema` already effective, most-specific first. Either way, plus the
/// files that did not load.
///
/// # Errors
/// `400:` `node` is not a node name. `500:` a prefix directory that cannot be
/// listed.
pub fn prefixes(
    registry: &Registry,
    node: Option<&str>,
    tags: Option<&[String]>,
) -> Result<Vec<PrefixEntry>, ApiError> {
    let node = node.map(node_name).transpose()?;
    let (loaded, failures) = match tags {
        Some(tags) => registry.prefixes_for_guest(node.as_ref(), tags)?,
        None => registry.list_prefixes()?,
    };
    Ok(prefixes_list(&loaded, &failures))
}

/// The prefixes `put_document` enforces for `doc_id`: for a guest, the set
/// reaching it on the caller's `node` and `tags`
/// ([`Registry::prefixes_for_guest`]) -- the same resolution
/// `GET /meta/prefixes?id=` lists. Nothing for a registry document, which
/// has its own gate.
///
/// # Errors
/// `500:` a prefix directory that cannot be listed.
pub fn effective_prefixes(registry: &Registry, doc_id: &DocId, acl: &CallerAcl) -> Result<Vec<PrefixDef>, ApiError> {
    match doc_id {
        DocId::Guest(_) => Ok(registry.prefixes_for_guest(acl.node.as_ref(), &acl.tags)?.0),
        DocId::Registry(..) => Ok(Vec::new()),
    }
}

/// `GET /meta/prefixes`' rows: every prefix given, in the order given
/// (most-specific first, as the registry loads them), readable by every
/// authenticated user, plus every file that did not load.
pub fn prefixes_list(prefixes: &[PrefixDef], failures: &[RegistryFailure]) -> Vec<PrefixEntry> {
    prefixes
        .iter()
        .cloned()
        .map(PrefixEntry::Loaded)
        .chain(failures.iter().cloned().map(|f| PrefixEntry::Failed(f.into())))
        .collect()
}

/// `GET /meta/guests`: for every guest Perl passed in that the caller has
/// read access to, its metadata (`docs/DESIGN.md` §4, §8).
///
/// A guest the caller cannot read is omitted entirely.
///
/// A `has` that names a comment key is a `400`, as a view naming one is without
/// `comments`: a note is not something a guest has.
///
/// # Errors
/// `400:` if `has` is not a valid path, or names a comment key.
pub fn list_guests(
    store: &MetaStore,
    guests: &[GuestInput],
    has: Option<&str>,
) -> Result<Vec<GuestListEntry>, ApiError> {
    let has_path = has.map(DocPath::parse).transpose()?;
    if let Some(path) = has_path.as_ref().filter(|p| view::names_comment(p)) {
        return Err(bad_request(format!(
            "{path}: a comment key is a note, not something a guest has"
        )));
    }

    let mut out = Vec::with_capacity(guests.len());
    for guest in guests {
        if !guest.read {
            continue;
        }

        let stored = read_stored(store, &DocId::Guest(guest.vmid))?;

        if let Some(path) = &has_path {
            if model::get_path(&stored.value, path).is_none() {
                continue;
            }
        }

        out.push(GuestListEntry {
            vmid: guest.vmid,
            node: guest.node.clone(),
            kind: guest.kind.clone(),
            name: guest.name.clone(),
            tags: guest.tags.clone(),
            digest: stored.digest,
        });
    }
    Ok(out)
}

/// `GET /meta/guests/{vmid}`, and the registry documents' `GET`.
///
/// Access is `acl.read` alone (`docs/DESIGN.md` §4): a caller with it sees the
/// whole document (or the named `view` into it); one without it gets a 403,
/// not an empty document with the real digest (which would be a
/// change-detection oracle over content they may not see). There is no
/// partial access, so `view` only narrows what is returned, never what may be.
///
/// **A document whose content could not be recovered** (`docs/DESIGN.md` §7
/// and [`Stored::unrecoverable`] — it does not parse, it is above the read
/// cap, or it is not a mapping): `format=yaml` with `comments` answers `200`
/// with the file's raw text plus `parse_error`, so an administrator can see
/// what to repair. Every other read — `format=json`, or without `comments` —
/// gets `422` naming the condition. It is reported, never rendered as an
/// empty document: the file is there, and the caller has to know that before
/// writing over it.
///
/// **Comment keys are notes** (`docs/DESIGN.md` §2): without `comments` the
/// answer carries none, at any depth, and `format=yaml` is the canonical dump of
/// what is left; a `view` naming one is a `400`. With `comments` the answer is
/// the stored content, and the root view in YAML the file's own text. `digest`
/// is the file's either way.
///
/// # Errors
/// `400:` invalid id/view/format, or a view naming a comment key without
/// `comments`. `403:` no read access. `422:` the stored document's content
/// could not be recovered.
pub fn get_document(
    store: &MetaStore,
    id: &str,
    view: Option<&str>,
    format_name: &str,
    comments: bool,
    acl: &CallerAcl,
) -> Result<ApiViewDocument, ApiError> {
    let doc_id = parse_id(id)?;
    let fmt = parse_view_format(format_name)?;
    let view_path = parse_view(view)?;
    if !comments {
        refuse_comment(&Value::Null, &view_path)?;
    }

    if !acl.read {
        return Err(forbidden(&view_path));
    }

    let stored = read_stored(store, &doc_id)?;

    if let Some(err) = &stored.unrecoverable {
        if fmt == Format::Yaml && comments {
            if let Some(raw) = &stored.raw {
                return Ok(ApiViewDocument {
                    id: doc_id.to_string(),
                    view: view_out(view),
                    digest: stored.digest,
                    data: None,
                    text: Some(raw.clone()),
                    parse_error: Some(err.clone()),
                });
            }
        }
        return Err(ApiError {
            status: 422,
            msg: format!(
                "the stored document cannot be rendered: {err} \
                 (read it raw with format=yaml&comments=1 (CLI: --comments); \
                 replace it with a full document (no 'view', mode=replace) or delete it)"
            ),
        });
    }

    let result_value = if view.is_some() {
        view::extract(&stored.value, &view_path).unwrap_or_else(|| Value::Object(Map::new()))
    } else {
        stored.value.clone()
    };
    let result_value = if comments { result_value } else { view::strip_comments(&result_value) };

    let (data, text) = match fmt {
        Format::Json => (Some(result_value), None),
        // The root view with the notes renders the file's own text; every
        // other read is a canonical dump of what it sees.
        Format::Yaml => {
            let own_text = stored.raw.as_deref().filter(|raw| !raw.is_empty() && comments);
            let text = if let (None, Some(raw)) = (view, own_text) {
                raw.to_string()
            } else {
                view::render(&result_value, Format::Yaml)
            };
            (None, Some(text))
        }
    };

    Ok(ApiViewDocument {
        id: doc_id.to_string(),
        view: view_out(view),
        digest: stored.digest,
        data,
        text,
        parse_error: None,
    })
}

/// A `400` naming the first comment key in `value` placed at `view_path` (the
/// view itself, when it names one): what a read or a `replace` that did not ask
/// for `comments` may not name or carry (`docs/DESIGN.md` §7).
fn refuse_comment(value: &Value, view_path: &DocPath) -> Result<(), ApiError> {
    match view::first_comment(value, view_path) {
        Some(path) => Err(bad_request(format!(
            "{path}: a comment key is read and written only with comments=1"
        ))),
        None => Ok(()),
    }
}

/// Refuses every write against a document whose content could not be
/// recovered — see [`Stored::unrecoverable`] for the three causes — except
/// the two that *replace the file whole*: a root `replace` and a root
/// `DELETE` (`docs/DESIGN.md` §7).
///
/// The value planned against is the empty document (nothing else can be
/// recovered), so any narrower write would silently discard everything the
/// file contains.
///
/// One condition, one gate: whatever makes a document unrecoverable, the
/// repair is the same two shapes and nothing narrower — a root `merge` very
/// much included, since it is planned against the empty document too. The
/// caller has already been required to hold write access to reach here
/// ([`authorize_write`]), so a root replace/delete needs nothing further.
fn check_repairable(stored: &Stored, view_path: &DocPath, is_merge: bool) -> Result<(), ApiError> {
    let Some(err) = &stored.unrecoverable else {
        return Ok(());
    };
    if view_path.is_root() && !is_merge {
        return Ok(());
    }
    Err(bad_request(format!(
        "the stored document cannot be read back and can only be repaired as a whole: \
         replace it with a full document (no 'view', mode=replace) or delete it ({err})"
    )))
}

/// Every write's authorization gate: `acl.write` alone (`docs/DESIGN.md` §4).
/// There is no per-path check -- what a write changes is irrelevant to
/// whether it is allowed.
fn authorize_write(acl: &CallerAcl) -> Result<(), ApiError> {
    if acl.write {
        return Ok(());
    }
    Err(ApiError {
        status: 403,
        msg: "not permitted: no write access to this document".to_string(),
    })
}

/// Runs the planned mutation against `planned` (already a clone of the
/// stored document) and runs **the** lint on the result.
///
/// The lint lives here, before the `dry_run` branch, so a dry run validates
/// exactly what the write validates — and it is the same lint for every
/// caller, on the whole planned document, naming the offending path
/// (`docs/DESIGN.md` §7). With `kept_notes` -- a `replace` that did not ask for
/// `comments`, whose payload carried none -- a finding on a comment key is about
/// a stored note, and says how to reach it.
fn plan_write(
    planned: &mut Value,
    kept_notes: bool,
    mutate: impl FnOnce(&mut Value) -> Result<Vec<Touched>, ApiError>,
) -> Result<Vec<Touched>, ApiError> {
    let touched = mutate(planned)?;

    let lints = model::lint(planned);
    if !lints.is_empty() {
        let detail = lints
            .iter()
            .map(|l| {
                if kept_notes && view::names_comment(&l.path) {
                    format!("{l} (a stored note; fix it with comments=1)")
                } else {
                    l.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("; ");
        return Err(bad_request(format!("document failed validation: {detail}")));
    }
    Ok(touched)
}

/// The extra gate a registry document passes and a guest document does not:
/// the text about to be written must parse as a prefix (`registry::parse_prefix`).
///
/// The loader **skips** a malformed file with a warning and carries on, so
/// without this check the editor's most likely mistake (a typo in `selector:`)
/// would be answered by the prefix silently disappearing from the list, with
/// a 200 on the write that removed it. The rule is the parser itself, not a
/// copy of it, so what the API accepts and what the loader reads back cannot
/// drift.
///
/// Guest documents have no shape beyond `model::lint`: they
/// hold whatever an administrator puts in them, which is the point of them.
fn check_registry_shape(doc_id: &DocId, text: &str) -> Result<(), ApiError> {
    let (kind, name) = match doc_id {
        DocId::Guest(_) => return Ok(()),
        DocId::Registry(kind, name) => (kind, name),
    };
    let parsed = match kind {
        RegistryKind::PrefixDef => registry::parse_prefix(name, text).map(|_| ()),
    };
    parsed.map_err(|e| {
        let kind = match kind {
            RegistryKind::PrefixDef => "prefix",
        };
        bad_request(format!(
            "the result would not be a valid {kind}: {e} \
             (the loader would skip the file, so the write is refused instead)"
        ))
    })
}

/// `PUT /meta/guests/{vmid}`, and the registry documents' `PUT`.
///
/// `mode` is `"replace"` (default: the view's subtree is replaced by
/// `payload` wholesale — an empty object stores an empty map) or `"merge"`
/// (RFC 7386-style merge-patch relative to the view, where `null` deletes).
///
/// `prefixes` are the declared prefixes in effect for this guest's node
/// ([`effective_prefixes`]): the ones that reach this guest and
/// say `enforce: true` refuse a write that would leave their subtree not
/// matching their schema -- for the paths the write changed, never for
/// what was already wrong elsewhere in the document -- unless `force`. That
/// is the editor's "Save anyway" tick, and it is available to anyone who may
/// write: enforcement makes a mismatch a deliberate act, not an impossible
/// one, so a drifted schema can never lock an administrator out (§7).
///
/// **A `replace` without `comments` keeps the notes it could not see**
/// (`docs/DESIGN.md` §7): its payload may carry no comment key, and every
/// stored note under the view whose subject the payload keeps is put back
/// ([`view::keep_comments`]) before the write is planned, so it is neither lost
/// nor a touched path. A note whose subject the payload drops goes with it.
/// With `comments` the payload is the subtree, notes included. A `merge`
/// names what it changes and is the same either way.
///
/// # Errors
/// `400:` invalid id/view/format/mode/payload, a comment key in a `replace`
/// without `comments`, or the planned document fails the lint. `409:` digest
/// mismatch. `403:` no write access. `422:` the write introduces a finding
/// under an enforcing prefix and `force` is not set.
#[allow(clippy::too_many_arguments)] // matches the PUT endpoint's parameter set 1:1 (docs/DESIGN.md §8)
pub fn put_document(
    store: &MetaStore,
    prefixes: &[PrefixDef],
    id: &str,
    view: Option<&str>,
    format_name: &str,
    payload: &str,
    mode: &str,
    digest: Option<&str>,
    dry_run: bool,
    force: bool,
    comments: bool,
    acl: &CallerAcl,
) -> Result<ApiPutResult, ApiError> {
    let doc_id = parse_id(id)?;
    let fmt = parse_view_format(format_name)?;
    let view_path = parse_view(view)?;

    // (1) Authorize the *request* before computing anything.
    authorize_write(acl)?;

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
        view::parse_patch(payload, fmt)?
    } else {
        view::parse(payload, fmt)?
    };
    let keep_notes = !is_merge && !comments;
    if keep_notes {
        refuse_comment(&payload_value, &view_path)?;
    }

    store.check_precondition(&doc_id, digest)?;
    let stored = read_stored(store, &doc_id)?;
    check_repairable(&stored, &view_path, is_merge)?;

    // (2) Plan the mutation against a *copy*; the stored document is only
    //     touched once the plan has passed every check.
    let mut planned = stored.value.clone();
    let touched = plan_write(&mut planned, keep_notes, |v| {
        if is_merge {
            view::merge(v, &view_path, &payload_value).map_err(ApiError::from)
        } else {
            let subtree = match view::extract(v, &view_path) {
                Some(old) if keep_notes => view::keep_comments(&old, payload_value.clone()),
                _ => payload_value.clone(),
            };
            view::replace(v, &view_path, subtree).map_err(ApiError::from)
        }
    })?;

    let text = format::dump(DISK_FORMAT, &planned);
    check_registry_shape(&doc_id, &text)?;
    if !force {
        check_enforced(prefixes, &doc_id, &stored.value, &planned, &touched)?;
    }

    // (3) Apply — unless there is nothing to apply. A write that changes no
    //     path *and* would put back the bytes already on disk is skipped
    //     entirely: rewriting the file advances its mtime and so
    //     `version()`'s `changed`, while the content token correctly does not
    //     move, and "a merge that touches nothing changes nothing"
    //     (`docs/DESIGN.md` §7) is not true of a file whose timestamp jumped.
    //     Both halves of the condition are needed: `touched: []` alone still
    //     covers the repair of a document that reads back as empty because it
    //     is unrecoverable, and a byte comparison alone would skip nothing a
    //     canonical dump ever produces.
    let unchanged =
        touched.is_empty() && stored.unrecoverable.is_none() && stored.raw.as_deref() == Some(&text);
    let new_digest = if dry_run || unchanged {
        crate::digest::digest(text.as_bytes())
    } else {
        let written = store.put_raw(&doc_id, &text, digest)?.document.digest;
        crate::audit(&format!(
            "{} wrote {doc_id} (view '{}', mode {}): {} path(s) touched, digest {}",
            acl.authid,
            view_out(view),
            if is_merge { "merge" } else { "replace" },
            touched.len(),
            &written[..12.min(written.len())],
        ));
        written
    };

    Ok(ApiPutResult {
        id: doc_id.to_string(),
        view: view_out(view),
        digest: new_digest,
        touched: touched_out(&touched),
    })
}

/// The opt-in schema gate: of the prefixes that reach this guest, those with
/// `enforce: true` refuse a write that *introduces* a finding under them
/// ([`shape::introduced`]: new since the stored document, or on a path the
/// write changed). A guest whose document was already wrong elsewhere is
/// still editable elsewhere. Format checks are never enforced
/// ([`Shape::enforced_findings`]). Registry documents have their own gate
/// ([`check_registry_shape`]).
fn check_enforced(
    prefixes: &[PrefixDef],
    doc_id: &DocId,
    stored: &Value,
    planned: &Value,
    touched: &[Touched],
) -> Result<(), ApiError> {
    let DocId::Guest(_) = doc_id else {
        return Ok(());
    };
    let shape = Shape::of_guest(prefixes);
    let changed: Vec<DocPath> = touched.iter().map(|t| t.path.clone()).collect();
    let refused = shape::introduced(
        &shape.enforced_findings(stored),
        &shape.enforced_findings(planned),
        &changed,
    );
    if refused.is_empty() {
        return Ok(());
    }
    let detail = refused
        .iter()
        .map(|f| format!("{}: {}", f.path, f.msg))
        .collect::<Vec<_>>()
        .join("; ");
    Err(ApiError {
        status: 422,
        msg: format!(
            "the result does not match an enforced schema: {detail} \
             (the prefix declares enforce: true; pass force=1 to store it anyway)"
        ),
    })
}

/// `DELETE /meta/guests/{vmid}`, and the registry documents' `DELETE`:
/// removes the subtree at `view`, or the whole document if `view` is absent.
///
/// Removes **only the current document**: snapshot copies belong to the
/// snapshot hooks and are never touched from the REST API
/// (`docs/DESIGN.md` §9).
///
/// # Errors
/// `400:` invalid id/view. `409:` digest mismatch. `403:` no write access.
pub fn delete_document(
    store: &MetaStore,
    id: &str,
    view: Option<&str>,
    digest: Option<&str>,
    acl: &CallerAcl,
) -> Result<ApiPutResult, ApiError> {
    let doc_id = parse_id(id)?;
    let view_path = parse_view(view)?;

    authorize_write(acl)?;

    store.check_precondition(&doc_id, digest)?;
    let stored = read_stored(store, &doc_id)?;
    // A root DELETE removes the file whole, so it repairs an unrecoverable
    // document exactly like a root replace does.
    check_repairable(&stored, &view_path, false)?;

    let mut planned = stored.value.clone();
    let touched = plan_write(&mut planned, false, |v| {
        view::remove(v, &view_path).map_err(ApiError::from)
    })?;

    // "Is there a file?" is answered by the read that already happened — a
    // missing document is the only one that reports an empty digest — rather
    // than by a fresh `locate`, whose answer could already be stale by the
    // time it is acted on. `MetaStore::delete` is idempotent for the same
    // reason: losing the race to another `DELETE` or to `pve-meta rm` is this
    // request's own outcome, not a 500.
    let existed = !stored.digest.is_empty();
    let new_digest = if view_path.is_root() {
        if store.delete(&doc_id)? {
            crate::audit(&format!("{} removed {doc_id}", acl.authid));
        }
        String::new()
    } else if existed {
        let text = format::dump(DISK_FORMAT, &planned);
        // A partial delete is a write, and a write of a registry document must
        // still leave a file its own loader will read: the same rule has to
        // hold on this path as on `put_document`'s.
        check_registry_shape(&doc_id, &text)?;
        let written = store.put_raw(&doc_id, &text, digest)?.document.digest;
        crate::audit(&format!(
            "{} removed view '{}' of {doc_id}: {} path(s) touched, digest {}",
            acl.authid,
            view_out(view),
            touched.len(),
            &written[..12.min(written.len())],
        ));
        written
    } else {
        String::new()
    };

    Ok(ApiPutResult {
        id: doc_id.to_string(),
        view: view_out(view),
        digest: new_digest,
        touched: touched_out(&touched),
    })
}
