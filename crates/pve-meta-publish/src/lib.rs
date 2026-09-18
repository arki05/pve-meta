//! `pve-meta-publish`: writes views of a container's pve-meta document into
//! that container as files, one way, host to guest (`docs/PUBLISH.md`).
//! Reads documents with [`pve_meta_core`]; acts inside a guest via `pct exec`.

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
