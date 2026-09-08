//! The API layer: everything `PVE::API2::Ext::Meta` does that is not
//! parameters, PVE ACLs or locking (`docs/DESIGN.md` §3).
//!
//! `crates/pve-meta-perl` exports these functions to Perl as
//! `PVE::RS::Meta::api_*`, adding nothing but a [`crate::store::MetaStore`]
//! rooted at `$PVE_META_ROOT`. They live here, in the platform-independent
//! core, because they are the project's security boundary: the write
//! authorization below must be unit-testable on any machine, without
//! `libperl-dev` and without a PVE cluster.
//!
//! Perl does parameters, PVE ACL checks, `scopes` lookup, the vmlist and the
//! per-document write lock, and hands this module a `grants_json` describing
//! the caller's effective access (`docs/DESIGN.md` §2, [`crate::scopes::Grants`]).
//! This module does everything else: view extraction/replace/merge/remove,
//! touched-path computation against those grants, YAML/JSON rendering, and
//! digesting.
//!
//! ## Authorization
//!
//! Authorization is decided **from the request, never from a diff**
//! (`docs/DESIGN.md` §8):
//!
//! 1. `Grants::can_write(view)` must hold before anything is computed, and a
//!    caller without `full_write` may not write the root view at all;
//! 2. the mutation is planned against a **clone** of the stored document and
//!    every path the plan touches is checked with `Grants::check_write`;
//! 3. a write touching the datacenter document's `scopes` map additionally
//!    requires `full_write` — the access-control map is admin-only whatever
//!    the scopes say (`docs/DESIGN.md` §9, [`check_scopes_write`]);
//! 4. only then is the planned value written.
//!
//! A 403 never names a path the caller cannot read: the
//! error message is a generic "not permitted" unless the caller has read
//! access to the offending path's parent.
//!
//! ## Wire contract
//!
//! A view's arbitrary content (`data`/`text`) crosses to Perl as a JSON
//! **string** when `format=json` (`data_json`, decoded there with
//! `JSON::PP` to preserve numbers/booleans faithfully) or as already-encoded
//! YAML text when `format=yaml` (`text`, passed straight through). Grants
//! (`grants_json`, the per-guest `grants` field of `api_list_guests`, and
//! `PVE::RS::Meta::api_grants`'s own return value) are JSON strings too,
//! since they cross the boundary in both directions and keeping one
//! convention avoids two different perlmod-conversion paths for what is
//! structurally the same kind of value. Everything else (ids, digests, view
//! paths, key lists, touched entries) is plain and well-typed, so perlmod's
//! `serde`-based conversion renders it directly as a native Perl
//! scalar/array/hash.
//!
//! `data` (JSON) is an **unordered** object on the wire (`docs/DESIGN.md`
//! §8): Perl hashes do not preserve order. Clients that need the document's
//! key order use `format=yaml`, or the ordered `keys` array that every read
//! returns alongside it.
//!
//! Errors are `anyhow::Error`s whose `Display` is `"<NNN>: <message>"` (an
//! HTTP status prefix and a human-readable message); the Perl layer parses
//! that prefix back out and re-raises via `PVE::Exception::raise`.

use std::collections::BTreeSet;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::Error as CoreError;
use crate::format::{self, Format};
use crate::model;
use crate::patch::{Op, Touched};
use crate::path::Path as DocPath;
use crate::scopes::{self, Grants};
use crate::store::{DocId, MetaStore, WriteGate, DISK_FORMAT};
use crate::view;

