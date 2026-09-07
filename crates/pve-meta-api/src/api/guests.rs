//! `/meta/guests` and `/meta/guests/{vmid}/...`.

use anyhow::Error;
use serde_json::Value;

use proxmox_router::{Permission, Router, RpcEnvironment, SubdirMap};
use proxmox_schema::api;

use pve_meta_core::model;
use pve_meta_core::store::DocId;

use crate::error::to_http;
use crate::store::store;

use super::common;

const VMID_SCHEMA: proxmox_schema::Schema = proxmox_schema::IntegerSchema::new("The guest's vmid.")
    .minimum(100)
    .maximum(999_999_999)
    .schema();

#[api(
    input: {
        properties: {
            has: {
                type: String,
                optional: true,
                description: "Only list guests whose document has data at this dotted path.",
            },
        },
    },
    access: { permission: &Permission::Anybody },
)]
/// Lists all guest documents.
pub fn list_guests(has: Option<String>) -> Result<Value, Error> {
    let vmlist = pve_meta_core::vmlist::read_vmlist(crate::store::vmlist_path()).ok();
    let has_path = has
        .as_deref()
        .map(pve_meta_core::path::Path::parse)
        .transpose()
        .map_err(to_http)?;

    let mut out = Vec::new();
    for entry in store().list_guests().map_err(to_http)? {
        let doc = store().read(DocId::Guest(entry.vmid)).map_err(to_http)?;
        let mut value = doc.value;
        model::strip_comments(&mut value);

        if let Some(path) = &has_path {
            if model::get_path(&value, path).is_none() {
                continue;
            }
        }

        let info = vmlist.as_ref().and_then(|l| l.guests.get(&entry.vmid));
        let mtime = entry
            .mtime
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        out.push(serde_json::json!({
            "vmid": entry.vmid,
            "node": info.map(|i| i.node.clone()),
            "type": info.map(|i| match i.kind {
                pve_meta_core::vmlist::GuestKind::Qemu => "qemu",
                pve_meta_core::vmlist::GuestKind::Lxc => "lxc",
            }),
            "format": entry.format.to_string(),
            "digest": entry.digest,
            "mtime": mtime,
            "size": entry.size,
            "namespaces": model::namespaces(&value),
            "orphan": info.is_none(),
        }));
    }
    Ok(Value::Array(out))
}

#[api(
    input: {
        properties: {
            vmid: { schema: VMID_SCHEMA },
            comments: { type: Boolean, optional: true, default: false, description: "Keep comment keys." },
            raw: { type: Boolean, optional: true, default: false, description: "Include the raw file text." },
        },
    },
    access: { permission: &Permission::Anybody },
)]
/// Gets a guest's document.
pub fn get_guest(vmid: u32, comments: bool, raw: bool) -> Result<Value, Error> {
    common::get_document(DocId::Guest(vmid), comments, raw)
}

#[api(
    input: {
        properties: {
            vmid: { schema: VMID_SCHEMA },
            patch: { type: Object, properties: {}, additional_properties: true, description: "Merge patch (null deletes)." },
            digest: { type: String, optional: true, description: "Expected current digest." },
            dry_run: { type: Boolean, optional: true, default: false, description: "Validate/diff without writing." },
        },
    },
    access: { permission: &Permission::Anybody },
)]
/// Applies a merge patch to a guest's document (creating it if absent).
pub fn patch_guest(
    vmid: u32,
    patch: Value,
    digest: Option<String>,
    dry_run: bool,
    rpcenv: &mut dyn RpcEnvironment,
) -> Result<Value, Error> {
    let auth_id = rpcenv.get_auth_id().unwrap_or_default();
    let result = common::patch_document(DocId::Guest(vmid), &patch, digest.as_deref(), dry_run);
    if let Ok(response) = &result {
        if !dry_run {
            tracing::info!(auth_id = %auth_id, vmid = %vmid, touched = %response["touched"], "patched guest document");
        }
    }
    result
}

