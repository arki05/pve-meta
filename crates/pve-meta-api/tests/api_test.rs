//! Integration tests for the `pve-meta-api` router tree, driven the way `docs/DAEMON-SPEC.md`
//! prescribes: through `ApiMethod.handler` (the exact code path a real HTTP request or the CLI
//! takes), with a `CliEnvironment` standing in for the request environment, against a tempdir
//! store. No HTTP involved.
//!
//! The store is a process-wide `OnceLock` (see `pve_meta_api::store`), matching how the real
//! daemon/CLI use it, so every test in this binary shares one tempdir; tests avoid colliding by
//! using disjoint vmid ranges instead of separate stores.

use std::sync::OnceLock;

use serde_json::{json, Value};
use tempfile::TempDir;

use proxmox_router::cli::CliEnvironment;
use proxmox_router::{ApiHandler, ApiMethod, RpcEnvironment};
use proxmox_schema::ObjectSchemaType as _;

use pve_meta_api::api::{datacenter, guests, registry};

fn store_dir() -> &'static std::path::Path {
    static DIR: OnceLock<TempDir> = OnceLock::new();
    static INIT: OnceLock<()> = OnceLock::new();
    let dir = DIR.get_or_init(|| tempfile::tempdir().expect("tempdir"));
    INIT.get_or_init(|| {
        pve_meta_api::store::init(dir.path(), dir.path().join(".vmlist"));
    });
    dir.path()
}

fn env() -> CliEnvironment {
    let mut env = CliEnvironment::new();
    env.set_auth_id(Some("test@pve".to_string()));
    env
}

/// Calls a synchronous `#[api]` method exactly the way rest-server/the CLI would: through
/// `ApiMethod.handler`, not by calling the underlying Rust function directly. This exercises the
/// declared input schema (required/optional fields, `IntegerSchema` bounds, etc.), not just the
/// business logic.
fn call(method: &'static ApiMethod, params: Value) -> Result<Value, anyhow::Error> {
    store_dir();
    // Mirrors what proxmox-rest-server/the CLI do before ever reaching `ApiMethod.handler`:
    // validate the raw JSON against the declared input schema (required fields, `IntegerSchema`
    // bounds, etc.) — the generated wrapper itself only extracts fields, it does not re-verify.
    method.parameters.verify_json(&params)?;
    let mut rpcenv = env();
    match method.handler {
        ApiHandler::Sync(f) => f(params, method, &mut rpcenv),
        _ => panic!("unexpected handler kind (expected ApiHandler::Sync)"),
    }
}

fn status_code(err: &anyhow::Error) -> Option<u16> {
    err.downcast_ref::<proxmox_router::HttpError>().map(|e| e.code.as_u16())
}

#[test]
fn create_get_and_subtree() {
    let vmid = 100;
    let created = call(
        &guests::API_METHOD_PATCH_GUEST,
        json!({ "vmid": vmid, "patch": { "traefik": { "spec": { "host": "wiki.example" } } } }),
    )
    .expect("create via patch");
    assert_eq!(created["data"]["traefik"]["spec"]["host"], "wiki.example");
    assert_eq!(created["touched"][0]["op"], "set");

    let got = call(&guests::API_METHOD_GET_GUEST, json!({ "vmid": vmid })).expect("get");
    assert_eq!(got["data"]["traefik"]["spec"]["host"], "wiki.example");
    assert_eq!(got["format"], "yaml");
    assert!(got["digest"].as_str().unwrap().len() == 64);

    let subtree = call(
        &guests::API_METHOD_GET_GUEST_SUBTREE,
        json!({ "vmid": vmid, "path": "traefik.spec" }),
    )
    .expect("subtree");
    assert_eq!(subtree["data"]["host"], "wiki.example");

    let missing = call(
        &guests::API_METHOD_GET_GUEST_SUBTREE,
        json!({ "vmid": vmid, "path": "does.not.exist" }),
    );
    let err = missing.unwrap_err();
    assert_eq!(status_code(&err), Some(404));
}

#[test]
fn patch_digest_mismatch_is_conflict() {
    let vmid = 101;
    call(
        &guests::API_METHOD_PATCH_GUEST,
        json!({ "vmid": vmid, "patch": { "a": 1 } }),
    )
    .unwrap();

    let err = call(
        &guests::API_METHOD_PATCH_GUEST,
        json!({ "vmid": vmid, "patch": { "a": 2 }, "digest": "0".repeat(64) }),
    )
    .unwrap_err();
    assert_eq!(status_code(&err), Some(409));
}

