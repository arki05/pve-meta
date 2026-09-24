//! The API layer: everything `PVE::API2::Ext::Meta` does that is not
//! parameters, PVE ACLs or locking (`docs/DESIGN.md` §6). Perl hands this
//! module a [`CallerAcl`] (§4); errors are [`ApiError`], naming the path (§1).

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

/// An HTTP status plus a message, `Display`ed as `"{status}: {msg}"` -- every
/// `api::*` function's `Err` side, reraised by Perl via `PVE::Exception::raise`.
/// `status` is a field, not a string prefix, so it can't be left off by accident.
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

/// Parses an API `id` into a [`DocId`]: a vmid, or `prefixes/<name>` (the
/// path the registry document is reached at). `<name>` is checked with
/// [`registry::is_valid_file_name`] so an id can never address a file outside
/// its directory.
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
/// document); no segment is reserved (`docs/DESIGN.md` §5).
fn parse_view(view: Option<&str>) -> Result<DocPath, ApiError> {
    match view {
        Some(s) => DocPath::parse(s).map_err(ApiError::from),
        None => Ok(DocPath::root()),
    }
}

fn view_out(view: Option<&str>) -> String {
    view.unwrap_or("").to_string()
}

/// Parses a view's wire `format`: `json` or `yaml` (`docs/DESIGN.md` §5).
fn parse_view_format(name: &str) -> Result<Format, ApiError> {
    Format::from_ext(name)
        .ok_or_else(|| bad_request(format!("invalid format '{name}': expected 'json' or 'yaml'")))
}

/// What one document read gave us.
struct Stored {
    /// The stored document, or the empty document when [`Stored::unrecoverable`] is set.
    value: Value,
    /// The digest on disk (`""` for a document that does not exist).
    digest: String,
    /// `Some(reason)` when the file exists but is not valid YAML, is above
    /// the read cap, or does not parse to a mapping (`docs/DESIGN.md` §5).
    unrecoverable: Option<String>,
    /// The file's own text, or `None` when the bytes were never read.
    raw: Option<String>,
}

impl Stored {
    /// The empty document: what a caller sees for an id with no file
    /// (`docs/DESIGN.md` §2).
    fn absent() -> Stored {
        Stored {
            value: Value::Object(Map::new()),
            digest: String::new(),
            unrecoverable: None,
            raw: None,
        }
    }
}

/// The empty document plus `reason`, keeping `digest` and `raw` from disk.
fn unrecoverable(digest: String, raw: Option<String>, reason: String) -> Stored {
    Stored {
        value: Value::Object(Map::new()),
        digest,
        unrecoverable: Some(reason),
        raw,
    }
}

/// Reads `id`'s document without ever failing on its content: unrecoverable
/// content comes back as the empty document with its real digest and a
/// reason (`docs/DESIGN.md` §5), rather than as an error.
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
/// read access to, its metadata (`docs/DESIGN.md` §4, §6).
///
/// A guest the caller cannot read is omitted entirely.
///
/// `has` is checked against the same notes-left-out view a read returns
/// (`docs/DESIGN.md` §2): naming a note finds nothing, like any other path a
/// stripped document does not have.
///
/// # Errors
/// `400:` if `has` is not a valid path.
pub fn list_guests(
    store: &MetaStore,
    guests: &[GuestInput],
    has: Option<&str>,
) -> Result<Vec<GuestListEntry>, ApiError> {
    let has_path = has.map(DocPath::parse).transpose()?;

    let mut out = Vec::with_capacity(guests.len());
    for guest in guests {
        if !guest.read {
            continue;
        }

        let id = DocId::Guest(guest.vmid);
        let digest = match &has_path {
            // The filter is the only thing a listing needs the content for.
            Some(path) => {
                let stored = read_stored(store, &id)?;
                if model::get_path(&view::strip_comments(&stored.value), path).is_none() {
                    continue;
                }
                stored.digest
            }
            // Without it, the row carries the digest and nothing else, and
            // that is a `stat` and a hash rather than a YAML parse per guest.
            None => store.digest_of(&id)?.unwrap_or_default(),
        };

        out.push(GuestListEntry {
            vmid: guest.vmid,
            node: guest.node.clone(),
            kind: guest.kind.clone(),
            name: guest.name.clone(),
            tags: guest.tags.clone(),
            digest,
        });
    }
    Ok(out)
}

