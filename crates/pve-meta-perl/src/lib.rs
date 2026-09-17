//! `pve-meta-rs`: the `PVE::RS::Meta` Perl bindings.
//!
//! Three families of exports, all thin wrappers over [`pve_meta_core`]:
//!
//! * the **lifecycle hooks** (`on_create`/`on_destroy`, and
//!   `on_snapshot`/`on_rollback`/`on_delsnap`), called from the patched
//!   `PVE/AbstractConfig.pm` (package `libpve-guest-common-perl`;
//!   `docs/DESIGN.md` §9), plus `export_for_backup` and `notes_import`, the
//!   two ends of a backup: called from the patched vzdump `assemble` of
//!   `qemu-server` and `pve-container`, and from `write_config` in that same
//!   `AbstractConfig.pm`. Clone is not carried;
//! * `stored_vmids`, the store's own list of guest vmids, which is what lets
//!   `pve-meta ls --orphans` and `pve-meta rm` find and remove a document
//!   whose guest config was deleted out of band (`docs/DESIGN.md` §10); and
//! * the **`api_*` functions** backing `perl/PVE/API2/Ext/Meta.pm`
//!   (`docs/DESIGN.md` §8), implemented in [`pve_meta_core::api`] — this
//!   crate only supplies the store and the registry.
//!
//! **Everything crosses as a native Perl structure** (`docs/DESIGN.md` §8):
//! the caller's ACL hash and tags in, guest rows in, documents and results
//! out. The single exception is the client-supplied `data` parameter, which
//! is a JSON string because that is what the REST parameter is; it is decoded
//! once, in Rust. `perlmod` converts a Perl scalar to a `bool` by truthiness,
//! so the ACL flags are ordinary `1`/`0` (or `undef`) on the Perl side —
//! `test/basic.pl` asserts that.
//!
//! Built as a `cdylib` (`libpve_meta_rs.so`); the `.pm` glue files
//! (`PVE/RS/Meta.pm`, `Proxmox/Lib/PVEMeta.pm`) are generated separately by
//! `genpackage.pl` (see `Makefile`), not by this crate.
//!
//! The store root defaults to `/etc/pve/meta` and can be overridden with the
//! `PVE_META_ROOT` environment variable (used by tests and by
//! `test/basic.pl`); the prefix and permission directories likewise with
//! `PVE_META_PREFIX_DIRS`/`PVE_META_PERMISSION_DIRS`, and the nodes directory
//! holding each node's prefix files with `PVE_META_NODES_DIR`. A store under
//! `/etc/pve` refuses every call while pmxcfs is not mounted (a `503:`), and
//! `PVE_META_CLUSTER_MARKER` names the marker it checks instead
//! (`MetaStore::new`).

use std::path::PathBuf;

use pve_meta_core::store::{MetaStore, RollbackOutcome};

/// Opens a fresh [`MetaStore`] rooted at `$PVE_META_ROOT`, or
/// `/etc/pve/meta` if unset. Cheap: `MetaStore::new` does no I/O.
fn open_store() -> MetaStore {
    let root = std::env::var_os("PVE_META_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/etc/pve/meta"));
    MetaStore::new(root)
}

/// A vmid as it arrives from Perl: **an integer or a string**, whichever
/// the caller happens to hold.
///
/// PVE never normalises the scalar. `qemu-server` passes the API parameter
/// through as the string it was parsed from (`"300"`), while `pve-container`
/// has usually done arithmetic on it by the time a hook runs, which gives
/// the scalar an integer slot. perlmod deserialises a `u32` from the
/// integer slot only and refuses a plain string (`invalid type: string
/// "300", expected u32`), so a `u32` parameter silently made every hook a
/// no-op for QEMU guests. This type takes both shapes and is what every
/// export below declares for a vmid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Vmid(pub u32);

impl<'de> serde::Deserialize<'de> for Vmid {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl serde::de::Visitor<'_> for V {
            type Value = Vmid;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a vmid, as an integer or a string of digits")
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Vmid, E> {
                u32::try_from(v).map(Vmid).map_err(E::custom)
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Vmid, E> {
                u32::try_from(v).map(Vmid).map_err(E::custom)
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Vmid, E> {
                if v.fract() != 0.0 || v < 0.0 || v > f64::from(u32::MAX) {
                    return Err(E::custom(format!("{v} is not a vmid")));
                }
                Ok(Vmid(v as u32))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Vmid, E> {
                v.trim().parse().map(Vmid).map_err(|_| E::custom(format!("{v:?} is not a vmid")))
            }
        }
        d.deserialize_any(V)
    }
}

/// The node-local directory of restore markers, `$PVE_META_RUN_DIR` or
/// `/run/pve-meta`: one empty file per vmid whose config was just created
/// by `create_and_lock_config` and not yet written since. See
/// [`pve_rs_meta::mark_created`].
fn run_dir() -> PathBuf {
    std::env::var_os("PVE_META_RUN_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/run/pve-meta"))
}

