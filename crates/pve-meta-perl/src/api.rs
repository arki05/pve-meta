//! API-shaped helpers backing the `api_*` exports of `PVE::RS::Meta`
//! (`docs/NATIVE-API-SPEC.md`), called from the `#[export]` functions in
//! `lib.rs`'s `pve_rs_meta` package.
//!
//! These mirror the handlers in `crates/pve-meta-api/src/api/*.rs` (the
//! standalone `pve-metad` daemon's implementation of the same
//! `docs/API.md` tree) closely enough that the two servers agree on
//! behavior, but are independent of `proxmox-router`/`proxmox-schema` (and
//! that crate's process-global store) so this `cdylib` — loaded directly
//! into pveproxy/pvedaemon — stays free of that dependency tree and keeps
//! this crate's existing "open a fresh `MetaStore` from `$PVE_META_ROOT` on
//! every call" convention (see [`super::open_store`]).
//!
//! ## Wire contract
//!
//! Document content that is arbitrary (a guest/datacenter document's
//! `data`, a subtree, a JSON Schema from the registry) crosses to Perl as a
//! JSON **string** (fields named `*_json`), decoded there with `JSON::PP`
//! to preserve numbers/booleans faithfully — see `docs/NATIVE-API-SPEC.md`
//! ("Document values cross the boundary as JSON strings"). Every other
//! field (ids, digests, timestamps, flags, path/namespace lists, touched
//! entries) is plain and well-typed, so perlmod's `serde`-based conversion
//! renders it directly as a native Perl scalar/array/hash.
//!
//! Errors are `anyhow::Error`s whose `Display` is `"<NNN>: <message>"` (an
//! HTTP status prefix and a human-readable message); the Perl layer parses
//! that prefix back out and re-raises via `PVE::Exception::raise`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{Map, Value};

use pve_meta_core::patch::{self, Op, Touched};
use pve_meta_core::path::Path as DocPath;
use pve_meta_core::store::{DocId, Document};
use pve_meta_core::vmlist::GuestKind;
use pve_meta_core::Error as CoreError;

use super::open_store;

/// Env var overriding the pmxcfs mount root used to read guest configs for
/// [`inventory`] (default `/etc/pve`). Mirrors `pve-meta-api`'s
/// `PVE_META_PVE_ROOT` (`crates/pve-meta-api/src/api/meta.rs`) so both
/// servers agree, but (like that one) is a test/dev seam, not part of the
/// wire contract.
pub const PVE_ROOT_ENV: &str = "PVE_META_PVE_ROOT";
/// Env var overriding the vmlist path used by [`inventory`] and
/// [`list_guests`] (default `<pve-root>/.vmlist`).
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
/// expects (`docs/NATIVE-API-SPEC.md`): `409` digest mismatch/conflict,
/// `404` not found, `400` lint/parse/invalid-path/invalid-name/too-large,
/// `500` everything else. Mirrors `pve-meta-api`'s `error::to_http`
/// (`crates/pve-meta-api/src/error.rs`) status mapping.
pub fn api_err(err: CoreError) -> anyhow::Error {
    let status: u16 = match &err {
        CoreError::DigestMismatch { .. } | CoreError::Conflict(_) => 409,
        CoreError::NotFound(_) => 404,
        CoreError::Lint(_)
        | CoreError::Parse { .. }
        | CoreError::InvalidPath(_)
        | CoreError::InvalidName(_)
        | CoreError::TooLarge { .. } => 400,
        CoreError::Io(_) | CoreError::Other(_) => 500,
    };
    anyhow::anyhow!("{status}: {err}")
}

fn bad_request(msg: impl std::fmt::Display) -> anyhow::Error {
    anyhow::anyhow!("400: {msg}")
}