/// `GET /meta/guests/{vmid}`, and the registry documents' `GET`.
///
/// `acl.read` alone gates access (`docs/DESIGN.md` §4): without it, a 403
/// naming the view -- never an empty document with a real digest, which would
/// let a caller without access detect changes to content it cannot see.
///
/// An unrecoverable document (`docs/DESIGN.md` §5): `format=yaml&comments=1`
/// answers `200` with the raw text and `parse_error`; every other read is a
/// `422` naming the condition instead of an empty document.
///
/// Without `comments`, notes are stripped at any depth before `view` is taken
/// (`docs/DESIGN.md` §2); a `view` naming one then finds nothing, like any
/// other absent path. `digest` is the file's either way.
///
/// # Errors
/// `400:` invalid id/view/format, including a view through an array or a
/// scalar. `403:` no read access. `422:` the stored document's content could
/// not be recovered.
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

    // Notes are stripped from the *whole* document before the view is taken,
    // not after: a view naming a note key then finds nothing there, the same
    // as any other absent path, with no rule of its own (`docs/DESIGN.md` §2).
    let base = if comments { stored.value.clone() } else { view::strip_comments(&stored.value) };
    let result_value = if view.is_some() {
        // A view that runs through an array or a scalar is refused here as it
        // is on a write (400); only a *missing* path reads as the empty
        // document.
        view::extract(&base, &view_path)?.unwrap_or_else(|| Value::Object(Map::new()))
    } else {
        base
    };

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

/// Refuses every write against an unrecoverable document
/// ([`Stored::unrecoverable`]) except a root `replace` or root `DELETE`,
/// which replace the file whole rather than plan against the empty document
/// (`docs/DESIGN.md` §5).
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
fn authorize_write(acl: &CallerAcl) -> Result<(), ApiError> {
    if acl.write {
        return Ok(());
    }
    Err(ApiError {
        status: 403,
        msg: "not permitted: no write access to this document".to_string(),
    })
}

/// Runs `mutate` against `planned` and lints the result (`docs/DESIGN.md`
/// §5). `kept_notes` marks a finding on a comment key as a stored note, with
/// how to reach it.
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
/// the text about to be written must parse as a prefix
/// (`registry::parse_prefix`), the same parser the loader uses, so a write
/// the loader would silently skip is refused instead.
fn check_registry_shape(doc_id: &DocId, text: &str) -> Result<(), ApiError> {
    let (kind, name) = match doc_id {
        DocId::Guest(_) => return Ok(()),
        DocId::Registry(kind, name) => (kind, name),
    };
    let parsed = match kind {
        RegistryKind::PrefixDef => registry::parse_prefix(name, text).map(|_| ()),
    };
    parsed.map_err(|e| {
        bad_request(format!(
            "the result would not be a valid {}: {e} \
             (the loader would skip the file, so the write is refused instead)",
            kind.noun(),
        ))
    })
}