/// The wall clock as unix seconds, for the notes block's `time=` field. The
/// core takes the instant as an argument so it never touches the clock
/// itself (its wasm build has none).
fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The `Proxmox::Lib::PVEMeta` library-loader package generated by
/// `genpackage.pl` (`--lib-package=Proxmox::Lib::PVEMeta`, see `Makefile`)
/// bootstraps itself on `use` (its own generated `BEGIN` block), so this crate
/// must export a matching `boot_Proxmox__Lib__PVEMeta` symbol even though it
/// has no functions of its own to export -- mirrors upstream `pve-rs`'s
/// `Proxmox::Lib::PVE` package (`proxmox-perl-rs/pve-rs/src/bindings/mod.rs`).
#[perlmod::package(name = "Proxmox::Lib::PVEMeta", lib = "pve_meta_rs")]
mod proxmox_lib_pve_meta {}

#[perlmod::package(name = "PVE::RS::Meta", lib = "pve_meta_rs")]
mod pve_rs_meta {
    //! The `PVE::RS::Meta` package: the guest lifecycle hooks, `stored_vmids`,
    //! and the `api_*` functions used by `PVE::API2::Ext::Meta`.

    use anyhow::Error;

    use pve_meta_core::api::{self, CallerAcl, GuestInput};
    use pve_meta_core::backup;
    use pve_meta_core::registry::Permission;
    use pve_meta_core::store::MetaStore;

    use super::{now_unix, open_store, run_dir, RollbackOutcome, Vmid};

    /// Every grant (`docs/DESIGN.md` §4). Cluster-only on purpose: an
    /// operator's `.deb` may ship a prefix but must never ship its own
    /// grant. Read per request — the directory is tiny, pmxcfs caches it, and
    /// a stale grant is a wrong answer about who may write.
    ///
    /// Goes through `store`'s own registry rather than reading
    /// `PVE_META_PERMISSION_DIRS` again: `store` already read it once
    /// (`MetaStore::new`), and a second, independent read of the same
    /// variable is exactly the kind of state that only agreed with the
    /// store's by coincidence.
    fn open_permissions(store: &MetaStore) -> Result<Vec<Permission>, api::ApiError> {
        Ok(store.registry()?.load_permissions()?)
    }

    // No `open_prefixes` counterpart: a prefix names no principal, so it never
    // gates a read or write, and the two exports that read prefixes want
    // different sets -- `api_prefixes` the listing with its failures,
    // `api_put` the set for the guest's node (`api::effective_prefixes`).

    // -- snapshot hooks (`docs/DESIGN.md` §9) -----------------------------
    //
    // Called from the patched `PVE/AbstractConfig.pm`. They run inside PVE's
    // own guest locks and copy whole files; they do not consult permissions.

    /// Copies `$vmid`'s current document to its `$snapname` snapshot file.
    /// A no-op if the guest has no document.
    ///
    /// Returns `1` if a copy was made, `0` otherwise.
    #[export]
    pub fn on_snapshot(vmid: Vmid, snapname: &str) -> Result<bool, Error> {
        Ok(open_store().snapshot(vmid.0, snapname)?)
    }

    /// Restores `$vmid`'s `$snapname` snapshot copy over its current
    /// document. If no snapshot copy exists but a current document does, the
    /// current document is removed (the guest had no metadata when the
    /// snapshot was taken).
    ///
    /// Returns `"restored"`, `"removed"`, or `"none"`.
    #[export]
    pub fn on_rollback(vmid: Vmid, snapname: &str) -> Result<String, Error> {
        let outcome = open_store().rollback(vmid.0, snapname)?;
        Ok(match outcome {
            RollbackOutcome::Restored => "restored",
            RollbackOutcome::RemovedNoSnapshot => "removed",
            RollbackOutcome::NoOp => "none",
        }
        .to_string())
    }

    /// Removes `$vmid`'s `$snapname` snapshot copy. Idempotent.
    ///
    /// Returns `1` if a copy existed and was removed, `0` otherwise.
    #[export]
    pub fn on_delsnap(vmid: Vmid, snapname: &str) -> Result<bool, Error> {
        Ok(open_store().delete_snapshot(vmid.0, snapname)?)
    }

