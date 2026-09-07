//! `pve-meta-core`: the pure-Rust, platform-independent document model,
//! formats, patch engine and file store shared by the `pve-meta` daemon, CLI
//! and Perl bindings.
//!
//! No networking, no async, no PVE-specific crates. Builds and passes tests
//! on macOS and Linux.
//!
//! # Module map
//!
//! - [`model`] — the [`model::Value`] alias (an order-preserving
//!   `serde_json::Value`), document [`model::lint`] rules, comment-key
//!   handling (`foo__`), and path lookup.
//! - [`path`] — [`path::Path`], dotted/slash addressing into a document.
//! - [`patch`] — merge-patch semantics (RFC 7386) with explicit delete:
//!   [`patch::apply_patch`], [`patch::diff`], [`patch::make_patch`].
//! - [`format`] — [`format::Format`] (YAML/TOML/JSON) and canonical
//!   [`format::parse`]/[`format::dump`].
//! - [`edit`] — [`edit::apply_patch_text`], applying a patch to a document's
//!   *text*; format-preserving for TOML.
//! - [`digest`] — [`digest::digest`], the SHA-256 content digest used
//!   throughout the store.
//! - [`store`] — [`store::MetaStore`], the atomic on-disk file store: guest
//!   and datacenter documents, snapshots, and cheap version polling.
//! - [`vmlist`] — parsing pmxcfs's own `/etc/pve/.vmlist`.
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
//! assert_eq!(model::namespaces(&doc), vec!["name".to_string(), "tags".to_string()]);
//! ```

pub mod digest;
pub mod edit;
pub mod error;
pub mod format;
pub mod model;
pub mod patch;
pub mod path;
pub mod store;
pub mod vmlist;

pub use error::{Error, Result};
pub use model::Value;
