//! `pve-meta-core`: the pure-Rust, platform-independent document model,
//! formats, patch engine, registry and file store behind `pve-meta`.
//!
//! Its only consumer is `crates/pve-meta-perl` (`PVE::RS::Meta`), which
//! exposes the snapshot hooks, the GC and the `api_*` functions backing
//! `perl/PVE/API2/Ext/Meta.pm` (`docs/DESIGN.md` §5). There is no daemon and
//! no CLI.
//!
//! No networking, no async, no PVE-specific crates. Builds and passes tests
//! on macOS and Linux.
//!
//! # Module map
//!
//! - [`api`] — the request-shaped API layer (view reads/writes, write
//!   authorization, the lint, `touched` reporting, the GC) that
//!   `PVE::RS::Meta`'s `api_*` functions export to
//!   `perl/PVE/API2/Ext/Meta.pm` (`docs/DESIGN.md` §5).
//! - [`model`] — the [`model::Value`] alias (an order-preserving
//!   `serde_json::Value`), the one document [`model::lint`], comment keys
//!   (`foo__`), and path lookup.
//! - [`path`] — [`path::Path`], dotted/slash addressing into a document.
//! - [`patch`] — merge-patch semantics (RFC 7386) with explicit delete:
//!   [`patch::apply_patch`], [`patch::diff`].
//! - [`crate::format`] — [`format::Format`] (YAML on disk, JSON as a wire
//!   format only) and canonical [`format::parse`]/[`format::dump`].
//! - [`digest`] — [`digest::digest`], the SHA-256 content digest used
//!   throughout the store.
//! - [`store`] — [`store::MetaStore`], the atomic on-disk file store: guest
//!   and datacenter documents, snapshot copies, and content-hashed version
//!   polling.
//! - [`view`] — [`view::extract`]/[`view::replace`]/[`view::merge`]/
//!   [`view::remove`]/[`view::filter`], the prefix-addressed "view" read/write
//!   operations (`docs/DESIGN.md` §2), plus [`view::render`]/[`view::parse`]/
//!   [`view::parse_patch`] for a view's wire text.
//! - [`registry`] — the two drop-directories: **namespaces**
//!   (`/usr/share/pve-meta/namespaces`, `/etc/pve/meta.d/namespaces`), which
//!   say what a prefix is and carry its schema, and **grants**
//!   (`/etc/pve/meta.d/grants`, cluster-only), which say who may touch one
//!   (`docs/DESIGN.md` §3). They nest by opposite rules: namespaces shadow
//!   most-specific-first ([`registry::governing`]), grants accumulate by
//!   containment ([`registry::scopes_for`]).
//! - [`scopes`] — [`scopes::Grants`], a principal's effective access to one
//!   document: the PVE ACL answers plus the grant scopes whose selector
//!   matches the guest.
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
//! assert!(model::lint(&doc).is_empty());
//! ```

// Doc comments here reference internal helpers by intra-doc link on purpose:
// `covers` (the coverage rule), `identify`, `is_valid_segment` and
// `Stored::unrecoverable` are what the prose is *about*, and a plain code span
// would drop the navigation under `cargo doc --document-private-items` -- the
// only way anyone reads this crate, since its sole consumer is the perlmod
// crate next door. Rustdoc renders such a link as plain text in the public
// docs, so nothing is broken there either; the lint only warns that it did.
// `make doc` still fails on a link that resolves to nothing at all.
#![allow(rustdoc::private_intra_doc_links)]

pub mod api;
pub mod digest;
pub mod error;
pub mod format;
pub mod model;
pub mod patch;
pub mod path;
pub mod registry;
pub mod scopes;
pub mod store;
pub mod view;

pub use error::{Error, Result};
pub use model::Value;
