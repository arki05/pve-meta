//! `pve-meta-core`: the pure-Rust, platform-independent document model,
//! formats, patch engine, registry and file store behind `pve-meta`.
//!
//! Two consumers, neither of which reimplements anything here:
//! `crates/pve-meta-perl` (`PVE::RS::Meta`), which exposes the snapshot
//! hooks, the GC and the `api_*` functions backing
//! `perl/PVE/API2/Ext/Meta.pm` (`docs/DESIGN.md` §5); and
//! `crates/pve-meta-wasm`, the browser build the editor asks for the codec,
//! the path rules, [`scopes::Effective`], [`shape::Shape`] and
//! [`edit::EditSet`]. There is no daemon and no CLI.
//!
//! No networking, no async, no PVE-specific crates. Builds and passes tests
//! on macOS and Linux, and builds for `wasm32-unknown-unknown` (the
//! filesystem-facing modules compile there and are simply never called).
//!
//! # Module map
//!
//! - [`api`] — the request-shaped API layer (view reads/writes, write
//!   authorization, the lint, `touched` reporting, the GC) that
//!   `PVE::RS::Meta`'s `api_*` functions export to
//!   `perl/PVE/API2/Ext/Meta.pm` (`docs/DESIGN.md` §5).
//! - [`shape`] — [`shape::Shape`], the prefixes that reach one document,
//!   most-specific first: what governs a path, what its schema says
//!   (`docs/DESIGN.md` §3.1). Schemas shadow.
//! - [`edit`] — [`edit::EditSet`], the editor's staged edits: apply them to
//!   the stored document, recover them from an edited one, and the narrowest
//!   view one Apply writes (`docs/DESIGN.md` §8).
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
//!   documents, snapshot copies, and content-hashed version polling.
//! - [`view`] — [`view::extract`]/[`view::replace`]/[`view::merge`]/
//!   [`view::remove`]/[`view::filter`], the prefix-addressed "view" read/write
//!   operations (`docs/DESIGN.md` §2), plus [`view::render`]/[`view::parse`]/
//!   [`view::parse_patch`] for a view's wire text.
//! - [`metaschema`] — the two registry file formats, written as schemas in the
//!   same dialect a prefix uses, so the editor can show a prefix or grant
//!   file as a typed tree (`docs/DESIGN.md` §3.6).
//! - [`registry`] — the two drop-directories: **prefixes**
//!   (`/usr/share/pve-meta/prefixes`, `/etc/pve/meta.d/prefixes`), which
//!   say what a prefix is and carry its schema, and **grants**
//!   (`/etc/pve/meta.d/permissions`, cluster-only), which say who may touch one
//!   (`docs/DESIGN.md` §3). They nest by opposite rules: prefixes shadow
//!   most-specific-first ([`shape::Shape::governing`]), grants accumulate by
//!   containment ([`registry::scopes_for`]).
//! - [`scopes`] — [`scopes::Effective`], a principal's effective access to one
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

/// Warns about something the caller cannot see: a file being skipped, a
/// document that would not parse. Tagged `pve-meta:` so the line is greppable
/// wherever it lands.
///
/// **Where a warning has to go, and why it is two places.** Nothing anywhere
/// initialises a `tracing` subscriber, so every `tracing::warn!` this crate
/// used to emit was discarded outright, and `docs/DESIGN.md` §3.3's "a
/// malformed file is skipped with a warning" was not true. stderr looked like
/// the obvious replacement and is not: `PVE::Daemon` opens STDOUT to
/// `/dev/null` and dups STDERR onto it before a worker ever runs
/// (`/usr/share/perl5/PVE/Daemon.pm`), so a `.so` inside pvedaemon or pveproxy
/// writes its warnings into the void just as thoroughly. Verified on the lab:
/// a malformed prefix file produced nothing in the journal at all.
///
/// So both, because the two sinks are each right in a different caller and
/// neither is right in both:
///
/// * **syslog** is the daemons' only route out, and it is the one PVE's own
///   Perl uses for exactly this kind of line.
/// * **stderr** is what the `pve-meta` CLI and the test binaries show, where
///   syslog would be an odd place to look for the answer to a command you just
///   typed.
///
/// Deliberately no `openlog`: the ident is process-global, and setting it
/// from inside a library would relabel the host process's own log lines with
/// ours. The line therefore inherits whatever ident that process last set --
/// on PVE that turns out to be `IPCC.xs`, not `pveproxy`, which is exactly why
/// the message carries its own tag. Grep the journal for `pve-meta:`, not for
/// a unit or an ident.
///
/// This is a report, not a channel. A caller that must *act* on the failure
/// needs it in a return value, not in a log line -- see `docs/DESIGN.md` §3.3
/// on surfacing an unreadable registry file in the UI.
pub fn warn(msg: &str) {
    eprintln!("pve-meta: {msg}");
    syslog_line(libc_priority::WARNING, msg);
}

/// Records a write: who changed which document, where, and how much. One
/// line per `PUT`/`DELETE` that actually changed a file, at `info`, tagged
/// `pve-meta audit:` so `journalctl | grep 'pve-meta audit'` is the history
/// of the store. PVE's task log never sees these writes -- they are plain
/// API calls, not tasks -- and nothing else recorded who did what.
///
/// syslog only, deliberately: the daemons have no other route out, and the
/// CLI prints its own result, so a second line on its stderr would be noise.
pub fn audit(msg: &str) {
    syslog_line(libc_priority::INFO, msg);
}

/// The two syslog priorities this crate uses, named so the calls above read.
/// Values are libc's on unix and unused elsewhere.
mod libc_priority {
    #[cfg(unix)]
    pub const WARNING: i32 = libc::LOG_WARNING;
    #[cfg(unix)]
    pub const INFO: i32 = libc::LOG_INFO;
    #[cfg(not(unix))]
    pub const WARNING: i32 = 4;
    #[cfg(not(unix))]
    pub const INFO: i32 = 6;
}

#[cfg(unix)]
fn syslog_line(priority: i32, msg: &str) {
    // An interior NUL would truncate the line at the C boundary; a warning is
    // often *about* a hostile or corrupt file name, so it is not a case that
    // can be assumed away.
    let tag = if priority == libc_priority::INFO { "pve-meta audit" } else { "pve-meta" };
    let line: String = format!("{tag}: {msg}")
        .chars()
        .map(|c| if c == '\0' { ' ' } else { c })
        .collect();
    let Ok(c) = std::ffi::CString::new(line) else {
        return;
    };
    // `"%s"`, never the message as the format string: a file name containing a
    // `%` would otherwise be read as a conversion and print whatever happened
    // to be next on the stack.
    //
    // SAFETY: `syslog` is async-signal-safe and thread-safe, both pointers are
    // valid NUL-terminated C strings that outlive the call, and the variadic
    // argument matches the `%s` in the format.
    unsafe {
        libc::syslog(priority | libc::LOG_DAEMON, c"%s".as_ptr(), c.as_ptr());
    }
}

#[cfg(not(unix))]
fn syslog_line(_priority: i32, _msg: &str) {}

/// Warns through [`warn`], formatting like `println!`.
#[macro_export]
macro_rules! warn_line {
    ($($arg:tt)*) => {
        $crate::warn(&format!($($arg)*))
    };
}

pub mod api;
pub mod digest;
pub mod edit;
pub mod error;
pub mod format;
pub mod metaschema;
pub mod model;
pub mod patch;
pub mod path;
pub mod registry;
pub mod scopes;
pub mod shape;
pub mod store;
pub mod view;

pub use error::{Error, Result};
pub use model::Value;
