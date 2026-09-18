//! The node the daemon runs on: its name, the cluster's vmlist, which of its
//! containers run, and whether their configs are locked.

use std::collections::BTreeMap;

use anyhow::{anyhow, Context, Result};
use serde_json::Value;

/// pmxcfs's vmid map: every guest in the cluster, its type and its node.
pub const VMLIST: &str = "/etc/pve/.vmlist";

/// The kind of guest a vmid is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Lxc,
    Qemu,
    Other,
}

/// One guest of the vmlist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Guest {
    pub node: String,
    pub kind: Kind,
}

/// Parses `/etc/pve/.vmlist`.
pub fn parse_vmlist(text: &str) -> Result<BTreeMap<u32, Guest>> {
    let v: Value = serde_json::from_str(text).context("the vmlist is not JSON")?;
    let ids =
        v.get("ids").and_then(Value::as_object).ok_or_else(|| anyhow!("the vmlist has no ids"))?;
    let mut out = BTreeMap::new();
    for (id, entry) in ids {
        let Ok(vmid) = id.parse::<u32>() else { continue };
        let node = entry.get("node").and_then(Value::as_str).unwrap_or_default().to_string();
        let kind = match entry.get("type").and_then(Value::as_str) {
            Some("lxc") => Kind::Lxc,
            Some("qemu") => Kind::Qemu,
            _ => Kind::Other,
        };
        out.insert(vmid, Guest { node, kind });
    }
    Ok(out)
}

/// Reads the vmlist. An error when pmxcfs is not mounted: the store is not
/// there either, and every document would read as absent.
pub fn vmlist() -> Result<BTreeMap<u32, Guest>> {
    let text = std::fs::read_to_string(VMLIST).with_context(|| format!("cannot read {VMLIST}"))?;
    parse_vmlist(&text)
}

/// The local node's name: where `/etc/pve/local` points. Without it pmxcfs
/// is not mounted, and nothing else can be read either.
pub fn nodename() -> Result<String> {
    let target = std::fs::read_link("/etc/pve/local").context("cannot read /etc/pve/local")?;
    let name = target.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    if name.is_empty() {
        return Err(anyhow!("/etc/pve/local does not name a node"));
    }
    Ok(name.to_string())
}

/// The running containers in `/proc/net/unix`'s text, each with the inode of
/// its LXC command socket, `@/var/lib/lxc/<vmid>/command`: the test
/// `PVE::LXC::list_active_containers` makes, without a process per guest. A
/// container that restarted between two reads has a new inode.
pub fn parse_active_containers(text: &str) -> BTreeMap<u32, u64> {
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let [.., inode, path] = fields.as_slice() else { continue };
        let vmid = path
            .strip_prefix("@/var/lib/lxc/")
            .and_then(|p| p.strip_suffix("/command"))
            .and_then(|v| v.parse::<u32>().ok());
        if let (Some(vmid), Ok(inode)) = (vmid, inode.parse::<u64>()) {
            out.entry(vmid).or_insert(inode);
        }
    }
    out
}

/// The containers running on this node, with their command socket's inode.
pub fn active_containers() -> Result<BTreeMap<u32, u64>> {
    let text = std::fs::read_to_string("/proc/net/unix").context("cannot read /proc/net/unix")?;
    Ok(parse_active_containers(&text))
}

/// The `lock:` of a container config (a backup, snapshot, migration or the
/// like is running); only the current section, before the first `[snapshot]`,
/// counts.
pub fn parse_lock(text: &str) -> Option<String> {
    text.lines()
        .take_while(|l| !l.starts_with('['))
        .find_map(|l| l.strip_prefix("lock:").map(|v| v.trim().to_string()))
}

/// The `lock:` of container `vmid` on `node`.
pub fn container_lock(node: &str, vmid: u32) -> Result<Option<String>> {
    let path = format!("/etc/pve/nodes/{node}/lxc/{vmid}.conf");
    let text = std::fs::read_to_string(&path).with_context(|| format!("cannot read {path}"))?;
    Ok(parse_lock(&text))
}

/// Why `vmid` cannot be synced right now: not in `active`, or locked (a
/// backup, snapshot or migration); `None` when it can be.
pub fn stopped_or_locked(
    node: &str,
    vmid: u32,
    active: &BTreeMap<u32, u64>,
) -> Result<Option<String>> {
    if !active.contains_key(&vmid) {
        return Ok(Some("stopped".into()));
    }
    Ok(container_lock(node, vmid)?.map(|l| format!("locked ({l})")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vmlist() {
        let list = parse_vmlist(
            r#"{"version": 42, "ids": {
                "105": {"node": "pve1", "type": "lxc", "version": 3},
                "106": {"node": "pve2", "type": "qemu", "version": 4},
                "x": {"node": "pve1", "type": "lxc"}
            }}"#,
        )
        .unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[&105], Guest { node: "pve1".into(), kind: Kind::Lxc });
        assert_eq!(list[&106].kind, Kind::Qemu);
        assert!(parse_vmlist("{}").is_err());
        assert!(parse_vmlist("").is_err());
    }

    #[test]
    fn active_containers() {
        let text = "Num       RefCount Protocol Flags    Type St Inode Path\n\
                    0000000000000000: 00000002 00000000 00010000 0001 01 31415 @/var/lib/lxc/105/command\n\
                    0000000000000000: 00000002 00000000 00010000 0001 01 31416 @/var/lib/lxc/1050/command\n\
                    0000000000000000: 00000003 00000000 00000000 0001 03 31417 /run/systemd/journal/stdout\n\
                    0000000000000000: 00000002 00000000 00010000 0001 01 31418 @/var/lib/lxc/x/command\n\
                    0000000000000000: 00000002 00000000 00010000 0001 01 31419 @/var/lib/lxc/107/monitor\n\
                    0000000000000000: 00000002 00000000 00010000 0001 01 31420\n";
        let active = parse_active_containers(text);
        assert_eq!(active.into_iter().collect::<Vec<_>>(), [(105, 31415), (1050, 31416)]);
    }

    #[test]
    fn lock() {
        let text = "arch: amd64\nlock: backup\n\n[snap]\nlock: snapshot\n";
        assert_eq!(parse_lock(text).as_deref(), Some("backup"));
        assert_eq!(parse_lock("hostname: x\n\n[snap]\nlock: snapshot\n"), None);
    }
}
