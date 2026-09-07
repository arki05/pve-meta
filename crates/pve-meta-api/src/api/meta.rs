//! `/meta/version`, `/meta/health`, `/meta/inventory`.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Error;
use serde_json::Value;

use proxmox_router::{Permission, RpcEnvironment};
use proxmox_schema::api;

use pve_meta_core::vmlist::GuestKind;

use crate::error::to_http;
use crate::store::store;

/// Env var overriding the pmxcfs mount root used to read guest configs for `/meta/inventory`
/// (default `/etc/pve`). Not part of `docs/API.md`; exists purely so tests/dev don't need a
/// real pmxcfs mount.
pub const PVE_ROOT_ENV: &str = "PVE_META_PVE_ROOT";

pub fn pve_root() -> PathBuf {
    std::env::var_os(PVE_ROOT_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/etc/pve"))
}

/// Best-effort read of a guest's display name from its config
/// (`nodes/<node>/{qemu-server,lxc}/<vmid>.conf`): `name:` for qemu, `hostname:` for lxc.
pub fn guest_display_name(node: &str, vmid: u32, kind: GuestKind) -> Option<String> {
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

#[api(
    input: {
        properties: {
            wait: {
                type: Integer,
                optional: true,
                minimum: 0,
                maximum: 60,
                description: "Long-poll up to this many seconds for the version to change.",
            },
            since: {
                type: String,
                optional: true,
                description: "Long-poll until the token differs from this one.",
            },
        },
    },
    access: { permission: &Permission::Anybody },
)]
/// The store's current change-version token, optionally long-polled.
pub async fn version(wait: Option<u32>, since: Option<String>) -> Result<Value, Error> {
    let wait = Duration::from_secs(wait.unwrap_or(0) as u64);
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let v = store().version().map_err(to_http)?;
        let changed = since.as_deref().is_none_or(|s| s != v.token);
        if changed || tokio::time::Instant::now() >= deadline {
            let changed_secs = v
                .changed
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            return Ok(serde_json::json!({ "token": v.token, "changed": changed_secs }));
        }
        tokio::time::sleep(Duration::from_millis(500).min(deadline - tokio::time::Instant::now())).await;
    }
}

#[api(access: { permission: &Permission::World })]
/// Health check: no authentication required, so monitoring works without a ticket.
pub fn health() -> Result<Value, Error> {
    let root = store().root();
    let guests = store().list_guests().map_err(to_http)?;
    let bytes: u64 = guests.iter().map(|g| g.size).sum();
    let keyring_keys = crate::auth::global().map(|a| a.key_count()).unwrap_or(0);
    Ok(serde_json::json!({
        "store": {
            "root": root.display().to_string(),
            "files": guests.len(),
            "bytes": bytes,
        },
        "hooks": {},
        "version": env!("CARGO_PKG_VERSION"),
        "auth": { "keyring_keys": keyring_keys },
    }))
}

#[api(access: { permission: &Permission::Anybody })]
/// Guests from `/etc/pve/.vmlist`, enriched with their display name from their guest config.
pub fn inventory(rpcenv: &mut dyn RpcEnvironment) -> Result<Value, Error> {
    let _ = rpcenv;
    let vmlist = pve_meta_core::vmlist::read_vmlist(crate::store::vmlist_path()).map_err(to_http)?;
    let guest_docs: std::collections::HashSet<u32> = store()
        .list_guests()
        .map_err(to_http)?
        .into_iter()
        .map(|g| g.vmid)
        .collect();

    let mut out = Vec::new();
    for (vmid, info) in &vmlist.guests {
        let name = guest_display_name(&info.node, *vmid, info.kind);
        let has_meta = guest_docs.contains(vmid);
        let format = if has_meta {
            store()
                .locate(pve_meta_core::store::DocId::Guest(*vmid))
                .ok()
                .flatten()
                .map(|l| l.format.to_string())
        } else {
            None
        };
        out.push(serde_json::json!({
            "vmid": vmid,
            "node": info.node,
            "type": match info.kind {
                GuestKind::Qemu => "qemu",
                GuestKind::Lxc => "lxc",
            },
            "name": name,
            "has_meta": has_meta,
            "format": format,
        }));
    }
    Ok(Value::Array(out))
}
