//! `pve-meta-publish`: writes views of a container's pve-meta document into
//! that container as files, one way, host to guest (`docs/DESIGN.md`
//! §13).
//!
//! ```yaml
//! publish:
//!   swap:
//!     view: llm.swap              # a view into this guest's own document
//!     path: llm/llama-swap.yaml   # under /etc/pve-meta inside the guest
//!     format: yaml                # yaml | json | raw
//!     mode: "0444"
//!     owner: "0:0"
//!     local_edits: keep           # keep | overwrite
//! ```
//!
//! The store stays passive: nothing here is linked into pveproxy or
//! pvedaemon. This crate reads documents with [`pve_meta_core`] directly, as
//! root on the node, and does everything inside a guest through `pct exec`.
//!
//! # Module map
//!
//! - [`entry`] — the `publish` key: entries, their validation (paths, modes,
//!   owners) and the rendered content of each; reading it from the store.
//! - [`manifest`] — `/etc/pve-meta/.published`, what was written, so only
//!   that is ever replaced or removed.
//! - [`plan`] — the decision table: desired entries, the manifest and the
//!   files as they are in the guest, in; the file operations and the next
//!   manifest, out.
//! - [`guest`] — [`guest::Guest`], the file operations inside a guest the
//!   plan needs, and [`guest::sync`], which runs one reconcile through them.
//! - [`pct`] — [`guest::Guest`] over `pct exec`.
//! - [`node`] — the vmlist, the node name, and which containers run.
//! - [`daemon`] — the loop: document changes, container starts, drift.
//! - [`lock`] — one lock per guest, so the daemon and a hand-run `sync` take
//!   turns.

pub mod daemon;
pub mod entry;
pub mod guest;
pub mod lock;
pub mod manifest;
pub mod node;
pub mod pct;
pub mod plan;

/// The top-level key of a document this crate reads.
pub const PREFIX: &str = "publish";

/// The directory inside a guest relative paths resolve under; it also holds
/// the manifest.
pub const GUEST_ROOT: &str = "/etc/pve-meta";

/// The largest content one entry may publish, in bytes: the store's own write
/// cap ([`pve_meta_core::store::MAX_BYTES`]), which is the source of truth.
///
/// A view is a part of a document, so a document written through the API can
/// never carry more than this. Two things still reach the check: a document
/// written out of band, which the store reads up to
/// [`pve_meta_core::store::MAX_READ_BYTES`], and `json`, whose indentation and
/// quoting render a view larger than the text it was stored as.
pub const MAX_CONTENT_BYTES: usize = pve_meta_core::store::MAX_BYTES as usize;

#[cfg(test)]
mod tests {
    /// The packaged prefix is one the registry loads, with the selector and
    /// enforcement the design says.
    #[test]
    fn packaged_prefix_loads() {
        use pve_meta_core::registry::{self, Selector};
        let def = registry::parse_prefix(crate::PREFIX, include_str!("../prefixes/publish.yaml"))
            .expect("prefixes/publish.yaml loads");
        assert_eq!(def.selector, Selector::All);
        assert!(def.enforce);
        assert!(def.schema.is_some());
    }
}
