//! API-shaped helpers backing the `api_*` exports of `PVE::RS::Meta`
//! (`docs/DESIGN.md` §3), called from the `#[export]` functions in
//! `lib.rs`'s `pve_rs_meta` package.
//!
//! This crate stays free of `proxmox-router`/`proxmox-schema` (it is loaded
//! directly into pveproxy/pvedaemon); Perl does parameters, PVE ACL checks
//! and `scopes` lookup, and hands this module a `grants_json` describing the
//! caller's effective access (`docs/DESIGN.md` §2,
//! [`pve_meta_core::scopes::Grants`]). This module does everything else:
//! view extraction/replace/merge/remove, touched-path computation against
//! those grants, YAML/JSON rendering, and digesting.
//!
//! ## Wire contract
//!
//! A view's arbitrary content (`data`/`text`) crosses to Perl as a JSON
//! **string** when `format=json` (`data_json`, decoded there with
//! `JSON::PP` to preserve numbers/booleans faithfully) or as already-encoded
//! YAML text when `format=yaml` (`text`, passed straight through). Grants
//! (`grants_json`, and `PVE::RS::Meta::api_grants`'s own return value) are
//! JSON strings too, since they cross the boundary in both directions and
//! keeping one convention avoids two different perlmod-conversion paths for
//! what is structurally the same kind of value. Everything else (ids,
//! digests, view paths, touched entries) is plain and well-typed, so
//! perlmod's `serde`-based conversion renders it directly as a native Perl
//! scalar/array/hash.
//!
//! Errors are `anyhow::Error`s whose `Display` is `"<NNN>: <message>"` (an
//! HTTP status prefix and a human-readable message); the Perl layer parses
//! that prefix back out and re-raises via `PVE::Exception::raise`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{Map, Value};

use pve_meta_core::format::Format;
use pve_meta_core::model;
use pve_meta_core::patch::{Op, Touched};
use pve_meta_core::path::Path as DocPath;
use pve_meta_core::scopes::{self, Grants};
use pve_meta_core::store::{DocId, MetaStore};
use pve_meta_core::vmlist::GuestKind;
use pve_meta_core::view;
use pve_meta_core::Error as CoreError;

use super::open_store;

/// Env var overriding the pmxcfs mount root used to read guest configs for
/// [`list_guests`]'s display names (default `/etc/pve`). A test/dev seam,
/// not part of the wire contract.
pub const PVE_ROOT_ENV: &str = "PVE_META_PVE_ROOT";
/// Env var overriding the vmlist path used by [`list_guests`] (default
/// `<pve-root>/.vmlist`).
pub const VMLIST_ENV: &str = "PVE_META_VMLIST";

fn pve_root() -> PathBuf {
    std::env::var_os(PVE_ROOT_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/etc/pve"))
}

fn vmlist_path() -> PathBuf {
    std::env::var_os(VMLIST_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| pve_root().join(".vmlist"))
}

fn unix_secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Maps a [`CoreError`] to the `"NNN: message"` string the Perl layer
/// expects: `409` digest mismatch/conflict, `404` not found (in practice
/// only [`pve_meta_core::store::MetaStore::locate`]-adjacent conflicts, not
/// a plain missing document -- see `docs/DESIGN.md` §2, "a non-existent
/// document is an empty document with digest \"\""), `400`
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
    anyhow::anyhow!("{status}: {err}")
}

fn bad_request(msg: impl std::fmt::Display) -> anyhow::Error {
    anyhow::anyhow!("400: {msg}")
}

