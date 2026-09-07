//! `/meta/datacenter/...`.

use anyhow::Error;
use serde_json::Value;

use proxmox_router::{Permission, Router, RpcEnvironment, SubdirMap};
use proxmox_schema::api;

use pve_meta_core::store::DocId;

use super::common;

const DC: DocId = DocId::Datacenter;

#[api(
    input: {
        properties: {
            comments: { type: Boolean, optional: true, default: false, description: "Keep comment keys." },
            raw: { type: Boolean, optional: true, default: false, description: "Include the raw file text." },
        },
    },
    access: { permission: &Permission::Anybody },
)]
/// Gets the datacenter document.
pub fn get_datacenter(comments: bool, raw: bool) -> Result<Value, Error> {
    common::get_document(DC, comments, raw)
}

#[api(
    input: {
        properties: {
            patch: { type: Object, properties: {}, additional_properties: true, description: "Merge patch (null deletes)." },
            digest: { type: String, optional: true, description: "Expected current digest." },
            dry_run: { type: Boolean, optional: true, default: false, description: "Validate/diff without writing." },
        },
    },
    access: { permission: &Permission::Anybody },
)]
/// Applies a merge patch to the datacenter document (creating it if absent).
pub fn patch_datacenter(
    patch: Value,
    digest: Option<String>,
    dry_run: bool,
    rpcenv: &mut dyn RpcEnvironment,
) -> Result<Value, Error> {
    let auth_id = rpcenv.get_auth_id().unwrap_or_default();
    let result = common::patch_document(DC, &patch, digest.as_deref(), dry_run);
    if let Ok(response) = &result {
        if !dry_run {
            tracing::info!(auth_id = %auth_id, touched = %response["touched"], "patched datacenter document");
        }
    }
    result
}

#[api(
    input: { properties: { path: { type: String, description: "Dotted path into the document." } } },
    access: { permission: &Permission::Anybody },
)]
/// Gets a subtree of the datacenter document at a dotted path.
pub fn get_datacenter_subtree(path: String) -> Result<Value, Error> {
    common::get_subtree(DC, &path)
}

#[api(
    input: {
        properties: {
            content: { type: String, description: "Full replacement file text." },
            format: { type: String, optional: true, description: "Switch the document's format/extension." },
            digest: { type: String, optional: true, description: "Expected current digest." },
            dry_run: { type: Boolean, optional: true, default: false, description: "Validate/diff without writing." },
        },
    },
    access: { permission: &Permission::Anybody },
)]
/// Replaces the datacenter document with raw text.
pub fn put_datacenter_raw(
    content: String,
    format: Option<String>,
    digest: Option<String>,
    dry_run: bool,
    rpcenv: &mut dyn RpcEnvironment,
) -> Result<Value, Error> {
    let auth_id = rpcenv.get_auth_id().unwrap_or_default();
    let format = format.map(|f| common::parse_format(&f)).transpose()?;
    let result = common::put_raw(DC, &content, format, digest.as_deref(), dry_run);
    if !dry_run && result.is_ok() {
        tracing::info!(auth_id = %auth_id, "put raw datacenter document");
    }
    result
}

#[api(
    input: {
        properties: {
            format: { type: String, description: "Target format." },
            digest: { type: String, optional: true, description: "Expected current digest." },
        },
    },
    access: { permission: &Permission::Anybody },
)]
/// Converts the datacenter document to another format.
pub fn convert_datacenter(format: String, digest: Option<String>, rpcenv: &mut dyn RpcEnvironment) -> Result<Value, Error> {
    let auth_id = rpcenv.get_auth_id().unwrap_or_default();
    let to = common::parse_format(&format)?;
    let result = common::convert(DC, to, digest.as_deref());
    if result.is_ok() {
        tracing::info!(auth_id = %auth_id, format = %to, "converted datacenter document");
    }
    result
}

#[api(access: { permission: &Permission::Anybody })]
/// Deletes the datacenter document.
pub fn delete_datacenter(rpcenv: &mut dyn RpcEnvironment) -> Result<Value, Error> {
    let auth_id = rpcenv.get_auth_id().unwrap_or_default();
    let result = common::delete(DC);
    if result.is_ok() {
        tracing::info!(auth_id = %auth_id, "deleted datacenter document");
    }
    result
}

const DATACENTER_SUBDIRS: SubdirMap = &[
    ("convert", &Router::new().post(&API_METHOD_CONVERT_DATACENTER)),
    ("raw", &Router::new().put(&API_METHOD_PUT_DATACENTER_RAW)),
    ("subtree", &Router::new().get(&API_METHOD_GET_DATACENTER_SUBTREE)),
];

pub const DATACENTER_ROUTER: Router = Router::new()
    .get(&API_METHOD_GET_DATACENTER)
    .put(&API_METHOD_PATCH_DATACENTER)
    .delete(&API_METHOD_DELETE_DATACENTER)
    .subdirs(DATACENTER_SUBDIRS);
