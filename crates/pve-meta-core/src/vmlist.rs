//! Parsing `/etc/pve/.vmlist`, pmxcfs's own guest index (not part of the
//! `pve-meta` document model).

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::format::Format;

/// The kind of guest, as recorded in `.vmlist`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GuestKind {
    /// A QEMU/KVM virtual machine.
    Qemu,
    /// An LXC container.
    Lxc,
}

/// One guest's entry in `.vmlist`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GuestInfo {
    /// The cluster node currently hosting the guest.
    pub node: String,
    /// The guest kind.
    #[serde(rename = "type")]
    pub kind: GuestKind,
    /// pmxcfs's internal change-version counter for this guest.
    pub version: u64,
}

/// The parsed contents of `/etc/pve/.vmlist`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct VmList {
    /// pmxcfs's internal change-version counter for the whole list.
    pub version: u64,
    /// Guests, keyed by vmid.
    #[serde(rename = "ids")]
    pub guests: BTreeMap<u32, GuestInfo>,
}

/// Parses the JSON contents of `/etc/pve/.vmlist`.
pub fn parse_vmlist(text: &str) -> Result<VmList> {
    serde_json::from_str(text).map_err(|e| Error::Parse {
        format: Format::Json,
        msg: e.to_string(),
    })
}

/// Reads and parses `/etc/pve/.vmlist` (or any file with the same shape) from
/// `path`.
pub fn read_vmlist(path: impl AsRef<std::path::Path>) -> Result<VmList> {
    let text = std::fs::read_to_string(path)?;
    parse_vmlist(&text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const SAMPLE: &str = r#"{
        "version": 4913,
        "ids": {
            "100": { "node": "arkantos", "type": "lxc", "version": 4919 },
            "101": { "node": "arkantos", "type": "qemu", "version": 4001 }
        }
    }"#;

    #[test]
    fn parses_sample() {
        let list = parse_vmlist(SAMPLE).unwrap();
        assert_eq!(list.version, 4913);
        assert_eq!(list.guests.len(), 2);
        assert_eq!(
            list.guests[&100],
            GuestInfo {
                node: "arkantos".to_string(),
                kind: GuestKind::Lxc,
                version: 4919
            }
        );
        assert_eq!(list.guests[&101].kind, GuestKind::Qemu);
        // BTreeMap orders by key regardless of JSON insertion order.
        assert_eq!(list.guests.keys().collect::<Vec<_>>(), vec![&100, &101]);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_vmlist("not json").is_err());
    }

    #[test]
    fn read_vmlist_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".vmlist");
        std::fs::write(&path, SAMPLE).unwrap();
        let list = read_vmlist(&path).unwrap();
        assert_eq!(list.version, 4913);
    }
}
