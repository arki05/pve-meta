//! The manifest: `/etc/pve-meta/.guest-files` inside the guest, one record per
//! file written.
//!
//! ```json
//! {
//!   "version": 1,
//!   "files": [
//!     { "entry": "swap", "path": "/etc/pve-meta/llm/swap.yaml", "sha256": "…", "source": "user/swap" }
//!   ]
//! }
//! ```
//!
//! `source` is who asked for the file: `user/<entry>` for document entries,
//! `managed/<operator>/<name>` for operator-handed ones. A file the manifest
//! attributes to another source is refused to the claimant, by name, so two
//! writers sharing one guest refuse each other instead of overwriting. Files
//! recorded before sources existed read back as `user/<entry>`.
//!
//! It is how a file the daemon wrote is told apart from a file someone else
//! wrote or edited: a file is replaced or removed only while its content
//! still hashes to its record. It lives in the guest, beside what it
//! describes, and travels with the container through a backup, a restore and
//! a migration.
//!
//! Root `0600`: nothing in the guest needs to read it.
//!
//! A manifest that is damaged, truncated or edited by hand, with any record
//! whose path does not validate, is not trusted at all: nothing in it is
//! deleted, and files exactly as wanted are adopted again.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::entry::GuestPath;
use crate::GUEST_ROOT;

/// The one format version this crate reads and writes.
pub const VERSION: u32 = 1;

/// The mode of the manifest file itself.
pub const MODE: u32 = 0o600;

/// The manifest's name under [`GUEST_ROOT`]. No entry path may be it or run
/// through it.
pub const NAME: &str = ".guest-files";

/// The largest manifest read back from a guest, in bytes.
pub const MAX_BYTES: usize = 1024 * 1024;

/// The absolute path of the manifest inside the guest.
pub fn path() -> String {
    format!("{GUEST_ROOT}/{NAME}")
}

/// The directories the manifest lives in, parent first. They have to be safe
/// to write through for anything to be written.
pub fn directories() -> Vec<String> {
    vec!["/etc".to_string(), GUEST_ROOT.to_string()]
}

/// One file written: by the daemon for a document entry, or by an operator
/// through [`crate::entry::Desired::direct`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub entry: String,
    pub path: GuestPath,
    /// Of the content as written.
    pub sha256: String,
    /// `user/<entry>` or `managed/<operator>/<name>`.
    pub source: String,
}

/// Every record, by path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Manifest {
    pub records: BTreeMap<GuestPath, Record>,
}

#[derive(Serialize, Deserialize)]
struct RawManifest {
    version: u32,
    files: Vec<RawRecord>,
}

#[derive(Serialize, Deserialize)]
struct RawRecord {
    entry: String,
    path: String,
    sha256: String,
    #[serde(default)]
    source: String,
}

impl Manifest {
    /// Reads a manifest. A file that is not one, or that has a record whose
    /// path does not validate, is an error: nothing in it is trusted. A
    /// guest-written path in the error is `{:?}`-formatted, so it cannot
    /// break the error onto more than one line.
    pub fn parse(bytes: &[u8]) -> Result<Manifest, String> {
        let raw: RawManifest =
            serde_json::from_slice(bytes).map_err(|e| format!("not a manifest: {e}"))?;
        if raw.version != VERSION {
            return Err(format!("manifest version {} is not {VERSION}", raw.version));
        }
        let mut out = Manifest::default();
        for mut r in raw.files {
            // Records written before sources existed carry no source: they
            // came from document entries, so they read back as their own.
            if r.source.is_empty() {
                r.source = crate::entry::user_source(&r.entry);
            }
            let path = GuestPath::absolute(&r.path)
                .map_err(|why| format!("record {:?}: {why}", r.path))?;
            let record =
                Record { entry: r.entry, path: path.clone(), sha256: r.sha256, source: r.source };
            if out.records.insert(path.clone(), record).is_some() {
                return Err(format!("'{path}' is recorded twice"));
            }
        }
        Ok(out)
    }

    /// The manifest's text: pretty JSON, sorted by path, one trailing newline.
    pub fn render(&self) -> Vec<u8> {
        let raw = RawManifest {
            version: VERSION,
            files: self
                .records
                .values()
                .map(|r| RawRecord {
                    entry: r.entry.clone(),
                    path: r.path.to_string(),
                    sha256: r.sha256.clone(),
                    source: r.source.clone(),
                })
                .collect(),
        };
        let mut text = serde_json::to_string_pretty(&raw).expect("a manifest serialises");
        text.push('\n');
        text.into_bytes()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let mut m = Manifest::default();
        for (path, entry, source) in [
            ("/etc/traefik/dynamic.yaml", "traefik", "managed/traefik/dyn"),
            ("llm/swap.yaml", "swap", "user/swap"),
        ] {
            let path = GuestPath::parse(path).unwrap();
            m.records.insert(
                path.clone(),
                Record { entry: entry.into(), path, sha256: "ab".repeat(32), source: source.into() },
            );
        }
        let text = m.render();
        assert_eq!(Manifest::parse(&text).unwrap(), m);
        let s = String::from_utf8(text).unwrap();
        assert!(s.ends_with("}\n") && s.contains("\"files\": ["));
        assert!(s.find("/etc/pve-meta/llm").unwrap() < s.find("/etc/traefik").unwrap());
        assert!(Manifest::parse(&Manifest::default().render()).unwrap().is_empty());
    }

    #[test]
    fn records_without_a_source_read_back_as_their_own_entry() {
        let sha = "ab".repeat(32);
        let text = format!(r#"{{"version": 1, "files": [{{"entry": "e", "path": "/etc/e", "sha256": "{sha}"}}]}}"#);
        let m = Manifest::parse(text.as_bytes()).unwrap();
        assert_eq!(m.records[&GuestPath::parse("/etc/e").unwrap()].source, "user/e");
    }

    #[test]
    fn anything_invalid_distrusts_the_whole_file() {
        let sha = "ab".repeat(32);
        let esc = "\\u001b";
        let good = format!(r#"{{"entry": "e", "path": "/etc/e", "sha256": "{sha}"}}"#);
        let bad = [
            "".to_string(),
            r#"{"version": 2, "files": []}"#.to_string(),
            r#"{"version": 1, "entries": []}"#.to_string(),
            format!(r#"{{"entry": "a", "path": "/etc/pve-meta/../shadow", "sha256": "{sha}"}}"#),
            format!(r#"{{"entry": "b", "path": "relative", "sha256": "{sha}"}}"#),
            format!(r#"{{"entry": "g", "path": "/etc/g{esc}", "sha256": "{sha}"}}"#),
            good.clone(),
        ];
        for (i, b) in bad.iter().enumerate() {
            let text = if b.starts_with("{\"entry\"") {
                let second = if i == bad.len() - 1 { good.clone() } else { b.clone() };
                format!(r#"{{"version": 1, "files": [{good}, {second}]}}"#)
            } else {
                b.clone()
            };
            let err = Manifest::parse(text.as_bytes()).unwrap_err();
            assert!(!err.chars().any(char::is_control), "{err:?}");
        }
        let one = format!(r#"{{"version": 1, "files": [{good}]}}"#);
        assert_eq!(Manifest::parse(one.as_bytes()).unwrap().records.len(), 1);
    }
}