fn unix_secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Maps a [`CoreError`] to the `"NNN: message"` string the Perl layer
/// expects: `409` digest mismatch, `404` not found, `400`
/// lint/parse/invalid-path/invalid-name/invalid-scopes/too-large, `500`
/// everything else.
pub fn api_err(err: CoreError) -> anyhow::Error {
    let status: u16 = match &err {
        CoreError::DigestMismatch { .. } => 409,
        CoreError::NotFound(_) => 404,
        CoreError::Lint(_)
        | CoreError::Parse { .. }
        | CoreError::InvalidPath(_)
        | CoreError::InvalidName(_)
        | CoreError::InvalidScopes(_)
        | CoreError::TooLarge { .. } => 400,
        CoreError::Io(_) | CoreError::Other(_) => 500,
    };
    // An empty digest is what a *missing* document reports; render it
    // visibly rather than as nothing at all (review F13).
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

/// A 403 that never teaches the caller anything about a document they cannot
/// read (`docs/DESIGN.md` §8, review F3).
///
/// The old message named the first offending path from a diff against the
/// *real* stored content, which turned any authenticated principal into a
/// value-confirmation oracle and let them walk the whole key structure of
/// any document — including the datacenter document's `scopes` map, i.e.
/// every other principal's authid and prefixes. The path is now included
/// only when the caller may read its parent, i.e. only when they could have
/// discovered it with a plain `GET` anyway.
fn forbidden(grants: &Grants, path: &DocPath) -> anyhow::Error {
    if path.is_root() {
        // Reveals nothing: the caller asked for the whole document.
        return anyhow::anyhow!("403: not permitted: the whole document");
    }
    if may_name(grants, path) {
        anyhow::anyhow!("403: not permitted: {path}")
    } else {
        anyhow::anyhow!("403: not permitted")
    }
}

/// The one predicate deciding whether an error message may spell out `path`:
/// only when the caller could have discovered it with a plain `GET`, i.e.
/// when they may read its parent (`docs/DESIGN.md` §8).
///
/// Shared by [`forbidden`] (403) and [`lint_error`] (400). The 400 path used
/// its own answer — "always" — which turned a lint finding anywhere in the
/// document into a spelling of that key for any principal who could attempt a
/// write (review pass 3 R6). The root path names nothing document-specific
/// and is always allowed, and so is a path the caller may read *itself*: a
/// scoped writer holds `traefik`, so "traefik__ must be a string" tells them
/// only what their own payload already said.
fn may_name(grants: &Grants, path: &DocPath) -> bool {
    path.is_root()
        || grants.can_read(path)
        || path.parent().is_some_and(|p| grants.can_read(&p))
}

/// Parses an API `id` (a vmid, or the literal `"datacenter"`) into a
/// [`DocId`].
pub fn parse_id(id: &str) -> Result<DocId, anyhow::Error> {
    if id == "datacenter" {
        return Ok(DocId::Datacenter);
    }
    id.parse::<u32>()
        .map(DocId::Guest)
        .map_err(|_| bad_request(format!("invalid id '{id}': must be a vmid or 'datacenter'")))
}

fn id_str(id: DocId) -> String {
    match id {
        DocId::Guest(vmid) => vmid.to_string(),
        DocId::Datacenter => "datacenter".to_string(),
    }
}

/// Parses a `view` parameter (a dotted/slash path, or absent = the whole
/// document).
///
/// The `scopes` map is an **opaque leaf** (`docs/DESIGN.md` §9, review P9):
/// `?view=scopes` addresses the whole access-control map, and nothing
/// addresses one entry of it. Its keys are PVE authids, which may contain the
/// path separator (`john.doe@pve`), so a per-entry view could never have
/// worked for a large class of legitimate principals; entries are added and
/// removed by writing the map (`mode=merge` with `null` deletes one).
///
/// A **comment key is a leaf** (`docs/DESIGN.md` §9, review pass 4 Q1): its
/// value must be a string, so nothing can legitimately live below one.
/// `?view=traefik__` (the note itself) is fine; `?view=traefik__.x` is not,
/// and neither is a comment key in any *intermediate* segment — the last
/// segment is the only one `model::lint_at` anchors on, so
/// `?view=p.q__.r` would otherwise let a narrow write materialise `q__` as a
/// map through `view::descend_creating` and store a document that the
/// whole-document `model::lint` refuses. That is checked here, at the one
/// place every caller passes through, rather than in the write path, so the
/// answer is the same for a `full_write` caller, for GET and for DELETE.
fn parse_view(view: Option<&str>) -> Result<DocPath, anyhow::Error> {
    let path = match view {
        Some(s) => DocPath::parse(s).map_err(api_err)?,
        None => return Ok(DocPath::root()),
    };
    if path.segments().len() > 1 && path.segments()[0] == model::SCOPES_KEY {
        return Err(bad_request(
            "the 'scopes' map is addressed as a whole: use view 'scopes' \
             (a single entry is not path-addressable -- an authid may contain dots); \
             add or remove one entry with mode=merge",
        ));
    }
    // Every segment but the last: the last one may be a comment key (that is
    // the note itself), an intermediate one may not.
    if let Some((_, ancestors)) = path.segments().split_last() {
        if let Some(seg) = ancestors.iter().find(|s| model::is_comment_key(s)) {
            return Err(bad_request(format!(
                "invalid view '{path}': '{seg}' is a comment key and a comment key's value \
                 is a string ('{}' documents the map it sits in, 'k{}' documents the \
                 sibling key 'k'), so nothing is addressable below it; view the comment \
                 key itself, or a path that does not pass through one",
                model::COMMENT_SUFFIX,
                model::COMMENT_SUFFIX,
            )));
        }
    }
    Ok(path)
}

fn view_out(view: Option<&str>) -> String {
    view.unwrap_or("").to_string()
}

/// Parses a view's wire `format`: `json` or `yaml` (`docs/DESIGN.md` §3).
fn parse_view_format(name: &str) -> Result<Format, anyhow::Error> {
    Format::from_ext(name)
        .ok_or_else(|| bad_request(format!("invalid format '{name}': expected 'json' or 'yaml'")))
}

fn parse_grants(grants_json: &str) -> Result<Grants, anyhow::Error> {
    let grants: Grants =
        serde_json::from_str(grants_json).map_err(|e| bad_request(format!("invalid grants: {e}")))?;
    // The wire boundary applies the same prefix rules as the datacenter
    // document's own gate (review pass 3 §5, `scopes.rs:55`): an empty prefix
    // deserialized to `Path::root()` and covered every path of every
    // document, `scopes` included.
    grants.validate().map_err(api_err)?;
    Ok(grants)
}

/// What one document read gave us: the parsed value (the *empty* document if
/// the stored text does not parse), the digest of the bytes actually on disk,
/// and the parse failure if there was one.
struct Stored {
    value: Value,
    digest: String,
    parse_error: Option<String>,
    /// The bytes behind `digest`, kept from the *same* read so a caller that
    /// hands them out cannot pair them with a different digest.
    raw: String,
}

/// Reads `id`'s document, or the empty document with digest `""` if it does
/// not exist (`docs/DESIGN.md` §2: "a non-existent document is an empty
/// document ... there is no explicit create").
///
/// A document whose text does not parse is *not* an error here (review pass 3
/// R1): it comes back as the empty document with its real digest and a
/// `parse_error`, because both write handlers read before they plan, so a
/// fatal read made a hand-edit typo unrepairable through the API — the one
/// thing the P2 fix set out to prevent. Readers and writers each decide what
/// to do with it; the real digest is what lets a repairing write still carry
/// a compare-and-swap precondition.
fn read_or_empty(store: &MetaStore, id: DocId) -> Result<Stored, anyhow::Error> {
    match store.read(id) {
        Ok(doc) => Ok(Stored {
            value: doc.value,
            digest: doc.digest,
            parse_error: doc.parse_error,
            raw: doc.raw,
        }),
        Err(CoreError::NotFound(_)) => Ok(Stored {
            value: Value::Object(Map::new()),
            digest: String::new(),
            parse_error: None,
            raw: String::new(),
        }),
        Err(e) => Err(api_err(e)),
    }
}

/// [`read_or_empty`] for every caller that must not be taken down by *one*
/// unreadable document: the two write handlers, and [`list_guests`].
///
/// A `GET` of a document above the store's read cap is a plain `400` naming
/// the size — that is the cap doing its job, and it names the document the
/// caller actually asked for. Everything else must degrade instead of
/// failing, or the cap becomes R1 with a different message:
///
/// * a **write** would leave the file unrepairable through the API — too big
///   to read, too big to rewrite, removable only with `rm` as root;
/// * a **listing** would 400 for every principal because of one oversized
///   file belonging to one guest, which is the cluster-wide blast radius R1
///   is about.
///
/// It is handled like unparseable content — the empty value plus a
/// `parse_error` — with the digest hashed straight off the disk, so
/// [`check_repairable`] lets through the same two whole-file shapes and
/// nothing narrower, and a listing shows the document with no keys.
fn read_tolerant(store: &MetaStore, id: DocId) -> Result<Stored, anyhow::Error> {
    match store.read(id) {
        Err(e @ CoreError::TooLarge { .. }) => Ok(Stored {
            value: Value::Object(Map::new()),
            digest: store.digest_of(id).map_err(api_err)?.unwrap_or_default(),
            parse_error: Some(e.to_string()),
            raw: String::new(),
        }),
        _ => read_or_empty(store, id),
    }
}

/// Renders a touched list for the wire, collapsing anything inside the
/// `scopes` map to `scopes` itself and de-duplicating the result.
///
/// [`Grants::check_write`] and `parse_view` both treat that map as an opaque
/// leaf (`docs/DESIGN.md` §9); this is the one place the client saw it, and
/// it did not — a `PUT ?view=scopes` reported `scopes.john.doe@pve`, which is
/// not a valid view, cannot be parsed back into its segments, and is
/// indistinguishable from a three-segment path (review pass 3 §5,
/// `api.rs:209`).
fn touched_out(touched: &[Touched]) -> Vec<ApiTouched> {
    let mut out: Vec<ApiTouched> = Vec::with_capacity(touched.len());
    for t in touched {
        let entry = ApiTouched {
            path: scopes::opaque_scopes_path(&t.path).to_string(),
            op: match t.op {
                Op::Set => "set".to_string(),
                Op::Delete => "delete".to_string(),
            },
        };
        if !out.contains(&entry) {
            out.push(entry);
        }
    }
    out
}

#[derive(Debug, Clone, Serialize)]
pub struct ApiVersion {
    pub token: String,
    pub changed: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct GuestListEntry {
    pub vmid: u32,
    pub node: Option<String>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub name: Option<String>,
    pub digest: String,
    pub keys: Vec<String>,
    /// `1` for an **orphan**: a document whose vmid is no longer in the
    /// vmlist (`docs/DESIGN.md` §9). Absent otherwise, so an ordinary row is
    /// unchanged on the wire.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orphan: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApiTouched {
    pub path: String,
    pub op: String,
}

/// `api_get`'s result: exactly one of `data_json` (format=json) or `text`
/// (format=yaml) is populated (`docs/DESIGN.md` §3). `keys` is the visible
/// value's top-level keys **in document order** — the order-preserving
/// counterpart to `data`, which is an unordered object on the wire (review
/// F23).
#[derive(Debug, Clone, Serialize)]
pub struct ApiViewDocument {
    pub id: String,
    pub view: String,
    pub digest: String,
    pub keys: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_json: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Present only when the *stored* document is not valid YAML
    /// (`docs/DESIGN.md` §9, review pass 3 R1): the parser's message. `data`
    /// / `text` then describe the empty document and `keys` is empty — there
    /// is no structure to report — but `digest` is the real digest of the
    /// bytes on disk, so a full-write caller can repair the document with a
    /// compare-and-swap root replace.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parse_error: Option<String>,
    /// The document's raw text, present only alongside `parse_error` and only
    /// for a caller with `full_read`.
    ///
    /// A caller who may read the whole document learns nothing from the bytes
    /// they could not already read — and needs them, because "repair the
    /// document" means editing text no view can render (review P2's own
    /// remit: "a full-ACL admin cannot GET the document to see what to fix").
    /// A scoped caller gets `parse_error` alone: a document nobody can parse
    /// has no key structure to filter, so there is no honest subset of it to
    /// return.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
}

/// `api_put`/`api_delete`'s result.
#[derive(Debug, Clone, Serialize)]
pub struct ApiPutResult {
    pub id: String,
    pub view: String,
    pub digest: String,
    pub touched: Vec<ApiTouched>,
}

/// One entry of `api_list_guests`'s input: the vmlist row Perl already has,
/// plus that guest's grants. Perl owns the vmlist and the display names —
/// there is exactly one reader of `/etc/pve/.vmlist` per request, and guest
/// config parsing is not re-implemented here (review §5, `api.rs:304`).
#[derive(Debug, Deserialize)]
struct GuestInput {
    vmid: u32,
    #[serde(default)]
    node: Option<String>,
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    name: Option<String>,
    /// This guest's [`Grants`], as a JSON string (see the wire contract).
    grants: String,
}

/// `api_version()` -> `{ token, changed }`.
pub fn version(store: &MetaStore) -> Result<ApiVersion, anyhow::Error> {
    let v = store.version().map_err(api_err)?;
    Ok(ApiVersion {
        token: v.token,
        changed: unix_secs(v.changed),
    })
}

/// `api_grants($authid)`: the datacenter document's `scopes` entries for
/// `authid`, as a JSON array of `{prefix, mode}` (empty array if it has
/// none, or if there is no datacenter document at all).
///
/// Reads leniently: anything malformed — the `scopes` container itself, or an
/// entry, whoever it belongs to — is skipped with a warning and grants
/// nothing, rather than failing a lookup that every guest request depends on
/// (`docs/DESIGN.md` §9, review F7/P2/P3). The document itself is read
/// without lint, so an unrelated out-of-band edit elsewhere in
/// `datacenter.yaml` cannot deny service either.
///
/// **A `datacenter.yaml` that does not parse at all, or that is too large to
/// read, grants nothing and warns** (review pass 3 R1). This function runs on
/// every guest GET/PUT/DELETE, `/meta/guests`, `/meta/access` and
/// `/meta/datacenter`, so any error it returns is a cluster-wide outage for
/// every principal — including the administrator who has to repair the file.
/// Guest operations for ACL holders keep working; scope holders lose their
/// grants until the document is fixed, which is the same failure mode as a
/// malformed entry.
///
/// # Errors
/// `500:` only if the datacenter document cannot be read from the disk at all
/// (an I/O failure).
pub fn grants(store: &MetaStore, authid: &str) -> Result<String, anyhow::Error> {
    let dc_value = match store.read(DocId::Datacenter) {
        Ok(doc) => {
            if let Some(err) = &doc.parse_error {
                tracing::warn!(
                    error = %err,
                    "datacenter.yaml does not parse: every scope grants nothing until it is repaired"
                );
            }
            doc.value
        }
        Err(CoreError::NotFound(_)) => Value::Object(Map::new()),
        Err(e @ CoreError::TooLarge { .. }) => {
            tracing::warn!(
                error = %e,
                "datacenter.yaml is too large to read: every scope grants nothing until it is repaired"
            );
            Value::Object(Map::new())
        }
        Err(e) => return Err(api_err(e)),
    };
    let scopes = scopes::scopes_for(&dc_value, authid);
    Ok(serde_json::to_string(&scopes).expect("Vec<Scope> always serializes"))
}

/// `api_list_guests($guests_json, $has, $orphans)`: for every guest Perl
/// passed in, the metadata the caller may see.
///
/// A guest the caller can read *nothing* of is omitted entirely
/// (`docs/DESIGN.md` §8: "reads require a grant"), and `node`/`name` are
/// returned only to a caller with `VM.Audit` on that guest (`full_read`).
///
/// With `orphans` (the caller has datacenter read, `Sys.Audit` on `/`), the
/// list additionally contains every document whose vmid is **not** in the
/// rows Perl passed in, marked `orphan: 1` (`docs/DESIGN.md` §9, review P5).
/// Those are documents whose guest is gone — destroyed while its node was
/// down, restored onto a different vmid, config removed by hand. Without this
/// they are invisible to every API listing, yet still replicated by pmxcfs
/// and still inherited by a future guest created at that vmid; a datacenter
/// writer removes one with `DELETE /meta/guests/<vmid>`.
///
/// # Errors
/// `400:` if `guests_json` or `has` is malformed.
pub fn list_guests(
    store: &MetaStore,
    guests_json: &str,
    has: Option<&str>,
    orphans: bool,
) -> Result<Vec<GuestListEntry>, anyhow::Error> {
    let guests: Vec<GuestInput> = serde_json::from_str(guests_json)
        .map_err(|e| bad_request(format!("invalid guest list: {e}")))?;
    let has_path = has.map(DocPath::parse).transpose().map_err(api_err)?;
    let matches_has = |visible: &Value| match &has_path {
        Some(path) => model::get_path(visible, path).is_some(),
        None => true,
    };

    let mut out = Vec::with_capacity(guests.len());
    for guest in &guests {
        let grants = parse_grants(&guest.grants)?;
        let readable = grants.readable_prefixes();
        if readable.is_empty() {
            continue;
        }

        let stored = read_tolerant(store, DocId::Guest(guest.vmid))?;
        let visible = view::filter(&stored.value, &readable);

        if !matches_has(&visible) {
            continue;
        }

        out.push(GuestListEntry {
            vmid: guest.vmid,
            node: grants.full_read.then(|| guest.node.clone()).flatten(),
            kind: guest.kind.clone(),
            name: grants.full_read.then(|| guest.name.clone()).flatten(),
            digest: stored.digest,
            keys: model::top_level_keys(&visible),
            orphan: None,
        });
    }

    if orphans {
        let known: std::collections::HashSet<u32> = guests.iter().map(|g| g.vmid).collect();
        for vmid in store.guest_ids().map_err(api_err)? {
            if known.contains(&vmid) {
                continue;
            }
            let stored = read_tolerant(store, DocId::Guest(vmid))?;
            if !matches_has(&stored.value) {
                continue;
            }
            // No guest means no `/vms/<vmid>` ACL to consult and no vmlist
            // row to take `node`/`type`/`name` from: the datacenter read is
            // the whole permission, and it sees the whole document.
            out.push(GuestListEntry {
                vmid,
                node: None,
                kind: None,
                name: None,
                digest: stored.digest,
                keys: model::top_level_keys(&stored.value),
                orphan: Some(1),
            });
        }
    }
    Ok(out)
}

/// `api_get($id, $view, $format, $comments, $grants_json)`.
///
/// With a `view`, requires read access to it ([`Grants::can_read`]); without
/// one, returns the union of the caller's readable subtrees
/// ([`Grants::readable_prefixes`] + [`view::filter`]) -- the whole document
/// for full-read grants.
///
/// A caller with **no** read grant covering anything in the document gets a
/// 403, not an empty document with the real digest (`docs/DESIGN.md` §8,
/// review F21): the digest alone is a change-detection oracle over content
/// they may not see. Callers with a *partial* grant still get the whole
/// document's digest — scoped writers need it for compare-and-swap PUTs.
///
/// A document whose stored text does not parse is answered, not refused
/// (review pass 3 R1): `200` with an empty value, no `keys`, the real digest
/// and a `parse_error` describing the syntax failure — plus the raw text for
/// a `full_read` caller, who needs it to write the repair and could have read
/// it anyway.
///
/// # Errors
/// `400:` invalid id/view/format/grants, or a stored document above the read
/// size cap. `403:` no read grant, or a `view` that is not readable.
pub fn get_document(
    store: &MetaStore,
    id: &str,
    view: Option<&str>,
    format_name: &str,
    comments: bool,
    grants_json: &str,
) -> Result<ApiViewDocument, anyhow::Error> {
    let doc_id = parse_id(id)?;
    let grants = parse_grants(grants_json)?;
    let fmt = parse_view_format(format_name)?;
    let view_path = parse_view(view)?;

    let readable = grants.readable_prefixes();
    if readable.is_empty() {
        return Err(forbidden(&grants, &view_path));
    }
    if view.is_some() && !grants.can_read(&view_path) {
        return Err(forbidden(&grants, &view_path));
    }

    let stored = read_or_empty(store, doc_id)?;

    let mut result_value = if view.is_some() {
        view::extract(&stored.value, &view_path).unwrap_or_else(|| Value::Object(Map::new()))
    } else {
        view::filter(&stored.value, &readable)
    };

    if !comments {
        model::strip_comments(&mut result_value);
    }

    let keys = model::top_level_keys(&result_value);
    let (data_json, text) = match fmt {
        Format::Json => (
            Some(serde_json::to_string(&result_value).expect("serde_json::Value always serializes")),
            None,
        ),
        Format::Yaml => (None, Some(view::render(&result_value, Format::Yaml))),
    };

    // The raw text of an unparseable document goes only to a caller who may
    // read the whole document anyway (see `ApiViewDocument::raw`), and comes
    // from the same read as `digest`, so the two always describe one state of
    // the file.
    let raw = match (&stored.parse_error, grants.full_read) {
        (Some(_), true) => Some(stored.raw),
        _ => None,
    };

    Ok(ApiViewDocument {
        id: id_str(doc_id),
        view: view_out(view),
        digest: stored.digest,
        keys,
        data_json,
        text,
        parse_error: stored.parse_error,
        raw,
    })
}

/// Which [`crate::store::WriteGate`] a caller's write is stored under: the full
/// document lint for a caller who may replace the whole document, the
/// caller-linted gate for everybody else (see [`plan_write`]).
///
/// This must stay in step with `plan_write`'s choice, or the store would
/// re-apply a lint the API layer deliberately narrowed — and re-leak the
/// paths it deliberately withheld.
fn write_gate(grants: &Grants) -> WriteGate {
    if grants.full_write {
        WriteGate::Document
    } else {
        WriteGate::CallerLinted
    }
}

/// Refuses every write against a document whose content could not be
/// recovered — it does not parse, or it is above the store's read cap —
/// except the two that *replace the file whole*: a root `replace` and a root
/// `DELETE` (review pass 3 R1).
///
/// The value planned against is the empty document (nothing else can be
/// recovered), so any narrower write would silently discard everything the
/// file contains — a scoped `PUT ?view=traefik` would turn the whole
/// document into `{traefik: …}`. `authorize_view_write` has already
/// established that a root view requires `full_write`, so this is the
/// documented repair path and nothing else: read the document (`parse_error`
/// plus, for a full reader, its raw text), fix it, `PUT` it whole with the
/// digest — or delete it.
///
/// The parser's own message is included: it is positional ("did not find
/// expected key at line 4 column 1"), never a quotation of the document, so
/// it teaches a scoped caller nothing about content they may not read.
fn check_repairable(
    stored: &Stored,
    view_path: &DocPath,
    is_merge: bool,
) -> Result<(), anyhow::Error> {
    let Some(err) = &stored.parse_error else {
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

/// Every write's up-front, request-shaped authorization gate
/// (`docs/DESIGN.md` §8, review F1): the caller must be able to write the
/// view they named, and only a caller with full write access may write the
/// root view.
fn authorize_view_write(grants: &Grants, view_path: &DocPath) -> Result<(), anyhow::Error> {
    if view_path.is_root() && !grants.full_write {
        return Err(anyhow::anyhow!(
            "403: not permitted: writing the whole document requires full write access; \
             name an explicit view inside a writable prefix"
        ));
    }
    if !grants.can_write(view_path) {
        return Err(forbidden(grants, view_path));
    }
    Ok(())
}

/// Runs the planned mutation against `planned` (already a clone of the
/// stored document) and validates it.
///
/// The lint lives here, *before* the `dry_run` branch, so a dry run validates
/// exactly what the write validates (review F14): the per-view lint inside
/// [`view::replace`]/[`view::merge`] only sees the payload.
///
/// **How much of the document is linted depends on how much of it the caller
/// may write** (`docs/DESIGN.md` §9: "strict lint runs only on the content
/// being written", review pass 3 R6):
///
/// * a caller with `full_write` is offered the whole document, so the whole
///   document is linted — a root replace is the shape that repairs it, and
///   they can perform one;
/// * anyone else may only write inside `view_path`, so only the subtree at
///   `view_path` is linted. Before this, one out-of-band bad key anywhere
///   blocked every write by everybody who could not replace the whole
///   document, and the 400 named the offending key to a caller who could not
///   read it. The store's own gate is narrowed to match
///   ([`crate::store::WriteGate`]), since it would otherwise re-apply the whole-
///   document lint — and the same message — one layer down.
///
/// Either way the rendered message names only paths the caller may read; see
/// [`lint_error`].
///
/// **The narrowing's safety net** (review pass 4 Q1). Narrowing the *scope*
/// of the lint may not narrow the *rules*: whatever a scoped write is allowed
/// to do, it may never leave behind a document that the whole-document
/// `model::lint` — the gate every `full_write` caller still runs — rejects
/// for a reason that was not already there, because that would let a
/// low-privileged principal permanently disable a high-privileged one's
/// narrow writes. So a non-`full_write` write additionally compares
/// `model::lint(stored)` before and after and refuses the write if the
/// finding set *grew*. Pre-existing findings are carried through untouched —
/// that availability is exactly what the narrowing bought — and the refusal
/// is deliberately path-free, since by construction the new finding may sit
/// outside what the caller may read.
fn plan_write(
    doc_id: DocId,
    view_path: &DocPath,
    planned: &mut Value,
    grants: &Grants,
    mutate: impl FnOnce(&mut Value) -> Result<Vec<Touched>, anyhow::Error>,
) -> Result<Vec<Touched>, anyhow::Error> {
    // Computed before the mutation, from the same value the mutation runs
    // against, so no clone of the document is needed.
    let findings_before = (!grants.full_write).then(|| lint_findings(planned));

    let touched = mutate(planned)?;

    if let Err(denied) = grants.check_write(&touched) {
        return Err(forbidden(grants, &denied));
    }

    let lints = if grants.full_write {
        model::lint(planned)
    } else {
        // The written subtree, in the document's own coordinates. `None`
        // means the plan wrote nothing there (a no-op merge), which has
        // nothing to lint.
        match view::extract(planned, view_path) {
            // `lint_at`, not `lint_relaxed_at`: the key rule and the
            // comment-key-value rule for the view's *own* segment live in its
            // parent map, so a subtree-only lint would drop them (see
            // `model::lint_at`).
            Some(subtree) => model::lint_at(&subtree, view_path),
            None => Vec::new(),
        }
    };
    if !lints.is_empty() {
        return Err(lint_error(grants, lints));
    }

    if let Some(before) = findings_before {
        let after = lint_findings(planned);
        if !after.is_subset(&before) {
            return Err(bad_request(
                "document failed validation: this write would leave the stored document \
                 invalid as a whole -- narrow it, or ask a caller with full write access \
                 to repair the document first",
            ));
        }
    }

    check_scopes_write(doc_id, planned, &touched, grants)?;
    Ok(touched)
}

/// [`model::lint`]'s findings as a comparable set, for [`plan_write`]'s
/// before/after invariant. Rendered rather than structural because
/// [`model::Lint`] is not `Ord`/`Hash`, and its `Display` (`path: msg`) is
/// exactly the identity we want to compare.
fn lint_findings(doc: &Value) -> BTreeSet<String> {
    model::lint(doc).iter().map(ToString::to_string).collect()
}

/// Renders lint findings into a `400` that names only what the caller may
/// read (`docs/DESIGN.md` §8, review pass 3 R6).
///
/// `forbidden()` has always applied this filter to 403s; the 400 path joined
/// every finding's absolute path verbatim, so a principal holding one prefix
/// could learn the existence and exact spelling of keys in subtrees they
/// cannot read by attempting a write. Findings they may read are reported as
/// before — the message has to stay actionable — and the rest collapse into
/// one path-free sentence.
fn lint_error(grants: &Grants, lints: Vec<model::Lint>) -> anyhow::Error {
    let (visible, hidden): (Vec<_>, Vec<_>) =
        lints.into_iter().partition(|l| may_name(grants, &l.path));
    let shown = visible
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ");
    let elsewhere = match hidden.len() {
        0 => String::new(),
        1 => "1 further problem in a part of the document you cannot read".to_string(),
        n => format!("{n} further problems in parts of the document you cannot read"),
    };
    let detail = match (shown.is_empty(), elsewhere.is_empty()) {
        (false, true) => shown,
        (false, false) => format!("{shown}; and {elsewhere}"),
        (true, false) => elsewhere,
        // `lints` is never empty at the call site; keep a sane message
        // rather than an assertion in the request path.
        (true, true) => "the document failed validation".to_string(),
    };
    bad_request(format!("document failed validation: {detail}"))
}

/// The two rules for a write that touches the datacenter document's `scopes`
/// map — the one subtree that decides who may write anything at all.
///
/// 1. **`scopes` is admin-only, whatever the scope grants**
///    (`docs/DESIGN.md` §9, review P1). `Grants::check_write` alone is not
///    enough here: it asks only whether the *path* is covered, and a scope
///    covering `scopes` (or, before it was rejected, an empty-prefix scope
///    covering everything) makes that true — so a `full_write:false`
///    principal could grant itself, or anyone else, arbitrary prefixes on
///    every guest document. Editing the access-control map requires the
///    datacenter ACL (`Sys.Modify`), full stop; no scope can confer it.
/// 2. **A malformed entry never reaches the disk.** `scopes` is
///    behaviour-controlling configuration inside a free-form user document,
///    so it is validated at write time, naming the offending entry
///    (`docs/DESIGN.md` §8, review F7). Reads are lenient
///    ([`scopes::scopes_for`]); this is the gate that keeps the document
///    parseable in the first place.
fn check_scopes_write(
    doc_id: DocId,
    planned: &Value,
    touched: &[Touched],
    grants: &Grants,
) -> Result<(), anyhow::Error> {
    if doc_id != DocId::Datacenter {
        return Ok(());
    }
    let scopes_path = DocPath::parse(model::SCOPES_KEY).expect("'scopes' is a valid path");
    if !touched.iter().any(|t| scopes_path.is_prefix_of(&t.path)) {
        return Ok(());
    }
    if !grants.full_write {
        return Err(forbidden(grants, &scopes_path));
    }
    scopes::parse_scopes(planned).map_err(api_err)?;
    Ok(())
}

/// `api_put($id, $view, $format, $payload, $mode, $digest, $dry_run, $grants_json)`.
///
/// `mode` is `"replace"` (default: the view's subtree is replaced by
/// `payload` wholesale — an empty object stores an empty map) or `"merge"`
/// (RFC 7386-style merge-patch relative to the view, where `null` deletes).
///
/// # Errors
/// `400:` invalid id/view/format/mode/payload/grants/scopes. `409:` digest
/// mismatch. `403:` the view is not writable, or a planned touched path is
/// outside the caller's write grants.
#[allow(clippy::too_many_arguments)] // matches the PUT endpoint's parameter set 1:1 (docs/DESIGN.md §3)
pub fn put_document(
    store: &MetaStore,
    id: &str,
    view: Option<&str>,
    format_name: &str,
    payload: &str,
    mode: &str,
    digest: Option<&str>,
    dry_run: bool,
    grants_json: &str,
) -> Result<ApiPutResult, anyhow::Error> {
    let doc_id = parse_id(id)?;
    let grants = parse_grants(grants_json)?;
    let fmt = parse_view_format(format_name)?;
    let view_path = parse_view(view)?;

    // (1) Authorize the *request* before computing anything.
    authorize_view_write(&grants, &view_path)?;

    // A merge payload is a patch (`null` deletes, review F8); a replace
    // payload is document content (no nulls anywhere).
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
        view::parse_patch(payload, fmt, &view_path).map_err(api_err)?
    } else {
        view::parse(payload, fmt, &view_path).map_err(api_err)?
    };

    store.check_precondition(doc_id, digest).map_err(api_err)?;
    let stored = read_tolerant(store, doc_id)?;
    check_repairable(&stored, &view_path, is_merge)?;

    // (2) Plan the mutation against a *copy*; the stored document is only
    //     touched once the plan has passed every check.
    let mut planned = stored.value.clone();
    let touched = plan_write(doc_id, &view_path, &mut planned, &grants, |v| {
        if is_merge {
            view::merge(v, &view_path, &payload_value).map_err(api_err)
        } else {
            view::replace(v, &view_path, payload_value.clone()).map_err(api_err)
        }
    })?;

    let text = format::dump(DISK_FORMAT, &planned);

    // (3) Apply.
    let new_digest = if dry_run {
        crate::digest::digest(text.as_bytes())
    } else {
        store
            .put_raw_gated(doc_id, &text, digest, write_gate(&grants))
            .map_err(api_err)?
            .document
            .digest
    };

    Ok(ApiPutResult {
        id: id_str(doc_id),
        view: view_out(view),
        digest: new_digest,
        touched: touched_out(&touched),
    })
}

/// `api_delete($id, $view, $digest, $grants_json)`: removes the subtree at
/// `view`, or the whole document if `view` is absent.
///
/// Removes **only the current document**: snapshot copies belong to the
/// guest lifecycle hooks and are never touched from the REST API
/// (`docs/DESIGN.md` §8, review F19).
///
/// # Errors
/// `400:` invalid id/view/grants. `409:` digest mismatch. `403:` the view is
/// not writable, or a planned touched path is outside the caller's write
/// grants.
pub fn delete_document(
    store: &MetaStore,
    id: &str,
    view: Option<&str>,
    digest: Option<&str>,
    grants_json: &str,
) -> Result<ApiPutResult, anyhow::Error> {
    let doc_id = parse_id(id)?;
    let grants = parse_grants(grants_json)?;
    let view_path = parse_view(view)?;

    authorize_view_write(&grants, &view_path)?;

    store.check_precondition(doc_id, digest).map_err(api_err)?;
    let stored = read_tolerant(store, doc_id)?;
    // A root DELETE removes the file whole, so it repairs an unparseable
    // document exactly like a root replace does.
    check_repairable(&stored, &view_path, false)?;

    let mut planned = stored.value.clone();
    let touched = plan_write(doc_id, &view_path, &mut planned, &grants, |v| {
        view::remove(v, &view_path).map_err(api_err)
    })?;

    let exists = store.locate(doc_id).map_err(api_err)?.is_some();
    let new_digest = if view_path.is_root() {
        if exists {
            store.delete(doc_id).map_err(api_err)?;
        }
        String::new()
    } else if exists {
        let text = format::dump(DISK_FORMAT, &planned);
        store
            .put_raw_gated(doc_id, &text, digest, write_gate(&grants))
            .map_err(api_err)?
            .document
            .digest
    } else {
        String::new()
    };

    Ok(ApiPutResult {
        id: id_str(doc_id),
        view: view_out(view),
        digest: new_digest,
        touched: touched_out(&touched),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn grants_json(full_read: bool, full_write: bool, scopes: Value) -> String {
        json!({"full_read": full_read, "full_write": full_write, "scopes": scopes}).to_string()
    }

    fn rw_traefik() -> String {
        grants_json(false, false, json!([{"prefix": "traefik", "mode": "rw"}]))
    }

    fn zero_grant() -> String {
        grants_json(false, false, json!([]))
    }

    /// A store over a fresh tempdir. No global state: every test owns its
    /// own root, so the suite runs in parallel like the rest of the crate's.
    fn store() -> (tempfile::TempDir, MetaStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = MetaStore::new(dir.path());
        (dir, store)
    }

    fn seed(store: &MetaStore, id: &str, text: &str) {
        store.put_raw(parse_id(id).unwrap(), text, None).unwrap();
    }

    fn read_raw(store: &MetaStore, id: &str) -> Option<String> {
        store.read(parse_id(id).unwrap()).ok().map(|d| d.raw)
    }

    fn status(err: &anyhow::Error) -> u16 {
        err.to_string()[..3].parse().expect("errors are 'NNN: msg'")
    }

    #[test]
    fn zero_grant_token_cannot_create_structure_through_an_empty_merge() {
        // The live reproduction of review F1/F2: `PUT ?view=zzz_hacked.deep
        // &mode=merge` with `{}` used to write `zzz_hacked: {deep: {}}` into
        // a real guest document while reporting `touched: []`, because
        // `check_write([])` is vacuously Ok.
        let (_dir, store) = store();
        seed(&store, "100", "traefik:\n  spec:\n    host: ct100.example\n");
        let before = read_raw(&store, "100").unwrap();

        for grants in [zero_grant(), rw_traefik()] {
            for (view, mode, payload) in [
                (Some("zzz_hacked.deep"), "merge", "{}"),
                (Some("zzz_hacked"), "merge", "{}"),
                (Some("zzz_hacked.deep"), "replace", "{}"),
                (Some("scopes"), "merge", "{}"),
            ] {
                let err = put_document(&store, "100", view, "json", payload, mode, None, false, &grants)
                    .expect_err("must be refused");
                assert_eq!(status(&err), 403, "{view:?}/{mode}: {err}");
                assert_eq!(read_raw(&store, "100").unwrap(), before, "{view:?}/{mode} mutated the document");
            }
        }
    }

    #[test]
    fn zero_grant_token_cannot_write_the_root_view() {
        let (_dir, store) = store();
        seed(&store, "100", "traefik:\n  host: x\n");
        let before = read_raw(&store, "100").unwrap();
        for grants in [zero_grant(), rw_traefik()] {
            for (mode, payload) in [("merge", "{}"), ("replace", "{\"a\": 1}")] {
                let err = put_document(&store, "100", None, "json", payload, mode, None, false, &grants)
                    .expect_err("root writes need full write access");
                assert_eq!(status(&err), 403, "{mode}: {err}");
            }
            let err = delete_document(&store, "100", None, None, &grants).expect_err("root delete");
            assert_eq!(status(&err), 403, "{err}");
        }
        assert_eq!(read_raw(&store, "100").unwrap(), before);
    }

    #[test]
    fn forbidden_message_is_generic_when_the_path_is_not_readable() {
        // Review F3: the 403 must not confirm guessed values or leak key
        // structure to a caller with no read grant covering the path.
        let (_dir, store) = store();
        seed(&store, "100", "traefik:\n  host: x\nnetbird:\n  groups:\n  - lan\n");

        let err = put_document(
            &store,
            "100",
            Some("netbird.groups"),
            "json",
            "[\"guess\"]",
            "replace",
            None,
            false,
            &rw_traefik(),
        )
        .unwrap_err();
        assert_eq!(err.to_string(), "403: not permitted", "leaked: {err}");

        // A full-read caller who merely lacks write does get the path named.
        let err2 = put_document(
            &store,
            "100",
            Some("netbird.groups"),
            "json",
            "[\"guess\"]",
            "replace",
            None,
            false,
            &grants_json(true, false, json!([])),
        )
        .unwrap_err();
        assert!(err2.to_string().contains("netbird.groups"), "{err2}");
    }

    #[test]
    fn no_grant_read_is_forbidden_not_an_empty_document_with_a_real_digest() {
        // Review F21.
        let (_dir, store) = store();
        seed(&store, "datacenter", "scopes:\n  a@pve:\n  - prefix: x\n    mode: ro\n");
        let err = get_document(&store, "datacenter", None, "json", true, &zero_grant()).unwrap_err();
        assert_eq!(status(&err), 403);
        assert_eq!(err.to_string(), "403: not permitted: the whole document");

        // A partial grant still gets the whole-document digest (needed for
        // compare-and-swap PUTs).
        let real = get_document(&store, "datacenter", None, "json", true, &grants_json(true, true, json!([])))
            .unwrap();
        let partial = get_document(
            &store,
            "datacenter",
            None,
            "json",
            true,
            &grants_json(false, false, json!([{"prefix": "scopes", "mode": "ro"}])),
        )
        .unwrap();
        assert_eq!(partial.digest, real.digest);
    }

    #[test]
    fn merge_with_null_deletes_end_to_end() {
        // Review F8, in both wire formats.
        let (_dir, store) = store();
        seed(&store, "100", "traefik:\n  spec:\n    host: a\n    port: 1\n");
        let grants = rw_traefik();

        let r = put_document(
            &store,
            "100",
            Some("traefik.spec"),
            "json",
            "{\"host\": null}",
            "merge",
            None,
            false,
            &grants,
        )
        .unwrap();
        assert_eq!(r.touched.len(), 1);
        assert_eq!(r.touched[0].op, "delete");
        assert_eq!(read_raw(&store, "100").unwrap(), "traefik:\n  spec:\n    port: 1\n");

        put_document(
            &store,
            "100",
            Some("traefik"),
            "yaml",
            "spec: null\n",
            "merge",
            None,
            false,
            &grants,
        )
        .unwrap();
        assert_eq!(read_raw(&store, "100").unwrap(), "traefik: {}\n");
    }

    #[test]
    fn replace_with_an_empty_object_stores_an_empty_map() {
        // Review F9: a namespace whose value is legitimately `{}` must round
        // trip through the editor.
        let (_dir, store) = store();
        seed(&store, "100", "traefik: {}\n");
        let r = put_document(&store, "100", Some("traefik"), "json", "{}", "replace", None, false, &rw_traefik())
            .unwrap();
        assert!(r.touched.is_empty());
        assert_eq!(read_raw(&store, "100").unwrap(), "traefik: {}\n");
    }

    #[test]
    fn dry_run_validates_exactly_what_the_write_validates() {
        // Review F14: whole-document lint used to live only inside put_raw,
        // so `?view=foo__&dry_run=1` returned 200 and the real PUT 400'd.
        let (_dir, store) = store();
        seed(&store, "100", "traefik:\n  host: x\n");
        let grants = grants_json(true, true, json!([]));

        let dry = put_document(&store, "100", Some("foo__"), "json", "{\"a\": 1}", "replace", None, true, &grants);
        let wet = put_document(&store, "100", Some("foo__"), "json", "{\"a\": 1}", "replace", None, false, &grants);
        assert!(dry.is_err(), "dry run must reject what the write rejects");
        assert!(wet.is_err());
        assert_eq!(status(&dry.unwrap_err()), 400);
        assert_eq!(status(&wet.unwrap_err()), 400);
        assert_eq!(read_raw(&store, "100").unwrap(), "traefik:\n  host: x\n");
    }

    #[test]
    fn dry_run_checks_the_digest_and_never_writes() {
        let (_dir, store) = store();
        seed(&store, "100", "traefik:\n  host: x\n");
        let grants = grants_json(true, true, json!([]));
        let err = put_document(
            &store,
            "100",
            Some("traefik"),
            "json",
            "{\"host\": \"y\"}",
            "replace",
            Some("deadbeef"),
            true,
            &grants,
        )
        .unwrap_err();
        assert_eq!(status(&err), 409);

        let ok = put_document(
            &store,
            "100",
            Some("traefik"),
            "json",
            "{\"host\": \"y\"}",
            "replace",
            None,
            true,
            &grants,
        )
        .unwrap();
        assert_eq!(ok.touched.len(), 1);
        assert_eq!(read_raw(&store, "100").unwrap(), "traefik:\n  host: x\n");
    }

    #[test]
    fn get_then_put_with_the_empty_digest_creates_a_document() {
        // Review F13: the documented create flow used to 409 forever.
        let (_dir, store) = store();
        let grants = grants_json(true, true, json!([]));
        let got = get_document(&store, "999", None, "yaml", true, &grants).unwrap();
        assert_eq!(got.digest, "");

        let put = put_document(
            &store,
            "999",
            Some("traefik"),
            "json",
            "{\"host\": \"new\"}",
            "replace",
            Some(""),
            false,
            &grants,
        )
        .unwrap();
        assert!(!put.digest.is_empty());
        assert_eq!(read_raw(&store, "999").unwrap(), "traefik:\n  host: new\n");

        // A stale empty digest against an existing document is a 409 whose
        // message renders the empty side visibly.
        let err = put_document(
            &store,
            "999",
            Some("traefik"),
            "json",
            "{\"host\": \"other\"}",
            "replace",
            Some(""),
            false,
            &grants,
        )
        .unwrap_err();
        assert_eq!(status(&err), 409);
        assert!(err.to_string().contains("<empty>"), "{err}");
    }

    #[test]
    fn malformed_scopes_are_rejected_at_write_time() {
        // Review F7(a).
        let (_dir, store) = store();
        seed(&store, "datacenter", "scopes:\n  good@pve:\n  - prefix: x\n    mode: ro\n");
        let grants = grants_json(true, true, json!([]));
        let before = read_raw(&store, "datacenter").unwrap();

        let err = put_document(
            &store,
            "datacenter",
            Some("scopes"),
            "json",
            "{\"bad@pve\": [{\"prefix\": \"x\", \"mode\": \"readwrite\"}]}",
            "merge",
            None,
            false,
            &grants,
        )
        .unwrap_err();
        assert_eq!(status(&err), 400);
        assert!(err.to_string().contains("bad@pve"), "{err}");
        assert_eq!(read_raw(&store, "datacenter").unwrap(), before);

        // The same for the other two write-time scope rules
        // (`docs/DESIGN.md` §9): an empty prefix, and a non-authid key.
        for (payload, expected) in [
            ("{\"ok@pve\": [{\"prefix\": \"\", \"mode\": \"rw\"}]}", "must not be empty"),
            ("{\"not-an-authid\": [{\"prefix\": \"x\", \"mode\": \"rw\"}]}", "PVE authid"),
            (
                "{\"ok@pve\": [{\"prefix\": \"scopes.other@pve\", \"mode\": \"rw\"}]}",
                "as a whole",
            ),
        ] {
            let err = put_document(
                &store,
                "datacenter",
                Some("scopes"),
                "json",
                payload,
                "merge",
                None,
                false,
                &grants,
            )
            .unwrap_err();
            assert_eq!(status(&err), 400, "{payload}: {err}");
            assert!(err.to_string().contains(expected), "{payload}: {err}");
            assert_eq!(read_raw(&store, "datacenter").unwrap(), before);
        }

        // An unrelated write to the same document is not affected.
        put_document(
            &store,
            "datacenter",
            Some("other"),
            "json",
            "{\"k\": 1}",
            "replace",
            None,
            false,
            &grants,
        )
        .unwrap();
    }

    #[test]
    fn a_scoped_principal_can_write_its_own_comment_key() {
        // Review F11: a scope on `p` covers the sibling comment key `p__`.
        let (_dir, store) = store();
        seed(&store, "100", "traefik:\n  host: x\n");
        put_document(
            &store,
            "100",
            Some("traefik__"),
            "json",
            "\"the ingress config\"",
            "replace",
            None,
            false,
            &rw_traefik(),
        )
        .unwrap();
        assert!(read_raw(&store, "100").unwrap().contains("traefik__"));

        // ... but not another key's comment.
        let err = put_document(
            &store,
            "100",
            Some("netbird__"),
            "json",
            "\"nope\"",
            "replace",
            None,
            false,
            &rw_traefik(),
        )
        .unwrap_err();
        assert_eq!(status(&err), 403);
    }

    #[test]
    fn no_scope_can_write_the_scopes_map() {
        // Review P1, reproduced against the unmodified crate: a
        // `full_write:false` principal holding one broad rw scope could grant
        // itself (or anyone) arbitrary prefixes on every guest document.
        // `docs/DESIGN.md` §9: editing `scopes` needs the datacenter ACL,
        // whatever the scopes say.
        let (_dir, store) = store();
        seed(&store, "datacenter", "scopes:\n  good@pve:\n  - prefix: traefik\n    mode: rw\nother: 1\n");
        let before = read_raw(&store, "datacenter").unwrap();

        // The broadest scope that is legal at all: an empty prefix is now
        // refused at the wire boundary itself (see
        // `an_empty_scope_prefix_is_refused_at_the_wire_boundary`), which is
        // the P1 hole closed one layer earlier.
        let broad = grants_json(false, false, json!([{"prefix": "scopes", "mode": "rw"}, {"prefix": "other", "mode": "rw"}]));
        let on_scopes = grants_json(false, false, json!([{"prefix": "scopes", "mode": "rw"}]));
        let payload = "{\"evil@pve\": [{\"prefix\": \"traefik\", \"mode\": \"rw\"}]}";

        for grants in [&broad, &on_scopes] {
            for mode in ["merge", "replace"] {
                let err =
                    put_document(&store, "datacenter", Some("scopes"), "json", payload, mode, None, false, grants)
                        .expect_err("writing scopes without full write must be refused");
                assert_eq!(status(&err), 403, "{mode}: {err}");
                assert_eq!(read_raw(&store, "datacenter").unwrap(), before, "{mode} wrote anyway");
            }
            // ... and the same for DELETE, at the map and at an entry.
            let err = delete_document(&store, "datacenter", Some("scopes"), None, grants)
                .expect_err("deleting scopes without full write must be refused");
            assert_eq!(status(&err), 403, "{err}");
            assert_eq!(read_raw(&store, "datacenter").unwrap(), before);

            // A dry run is refused identically (no oracle, no shortcut).
            let dry = put_document(&store, "datacenter", Some("scopes"), "json", payload, "merge", None, true, grants)
                .expect_err("dry run must refuse what the write refuses");
            assert_eq!(status(&dry), 403, "{dry}");

            // The rest of the document is still writable through the scope,
            // so this is an actor check on `scopes`, not a blanket refusal.
            // (Dry run: it runs the whole plan and authorization, and leaves
            // `before` intact for the next iteration.)
            if grants == &broad {
                put_document(&store, "datacenter", Some("other"), "json", "2", "replace", None, true, grants)
                    .expect("a broad scope still writes ordinary keys");
            }
        }

        // A full-write caller is unaffected, with or without any scope.
        put_document(
            &store,
            "datacenter",
            Some("scopes"),
            "json",
            payload,
            "merge",
            None,
            false,
            &grants_json(true, true, json!([])),
        )
        .expect("Sys.Modify may edit scopes");
        assert!(read_raw(&store, "datacenter").unwrap().contains("evil@pve"));
    }

    #[test]
    fn the_scopes_map_is_addressed_as_a_whole() {
        // Review P9: `scopes` keys are authids, which may contain the path
        // separator, so a per-entry view can never be right. The refusal says
        // why instead of emitting a generic "invalid path".
        let (_dir, store) = store();
        seed(&store, "datacenter", "scopes:\n  good@pve:\n  - prefix: traefik\n    mode: rw\n");
        let full = grants_json(true, true, json!([]));

        for view in ["scopes.good@pve", "scopes/good@pve", "scopes.a.b"] {
            let err = get_document(&store, "datacenter", Some(view), "json", true, &full).unwrap_err();
            assert_eq!(status(&err), 400, "{view}: {err}");
            assert!(err.to_string().contains("as a whole"), "{view}: {err}");

            let err = put_document(&store, "datacenter", Some(view), "json", "[]", "replace", None, false, &full)
                .unwrap_err();
            assert_eq!(status(&err), 400, "{view}: {err}");
            let err = delete_document(&store, "datacenter", Some(view), None, &full).unwrap_err();
            assert_eq!(status(&err), 400, "{view}: {err}");
        }

        // The whole map is addressable, and one entry is removed by merging
        // `null` into it.
        let whole = get_document(&store, "datacenter", Some("scopes"), "json", true, &full).unwrap();
        assert_eq!(whole.keys, vec!["good@pve"]);
        put_document(
            &store,
            "datacenter",
            Some("scopes"),
            "json",
            "{\"good@pve\": null}",
            "merge",
            None,
            false,
            &full,
        )
        .unwrap();
        assert_eq!(read_raw(&store, "datacenter").unwrap(), "scopes: {}\n");

        // A dotted authid -- the class of principal review P9 found could
        // never hold a scope -- is written through the map view, in both
        // modes. The payload is linted where it will *land*, so the key rule
        // is the one that applies at `scopes.<key>`, not at the payload root.
        for (mode, payload) in [
            ("merge", "{\"john.doe@pve\": [{\"prefix\": \"traefik\", \"mode\": \"ro\"}]}"),
            (
                "replace",
                "{\"john.doe@pve\": [{\"prefix\": \"traefik\", \"mode\": \"ro\"}], \
                  \"svc@ldap.corp!tok\": [{\"prefix\": \"netbird\", \"mode\": \"rw\"}]}",
            ),
        ] {
            put_document(&store, "datacenter", Some("scopes"), "json", payload, mode, None, false, &full)
                .unwrap_or_else(|e| panic!("{mode}: {e}"));
        }
        assert_eq!(
            grants(&store, "john.doe@pve").unwrap(),
            "[{\"prefix\":\"traefik\",\"mode\":\"ro\"}]"
        );
        // ... while a dotted key anywhere else is still an invalid key.
        let err = put_document(&store, "100", Some("ns"), "json", "{\"a.b\": 1}", "replace", None, false, &full)
            .unwrap_err();
        assert_eq!(status(&err), 400, "{err}");
        assert!(err.to_string().contains("no dots"), "{err}");

    }

    #[test]
    fn an_out_of_band_invalid_document_is_still_readable_and_repairable() {
        // Review P2: one bad key anywhere in `datacenter.yaml` used to 400
        // every guest operation cluster-wide for everyone -- and block the
        // admin's own repair, because `put_raw` re-parsed the *old* content
        // strictly before diffing.
        let (dir, store) = store();
        let full = grants_json(true, true, json!([]));

        // Hand-written, out of band: an invalid key and a null value, next to
        // a perfectly good `scopes` map.
        let broken = "scopes:\n  good@pve!tok:\n  - prefix: traefik\n    mode: rw\nbad key: 1\nempty:\n";
        std::fs::write(dir.path().join("datacenter.yaml"), broken).unwrap();

        // The scope lookup still works, for its owner and for everyone else.
        assert_eq!(
            grants(&store, "good@pve!tok").unwrap(),
            "[{\"prefix\":\"traefik\",\"mode\":\"rw\"}]"
        );
        assert_eq!(grants(&store, "root@pam").unwrap(), "[]");

        // The admin can see what to fix (rendered from the parsed value, so
        // the empty value comes back as an explicit `null`) ...
        let got = get_document(&store, "datacenter", None, "yaml", true, &full).unwrap();
        let text = got.text.clone().unwrap();
        assert!(text.contains("bad key: 1"), "{text}");
        assert!(text.contains("empty: null"), "{text}");
        assert!(text.contains("prefix: traefik"), "{text}");

        // ... and repair it with a root-level replace.
        let fixed = "scopes:\n  good@pve!tok:\n  - prefix: traefik\n    mode: rw\n";
        put_document(&store, "datacenter", None, "yaml", fixed, "replace", Some(&got.digest), false, &full)
            .unwrap();
        assert_eq!(read_raw(&store, "datacenter").unwrap(), fixed);

        // A guest document is no different: readable while invalid, and the
        // strict lint still applies to what is being written.
        std::fs::write(dir.path().join("100.yaml"), "bad key: 1\n").unwrap();
        assert!(get_document(&store, "100", None, "yaml", true, &full).is_ok());
        let err = put_document(&store, "100", Some("traefik"), "json", "{\"host\": \"x\"}", "replace", None, false, &full)
            .unwrap_err();
        assert_eq!(status(&err), 400, "{err}");
        assert!(err.to_string().contains("bad key"), "{err}");
    }

    #[test]
    fn an_unparseable_document_denies_nobody_and_is_repairable_through_the_api() {
        // Review pass 3 R1, the critical: a tab, an anchor or an indentation
        // slip in a hand-edited `datacenter.yaml` used to 400 *every*
        // endpoint for *every* principal, root included, and could not be
        // repaired through the API -- `put_document`/`delete_document` both
        // read the document before planning, so the outer read failed before
        // `put_raw`'s lenient old-bytes parse was ever reached.
        for broken in ["a: 1\n\tb: 2\n", "a: &x 1\nb: *x\n", "a: 1\n  b: 2\n", "a: [\n"] {
            let (dir, store) = store();
            let full = grants_json(true, true, json!([]));
            let scoped = rw_traefik();
            std::fs::write(dir.path().join("datacenter.yaml"), broken).unwrap();
            seed(&store, "100", "traefik:\n  host: x\n");

            // (1) The grants lookup -- which runs on every guest request --
            //     grants nothing instead of failing.
            assert_eq!(grants(&store, "scoped@pve!t1").unwrap(), "[]");
            assert_eq!(grants(&store, "root@pam").unwrap(), "[]");

            // (2) Unrelated guest operations keep working, for both an ACL
            //     holder and a scope holder.
            assert!(get_document(&store, "100", None, "yaml", true, &full).is_ok(), "{broken:?}");
            put_document(&store, "100", Some("traefik"), "json", "{\"host\":\"y\"}", "replace", None, false, &scoped)
                .unwrap_or_else(|e| panic!("{broken:?}: guest write refused: {e}"));

            // (2b) ... and so does the listing, with the broken document
            //      shown as having no keys rather than 400-ing the whole
            //      list for everybody.
            std::fs::write(dir.path().join("101.yaml"), broken).unwrap();
            let rows = json!([
                {"vmid": 100, "node": "n1", "type": "lxc", "name": "ok", "grants": full},
                {"vmid": 101, "node": "n1", "type": "lxc", "name": "broken", "grants": full},
            ])
            .to_string();
            let listed = list_guests(&store, &rows, None, true).unwrap();
            assert_eq!(listed.iter().map(|g| g.vmid).collect::<Vec<_>>(), vec![100, 101], "{broken:?}");
            assert!(listed[1].keys.is_empty());
            assert!(!listed[1].digest.is_empty());
            std::fs::remove_file(dir.path().join("101.yaml")).unwrap();

            // (3) The document itself answers 200 with `parse_error`, the
            //     real digest and no data. A full reader also gets the raw
            //     text -- they may read the whole document anyway, and need
            //     it to write the repair.
            let got = get_document(&store, "datacenter", None, "yaml", true, &full).unwrap();
            assert!(got.parse_error.is_some(), "{broken:?}");
            assert_eq!(got.raw.as_deref(), Some(broken));
            assert_eq!(got.text.as_deref(), Some("{}\n"));
            assert!(got.keys.is_empty());
            assert!(!got.digest.is_empty());

            // A scope-only reader gets the diagnosis but never the bytes.
            let dc_scoped = grants_json(false, false, json!([{"prefix": "traefik", "mode": "ro"}]));
            let scoped_got = get_document(&store, "datacenter", None, "json", true, &dc_scoped).unwrap();
            assert!(scoped_got.parse_error.is_some());
            assert_eq!(scoped_got.raw, None, "{broken:?} leaked its bytes to a scoped reader");
            assert_eq!(scoped_got.data_json.as_deref(), Some("{}"));

            // (4) A root replace repairs it, with the digest precondition.
            let fixed = "scopes:\n  svc@pve!tok:\n  - prefix: traefik\n    mode: rw\n";
            put_document(&store, "datacenter", None, "yaml", fixed, "replace", Some(&got.digest), false, &full)
                .unwrap_or_else(|e| panic!("{broken:?}: repair refused: {e}"));
            assert_eq!(read_raw(&store, "datacenter").unwrap(), fixed);
            assert_eq!(
                grants(&store, "svc@pve!tok").unwrap(),
                "[{\"prefix\":\"traefik\",\"mode\":\"rw\"}]"
            );

            // (5) ... and a root DELETE is the other repair shape.
            std::fs::write(dir.path().join("datacenter.yaml"), broken).unwrap();
            delete_document(&store, "datacenter", None, None, &full)
                .unwrap_or_else(|e| panic!("{broken:?}: delete refused: {e}"));
            assert!(read_raw(&store, "datacenter").is_none());
        }
    }

    #[test]
    fn a_narrow_write_against_an_unparseable_document_is_refused_not_silently_destructive() {
        // The other half of R1: the value planned against is the *empty*
        // document, so any write narrower than "replace the file whole" would
        // quietly drop everything the file contains. Only the two documented
        // repair shapes are allowed through.
        let (dir, store) = store();
        let full = grants_json(true, true, json!([]));
        let broken = "traefik:\n\thost: x\nnetbird: {}\n";
        std::fs::write(dir.path().join("datacenter.yaml"), broken).unwrap();

        for (view, mode) in [
            (Some("traefik"), "replace"),
            (Some("traefik"), "merge"),
            (None, "merge"), // a root *merge* would drop the rest just the same
        ] {
            let err = put_document(&store, "datacenter", view, "json", "{\"host\":\"y\"}", mode, None, false, &full)
                .expect_err("must be refused");
            assert_eq!(status(&err), 400, "{view:?}/{mode}: {err}");
            assert!(err.to_string().contains("repaired as a whole"), "{view:?}/{mode}: {err}");
            assert_eq!(read_raw(&store, "datacenter").unwrap(), broken, "{view:?}/{mode} wrote anyway");
        }
        let err = delete_document(&store, "datacenter", Some("traefik"), None, &full).unwrap_err();
        assert_eq!(status(&err), 400, "{err}");
        assert_eq!(read_raw(&store, "datacenter").unwrap(), broken);
    }

    #[test]
    fn a_document_above_the_read_cap_is_refused_on_read_and_repairable_on_write() {
        // Review pass 3 §5, `store.rs:235`: `MAX_BYTES` applied only to
        // writes, so a multi-megabyte out-of-band file was read and hashed on
        // every request that touched it -- and, with the cap alone, would
        // have become unreadable *and* unwritable, i.e. removable only with
        // `rm` as root. The read refuses loudly; the write keeps the two
        // whole-file repair shapes.
        let (dir, store) = store();
        let full = grants_json(true, true, json!([]));
        let big = format!("a: \"{}\"\n", "x".repeat(4 * 1024 * 1024));
        std::fs::write(dir.path().join("100.yaml"), &big).unwrap();

        let err = get_document(&store, "100", None, "json", true, &full).unwrap_err();
        assert_eq!(status(&err), 400, "{err}");
        assert!(err.to_string().contains("too large"), "{err}");

        // ... but one oversized document may not take the *listing* down for
        // everybody -- that is R1's blast radius with a different message.
        seed(&store, "101", "traefik:\n  host: x\n");
        let rows = json!([
            {"vmid": 100, "node": "n1", "type": "lxc", "name": "big", "grants": full},
            {"vmid": 101, "node": "n1", "type": "lxc", "name": "ok", "grants": full},
        ])
        .to_string();
        let listed = list_guests(&store, &rows, None, true).unwrap();
        assert_eq!(listed.iter().map(|g| g.vmid).collect::<Vec<_>>(), vec![100, 101]);
        assert!(listed[0].keys.is_empty(), "an unreadable document has no keys to show");
        assert!(!listed[0].digest.is_empty(), "but it still reports its real digest");
        assert_eq!(listed[1].keys, vec!["traefik"]);

        // A narrow write is refused (it would drop the file's content) ...
        let err = put_document(&store, "100", Some("traefik"), "json", "{\"a\":1}", "replace", None, false, &full)
            .unwrap_err();
        assert_eq!(status(&err), 400, "{err}");
        assert!(err.to_string().contains("repaired as a whole"), "{err}");

        // ... and the root replace goes through, with a digest precondition
        // that still works even though nothing could read the document.
        let stale = put_document(&store, "100", None, "yaml", "a: 1\n", "replace", Some("deadbeef"), false, &full)
            .unwrap_err();
        assert_eq!(status(&stale), 409, "{stale}");
        put_document(&store, "100", None, "yaml", "a: 1\n", "replace", None, false, &full).unwrap();
        assert_eq!(read_raw(&store, "100").unwrap(), "a: 1\n");

        // A root DELETE is the other shape.
        std::fs::write(dir.path().join("100.yaml"), &big).unwrap();
        delete_document(&store, "100", None, None, &full).unwrap();
        assert!(read_raw(&store, "100").is_none());
    }

    #[test]
    fn a_document_that_is_not_a_map_is_empty_for_a_scoped_reader() {
        // Review pass 3 R2, at the API level: `view::filter`'s catch-all used
        // to hand the whole value to a scope-only caller, through the
        // view-less GET and as a content oracle through `?has=`.
        let (dir, store) = store();
        let scoped = rw_traefik();
        let full = grants_json(true, true, json!([]));

        for (text, rendered) in [("- a\n- secret\n", "- a\n- secret\n"), ("just a scalar\n", "just a scalar\n")] {
            std::fs::write(dir.path().join("100.yaml"), text).unwrap();

            let got = get_document(&store, "100", None, "yaml", true, &scoped).unwrap();
            assert_eq!(got.text.as_deref(), Some("{}\n"), "{text:?} leaked");
            assert!(got.keys.is_empty());
            // An explicit view of it is nothing, not the value.
            let view = get_document(&store, "100", Some("traefik"), "json", true, &scoped).unwrap();
            assert_eq!(view.data_json.as_deref(), Some("{}"));

            // `?has=` cannot be used as an oracle over it either.
            let rows = json!([{"vmid": 100, "grants": scoped}]).to_string();
            assert!(list_guests(&store, &rows, Some("traefik"), false).unwrap().is_empty());
            let listed = list_guests(&store, &rows, None, false).unwrap();
            assert_eq!(listed.len(), 1);
            assert!(listed[0].keys.is_empty(), "{text:?} leaked its keys");

            // A full reader still sees exactly what is on disk.
            let root = get_document(&store, "100", None, "yaml", true, &full).unwrap();
            assert_eq!(root.text.as_deref(), Some(rendered), "{text:?}");

            // ... and the write gate refuses to store it again: only a root
            // replace with a real document repairs it.
            let err = put_document(&store, "100", None, "yaml", text, "replace", None, false, &full).unwrap_err();
            assert_eq!(status(&err), 400, "{text:?}: {err}");
            let err = put_document(&store, "100", Some("traefik"), "json", "{\"a\":1}", "replace", None, false, &scoped)
                .unwrap_err();
            assert_eq!(status(&err), 400, "a scoped write through a non-map root: {err}");
            assert_eq!(std::fs::read_to_string(dir.path().join("100.yaml")).unwrap(), text);
        }
    }

    #[test]
    fn a_lint_400_never_names_a_path_the_caller_cannot_read() {
        // Review pass 3 R6. `plan_write` linted the whole document and
        // rendered every finding verbatim, so a principal holding one prefix
        // learned the existence and exact spelling of keys in subtrees they
        // cannot read -- the filter `forbidden()` applies to every 403 was
        // simply missing from the 400 path.
        let (dir, store) = store();
        let scoped = rw_traefik();
        std::fs::write(
            dir.path().join("100.yaml"),
            "traefik:\n  host: x\nsecret_area:\n  customer name: acme\n",
        )
        .unwrap();
        let before = read_raw(&store, "100").unwrap();

        // (a) Availability: the scoped writer's own subtree is all that is
        //     linted, so an out-of-band bad key elsewhere denies nobody.
        put_document(&store, "100", Some("traefik"), "json", "{\"host\":\"y\"}", "replace", None, false, &scoped)
            .expect("an unrelated out-of-band bad key must not block a scoped write");
        assert!(read_raw(&store, "100").unwrap().contains("customer name"), "the bad key was dropped");

        // (b) Disclosure: their own bad payload is still named ...
        let err = put_document(&store, "100", Some("traefik"), "json", "{\"my bad\":1}", "replace", None, false, &scoped)
            .unwrap_err();
        assert_eq!(status(&err), 400, "{err}");
        assert!(err.to_string().contains("my bad"), "{err}");

        // ... and a full-write caller still gets the whole document's
        // findings, spelled out, because they can read it all.
        let full = grants_json(true, true, json!([]));
        let err = put_document(&store, "100", Some("traefik"), "json", "{\"host\":\"z\"}", "replace", None, false, &full)
            .unwrap_err();
        assert_eq!(status(&err), 400, "{err}");
        assert!(err.to_string().contains("customer name"), "{err}");

        // A write-only principal (Sys.Modify without Sys.Audit) gets the
        // count, not the spelling.
        let blind = grants_json(false, true, json!([]));
        let err = put_document(&store, "100", Some("traefik"), "json", "{\"host\":\"z\"}", "replace", None, false, &blind)
            .unwrap_err();
        assert_eq!(status(&err), 400, "{err}");
        assert!(!err.to_string().contains("customer name"), "leaked: {err}");
        assert!(err.to_string().contains("cannot read"), "{err}");

        assert_eq!(
            read_raw(&store, "100").unwrap(),
            before.replace("host: x", "host: y"),
            "a refused write must not have written"
        );
    }

    #[test]
    fn the_narrowed_lint_still_refuses_everything_inside_the_written_subtree() {
        // The invariant R6's fix must not give up: narrowing the *scope* of
        // the lint may not narrow the *rules*. Every one of these is a
        // finding the whole-document lint used to catch on a scoped write --
        // including the comment-key-value rule, which lives in the view's
        // parent map and is invisible to a plain subtree lint.
        let (_dir, store) = store();
        seed(&store, "100", "traefik:\n  host: x\n");
        let scoped = rw_traefik();
        let before = read_raw(&store, "100").unwrap();

        for (view, mode, payload, expect) in [
            (Some("traefik__"), "replace", "5", "comment key value must be a string"),
            (Some("traefik"), "replace", "{\"bad key\": 1}", "invalid key"),
            (Some("traefik"), "replace", "{\"a.b\": 1}", "no dots"),
            (Some("traefik"), "replace", "{\"deep\": {\"bad key\": 1}}", "invalid key"),
            (Some("traefik"), "replace", "{\"list\": [{\"bad key\": 1}]}", "invalid key"),
            (Some("traefik"), "replace", "{\"x__\": 5}", "comment key value must be a string"),
            (Some("traefik"), "merge", "{\"x__\": 5}", "comment key value must be a string"),
            (Some("traefik"), "replace", "{\"nul\": null}", "null values are not allowed"),
        ] {
            let err = put_document(&store, "100", view, "json", payload, mode, None, false, &scoped)
                .map(|ok| panic!("{view:?}/{payload} was accepted: {ok:?}"))
                .unwrap_err()
                .to_string();
            assert!(err.starts_with("400: "), "{payload}: {err}");
            assert!(err.contains(expect), "{payload}: {err}");
            assert_eq!(read_raw(&store, "100").unwrap(), before, "{payload} wrote anyway");
        }

        // ... and a legitimate comment-key write still goes through.
        put_document(&store, "100", Some("traefik__"), "json", "\"the ingress config\"", "replace", None, false, &scoped)
            .expect("a string comment value is fine");
    }

    #[test]
    fn a_view_through_a_comment_key_is_refused_for_every_caller() {
        // Review pass 4 Q1, the regression the R6 narrowing introduced.
        // `model::lint_at` anchors on the view path's *last* segment only, so
        // a comment key in any intermediate segment was materialised blindly
        // by `view::descend_creating` and checked nowhere: `PUT
        // ?view=p.q__.r` stored `p: {q__: {r: 1}}`, which the whole-document
        // `model::lint` refuses -- so a scoped principal could permanently
        // disable every narrow write of a `full_write` one. A comment key is
        // a leaf string; nothing is addressable below it, and `parse_view`
        // now says so for every caller and every verb.
        let (_dir, store) = store();
        seed(&store, "100", "p:\n  host: a\n  q__: the note\n");
        let before = read_raw(&store, "100").unwrap();

        let scoped = grants_json(false, false, json!([{"prefix": "p", "mode": "rw"}]));
        let full = grants_json(true, true, json!([]));

        for grants in [&scoped, &full] {
            for view in ["p.q__.r", "p.__.x", "p.a__.b.c"] {
                for (mode, payload) in [("replace", "1"), ("merge", "1"), ("merge", "{\"z\": 1}")] {
                    let err =
                        put_document(&store, "100", Some(view), "json", payload, mode, None, false, grants)
                            .map(|ok| panic!("?view={view} ({mode}) was accepted: {ok:?}"))
                            .unwrap_err();
                    assert_eq!(status(&err), 400, "?view={view} ({mode}): {err}");
                    assert!(err.to_string().contains("comment key"), "?view={view}: {err}");
                    assert_eq!(read_raw(&store, "100").unwrap(), before, "?view={view} wrote anyway");
                }
                // The same answer on the read and delete paths: one rule, one
                // owner (`parse_view`), so `touched` and `?view=` cannot drift.
                let err = get_document(&store, "100", Some(view), "json", true, grants).unwrap_err();
                assert_eq!(status(&err), 400, "GET ?view={view}: {err}");
                let err = delete_document(&store, "100", Some(view), None, grants).unwrap_err();
                assert_eq!(status(&err), 400, "DELETE ?view={view}: {err}");
                assert_eq!(read_raw(&store, "100").unwrap(), before, "DELETE ?view={view} wrote anyway");
            }
        }

        // A comment key as the *final* segment is untouched: that is the note
        // itself, and writing it a string is the documented way to set one.
        put_document(&store, "100", Some("p.q__"), "json", "\"a better note\"", "replace", None, false, &scoped)
            .expect("a view ending at a comment key is legal");
        assert!(read_raw(&store, "100").unwrap().contains("a better note"));
        get_document(&store, "100", Some("p.q__"), "json", true, &scoped).expect("and readable");
        // Including the bare `__`, which documents the map it sits in.
        put_document(&store, "100", Some("p.__"), "json", "\"about p\"", "replace", None, false, &scoped)
            .expect("a bare comment key is a legal leaf view");
    }

    #[test]
    fn an_accepted_scoped_write_never_adds_a_whole_document_lint_finding() {
        // The standing invariant behind review pass 4 Q1, asserted rather
        // than argued: `plan_write` lints only the written subtree for a
        // caller without `full_write`, so the narrowing is only safe while
        // *no* accepted scoped write can leave the document with a finding
        // `model::lint` did not already have. Run a corpus of writes -- legal
        // ones, illegal ones, comment keys, nested payloads, deletes, merges
        // -- against a document that already carries an out-of-band finding,
        // and check the finding set after every accepted one.
        let (dir, store) = store();
        // Written behind the store's back: `put_raw` lints, and the point is
        // to start from a document the whole-document lint already refuses,
        // so a pre-existing finding is *carried* rather than blocking anyone.
        std::fs::write(
            dir.path().join("100.yaml"),
            "p:\n  host: a\n  q__: the note\nsecret area:\n  k: v\n",
        )
        .unwrap();

        let scoped = grants_json(false, false, json!([{"prefix": "p", "mode": "rw"}]));
        let baseline = lint_findings(&read_or_empty(&store, DocId::Guest(100)).unwrap().value);
        assert!(!baseline.is_empty(), "the corpus starts from an already-invalid document");

        let views = ["p", "p.host", "p.q__", "p.__", "p.deep", "p.deep.x", "p.q__.r", "p.a__.b.c"];
        let payloads = [
            "1", "\"s\"", "null", "{}", "{\"a\": 1}", "{\"x__\": 5}", "{\"x__\": \"ok\"}",
            "{\"bad key\": 1}", "{\"a.b\": 1}", "{\"deep\": {\"y__\": 7}}", "[1, 2]",
            "{\"n\": null}", "{\"scopes\": {\"a@pve\": 1}}",
        ];

        let mut accepted = 0usize;
        for view in views {
            for payload in payloads {
                for mode in ["replace", "merge"] {
                    let before =
                        lint_findings(&read_or_empty(&store, DocId::Guest(100)).unwrap().value);
                    let outcome =
                        put_document(&store, "100", Some(view), "json", payload, mode, None, false, &scoped);
                    let after =
                        lint_findings(&read_or_empty(&store, DocId::Guest(100)).unwrap().value);
                    match outcome {
                        Ok(_) => {
                            accepted += 1;
                            let grew: Vec<_> = after.difference(&before).collect();
                            assert!(
                                grew.is_empty(),
                                "?view={view} ({mode}) {payload} was accepted and added {grew:?}"
                            );
                        }
                        Err(e) => {
                            assert_eq!(status(&e), 400, "?view={view} ({mode}) {payload}: {e}");
                            assert_eq!(after, before, "a refused write changed the document");
                        }
                    }
                    // The out-of-band finding is never *fixed* by a scoped
                    // write either, so it stays in the baseline throughout:
                    // availability, not a moving target.
                    assert!(baseline.is_subset(&after), "?view={view} ({mode}) {payload}");
                }
            }
        }
        assert!(accepted > 20, "the corpus must actually accept writes ({accepted})");

        // Whatever the corpus did, a `full_write` caller's narrow write --
        // the operation Q1's exploit disabled -- is still refused only by the
        // one pre-existing out-of-band key, and never by anything a scoped
        // caller stored.
        let full = grants_json(true, true, json!([]));
        let err = put_document(&store, "100", Some("p.host"), "json", "\"z\"", "replace", None, false, &full)
            .unwrap_err()
            .to_string();
        assert!(err.contains("secret area"), "{err}");
        assert_eq!(err.matches("invalid key").count(), 1, "one finding, the pre-existing one: {err}");
    }

    #[test]
    fn plan_write_refuses_a_scoped_write_that_would_add_a_finding() {
        // The safety net itself, exercised directly: `plan_write`'s
        // before/after comparison must fire even for a mutation the narrow
        // lint cannot see, and its 400 must name no path (the new finding may
        // sit outside what the caller may read).
        let (_dir, store) = store();
        seed(&store, "100", "p:\n  host: a\n");
        let scoped = grants_json(false, false, json!([{"prefix": "p", "mode": "rw"}]));
        let view = DocPath::parse("p.host").unwrap();
        let grants: Grants = serde_json::from_str(&scoped).unwrap();
        let mut planned = read_or_empty(&store, DocId::Guest(100)).unwrap().value;

        let err = plan_write(DocId::Guest(100), &view, &mut planned, &grants, |v| {
            // A mutation the narrow lint at `p.host` cannot see, standing in
            // for any future hole of Q1's shape.
            v["p"]["z__"] = json!({"r": 1});
            Ok(vec![Touched { path: DocPath::parse("p.host").unwrap(), op: Op::Set }])
        })
        .unwrap_err();
        assert_eq!(status(&err), 400, "{err}");
        assert!(!err.to_string().contains("z__"), "the refusal must name no path: {err}");

        // The same mutation confined to findings that already existed is fine.
        let mut planned = read_or_empty(&store, DocId::Guest(100)).unwrap().value;
        plan_write(DocId::Guest(100), &view, &mut planned, &grants, |v| {
            v["p"]["host"] = json!("b");
            Ok(vec![Touched { path: DocPath::parse("p.host").unwrap(), op: Op::Set }])
        })
        .expect("a clean scoped write is unaffected");
    }

    #[test]
    fn a_touched_path_inside_the_scopes_map_is_reported_as_the_map() {
        // Review pass 3 §5, `api.rs:209`: `check_write` and `parse_view` both
        // treat `scopes` as an opaque leaf; `touched` -- the one place the
        // client sees a path -- reported `scopes.john.doe@pve`, which is not
        // a valid view and cannot be parsed back into its segments.
        let (_dir, store) = store();
        let full = grants_json(true, true, json!([]));
        seed(&store, "datacenter", "scopes:\n  a@pve:\n  - prefix: traefik\n    mode: rw\n");

        let r = put_document(
            &store,
            "datacenter",
            Some("scopes"),
            "json",
            "{\"john.doe@pve\": [{\"prefix\": \"netbird\", \"mode\": \"ro\"}], \"a@pve\": null}",
            "merge",
            None,
            false,
            &full,
        )
        .unwrap();
        assert_eq!(
            r.touched,
            vec![
                ApiTouched { path: "scopes".to_string(), op: "set".to_string() },
                ApiTouched { path: "scopes".to_string(), op: "delete".to_string() },
            ],
            "collapsed to the map, de-duplicated, and still distinguishing set from delete"
        );
        // Every reported path is addressable as a view again.
        for t in &r.touched {
            assert!(parse_view(Some(&t.path)).is_ok(), "{} is not a valid view", t.path);
        }

        // A whole-map replace reports the map once, not one entry per authid.
        let r2 = put_document(
            &store,
            "datacenter",
            Some("scopes"),
            "json",
            "{\"x@pve\": [{\"prefix\": \"a\", \"mode\": \"ro\"}], \"y@pve\": [{\"prefix\": \"b\", \"mode\": \"ro\"}]}",
            "replace",
            None,
            false,
            &full,
        )
        .unwrap();
        assert_eq!(r2.touched.iter().filter(|t| t.op == "set").count(), 1);
        assert!(r2.touched.iter().all(|t| t.path == "scopes"));
    }

    #[test]
    fn an_empty_scope_prefix_is_refused_at_the_wire_boundary() {
        // Review pass 3 §5, `scopes.rs:55`: P1 rejected the empty prefix in
        // the *document*; `grants_json` still deserialized it straight to
        // `Path::root()`, i.e. write access to every path of every document.
        let (_dir, store) = store();
        seed(&store, "100", "traefik:\n  host: x\n");
        let before = read_raw(&store, "100").unwrap();

        for bad in ["", "scopes.other@pve", "__"] {
            let grants = grants_json(false, false, json!([{"prefix": bad, "mode": "rw"}]));
            for r in [
                get_document(&store, "100", None, "json", true, &grants).map(|_| ()),
                put_document(&store, "100", Some("x"), "json", "1", "replace", None, false, &grants).map(|_| ()),
                delete_document(&store, "100", Some("traefik"), None, &grants).map(|_| ()),
            ] {
                let err = r.expect_err("an invalid scope prefix must be refused at the boundary");
                assert_eq!(status(&err), 400, "{bad}: {err}");
            }
        }
        assert_eq!(read_raw(&store, "100").unwrap(), before);
    }

    #[test]
    fn the_bare_map_comment_is_not_disclosed_to_a_scoped_reader() {
        // Review P4, the live scenario: the document-root `__` documents the
        // whole document, so a scope on `traefik` must not receive it in the
        // view-less read while `?view=__` is a 403.
        let (_dir, store) = store();
        seed(
            &store,
            "100",
            "__: top level note - secret-ish\ntraefik__: about traefik\ntraefik:\n  host: x\nnetbird:\n  groups:\n  - lan\n",
        );
        let scoped = rw_traefik();

        let got = get_document(&store, "100", None, "yaml", true, &scoped).unwrap();
        assert_eq!(got.text.as_deref(), Some("traefik__: about traefik\ntraefik:\n  host: x\n"));
        assert_eq!(got.keys, vec!["traefik"]);

        // Both halves agree: what the union read omits, the explicit view
        // refuses.
        let err = get_document(&store, "100", Some("__"), "yaml", true, &scoped).unwrap_err();
        assert_eq!(status(&err), 403, "{err}");
        let err = put_document(&store, "100", Some("__"), "json", "\"mine now\"", "replace", None, false, &scoped)
            .unwrap_err();
        assert_eq!(status(&err), 403, "{err}");

        // A full reader still gets it.
        let full = get_document(&store, "100", None, "yaml", true, &grants_json(true, true, json!([]))).unwrap();
        assert!(full.text.unwrap().contains("top level note"));
    }

    #[test]
    fn delete_of_a_view_leaves_the_rest_and_reports_touched() {
        let (_dir, store) = store();
        seed(&store, "100", "traefik:\n  host: x\nnetbird:\n  groups:\n  - lan\n");
        let r = delete_document(&store, "100", Some("traefik"), None, &rw_traefik()).unwrap();
        assert_eq!(r.touched.len(), 1);
        assert_eq!(r.touched[0].op, "delete");
        assert_eq!(read_raw(&store, "100").unwrap(), "netbird:\n  groups:\n  - lan\n");
    }

    #[test]
    fn get_returns_ordered_keys_alongside_unordered_json_data() {
        // Review F23.
        let (_dir, store) = store();
        seed(&store, "100", "zeta: 1\nalpha__: about alpha\nalpha: 2\nmid: 3\n");
        let got = get_document(&store, "100", None, "json", true, &grants_json(true, true, json!([]))).unwrap();
        assert_eq!(got.keys, vec!["zeta", "alpha", "mid"]);
    }

    #[test]
    fn list_guests_uses_the_list_perl_passes_and_gates_node_and_name() {
        let (_dir, store) = store();
        seed(&store, "100", "traefik:\n  host: x\n");
        seed(&store, "200", "netbird:\n  groups:\n  - lan\n");
        let input = json!([
            {"vmid": 100, "node": "node1", "type": "lxc", "name": "web", "grants": rw_traefik()},
            {"vmid": 200, "node": "node1", "type": "lxc", "name": "db", "grants": rw_traefik()},
            {"vmid": 300, "node": "node1", "type": "qemu", "name": "none", "grants": zero_grant()},
        ])
        .to_string();

        let list = list_guests(&store, &input, None, false).unwrap();
        // 300 has no grant at all -> not listed.
        assert_eq!(list.iter().map(|g| g.vmid).collect::<Vec<_>>(), vec![100, 200]);
        // A scope-only caller gets no node/name (no VM.Audit).
        assert!(list[0].node.is_none() && list[0].name.is_none());
        assert_eq!(list[0].keys, vec!["traefik"]);
        // 200 has nothing the caller may see, but is still listed (the guest
        // itself is in scope) with an empty key list.
        assert!(list[1].keys.is_empty());

        // `has` filters on the *visible* data.
        let filtered = list_guests(&store, &input, Some("traefik"), false).unwrap();
        assert_eq!(filtered.iter().map(|g| g.vmid).collect::<Vec<_>>(), vec![100]);
        assert!(list.iter().all(|g| g.orphan.is_none()));

        // With VM.Audit, node and name come through.
        let audit = json!([
            {"vmid": 100, "node": "node1", "type": "lxc", "name": "web",
             "grants": grants_json(true, false, json!([]))},
        ])
        .to_string();
        let list2 = list_guests(&store, &audit, None, false).unwrap();
        assert_eq!(list2[0].node.as_deref(), Some("node1"));
        assert_eq!(list2[0].name.as_deref(), Some("web"));
    }

    #[test]
    fn list_guests_marks_documents_whose_guest_is_gone() {
        // Review P5: an orphan document was readable, invisible to every
        // listing, counted by `GET /meta/version`, replicated by pmxcfs, and
        // removable only with `rm` as root.
        let (_dir, store) = store();
        seed(&store, "100", "traefik:\n  host: x\n");
        seed(&store, "999500", "traefik:\n  host: gone\nnetbird: {}\n");
        seed(&store, "datacenter", "scopes: {}\n");
        store.snapshot(999500, "snapA").unwrap();
        let input = json!([
            {"vmid": 100, "node": "node1", "type": "lxc", "name": "web",
             "grants": grants_json(true, true, json!([]))},
        ])
        .to_string();

        // Without datacenter read, nothing changes.
        let plain = list_guests(&store, &input, None, false).unwrap();
        assert_eq!(plain.iter().map(|g| g.vmid).collect::<Vec<_>>(), vec![100]);

        let with_orphans = list_guests(&store, &input, None, true).unwrap();
        assert_eq!(with_orphans.iter().map(|g| g.vmid).collect::<Vec<_>>(), vec![100, 999500]);
        let orphan = &with_orphans[1];
        assert_eq!(orphan.orphan, Some(1));
        assert_eq!(orphan.keys, vec!["traefik", "netbird"]);
        assert!(orphan.node.is_none() && orphan.name.is_none() && orphan.kind.is_none());
        assert!(!orphan.digest.is_empty());
        // Neither the datacenter document nor a snapshot copy is a guest.
        assert!(with_orphans.iter().all(|g| g.vmid != 0));

        // `has` applies to orphans too.
        assert_eq!(
            list_guests(&store, &input, Some("netbird"), true)
                .unwrap()
                .iter()
                .map(|g| g.vmid)
                .collect::<Vec<_>>(),
            vec![999500]
        );
    }
}