#[api(
    input: {
        properties: {
            vmid: { schema: VMID_SCHEMA },
            path: { type: String, description: "Dotted path into the document." },
        },
    },
    access: { permission: &Permission::Anybody },
)]
/// Gets a subtree of a guest's document at a dotted path.
pub fn get_guest_subtree(vmid: u32, path: String) -> Result<Value, Error> {
    common::get_subtree(DocId::Guest(vmid), &path)
}

#[api(
    input: {
        properties: {
            vmid: { schema: VMID_SCHEMA },
            content: { type: String, description: "Full replacement file text." },
            format: { type: String, optional: true, description: "Switch the document's format/extension." },
            digest: { type: String, optional: true, description: "Expected current digest." },
            dry_run: { type: Boolean, optional: true, default: false, description: "Validate/diff without writing." },
        },
    },
    access: { permission: &Permission::Anybody },
)]
/// Replaces a guest's document with raw text.
pub fn put_guest_raw(
    vmid: u32,
    content: String,
    format: Option<String>,
    digest: Option<String>,
    dry_run: bool,
    rpcenv: &mut dyn RpcEnvironment,
) -> Result<Value, Error> {
    let auth_id = rpcenv.get_auth_id().unwrap_or_default();
    let format = format.map(|f| common::parse_format(&f)).transpose()?;
    let result = common::put_raw(DocId::Guest(vmid), &content, format, digest.as_deref(), dry_run);
    if !dry_run && result.is_ok() {
        tracing::info!(auth_id = %auth_id, vmid = %vmid, "put raw guest document");
    }
    result
}

#[api(
    input: {
        properties: {
            vmid: { schema: VMID_SCHEMA },
            format: { type: String, description: "Target format." },
            digest: { type: String, optional: true, description: "Expected current digest." },
        },
    },
    access: { permission: &Permission::Anybody },
)]
/// Converts a guest's document to another format.
pub fn convert_guest(vmid: u32, format: String, digest: Option<String>, rpcenv: &mut dyn RpcEnvironment) -> Result<Value, Error> {
    let auth_id = rpcenv.get_auth_id().unwrap_or_default();
    let to = common::parse_format(&format)?;
    let result = common::convert(DocId::Guest(vmid), to, digest.as_deref());
    if result.is_ok() {
        tracing::info!(auth_id = %auth_id, vmid = %vmid, format = %to, "converted guest document");
    }
    result
}

#[api(
    input: { properties: { vmid: { schema: VMID_SCHEMA } } },
    access: { permission: &Permission::Anybody },
)]
/// Deletes a guest's document (and its snapshots).
pub fn delete_guest(vmid: u32, rpcenv: &mut dyn RpcEnvironment) -> Result<Value, Error> {
    let auth_id = rpcenv.get_auth_id().unwrap_or_default();
    let result = common::delete(DocId::Guest(vmid));
    if result.is_ok() {
        tracing::info!(auth_id = %auth_id, vmid = %vmid, "deleted guest document");
    }
    result
}

#[api(
    input: { properties: { vmid: { schema: VMID_SCHEMA } } },
    access: { permission: &Permission::Anybody },
)]
/// Lists a guest's snapshot names.
pub fn list_snapshots(vmid: u32) -> Result<Value, Error> {
    let names = store().list_snapshots(vmid).map_err(to_http)?;
    Ok(Value::Array(names.into_iter().map(Value::String).collect()))
}

#[api(
    input: {
        properties: {
            vmid: { schema: VMID_SCHEMA },
            name: { type: String, description: "Snapshot name." },
        },
    },
    access: { permission: &Permission::Anybody },
)]
/// Snapshots a guest's current document. No-op if it has none.
pub fn snapshot_guest(vmid: u32, name: String, rpcenv: &mut dyn RpcEnvironment) -> Result<Value, Error> {
    let auth_id = rpcenv.get_auth_id().unwrap_or_default();
    let created = store().snapshot(vmid, &name).map_err(to_http)?;
    tracing::info!(auth_id = %auth_id, vmid = %vmid, snapshot = %name, created, "snapshot guest document");
    Ok(serde_json::json!({ "created": created }))
}