    // -- orphans (`docs/DESIGN.md` §9) -------------------------------------

    /// Clears any metadata left at `$vmid` — the document and every snapshot copy.
    /// Returns the number of files removed.
    ///
    /// Called from the patched `PVE::AbstractConfig::create_and_lock_config` **only when
    /// that call asserted the vmid was unused** (`$allow_existing` false, i.e.
    /// `PVE::Cluster::check_vmid_unused` has just passed). A genuinely new guest starts
    /// with no inherited metadata; a restore *over* an existing guest keeps its document
    /// here, and the restore itself then replaces it from the backup's notes block
    /// (`notes_import`), or leaves it if the backup carried none.
    ///
    /// This is what closes the vmid-reuse window a periodic sweep cannot: a guest
    /// destroyed and recreated at the same vmid between two sweeps is never stale from
    /// the sweep's point of view, because the vmid is back in the vmlist.
    #[export]
    pub fn on_create(vmid: Vmid) -> Result<usize, Error> {
        Ok(open_store().purge(vmid.0)?)
    }

    /// Removes `$vmid`'s document and every snapshot copy. Returns the number of files
    /// removed; idempotent, and 0 when there was nothing there.
    ///
    /// Called from the patched `PVE::AbstractConfig::destroy_config`, after the guest
    /// config itself has been unlinked — so it runs on every destroy path there is
    /// (primary destroy, create/restore failure cleanup, clone failure cleanup, remote
    /// migration abort), all of which funnel through that one method.
    #[export]
    pub fn on_destroy(vmid: Vmid) -> Result<usize, Error> {
        let removed = open_store().purge(vmid.0)?;
        // A create that failed part-way destroys its config; its marker goes
        // with it, or the vmid's next owner would import on its first write.
        let _ = std::fs::remove_file(run_dir().join(vmid.0.to_string()));
        Ok(removed)
    }

    // -- the restore marker (`docs/DESIGN.md` §9) ----------------------------
    //
    // `write_config` cannot tell a restore from any other config write, but
    // every restore (and create, and clone) begins with
    // `create_and_lock_config`, in the same patched file. That hook leaves a
    // node-local marker for the vmid; `write_config` consumes it on the first
    // write that carries a block, or the first write of an unlocked config
    // (a create keeps the config locked and writes it more than once), and
    // imports a notes block only on the write that consumed it. A block
    // pasted into a live guest's notes is therefore never imported by a plain
    // `qm set`; `pve-meta scan-notes` is the explicit way in. Node-local
    // because a restore runs entirely on one node, and in `/run` because a
    // marker must not outlive a reboot.

