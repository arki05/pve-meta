//! Process-global access to the [`pve_meta_core::store::MetaStore`] and vmlist path.
//!
//! Both `pve-metad` and `pve-meta` (the CLI) initialize this once at startup and then let the
//! `#[api]` handlers reach it via [`store()`]; tests initialize it against a tempdir.

use std::path::PathBuf;
use std::sync::OnceLock;

use pve_meta_core::store::MetaStore;

static STORE: OnceLock<MetaStore> = OnceLock::new();
static VMLIST_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Env var overriding the store root (default `/etc/pve/meta`).
pub const ROOT_ENV: &str = "PVE_META_ROOT";
/// Env var overriding the vmlist path (default `/etc/pve/.vmlist`).
pub const VMLIST_ENV: &str = "PVE_META_VMLIST";

fn default_root() -> PathBuf {
    std::env::var_os(ROOT_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/etc/pve/meta"))
}

fn default_vmlist() -> PathBuf {
    std::env::var_os(VMLIST_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/etc/pve/.vmlist"))
}

/// Initializes the global store from `PVE_META_ROOT`/`PVE_META_VMLIST` (or their defaults).
/// Safe to call more than once (e.g. from multiple tests in the same process); later calls are
/// no-ops if a store is already set.
pub fn init_default() {
    init(default_root(), default_vmlist());
}

/// Initializes the global store explicitly (used by tests to point at a tempdir).
pub fn init(root: impl Into<PathBuf>, vmlist: impl Into<PathBuf>) {
    let _ = STORE.set(MetaStore::new(root.into()));
    let _ = VMLIST_PATH.set(vmlist.into());
}

/// The process-global store. Panics if [`init`]/[`init_default`] was never called — every
/// binary entry point (daemon `main`, CLI `main`, and test setup) must call one of them first.
pub fn store() -> &'static MetaStore {
    STORE.get().expect("pve_meta_api::store::init[_default] was never called")
}

/// The configured vmlist path.
pub fn vmlist_path() -> &'static std::path::Path {
    VMLIST_PATH
        .get()
        .expect("pve_meta_api::store::init[_default] was never called")
}