#[api(
    input: {
        properties: {
            vmid: { schema: VMID_SCHEMA },
            name: { type: String, description: "Snapshot name." },
        },
    },
    access: { permission: &Permission::Anybody },
)]
/// Rolls a guest's document back to a snapshot.
pub fn rollback_guest(vmid: u32, name: String, rpcenv: &mut dyn RpcEnvironment) -> Result<Value, Error> {
    let auth_id = rpcenv.get_auth_id().unwrap_or_default();
    let outcome = store().rollback(vmid, &name).map_err(to_http)?;
    let outcome_str = match outcome {
        pve_meta_core::store::RollbackOutcome::Restored => "restored",
        pve_meta_core::store::RollbackOutcome::RemovedNoSnapshot => "removed_no_snapshot",
        pve_meta_core::store::RollbackOutcome::NoOp => "noop",
    };
    tracing::info!(auth_id = %auth_id, vmid = %vmid, snapshot = %name, outcome = outcome_str, "rolled back guest document");
    Ok(serde_json::json!({ "outcome": outcome_str }))
}

#[api(
    input: {
        properties: {
            vmid: { schema: VMID_SCHEMA },
            name: { type: String, description: "Snapshot name." },
        },
    },
    access: { permission: &Permission::Anybody },
)]
/// Deletes a guest's snapshot.
pub fn delete_snapshot(vmid: u32, name: String, rpcenv: &mut dyn RpcEnvironment) -> Result<Value, Error> {
    let auth_id = rpcenv.get_auth_id().unwrap_or_default();
    store().delete_snapshot(vmid, &name).map_err(to_http)?;
    tracing::info!(auth_id = %auth_id, vmid = %vmid, snapshot = %name, "deleted guest snapshot");
    Ok(Value::Null)
}

#[api(
    input: {
        properties: {
            vmid: { schema: VMID_SCHEMA },
            newid: {
                type: Integer,
                minimum: 100,
                maximum: 999_999_999,
                description: "The new guest's vmid.",
            },
        },
    },
    access: { permission: &Permission::Anybody },
)]
/// Clones a guest's document to another vmid.
pub fn clone_guest(vmid: u32, newid: u32, rpcenv: &mut dyn RpcEnvironment) -> Result<Value, Error> {
    let auth_id = rpcenv.get_auth_id().unwrap_or_default();
    let doc = store().clone(vmid, newid).map_err(to_http)?;
    tracing::info!(auth_id = %auth_id, vmid = %vmid, newid = %newid, "cloned guest document");
    common::get_document(doc.id, true, false)
}

const SNAPSHOTS_ROUTER: Router = Router::new()
    .get(&API_METHOD_LIST_SNAPSHOTS)
    .match_all("name", &Router::new().delete(&API_METHOD_DELETE_SNAPSHOT));

const GUEST_SUBDIRS: SubdirMap = &[
    ("clone", &Router::new().post(&API_METHOD_CLONE_GUEST)),
    ("convert", &Router::new().post(&API_METHOD_CONVERT_GUEST)),
    ("raw", &Router::new().put(&API_METHOD_PUT_GUEST_RAW)),
    ("rollback", &Router::new().post(&API_METHOD_ROLLBACK_GUEST)),
    ("snapshot", &Router::new().post(&API_METHOD_SNAPSHOT_GUEST)),
    ("snapshots", &SNAPSHOTS_ROUTER),
    ("subtree", &Router::new().get(&API_METHOD_GET_GUEST_SUBTREE)),
];

const GUEST_ITEM_ROUTER: Router = Router::new()
    .get(&API_METHOD_GET_GUEST)
    .put(&API_METHOD_PATCH_GUEST)
    .delete(&API_METHOD_DELETE_GUEST)
    .subdirs(GUEST_SUBDIRS);

pub const GUESTS_ROUTER: Router = Router::new()
    .get(&API_METHOD_LIST_GUESTS)
    .match_all("vmid", &GUEST_ITEM_ROUTER);