    /// Leaves the restore marker for `$vmid`. Called from the patched
    /// `create_and_lock_config`, after PVE has written the initial config.
    #[export]
    pub fn mark_created(vmid: Vmid) -> Result<(), Error> {
        let dir = run_dir();
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join(vmid.0.to_string()), b"")?;
        Ok(())
    }

    /// Removes `$vmid`'s restore marker and says whether there was one.
    /// Called from the patched `write_config` on a write that carries a
    /// block or writes an unlocked config; only a `1` lets that write import
    /// a notes block.
    #[export]
    pub fn take_created(vmid: Vmid) -> Result<bool, Error> {
        match std::fs::remove_file(run_dir().join(vmid.0.to_string())) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// Every vmid the store holds any file for -- a document, a snapshot
    /// copy, or both -- sorted ascending. A file whose name carries no vmid
    /// is not a guest and is never listed.
    ///
    /// What `pve-meta ls --orphans` subtracts the vmlist from, and what
    /// `pve-meta rm` checks before removing anything: the one case the
    /// create and destroy hooks cannot see is a guest config deleted out of
    /// band, and this is how an administrator finds what it left behind.
    #[export]
    pub fn stored_vmids() -> Result<Vec<u32>, Error> {
        Ok(open_store().stored_vmids()?)
    }

    // -- backup and restore (`docs/DESIGN.md` §9) ---------------------------
    //
    // A backup carries the document in the archive's copy of the guest's
    // notes; a restore reads it back out (`pve_meta_core::backup`).

    /// The notes block for `$vmid`'s current document, or `undef` if the
    /// guest has none. Called from the patched vzdump `assemble` of both
    /// guest types, which append it to the archive's copy of the notes.
    ///
    /// Dies, with the reason, for a document that is not carried: one above
    /// the size cap, or one that does not parse. The caller warns into the
    /// backup log and the backup runs without it.
    #[export]
    pub fn export_for_backup(vmid: Vmid) -> Result<Option<String>, Error> {
        Ok(backup::export(&open_store(), vmid.0, now_unix())?)
    }

    /// Reads a notes block out of `$description` into `$vmid`'s document and
    /// strips it. Returns `{ action, description }`: `action` is `imported`,
    /// `stripped` or `none`, and `description` is the notes without the
    /// block, or `undef` when there was nothing to remove.
    ///
    /// `$mode` is `restore` (the block is the backup being restored, so it
    /// replaces whatever document the vmid had) or `install` (the scan
    /// `pve-meta scan-notes` runs once at install: a document already there
    /// is kept and the block only stripped). The caller holds the document's
    /// `cfs_lock_domain` lock. An error leaves the notes untouched.
    #[export]
    pub fn notes_import(
        vmid: Vmid,
        description: &str,
        mode: &str,
    ) -> Result<backup::Import, Error> {
        let mode = backup::ImportMode::parse(mode)?;
        Ok(backup::import(&open_store(), vmid.0, description, mode)?)
    }

    /// Returns this crate's version string. Used by
    /// `crates/pve-meta-perl/test/basic.pl` and available to operators for
    /// checking which build a running pvedaemon/pveproxy has loaded.
    #[export]
    pub fn version() -> String {
        env!("CARGO_PKG_VERSION").to_string()
    }

    // -- API-shaped exports (`PVE::API2::Ext::Meta`, `docs/DESIGN.md` §8) --
    //
    // Thin wrappers over `pve_meta_core::api` (see that module's docs for the
    // wire contract and the authorization rules). All of these die with a
    // Rust `pve_meta_core::api::ApiError` whose `Display` is `"NNN: message"`
    // (an HTTP status prefix); the Perl layer parses that prefix and
    // re-raises via `PVE::Exception::raise`. `perlmod`'s `#[export]` only
    // needs the error type to be `Display`, so this needs no conversion back
    // to `anyhow::Error`.
    //
    // `$acl` is a native hash: `{ authid, read, write, tags => [...], node }`,
    // where `read`/`write` are the PVE ACL answers for the document being
    // addressed, `tags` are the guest's PVE tags, which resolve the
    // registrations' selectors, and `node` is the guest's current node, whose
    // prefix files join the packaged and cluster ones. Rust computes the
    // caller's scopes from it.

    /// `GET /meta/version` -> `{ token, changed }`, plus `documents`
    /// (`[{ id, digest }]`, sorted) when `$detail` is true.
    ///
    /// With `$id`, the token covers that one document plus the registry
    /// directories instead of the whole store — the cheap poll an open editor
    /// wants, and the only form whose cost does not grow with the cluster.
    /// `$node` (optional, trailing) is a guest's current node from the vmlist:
    /// its prefix directory and its name join that guest's token.
    #[export]
    pub fn api_version(
        detail: bool,
        id: Option<&str>,
        node: Option<&str>,
    ) -> Result<api::ApiVersion, api::ApiError> {
        api::version(&open_store(), detail, id, node)
    }

    /// `GET /meta/permissions` -> every permission, as native hashes, plus a
    /// row for any file in the directory that failed to load -- named, with
    /// `error` set, and nothing else -- so a hand-edit or a bad package
    /// upgrade that broke a file is visible here instead of just in the log
    /// (`docs/DESIGN.md` §1). This is the one export that reads
    /// `Registry::list_permissions` rather than `open_permissions`: every
    /// other export needs the parsed grants alone, because a file that did
    /// not load must never grant anything.
    #[export]
    pub fn api_permissions() -> Result<Vec<api::PermissionEntry>, api::ApiError> {
        let store = open_store();
        let (permissions, failures) = store.registry()?.list_permissions()?;
        Ok(api::permissions_list(&permissions, &failures))
    }

    /// `GET /meta/prefixes` -> the cluster-wide prefixes, most-specific first;
    /// with `$node`, the prefixes in effect for a guest on that node; with `$all`
    /// (optional, trailing), every prefix file there is, each with its `origin`
    /// (and `node`). Each plus a row for any file that failed to load. See
    /// [`api_permissions`] -- the same reasoning. The caller has checked that
    /// `$node` is in the cluster; Rust checks its shape, and refuses both at once.
    #[export]
    pub fn api_prefixes(
        node: Option<&str>,
        all: Option<bool>,
    ) -> Result<Vec<api::PrefixEntry>, api::ApiError> {
        api::prefixes(open_store().registry()?, node, all.unwrap_or(false))
    }

    /// `GET /meta/schemas` -> `{ prefix, permission }`, the two registry file
    /// formats described in the same dialect a prefix uses, so the editor
    /// can show one as a typed tree.
    #[export]
    pub fn api_schemas() -> Result<pve_meta_core::model::Value, Error> {
        Ok(api::schemas())
    }

    /// `GET /meta/access` -> `{ read, write, scopes }` for one document,
    /// with the permissions' selectors already resolved against `$acl`'s
    /// tags. `$id` is a vmid, `prefixes/<name>`, `nodes/<node>/prefixes/<name>`
    /// or `permissions/<name>`.
    #[export]
    pub fn api_access(id: &str, acl: CallerAcl) -> Result<api::ApiAccess, api::ApiError> {
        let doc_id = api::parse_id(id)?;
        Ok(api::access(&open_permissions(&open_store())?, &doc_id, &acl))
    }

    /// `GET /meta/guests`. `$guests` is the array of vmlist rows Perl already
    /// has — `[{vmid, node, type, name, tags, read, write}]` — as a native
    /// array of hashes. Rust never reads `.vmlist` or a guest config itself.
    #[export]
    pub fn api_list_guests(
        authid: &str,
        guests: Vec<GuestInput>,
        has: Option<&str>,
    ) -> Result<Vec<api::GuestListEntry>, api::ApiError> {
        let store = open_store();
        api::list_guests(&store, &open_permissions(&store)?, authid, &guests, has)
    }

    /// `GET /meta/guests/{vmid}` and the registry documents' `GET` (`$id` is
    /// a vmid, `prefixes/<name>`, `nodes/<node>/prefixes/<name>` or
    /// `permissions/<name>`). `$comments` (optional, trailing) keeps the comment
    /// keys, which a read otherwise leaves out.
    #[export]
    pub fn api_get(
        id: &str,
        view: Option<&str>,
        format: &str,
        acl: CallerAcl,
        comments: Option<bool>,
    ) -> Result<api::ApiViewDocument, api::ApiError> {
        let store = open_store();
        let comments = comments.unwrap_or(false);
        api::get_document(&store, &open_permissions(&store)?, id, view, format, comments, &acl)
    }

    /// `PUT /meta/guests/{vmid}` and the registry documents' `PUT`.
    ///
    /// `$payload` is the only string crossing: the client's `data` (JSON) or
    /// `text` (YAML) parameter, decoded once in Rust. `$force` (optional,
    /// trailing) stores the result even where a prefix declares
    /// `enforce: true` and it would not match that prefix's schema. `$comments`
    /// (optional, trailing) makes a replace's payload the subtree notes included;
    /// without it the payload carries none and the stored ones are kept. The
    /// prefixes enforced for a guest are those in effect on `$acl`'s `node`.
    ///
    /// The caller (`PVE::API2::Ext::Meta`) must already hold the document's
    /// `cfs_lock_domain` lock: the digest precondition is re-checked inside
    /// this call, but only a lock makes the read-modify-write atomic across
    /// nodes (`docs/DESIGN.md` §7).
    #[export]
    #[allow(clippy::too_many_arguments)] // matches the PUT endpoint's parameter set 1:1 (docs/DESIGN.md §8)
    pub fn api_put(
        id: &str,
        view: Option<&str>,
        format: &str,
        payload: &str,
        mode: &str,
        digest: Option<&str>,
        dry_run: bool,
        acl: CallerAcl,
        force: Option<bool>,
        comments: Option<bool>,
    ) -> Result<api::ApiPutResult, api::ApiError> {
        let store = open_store();
        let prefixes = api::effective_prefixes(store.registry()?, &api::parse_id(id)?, &acl)?;
        api::put_document(
            &store,
            &open_permissions(&store)?,
            &prefixes,
            id,
            view,
            format,
            payload,
            mode,
            digest,
            dry_run,
            force.unwrap_or(false),
            comments.unwrap_or(false),
            &acl,
        )
    }

    /// `DELETE /meta/guests/{vmid}` and the registry documents' `DELETE`. Removes the
    /// current document only -- never a snapshot copy.
    #[export]
    pub fn api_delete(
        id: &str,
        view: Option<&str>,
        digest: Option<&str>,
        acl: CallerAcl,
    ) -> Result<api::ApiPutResult, api::ApiError> {
        let store = open_store();
        api::delete_document(&store, &open_permissions(&store)?, id, view, digest, &acl)
    }
}