/// A write reached outside the caller's grants (`docs/DESIGN.md` §2): 403,
/// naming the offending path.
fn forbidden(path: &DocPath) -> anyhow::Error {
    let shown = if path.is_root() {
        "(the whole document)".to_string()
    } else {
        path.to_string()
    };
    anyhow::anyhow!("403: not permitted: {shown}")
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

/// Parses a view's wire `format`: only `json`/`yml`/`yaml` are valid here
/// (`docs/DESIGN.md` §3 -- TOML is a storage format, never a view's wire
/// format).
fn parse_view_format(name: &str) -> Result<Format, anyhow::Error> {
    match Format::from_ext(name) {
        Some(Format::Json) => Ok(Format::Json),
        Some(Format::Yaml) => Ok(Format::Yaml),
        _ => Err(bad_request(format!("invalid format '{name}': expected 'json' or 'yaml'"))),
    }
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

fn check_digest(expected: Option<&str>, actual: &str) -> Result<(), anyhow::Error> {
    if let Some(expected) = expected {
        if expected != actual {
            return Err(api_err(CoreError::DigestMismatch {
                expected: expected.to_string(),
                actual: actual.to_string(),
            }));
        }
    }
    Ok(())
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
/// (format=yaml) is populated (`docs/DESIGN.md` §3).
#[derive(Debug, Clone, Serialize)]
pub struct ApiViewDocument {
    pub id: String,
    pub view: String,
    pub digest: String,
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

/// `api_version()` -> `{ token, changed }`.
pub fn version() -> Result<ApiVersion, anyhow::Error> {
    let v = open_store().version().map_err(api_err)?;
    Ok(ApiVersion {
        token: v.token,
        changed: unix_secs(v.changed),
    })
}

/// `api_grants($authid)`: the datacenter document's `scopes` entries for
/// `authid`, as a JSON array of `{prefix, mode}` (empty array if it has
/// none, or if there is no datacenter document at all).
///
/// # Errors
/// `400:` if the datacenter document's `scopes` map is malformed.
pub fn grants(authid: &str) -> Result<String, anyhow::Error> {
    let store = open_store();
    let dc_value = match store.read(DocId::Datacenter) {
        Ok(doc) => doc.value,
        Err(CoreError::NotFound(_)) => Value::Object(Map::new()),
        Err(e) => return Err(api_err(e)),
    };
    let scopes = scopes::scopes_for(&dc_value, authid).map_err(api_err)?;
    Ok(serde_json::to_string(&scopes).expect("Vec<Scope> always serializes"))
}

/// Best-effort read of a guest's display name from its config
/// (`nodes/<node>/{qemu-server,lxc}/<vmid>.conf`): `name:` for qemu,
/// `hostname:` for lxc.
fn guest_display_name(node: &str, vmid: u32, kind: GuestKind) -> Option<String> {
    let (subdir, key) = match kind {
        GuestKind::Qemu => ("qemu-server", "name"),
        GuestKind::Lxc => ("lxc", "hostname"),
    };
    let path = pve_root().join("nodes").join(node).join(subdir).join(format!("{vmid}.conf"));
    let text = std::fs::read_to_string(path).ok()?;
    for line in text.lines() {
        // A "[PENDING]"/snapshot section header ends the guest's live config.
        if line.starts_with('[') {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            if k.trim() == key {
                return Some(v.trim().to_string());
            }
        }
    }
    None
}

/// `api_list_guests($grants_json, $has)`: every vmid in `.vmlist` the
/// caller can read anything of, with `keys` filtered to what they may see.
///
/// `grants_json` is a JSON *object* mapping each vmid (as a decimal string)
/// to that vmid's [`Grants`] -- `scopes` is the same list for every guest
/// (`docs/DESIGN.md` §2: scopes apply to every guest document), but
/// `full_read` varies per vmid (`VM.Audit`), so Perl computes and batches
/// the whole map in one call rather than one call per guest.
///
/// # Errors
/// `400:` if `grants_json` or `has` is malformed.
pub fn list_guests(grants_json: &str, has: Option<&str>) -> Result<Vec<GuestListEntry>, anyhow::Error> {
    let grants_map: HashMap<String, Grants> =
        serde_json::from_str(grants_json).map_err(|e| bad_request(format!("invalid grants: {e}")))?;
    let has_path = has.map(DocPath::parse).transpose().map_err(api_err)?;

    let Ok(vmlist) = pve_meta_core::vmlist::read_vmlist(vmlist_path()) else {
        return Ok(Vec::new());
    };
    let store = open_store();

    let mut out = Vec::new();
    for (vmid, info) in &vmlist.guests {
        let Some(grants) = grants_map.get(&vmid.to_string()) else {
            continue;
        };
        let readable = grants.readable_prefixes();
        if readable.is_empty() {
            continue;
        }

        let (value, digest) = read_or_empty(&store, DocId::Guest(*vmid))?;
        let visible = view::filter(&value, &readable);

        if let Some(path) = &has_path {
            if model::get_path(&visible, path).is_none() {
                continue;
            }
        }

        out.push(GuestListEntry {
            vmid: *vmid,
            node: Some(info.node.clone()),
            kind: Some(
                match info.kind {
                    GuestKind::Qemu => "qemu",
                    GuestKind::Lxc => "lxc",
                }
                .to_string(),
            ),
            name: guest_display_name(&info.node, *vmid, info.kind),
            digest,
            keys: model::namespaces(&visible),
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
/// # Errors
/// `400:` invalid id/view/format/grants. `403:` `view` given but not
/// readable.
pub fn get_document(
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

    let (value, digest) = read_or_empty(&open_store(), doc_id)?;

    let mut result_value = if view.is_some() {
        if !grants.can_read(&view_path) {
            return Err(forbidden(&view_path));
        }
        view::extract(&value, &view_path).unwrap_or_else(|| Value::Object(Map::new()))
    } else {
        view::filter(&value, &grants.readable_prefixes())
    };

    if !comments {
        model::strip_comments(&mut result_value);
    }

    let (data_json, text) = match fmt {
        Format::Json => (
            Some(serde_json::to_string(&result_value).expect("serde_json::Value always serializes")),
            None,
        ),
        Format::Yaml => (None, Some(view::render(&result_value, Format::Yaml))),
        Format::Toml => unreachable!("parse_view_format rejects toml"),
    };

    Ok(ApiViewDocument {
        id: id_str(doc_id),
        view: view_out(view),
        digest,
        data_json,
        text,
    })
}

fn apply_mode(mode: &str, value: &mut Value, view_path: &DocPath, payload: &Value) -> Result<Vec<Touched>, anyhow::Error> {
    match mode {
        "replace" | "" => view::replace(value, view_path, payload.clone()).map_err(api_err),
        "merge" => view::merge(value, view_path, payload).map_err(api_err),
        other => Err(bad_request(format!("invalid mode '{other}': expected 'replace' or 'merge'"))),
    }
}

/// `api_put($id, $view, $format, $payload, $mode, $digest, $dry_run, $grants_json)`.
///
/// `mode` is `"replace"` (default: the view's subtree is replaced by
/// `payload` wholesale) or `"merge"` (RFC 7386-style merge-patch relative to
/// the view). Every path the operation touches must be writable per
/// `grants_json` ([`Grants::check_write`]) -- not just the view path itself.
///
/// # Errors
/// `400:` invalid id/view/format/mode/payload/grants. `409:` digest
/// mismatch. `403:` a touched path is outside the caller's write grants.
#[allow(clippy::too_many_arguments)] // matches the PUT endpoint's parameter set 1:1 (docs/DESIGN.md §3)
pub fn put_document(
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
    let payload_value = view::parse(payload, fmt).map_err(api_err)?;

    let store = open_store();
    let (mut value, current_digest) = read_or_empty(&store, doc_id)?;
    check_digest(digest, &current_digest)?;

    let touched = apply_mode(mode, &mut value, &view_path, &payload_value)?;

    if let Err(denied) = grants.check_write(&touched) {
        return Err(forbidden(&denied));
    }

    let target_format = match store.locate(doc_id).map_err(api_err)? {
        Some(located) => located.format,
        None => store.default_format().map_err(api_err)?,
    };
    let text = pve_meta_core::format::dump(target_format, &value);

    let new_digest = if dry_run {
        pve_meta_core::digest::digest(text.as_bytes())
    } else {
        store
            .put_raw(doc_id, &text, Some(target_format), digest)
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
/// `view`, or the whole document (including, for a guest, its snapshots) if
/// `view` is absent.
///
/// # Errors
/// `400:` invalid id/view/grants. `409:` digest mismatch. `403:` a touched
/// path is outside the caller's write grants.
pub fn delete_document(
    id: &str,
    view: Option<&str>,
    digest: Option<&str>,
    grants_json: &str,
) -> Result<ApiPutResult, anyhow::Error> {
    let doc_id = parse_id(id)?;
    let grants = parse_grants(grants_json)?;
    let view_path = parse_view(view)?;

    let store = open_store();
    let (mut value, current_digest) = read_or_empty(&store, doc_id)?;
    check_digest(digest, &current_digest)?;

    let touched = view::remove(&mut value, &view_path).map_err(api_err)?;

    if let Err(denied) = grants.check_write(&touched) {
        return Err(forbidden(&denied));
    }

    let existing_format = store.locate(doc_id).map_err(api_err)?.map(|l| l.format);
    let new_digest = if view_path.is_root() {
        if existing_format.is_some() {
            store.delete(doc_id).map_err(api_err)?;
        }
        String::new()
    } else {
        match existing_format {
            None => String::new(),
            Some(fmt) => {
                let text = pve_meta_core::format::dump(fmt, &value);
                store
                    .put_raw(doc_id, &text, Some(fmt), digest)
                    .map_err(api_err)?
                    .document
                    .digest
            }
        }
    };

    Ok(ApiPutResult {
        id: id_str(doc_id),
        view: view_out(view),
        digest: new_digest,
        touched: touched_out(&touched),
    })
}