#[test]
fn dry_run_patch_does_not_write() {
    let vmid = 102;
    let result = call(
        &guests::API_METHOD_PATCH_GUEST,
        json!({ "vmid": vmid, "patch": { "a": 1 }, "dry_run": true }),
    )
    .expect("dry run");
    assert_eq!(result["data"]["a"], 1);

    let err = call(&guests::API_METHOD_GET_GUEST, json!({ "vmid": vmid })).unwrap_err();
    assert_eq!(status_code(&err), Some(404), "dry_run must not have created the document");
}

#[test]
fn raw_put_digest_mismatch_and_dry_run() {
    let vmid = 103;
    call(
        &guests::API_METHOD_PUT_GUEST_RAW,
        json!({ "vmid": vmid, "content": "a: 1\n" }),
    )
    .expect("initial raw put");

    let err = call(
        &guests::API_METHOD_PUT_GUEST_RAW,
        json!({ "vmid": vmid, "content": "a: 2\n", "digest": "0".repeat(64) }),
    )
    .unwrap_err();
    assert_eq!(status_code(&err), Some(409));

    let got = call(&guests::API_METHOD_GET_GUEST, json!({ "vmid": vmid })).unwrap();
    let digest = got["digest"].as_str().unwrap().to_string();

    let dry = call(
        &guests::API_METHOD_PUT_GUEST_RAW,
        json!({ "vmid": vmid, "content": "a: 2\n", "digest": digest, "dry_run": true }),
    )
    .expect("dry run raw put");
    assert_eq!(dry["data"]["a"], 2);
    assert_eq!(dry["touched"][0]["op"], "set");

    let unchanged = call(&guests::API_METHOD_GET_GUEST, json!({ "vmid": vmid })).unwrap();
    assert_eq!(unchanged["data"]["a"], 1, "dry_run must not have written");
}

#[test]
fn convert_switches_format() {
    let vmid = 104;
    call(
        &guests::API_METHOD_PATCH_GUEST,
        json!({ "vmid": vmid, "patch": { "a": 1 } }),
    )
    .unwrap();
    let converted = call(
        &guests::API_METHOD_CONVERT_GUEST,
        json!({ "vmid": vmid, "format": "toml" }),
    )
    .expect("convert");
    assert_eq!(converted["format"], "toml");
    let got = call(&guests::API_METHOD_GET_GUEST, json!({ "vmid": vmid })).unwrap();
    assert_eq!(got["format"], "toml");
    assert_eq!(got["data"]["a"], 1);
}

#[test]
fn list_with_has_filter() {
    let a = 110;
    let b = 111;
    call(
        &guests::API_METHOD_PATCH_GUEST,
        json!({ "vmid": a, "patch": { "traefik": { "spec": {} } } }),
    )
    .unwrap();
    call(
        &guests::API_METHOD_PATCH_GUEST,
        json!({ "vmid": b, "patch": { "other": { "x": 1 } } }),
    )
    .unwrap();

    let all = call(&guests::API_METHOD_LIST_GUESTS, json!({})).unwrap();
    let vmids: Vec<u64> = all.as_array().unwrap().iter().map(|g| g["vmid"].as_u64().unwrap()).collect();
    assert!(vmids.contains(&a));
    assert!(vmids.contains(&b));

    let filtered = call(&guests::API_METHOD_LIST_GUESTS, json!({ "has": "traefik" })).unwrap();
    let filtered_vmids: Vec<u64> = filtered
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["vmid"].as_u64().unwrap())
        .collect();
    assert!(filtered_vmids.contains(&a));
    assert!(!filtered_vmids.contains(&b));
}

#[test]
fn delete_removes_document() {
    let vmid = 120;
    call(
        &guests::API_METHOD_PATCH_GUEST,
        json!({ "vmid": vmid, "patch": { "a": 1 } }),
    )
    .unwrap();
    call(&guests::API_METHOD_DELETE_GUEST, json!({ "vmid": vmid })).expect("delete");
    let err = call(&guests::API_METHOD_GET_GUEST, json!({ "vmid": vmid })).unwrap_err();
    assert_eq!(status_code(&err), Some(404));
}

