//! `pve-meta-core`: the pure-Rust, platform-independent document model,
//! formats, patch engine and file store behind `pve-meta`.
//!
//! Its only consumer is `crates/pve-meta-perl` (`PVE::RS::Meta`), which
//! exposes the guest lifecycle hooks and the `api_*` functions backing
//! `perl/PVE/API2/Ext/Meta.pm` (`docs/DESIGN.md` §3). There is no daemon and
//! no CLI.
//!
//! No networking, no async, no PVE-specific crates. Builds and passes tests
//! on macOS and Linux.
//!
//! # Module map
//!
//! - [`api`] — the request-shaped API layer (view reads/writes, write
//!   authorization, `touched` reporting) that `PVE::RS::Meta`'s `api_*`
//!   functions export to `perl/PVE/API2/Ext/Meta.pm` (`docs/DESIGN.md` §3).
//! - [`model`] — the [`model::Value`] alias (an order-preserving
//!   `serde_json::Value`), document [`model::lint`] rules, comment-key
//!   handling (`foo__`), and path lookup.
//! - [`path`] — [`path::Path`], dotted/slash addressing into a document.
//! - [`patch`] — merge-patch semantics (RFC 7386) with explicit delete:
//!   [`patch::apply_patch`], [`patch::diff`], [`patch::lint_patch`].
//! - [`crate::format`] — [`format::Format`] (YAML on disk, JSON as a wire format
//!   only) and canonical [`format::parse`]/[`format::dump`].
//! - [`digest`] — [`digest::digest`], the SHA-256 content digest used
//!   throughout the store.
//! - [`store`] — [`store::MetaStore`], the atomic on-disk file store: guest
//!   and datacenter documents, snapshot copies, and content-hashed version
//!   polling.
//! - [`view`] — [`view::extract`]/[`view::replace`]/[`view::merge`]/
//!   [`view::remove`]/[`view::filter`], the prefix-addressed "view" read/write
//!   operations (`docs/DESIGN.md` §1), plus [`view::render`]/[`view::parse`]/
//!   [`view::parse_patch`] for a view's wire text.
//! - [`scopes`] — [`scopes::Grants`] (a principal's effective access) and
//!   [`scopes::parse_scopes`]/[`scopes::scopes_for`] (the datacenter
//!   document's `scopes` map, validated strictly on write and read
//!   leniently — a read never fails), implementing `docs/DESIGN.md` §2 and
//!   the `scopes` rules of §9 (admin-only writes, authid keys, non-empty
//!   prefixes, opaque leaf).
//! - [`error`] — the single [`error::Error`] type (and [`error::Result`]
//!   alias) returned throughout this crate.
//!
//! # Example
//!
//! ```
//! use pve_meta_core::format::{self, Format};
//! use pve_meta_core::model;
//!
//! let doc = format::parse(Format::Yaml, "name: web01\ntags: [prod, web]\n").unwrap();
//! assert_eq!(model::top_level_keys(&doc), vec!["name".to_string(), "tags".to_string()]);
//! ```

pub mod api;
pub mod digest;
pub mod error;
pub mod format;
pub mod model;
pub mod patch;
pub mod path;
pub mod scopes;
pub mod store;
pub mod view;

pub use error::{Error, Result};
pub use model::Value;
