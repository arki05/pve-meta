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
//! 3. only then is the planned value written.
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

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::Error as CoreError;
use crate::format::{self, Format};
use crate::model;
use crate::patch::{Op, Touched};
use crate::path::Path as DocPath;
use crate::scopes::{self, Grants};
use crate::store::{DocId, MetaStore, DISK_FORMAT};
use crate::view;

fn unix_secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Maps a [`CoreError`] to the `"NNN: message"` string the Perl layer
/// expects: `409` digest mismatch/conflict, `404` not found, `400`
/// lint/parse/invalid-path/invalid-name/invalid-scopes/too-large, `500`
/// everything else.
pub fn api_err(err: CoreError) -> anyhow::Error {
    let status: u16 = match &err {
        CoreError::DigestMismatch { .. } | CoreError::Conflict(_) => 409,
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
    let parent_readable = path.parent().is_some_and(|p| grants.can_read(&p));
    if parent_readable {
        anyhow::anyhow!("403: not permitted: {path}")
    } else {
        anyhow::anyhow!("403: not permitted")
    }
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
fn parse_view(view: Option<&str>) -> Result<DocPath, anyhow::Error> {
    match view {
        Some(s) => DocPath::parse(s).map_err(api_err),
        None => Ok(DocPath::root()),
    }
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
    serde_json::from_str(grants_json).map_err(|e| bad_request(format!("invalid grants: {e}")))
}

/// Reads `id`'s document, or the empty document with digest `""` if it does
/// not exist (`docs/DESIGN.md` §2: "a non-existent document is an empty
/// document ... there is no explicit create").
fn read_or_empty(store: &MetaStore, id: DocId) -> Result<(Value, String), anyhow::Error> {
    match store.read(id) {
        Ok(doc) => Ok((doc.value, doc.digest)),
        Err(CoreError::NotFound(_)) => Ok((Value::Object(Map::new()), String::new())),
        Err(e) => Err(api_err(e)),
    }
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
}

#[derive(Debug, Clone, Serialize)]
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
/// Reads leniently: a malformed entry belonging to *another* principal is
/// skipped with a warning rather than failing the whole lookup
/// (`docs/DESIGN.md` §8, review F7).
///
/// # Errors
/// `400:` if `scopes` is not a map, or if `authid`'s own entry is malformed.
pub fn grants(store: &MetaStore, authid: &str) -> Result<String, anyhow::Error> {
    let dc_value = match store.read(DocId::Datacenter) {
        Ok(doc) => doc.value,
        Err(CoreError::NotFound(_)) => Value::Object(Map::new()),
        Err(e) => return Err(api_err(e)),
    };
    let scopes = scopes::scopes_for(&dc_value, authid).map_err(api_err)?;
    Ok(serde_json::to_string(&scopes).expect("Vec<Scope> always serializes"))
}

/// `api_list_guests($guests_json, $has)`: for every guest Perl passed in,
/// the metadata the caller may see.
///
/// A guest the caller can read *nothing* of is omitted entirely
/// (`docs/DESIGN.md` §8: "reads require a grant"), and `node`/`name` are
/// returned only to a caller with `VM.Audit` on that guest (`full_read`).
///
/// # Errors
/// `400:` if `guests_json` or `has` is malformed.
pub fn list_guests(store: &MetaStore, guests_json: &str, has: Option<&str>) -> Result<Vec<GuestListEntry>, anyhow::Error> {
    let guests: Vec<GuestInput> = serde_json::from_str(guests_json)
        .map_err(|e| bad_request(format!("invalid guest list: {e}")))?;
    let has_path = has.map(DocPath::parse).transpose().map_err(api_err)?;

    let mut out = Vec::with_capacity(guests.len());
    for guest in &guests {
        let grants = parse_grants(&guest.grants)?;
        let readable = grants.readable_prefixes();
        if readable.is_empty() {
            continue;
        }

        let (value, digest) = read_or_empty(store, DocId::Guest(guest.vmid))?;
        let visible = view::filter(&value, &readable);

        if let Some(path) = &has_path {
            if model::get_path(&visible, path).is_none() {
                continue;
            }
        }

        out.push(GuestListEntry {
            vmid: guest.vmid,
            node: grants.full_read.then(|| guest.node.clone()).flatten(),
            kind: guest.kind.clone(),
            name: grants.full_read.then(|| guest.name.clone()).flatten(),
            digest,
            keys: model::top_level_keys(&visible),
        });
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
/// # Errors
/// `400:` invalid id/view/format/grants. `403:` no read grant, or a `view`
/// that is not readable.
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

    let (value, digest) = read_or_empty(store, doc_id)?;

    let mut result_value = if view.is_some() {
        view::extract(&value, &view_path).unwrap_or_else(|| Value::Object(Map::new()))
    } else {
        view::filter(&value, &readable)
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

    Ok(ApiViewDocument {
        id: id_str(doc_id),
        view: view_out(view),
        digest,
        keys,
        data_json,
        text,
    })
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
/// stored document) and validates it as a whole.
///
/// The whole-document lint lives here, *before* the `dry_run` branch, so a
/// dry run validates exactly what the write validates (review F14): the
/// per-view lint inside [`view::replace`]/[`view::merge`] only sees the
/// subtree.
fn plan_write(
    doc_id: DocId,
    planned: &mut Value,
    grants: &Grants,
    mutate: impl FnOnce(&mut Value) -> Result<Vec<Touched>, anyhow::Error>,
) -> Result<Vec<Touched>, anyhow::Error> {
    let touched = mutate(planned)?;

    if let Err(denied) = grants.check_write(&touched) {
        return Err(forbidden(grants, &denied));
    }

    let lints = model::lint(planned);
    if !lints.is_empty() {
        return Err(api_err(CoreError::Lint(lints)));
    }

    validate_scopes_if_touched(doc_id, planned, &touched)?;
    Ok(touched)
}

/// `scopes` is behaviour-controlling configuration living inside a free-form
/// user document, so it is validated **at write time**, naming the offending
/// entry (`docs/DESIGN.md` §8, review F7). Reads are lenient
/// ([`scopes::scopes_for`]); this is the gate that keeps a malformed entry
/// off the disk in the first place.
fn validate_scopes_if_touched(
    doc_id: DocId,
    planned: &Value,
    touched: &[Touched],
) -> Result<(), anyhow::Error> {
    if doc_id != DocId::Datacenter {
        return Ok(());
    }
    let scopes_path = DocPath::parse("scopes").expect("'scopes' is a valid path");
    if !touched.iter().any(|t| scopes_path.is_prefix_of(&t.path)) {
        return Ok(());
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
        view::parse_patch(payload, fmt).map_err(api_err)?
    } else {
        view::parse(payload, fmt).map_err(api_err)?
    };

    store.check_precondition(doc_id, digest).map_err(api_err)?;
    let (value, _current_digest) = read_or_empty(store, doc_id)?;

    // (2) Plan the mutation against a *copy*; the stored document is only
    //     touched once the plan has passed every check.
    let mut planned = value.clone();
    let touched = plan_write(doc_id, &mut planned, &grants, |v| {
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
            .put_raw(doc_id, &text, digest)
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
    let (value, _current_digest) = read_or_empty(store, doc_id)?;

    let mut planned = value.clone();
    let touched = plan_write(doc_id, &mut planned, &grants, |v| {
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
            .put_raw(doc_id, &text, digest)
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
                (Some("scopes.root@pam"), "merge", "{}"),
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
            Some("scopes.bad@pve"),
            "json",
            "[{\"prefix\": \"x\", \"mode\": \"readwrite\"}]",
            "replace",
            None,
            false,
            &grants,
        )
        .unwrap_err();
        assert_eq!(status(&err), 400);
        assert!(err.to_string().contains("bad@pve"), "{err}");
        assert_eq!(read_raw(&store, "datacenter").unwrap(), before);

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

        let list = list_guests(&store, &input, None).unwrap();
        // 300 has no grant at all -> not listed.
        assert_eq!(list.iter().map(|g| g.vmid).collect::<Vec<_>>(), vec![100, 200]);
        // A scope-only caller gets no node/name (no VM.Audit).
        assert!(list[0].node.is_none() && list[0].name.is_none());
        assert_eq!(list[0].keys, vec!["traefik"]);
        // 200 has nothing the caller may see, but is still listed (the guest
        // itself is in scope) with an empty key list.
        assert!(list[1].keys.is_empty());

        // `has` filters on the *visible* data.
        let filtered = list_guests(&store, &input, Some("traefik")).unwrap();
        assert_eq!(filtered.iter().map(|g| g.vmid).collect::<Vec<_>>(), vec![100]);

        // With VM.Audit, node and name come through.
        let audit = json!([
            {"vmid": 100, "node": "node1", "type": "lxc", "name": "web",
             "grants": grants_json(true, false, json!([]))},
        ])
        .to_string();
        let list2 = list_guests(&store, &audit, None).unwrap();
        assert_eq!(list2[0].node.as_deref(), Some("node1"));
        assert_eq!(list2[0].name.as_deref(), Some("web"));
    }
}