#[test]
fn snapshot_rollback_and_delete_cycle() {
    let vmid = 130;
    call(
        &guests::API_METHOD_PATCH_GUEST,
        json!({ "vmid": vmid, "patch": { "a": 1 } }),
    )
    .unwrap();
    call(
        &guests::API_METHOD_SNAPSHOT_GUEST,
        json!({ "vmid": vmid, "name": "before" }),
    )
    .expect("snapshot");

    let names = call(&guests::API_METHOD_LIST_SNAPSHOTS, json!({ "vmid": vmid })).unwrap();
    assert_eq!(names, json!(["before"]));

    call(
        &guests::API_METHOD_PATCH_GUEST,
        json!({ "vmid": vmid, "patch": { "a": 2 } }),
    )
    .unwrap();

    call(
        &guests::API_METHOD_ROLLBACK_GUEST,
        json!({ "vmid": vmid, "name": "before" }),
    )
    .expect("rollback");
    let got = call(&guests::API_METHOD_GET_GUEST, json!({ "vmid": vmid })).unwrap();
    assert_eq!(got["data"]["a"], 1);

    call(
        &guests::API_METHOD_DELETE_SNAPSHOT,
        json!({ "vmid": vmid, "name": "before" }),
    )
    .expect("delete snapshot");
    let names = call(&guests::API_METHOD_LIST_SNAPSHOTS, json!({ "vmid": vmid })).unwrap();
    assert_eq!(names, json!([]));
}

#[test]
fn clone_guest_copies_document() {
    let vmid = 140;
    let newid = 141;
    call(
        &guests::API_METHOD_PATCH_GUEST,
        json!({ "vmid": vmid, "patch": { "a": 1 } }),
    )
    .unwrap();
    let cloned = call(
        &guests::API_METHOD_CLONE_GUEST,
        json!({ "vmid": vmid, "newid": newid }),
    )
    .expect("clone");
    assert_eq!(cloned["data"]["a"], 1);
    let got = call(&guests::API_METHOD_GET_GUEST, json!({ "vmid": newid })).unwrap();
    assert_eq!(got["data"]["a"], 1);
}

#[test]
fn datacenter_document_and_registry() {
    call(
        &datacenter::API_METHOD_PATCH_DATACENTER,
        json!({
            "patch": {
                "operators": {
                    "traefik": {
                        "claims": [{ "prefix": "traefik", "scope": "rw" }],
                        "schemas": { "traefik": { "type": "object" } },
                        "description": "Traefik router provider",
                    }
                }
            }
        }),
    )
    .expect("patch datacenter");

    let doc = call(&datacenter::API_METHOD_GET_DATACENTER, json!({})).expect("get datacenter");
    assert_eq!(doc["id"], "datacenter");
    assert_eq!(
        doc["data"]["operators"]["traefik"]["claims"][0]["prefix"],
        "traefik"
    );

    let reg = call(&registry::API_METHOD_REGISTRY, json!({})).expect("registry");
    let entries = reg.as_array().unwrap();
    let traefik = entries.iter().find(|e| e["name"] == "traefik").expect("traefik entry");
    assert_eq!(traefik["claims"][0]["prefix"], "traefik");
    assert_eq!(traefik["schemas"]["traefik"]["type"], "object");

    // A guest that actually uses the "traefik" namespace should see its schema.
    let vmid = 150;
    call(
        &guests::API_METHOD_PATCH_GUEST,
        json!({ "vmid": vmid, "patch": { "traefik": { "spec": { "host": "x" } } } }),
    )
    .unwrap();
    let schemas = call(&registry::API_METHOD_SCHEMAS_FOR_GUEST, json!({ "vmid": vmid })).expect("schemas");
    assert_eq!(schemas["traefik"]["type"], "object");
}

#[test]
fn vmid_out_of_range_is_rejected_by_schema() {
    // vmid=1 is below IntegerSchema's declared minimum(100): schema verification must reject
    // it before the handler (which never even sees it) runs.
    let result = call(&guests::API_METHOD_GET_GUEST, json!({ "vmid": 1 }));
    assert!(result.is_err(), "vmid below the schema minimum must be rejected");
}