/// `PUT /meta/guests/{vmid}`, and the registry documents' `PUT`.
///
/// `mode` is `"replace"` (default: the view's subtree wholesale) or `"merge"`
/// (RFC 7386-style, `null` deletes). `prefixes` are the effective set for this
/// guest ([`effective_prefixes`]): an `enforce: true` one refuses a write that
/// breaks its schema on a changed path, unless `force` (`docs/DESIGN.md` §5).
///
/// A `replace` without `comments` keeps the stored note of every key and
/// map it keeps ([`view::keep_comments`], `docs/DESIGN.md` §2); with
/// `comments` the payload is the subtree, notes included. A `merge` names
/// what it changes either way.
///
/// # Errors
/// `400:` invalid id/view/format/mode/payload, or the planned document fails
/// the lint. `409:` digest mismatch. `403:` no write access. `422:` the write
/// introduces a finding under an enforcing prefix and `force` is not set.
#[allow(clippy::too_many_arguments)] // matches the PUT endpoint's parameter set 1:1 (docs/DESIGN.md §6)
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
            let subtree = match view::extract(v, &view_path)? {
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

    // (3) Apply -- unless nothing would change: `touched: []` alone still
    //     covers an unrecoverable-document repair that reads back empty, so
    //     skipping also requires the on-disk bytes to already match
    //     (`docs/DESIGN.md` §5: a write that changes nothing rewrites nothing).
    let unchanged =
        touched.is_empty() && stored.unrecoverable.is_none() && stored.raw.as_deref() == Some(&text);
    let new_digest = if dry_run || unchanged {
        // What `put_raw` would refuse, a dry run refuses too; a write that
        // changes nothing never gets that far, dry or not.
        if !unchanged {
            MetaStore::check_size(text.len() as u64)?;
        }
        crate::digest::digest(text.as_bytes())
    } else {
        let written = store.put_raw(&doc_id, &text, digest)?.digest;
        crate::audit(&format!(
            "{} wrote {doc_id} (view '{}', mode {}): {} path(s) touched, digest {}",
            acl.authid,
            view_out(view),
            if is_merge { "merge" } else { "replace" },
            touched.len(),
            crate::digest::short(&written),
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
/// (`docs/DESIGN.md` §7).
///
/// A prefix that only exists as a packaged file has nothing to remove: a
/// whole-document delete is a 404, a view delete writes the cluster file that
/// shadows it (`docs/DESIGN.md` §3), and says so in the audit line.
///
/// A view delete that touches nothing -- the key is not there -- writes
/// nothing and answers with the digest as it stands (`docs/DESIGN.md` §5: a
/// write that changes nothing rewrites nothing); for a packaged-only prefix
/// that is what keeps it from growing a cluster copy of the packaged file.
///
/// # Errors
/// `400:` invalid id/view. `404:` only a packaged prefix file is there.
/// `409:` digest mismatch. `403:` no write access.
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

    // What the read saw and what a write could touch are two files for a
    // registry document (`docs/DESIGN.md` §3). A whole-document delete of a
    // prefix that only exists packaged would otherwise diff the packaged
    // content, unlink nothing and answer 200 for a file still on disk.
    let own_file = store.has_own_file(&doc_id)?;
    let packaged_only = match &doc_id {
        DocId::Registry(kind, name) => (!own_file).then(|| (kind.noun(), name.clone())),
        DocId::Guest(_) => None,
    };
    if let Some((noun, name)) = &packaged_only {
        if view_path.is_root() && !stored.digest.is_empty() {
            return Err(ApiError {
                status: 404,
                msg: format!(
                    "no cluster file for {noun} '{name}' (the packaged file cannot be removed)"
                ),
            });
        }
    }

    let mut planned = stored.value.clone();
    let touched = plan_write(&mut planned, false, |v| {
        view::remove(v, &view_path).map_err(ApiError::from)
    })?;

    // Whether the file existed is the read's own answer, not a fresh `locate`
    // that could already be stale. `MetaStore::delete` is idempotent for the
    // same reason: losing the race to another `DELETE` or `pve-meta rm` is
    // this request's own outcome, not a 500.
    let existed = !stored.digest.is_empty();
    let new_digest = if view_path.is_root() {
        if store.delete(&doc_id)? {
            crate::audit(&format!("{} removed {doc_id}", acl.authid));
        }
        String::new()
    } else if touched.is_empty() {
        // Nothing to remove, so nothing to write: the digest is the one the
        // read saw, the packaged file's for a prefix that has no cluster copy
        // -- and must not get one for this.
        stored.digest.clone()
    } else if existed {
        let text = format::dump(DISK_FORMAT, &planned);
        // A partial delete is a write, and a write of a registry document must
        // still leave a file its own loader will read: the same rule has to
        // hold on this path as on `put_document`'s.
        check_registry_shape(&doc_id, &text)?;
        let written = store.put_raw(&doc_id, &text, digest)?.digest;
        // Removing a view of a packaged-only prefix edits no packaged file: it
        // writes the cluster file that shadows it, the packaged content minus
        // the view. The audit line names which of the two happened.
        let shadowing = if packaged_only.is_some() {
            " as a new cluster file shadowing the packaged one"
        } else {
            ""
        };
        crate::audit(&format!(
            "{} removed view '{}' of {doc_id}{shadowing}: {} path(s) touched, digest {}",
            acl.authid,
            view_out(view),
            touched.len(),
            crate::digest::short(&written),
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
