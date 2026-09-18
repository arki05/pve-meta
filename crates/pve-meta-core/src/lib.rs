//! `pve-meta-core`: the platform-independent document model, store and API
//! layer behind `pve-meta`, consumed by `pve-meta-perl` and `pve-meta-wasm`.
//! No networking, no async, no PVE-specific crates.

// Doc links here reach private items, for navigation under `cargo doc --document-private-items`.
#![allow(rustdoc::private_intra_doc_links)]

/// Warns about something the caller can't see, to both stderr and syslog under the `pve-meta:` tag.
pub fn warn(msg: &str) {
    eprintln!("pve-meta: {msg}");
    syslog_line(libc_priority::WARNING, msg);
}

/// Records a write to syslog only, tagged `pve-meta audit:` (`docs/DESIGN.md` §5).
pub fn audit(msg: &str) {
    syslog_line(libc_priority::INFO, msg);
}

/// The two syslog priorities this crate uses, named so the calls above read.
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
    // often about a corrupt or unusual file name.
    let tag = if priority == libc_priority::INFO { "pve-meta audit" } else { "pve-meta" };
    let line: String = format!("{tag}: {msg}")
        .chars()
        .map(|c| if c == '\0' { ' ' } else { c })
        .collect();
    let Ok(c) = std::ffi::CString::new(line) else {
        return;
    };
    // `"%s"`, never `msg` itself as the format string: a `%` in a file name
    // would otherwise be read as a conversion.
    //
    // SAFETY: both pointers are valid, NUL-terminated, and outlive the call.
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

// Each module documents itself with its own `//!` header.
pub mod api;
pub mod backup;
pub mod digest;
pub mod error;
pub mod format;
pub mod metaschema;
pub mod model;
pub mod patch;
pub mod path;
pub mod registry;
pub mod shape;
pub mod store;
pub mod view;

pub use error::{Error, Result};
pub use model::Value;