fn conflict_no_document(expected: &str) -> anyhow::Error {
    api_err(CoreError::DigestMismatch {
        expected: expected.to_string(),
        actual: String::new(),
    })
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

/// Parses a format name (`yaml`/`yml`/`toml`/`json`), for endpoints that
/// take one as a string parameter.
pub fn parse_format(name: &str) -> Result<pve_meta_core::format::Format, anyhow::Error> {
    pve_meta_core::format::Format::from_ext(name).ok_or_else(|| bad_request(format!("unknown format '{name}'")))
}

fn data_json(value: &Value, comments: bool) -> String {
    let mut v = value.clone();
    if !comments {
        pve_meta_core::model::strip_comments(&mut v);
    }
    serde_json::to_string(&v).expect("serde_json::Value always serializes")
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

/// `true` if a merge patch has a `null` at the top level -- mirrors
/// `pve_meta_core::store`'s private helper of the same name: creating a
/// document purely to immediately delete a top-level key from it is
/// rejected as [`CoreError::NotFound`], same as a real (non-dry-run) write
/// against a document that does not exist.
fn has_top_level_delete(patch: &Value) -> bool {
    patch.as_object().is_some_and(|m| m.values().any(Value::is_null))
}

fn normalize_trailing_newline(text: &str) -> String {
    let trimmed = text.trim_end_matches('\n');
    format!("{trimmed}\n")
}

fn document_view(doc: &Document, comments: bool, raw_text: Option<&str>, touched: Option<Vec<ApiTouched>>) -> ApiDocument {
    ApiDocument {
        id: id_str(doc.id),
        format: doc.format.to_string(),
        digest: doc.digest.clone(),
        mtime: unix_secs(doc.mtime),
        data_json: data_json(&doc.value, comments),
        raw: raw_text.map(str::to_string),
        touched,
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ApiVersion {
    pub token: String,
    pub changed: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApiHealthStore {
    pub root: String,
    pub files: u64,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApiHealth {
    pub store: ApiHealthStore,
    pub version: String,
    /// Always `{}` in v1 (reserved; see `docs/NATIVE-API-SPEC.md`).
    pub hooks: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct InventoryEntry {
    pub vmid: u32,
    pub node: Option<String>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub name: Option<String>,
    pub has_meta: bool,
    pub format: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GuestListEntry {
    pub vmid: u32,
    pub node: Option<String>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub format: String,
    pub digest: String,
    pub mtime: u64,
    pub size: u64,
    pub namespaces: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApiTouched {
    pub path: String,
    pub op: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApiDocument {
    pub id: String,
    pub format: String,
    pub digest: String,
    pub mtime: u64,
    pub data_json: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub touched: Option<Vec<ApiTouched>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApiSubtree {
    pub data_json: String,
    pub digest: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApiCreated {
    pub created: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApiOutcome {
    pub outcome: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApiClaim {
    pub prefix: String,
    pub scope: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApiRegistryEntry {
    pub name: String,
    pub claims: Vec<ApiClaim>,
    pub schemas_json: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// `api_version()`.
pub fn version() -> Result<ApiVersion, anyhow::Error> {
    let v = open_store().version().map_err(api_err)?;
    Ok(ApiVersion {
        token: v.token,
        changed: unix_secs(v.changed),
    })
}

/// `api_health()`.
pub fn health() -> Result<ApiHealth, anyhow::Error> {
    let store = open_store();
    let root = store.root().display().to_string();
    let guests = store.list_guests().map_err(api_err)?;
    let bytes: u64 = guests.iter().map(|g| g.size).sum();
    Ok(ApiHealth {
        store: ApiHealthStore {
            root,
            files: guests.len() as u64,
            bytes,
        },
        version: env!("CARGO_PKG_VERSION").to_string(),
        hooks: HashMap::new(),
    })
}

/// Best-effort read of a guest's display name from its config
/// (`nodes/<node>/{qemu-server,lxc}/<vmid>.conf`): `name:` for qemu,
/// `hostname:` for lxc. Mirrors `pve-meta-api`'s `meta::guest_display_name`.
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

/// `api_inventory()`.
pub fn inventory() -> Result<Vec<InventoryEntry>, anyhow::Error> {
    let vmlist = pve_meta_core::vmlist::read_vmlist(vmlist_path()).map_err(api_err)?;
    let store = open_store();
    let guest_docs: HashSet<u32> = store.list_guests().map_err(api_err)?.into_iter().map(|g| g.vmid).collect();

    let mut out = Vec::new();
    for (vmid, info) in &vmlist.guests {
        let has_meta = guest_docs.contains(vmid);
        let format = if has_meta {
            store.locate(DocId::Guest(*vmid)).ok().flatten().map(|l| l.format.to_string())
        } else {
            None
        };
        out.push(InventoryEntry {
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
            has_meta,
            format,
        });
    }
    Ok(out)
}

/// `api_list_guests($has)`.
pub fn list_guests(has: Option<&str>) -> Result<Vec<GuestListEntry>, anyhow::Error> {
    let vmlist = pve_meta_core::vmlist::read_vmlist(vmlist_path()).ok();
    let has_path = has.map(DocPath::parse).transpose().map_err(api_err)?;
    let store = open_store();

    let mut out = Vec::new();
    for entry in store.list_guests().map_err(api_err)? {
        let doc = store.read(DocId::Guest(entry.vmid)).map_err(api_err)?;
        let mut value = doc.value;
        pve_meta_core::model::strip_comments(&mut value);

        if let Some(path) = &has_path {
            if pve_meta_core::model::get_path(&value, path).is_none() {
                continue;
            }
        }

        let info = vmlist.as_ref().and_then(|l| l.guests.get(&entry.vmid));
        out.push(GuestListEntry {
            vmid: entry.vmid,
            node: info.map(|i| i.node.clone()),
            kind: info.map(|i| {
                match i.kind {
                    GuestKind::Qemu => "qemu",
                    GuestKind::Lxc => "lxc",
                }
                .to_string()
            }),
            format: entry.format.to_string(),
            digest: entry.digest,
            mtime: unix_secs(entry.mtime),
            size: entry.size,
            namespaces: pve_meta_core::model::namespaces(&value),
        });
    }
    Ok(out)
}

/// `api_get($id, $comments, $raw)`.
pub fn get_document(id: &str, comments: bool, raw: bool) -> Result<ApiDocument, anyhow::Error> {
    let doc_id = parse_id(id)?;
    let doc = open_store().read(doc_id).map_err(api_err)?;
    let raw_text = raw.then_some(doc.raw.as_str());
    Ok(document_view(&doc, comments, raw_text, None))
}

/// `api_subtree($id, $path)`. Comment keys are always stripped (matching
/// `docs/API.md`, not configurable).
pub fn get_subtree(id: &str, path: &str) -> Result<ApiSubtree, anyhow::Error> {
    let doc_id = parse_id(id)?;
    let doc = open_store().read(doc_id).map_err(api_err)?;
    let parsed = DocPath::parse(path).map_err(api_err)?;
    let mut value = doc.value;
    pve_meta_core::model::strip_comments(&mut value);
    let sub = pve_meta_core::model::get_path(&value, &parsed)
        .ok_or_else(|| anyhow::anyhow!("404: no data at path '{path}'"))?;
    Ok(ApiSubtree {
        data_json: serde_json::to_string(sub).expect("serde_json::Value always serializes"),
        digest: doc.digest,
    })
}

/// `api_patch($id, $patch_json, $digest, $dry_run)`. `dry_run` support is
/// implemented here (not in `pve_meta_core::store::MetaStore`, which only
/// ever writes): mirrors `pve-meta-api`'s `common::patch_document`.
pub fn patch_document(id: &str, patch_json: &str, digest: Option<&str>, dry_run: bool) -> Result<ApiDocument, anyhow::Error> {
    let doc_id = parse_id(id)?;
    let patch_value: Value =
        serde_json::from_str(patch_json).map_err(|e| bad_request(format!("invalid patch JSON: {e}")))?;
    let store = open_store();

    if !dry_run {
        let old_value = match store.read(doc_id) {
            Ok(doc) => doc.value,
            Err(CoreError::NotFound(_)) => Value::Object(Map::new()),
            Err(e) => return Err(api_err(e)),
        };
        let new_doc = store.patch(doc_id, &patch_value, digest).map_err(api_err)?;
        let touched = patch::diff(&old_value, &new_doc.value);
        Ok(document_view(&new_doc, true, None, Some(touched_out(&touched))))
    } else {
        let lints = patch::lint_patch(&patch_value);
        if !lints.is_empty() {
            return Err(api_err(CoreError::Lint(lints)));
        }
        let (fmt, old_text, old_value) = match store.locate(doc_id).map_err(api_err)? {
            Some(located) => {
                let doc = store.read(doc_id).map_err(api_err)?;
                if let Some(expected) = digest {
                    if expected != doc.digest {
                        return Err(api_err(CoreError::DigestMismatch {
                            expected: expected.to_string(),
                            actual: doc.digest,
                        }));
                    }
                }
                (located.format, doc.raw, doc.value)
            }
            None => {
                if let Some(expected) = digest {
                    return Err(conflict_no_document(expected));
                }
                if has_top_level_delete(&patch_value) {
                    return Err(api_err(CoreError::NotFound(doc_id)));
                }
                let fmt = store.default_format().map_err(api_err)?;
                let empty = Value::Object(Map::new());
                (fmt, pve_meta_core::format::dump(fmt, &empty), empty)
            }
        };
        let result = pve_meta_core::edit::apply_patch_text(fmt, &old_text, &patch_value).map_err(api_err)?;
        let touched = patch::diff(&old_value, &result.value);
        let dig = pve_meta_core::digest::digest(result.text.as_bytes());
        let synthetic = Document {
            id: doc_id,
            format: fmt,
            path: PathBuf::new(),
            raw: result.text,
            value: result.value,
            digest: dig,
            mtime: SystemTime::now(),
        };
        Ok(document_view(&synthetic, true, None, Some(touched_out(&touched))))
    }
}

/// `api_put_raw($id, $content, $format, $digest, $dry_run)`. Mirrors
/// `pve-meta-api`'s `common::put_raw`.
pub fn put_raw_document(
    id: &str,
    content: &str,
    format: Option<&str>,
    digest: Option<&str>,
    dry_run: bool,
) -> Result<ApiDocument, anyhow::Error> {
    let doc_id = parse_id(id)?;
    let format_override = format.map(parse_format).transpose()?;
    let store = open_store();

    if !dry_run {
        let result = store.put_raw(doc_id, content, format_override, digest).map_err(api_err)?;
        Ok(document_view(&result.document, true, None, Some(touched_out(&result.touched))))
    } else {
        let located = store.locate(doc_id).map_err(api_err)?;
        let (old_value, target_format) = match &located {
            Some(l) => {
                let doc = store.read(doc_id).map_err(api_err)?;
                if let Some(expected) = digest {
                    if expected != doc.digest {
                        return Err(api_err(CoreError::DigestMismatch {
                            expected: expected.to_string(),
                            actual: doc.digest,
                        }));
                    }
                }
                (doc.value, format_override.unwrap_or(l.format))
            }
            None => {
                if let Some(expected) = digest {
                    return Err(conflict_no_document(expected));
                }
                (
                    Value::Object(Map::new()),
                    format_override.unwrap_or(store.default_format().map_err(api_err)?),
                )
            }
        };
        let normalized = normalize_trailing_newline(content);
        let new_value = pve_meta_core::format::parse(target_format, &normalized).map_err(api_err)?;
        let touched = patch::diff(&old_value, &new_value);
        let dig = pve_meta_core::digest::digest(normalized.as_bytes());
        let synthetic = Document {
            id: doc_id,
            format: target_format,
            path: PathBuf::new(),
            raw: normalized.clone(),
            value: new_value,
            digest: dig,
            mtime: SystemTime::now(),
        };
        Ok(document_view(&synthetic, true, Some(&normalized), Some(touched_out(&touched))))
    }
}

/// `api_convert($id, $format, $digest)`.
pub fn convert_document(id: &str, to: &str, digest: Option<&str>) -> Result<ApiDocument, anyhow::Error> {
    let doc_id = parse_id(id)?;
    let target = parse_format(to)?;
    let doc = open_store().convert(doc_id, target, digest).map_err(api_err)?;
    Ok(document_view(&doc, true, None, None))
}

/// `api_delete($id)`.
pub fn delete_document(id: &str) -> Result<bool, anyhow::Error> {
    let doc_id = parse_id(id)?;
    open_store().delete(doc_id).map_err(api_err)?;
    Ok(true)
}

/// `api_snapshots($id)`.
pub fn list_guest_snapshots(vmid: u32) -> Result<Vec<String>, anyhow::Error> {
    open_store().list_snapshots(vmid).map_err(api_err)
}

/// `api_snapshot($id, $name)`.
pub fn snapshot_guest(vmid: u32, name: &str) -> Result<ApiCreated, anyhow::Error> {
    let created = open_store().snapshot(vmid, name).map_err(api_err)?;
    Ok(ApiCreated { created })
}

/// `api_rollback($id, $name)`.
pub fn rollback_guest(vmid: u32, name: &str) -> Result<ApiOutcome, anyhow::Error> {
    let outcome = open_store().rollback(vmid, name).map_err(api_err)?;
    let outcome_str = match outcome {
        pve_meta_core::store::RollbackOutcome::Restored => "restored",
        pve_meta_core::store::RollbackOutcome::RemovedNoSnapshot => "removed_no_snapshot",
        pve_meta_core::store::RollbackOutcome::NoOp => "noop",
    };
    Ok(ApiOutcome {
        outcome: outcome_str.to_string(),
    })
}

/// `api_delete_snapshot($id, $name)`.
pub fn delete_guest_snapshot(vmid: u32, name: &str) -> Result<bool, anyhow::Error> {
    open_store().delete_snapshot(vmid, name).map_err(api_err)?;
    Ok(true)
}

/// `api_clone($id, $newid)`.
pub fn clone_document(vmid: u32, newid: u32) -> Result<ApiDocument, anyhow::Error> {
    let doc = open_store().clone(vmid, newid).map_err(api_err)?;
    Ok(document_view(&doc, true, None, None))
}

/// Reads `datacenter.operators` (comment keys stripped), or an empty object
/// if the datacenter document does not exist.
fn read_operators() -> Result<Map<String, Value>, anyhow::Error> {
    let value = match open_store().read(DocId::Datacenter) {
        Ok(doc) => {
            let mut v = doc.value;
            pve_meta_core::model::strip_comments(&mut v);
            v
        }
        Err(CoreError::NotFound(_)) => Value::Object(Map::new()),
        Err(e) => return Err(api_err(e)),
    };
    Ok(value.get("operators").and_then(|v| v.as_object()).cloned().unwrap_or_default())
}

/// `api_registry()`.
pub fn registry() -> Result<Vec<ApiRegistryEntry>, anyhow::Error> {
    let operators = read_operators()?;
    Ok(operators
        .iter()
        .map(|(name, op)| {
            let claims = op
                .get("claims")
                .and_then(|c| c.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|c| {
                            Some(ApiClaim {
                                prefix: c.get("prefix")?.as_str()?.to_string(),
                                scope: c.get("scope").and_then(|s| s.as_str()).unwrap_or("rw").to_string(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            let schemas = op.get("schemas").cloned().unwrap_or_else(|| Value::Object(Map::new()));
            ApiRegistryEntry {
                name: name.clone(),
                claims,
                schemas_json: serde_json::to_string(&schemas).expect("serde_json::Value always serializes"),
                description: op.get("description").and_then(|d| d.as_str()).map(str::to_string),
            }
        })
        .collect())
}

/// `api_schemas($id)`: the JSON schemas applicable to a guest's document
/// (i.e. for namespaces it actually uses), keyed by namespace prefix, as a
/// single JSON object string (the whole map is "document value"-shaped
/// arbitrary content -- see the module docs).
pub fn schemas_for_guest(vmid: u32) -> Result<String, anyhow::Error> {
    let store = open_store();
    let doc = store.read(DocId::Guest(vmid)).map_err(api_err)?;
    let mut value = doc.value;
    pve_meta_core::model::strip_comments(&mut value);
    let namespaces: HashSet<String> = pve_meta_core::model::namespaces(&value).into_iter().collect();

    let operators = read_operators()?;
    let mut out = Map::new();
    for op in operators.values() {
        let Some(claims) = op.get("claims").and_then(|c| c.as_array()) else {
            continue;
        };
        let Some(schemas) = op.get("schemas").and_then(|s| s.as_object()) else {
            continue;
        };
        for claim in claims {
            let Some(prefix) = claim.get("prefix").and_then(|p| p.as_str()) else {
                continue;
            };
            if namespaces.contains(prefix) {
                if let Some(schema) = schemas.get(prefix) {
                    out.insert(prefix.to_string(), schema.clone());
                }
            }
        }
    }
    Ok(serde_json::to_string(&Value::Object(out)).expect("serde_json::Value always serializes"))
}
