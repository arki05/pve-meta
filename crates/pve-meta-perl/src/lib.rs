//! `pve-meta-rs`: the `PVE::RS::Meta` Perl bindings — lifecycle hooks,
//! `stored_vmids`, and the `api_*` functions behind
//! `perl/PVE/API2/Ext/Meta.pm` (`docs/DESIGN.md` §6-7), all thin wrappers
//! over [`pve_meta_core`].

use std::path::PathBuf;

use pve_meta_core::store::{MetaStore, RollbackOutcome};

/// Opens a [`MetaStore`] rooted at `$PVE_META_ROOT` (default `/etc/pve/meta`).
fn open_store() -> MetaStore {
    let root = std::env::var_os("PVE_META_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/etc/pve/meta"));
    MetaStore::new(root)
}

/// A vmid as Perl hands it over: an integer or a digit string. PVE doesn't
/// normalize the scalar, and perlmod's plain `u32` refuses a string, which
/// silently no-ops every hook for QEMU guests; this type accepts both.
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

/// The node-local directory of restore markers, `$PVE_META_RUN_DIR` (default
/// `/run/pve-meta`): one empty file per vmid awaiting [`pve_rs_meta::take_created`].
fn run_dir() -> PathBuf {
    std::env::var_os("PVE_META_RUN_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/run/pve-meta"))
}

/// The wall clock as unix seconds, for the notes block's `time=` field
/// (taken as an argument by the core so its wasm build never touches a clock).
fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `genpackage.pl`'s generated `Proxmox::Lib::PVEMeta` calls
/// `boot_Proxmox__Lib__PVEMeta` on `use`, so this module must exist even
/// though it exports no functions of its own.
#[perlmod::package(name = "Proxmox::Lib::PVEMeta", lib = "pve_meta_rs")]
mod proxmox_lib_pve_meta {}

#[perlmod::package(name = "PVE::RS::Meta", lib = "pve_meta_rs")]
mod pve_rs_meta {
    //! The `PVE::RS::Meta` package: the guest lifecycle hooks, `stored_vmids`,
    //! and the `api_*` functions used by `PVE::API2::Ext::Meta`.

    use anyhow::Error;

    use pve_meta_core::api::{self, CallerAcl, GuestInput};
    use pve_meta_core::backup;

    use super::{now_unix, open_store, run_dir, RollbackOutcome, Vmid};

    // No `open_prefixes`: `api_prefixes` wants every file (with failures);
    // `api_put` wants only those effective for the guest's node.

    // -- snapshot hooks, called from the patched `PVE/AbstractConfig.pm`
    // (`docs/DESIGN.md` §7, `docs/LIFECYCLE.md`) ---------------------------

    /// Copies `$vmid`'s current document to its `$snapname` snapshot file.
    /// A no-op if the guest has no document. Returns `1` if a copy was made.
    #[export]
    pub fn on_snapshot(vmid: Vmid, snapname: &str) -> Result<bool, Error> {
        Ok(open_store().snapshot(vmid.0, snapname)?)
    }

    /// Restores `$vmid`'s `$snapname` snapshot copy over its current
    /// document, or removes the current document if no copy exists. Returns
    /// `"restored"`, `"removed"`, or `"none"`.
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

    /// Removes `$vmid`'s `$snapname` snapshot copy. Idempotent; returns `1`
    /// if a copy existed.
    #[export]
    pub fn on_delsnap(vmid: Vmid, snapname: &str) -> Result<bool, Error> {
        Ok(open_store().delete_snapshot(vmid.0, snapname)?)
    }

    /// Clears any metadata at `$vmid`: the document and its snapshot copies.
    /// Called from `create_and_lock_config` when it just asserted the vmid
    /// unused (`docs/decisions/009-no-sweeper.md`), and from the patched
    /// `write_config` for a restore over an existing guest whose backup
    /// carried no document (`docs/LIFECYCLE.md`). Returns the files removed.
    #[export]
    pub fn on_create(vmid: Vmid) -> Result<usize, Error> {
        Ok(open_store().purge(vmid.0)?)
    }

    /// Removes `$vmid`'s document and every snapshot copy. Called from
    /// `destroy_config` after the guest config itself is unlinked, so it
    /// runs on every destroy path. Idempotent; returns the files removed.
    #[export]
    pub fn on_destroy(vmid: Vmid) -> Result<usize, Error> {
        let removed = open_store().purge(vmid.0)?;
        // A failed create's marker must go with it, or the vmid's next
        // owner would import a stale one on its first write.
        let _ = std::fs::remove_file(run_dir().join(vmid.0.to_string()));
        Ok(removed)
    }

    /// Leaves the restore marker for `$vmid`, taken by [`take_created`].
    /// Called from `create_and_lock_config` after PVE writes the initial
    /// config.
    #[export]
    pub fn mark_created(vmid: Vmid) -> Result<(), Error> {
        let dir = run_dir();
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join(vmid.0.to_string()), b"")?;
        Ok(())
    }

    /// Removes `$vmid`'s restore marker and says whether there was one; only
    /// a `1` lets the calling `write_config` import a notes block
    /// (`docs/LIFECYCLE.md`).
    #[export]
    pub fn take_created(vmid: Vmid) -> Result<bool, Error> {
        match std::fs::remove_file(run_dir().join(vmid.0.to_string())) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// Every vmid the store holds a file for, sorted ascending. What
    /// `pve-meta ls --orphans` subtracts the vmlist from, for a guest config
    /// deleted out of band (`docs/DESIGN.md` §8).
    #[export]
    pub fn stored_vmids() -> Result<Vec<u32>, Error> {
        Ok(open_store().stored_vmids()?)
    }

    /// Every file in the store's root that is neither a document nor a
    /// snapshot copy, by file name, sorted. `pve-meta ls` lists them as
    /// `unknown/<file>`; nothing removes them or calls them orphans
    /// (`docs/DESIGN.md` §8).
    #[export]
    pub fn unknown_files() -> Result<Vec<String>, Error> {
        Ok(open_store().unknown_files()?)
    }

    /// The notes block for `$vmid`'s current document, or `undef` if none.
    /// Called from the patched vzdump `assemble` of both guest types
    /// (`docs/LIFECYCLE.md`). Dies for a document too large or unparsable to
    /// carry; the caller warns into the backup log and continues.
    #[export]
    pub fn export_for_backup(vmid: Vmid) -> Result<Option<String>, Error> {
        Ok(backup::export(&open_store(), vmid.0, now_unix())?)
    }

    /// Reads a notes block out of `$description` into `$vmid`'s document and
    /// strips it. Returns `{ action, description }`: `action` is
    /// `imported`, `stripped` or `none`. `$mode` is `restore` (the block
    /// replaces the vmid's document) or `install` (`pve-meta scan-notes`:
    /// an existing document is kept, the block only stripped). The caller
    /// holds the document's `cfs_lock_domain` lock; an error leaves the
    /// notes untouched.
    #[export]
    pub fn notes_import(
        vmid: Vmid,
        description: &str,
        mode: &str,
    ) -> Result<backup::Import, Error> {
        let mode = backup::ImportMode::parse(mode)?;
        Ok(backup::import(&open_store(), vmid.0, description, mode)?)
    }

    /// A guest's tag string (`;`-separated in its config, `undef` when it has
    /// none) as a list: [`pve_meta_core::tags::split_tags`], the one
    /// splitting rule, which a prefix's selector is matched against.
    /// `PVE::API2::Ext::Meta::parse_tags` is this call.
    #[export]
    pub fn split_tags(raw: Option<&str>) -> Vec<String> {
        pve_meta_core::tags::split_tags(raw.unwrap_or(""))
    }

    /// This crate's version, checked by `test/basic.pl` and by operators
    /// against a running pvedaemon/pveproxy.
    #[export]
    pub fn version() -> String {
        env!("CARGO_PKG_VERSION").to_string()
    }

    // -- `api_*`: thin wrappers over `pve_meta_core::api` (`docs/DESIGN.md`
    // §6) backing `PVE::API2::Ext::Meta`. Each dies with an
    // `api::ApiError` (`Display` is `"NNN: message"`), which the Perl layer
    // re-raises via `PVE::Exception::raise`. `$acl` is
    // `{ authid, read, write, tags => [...], node }`: `read`/`write` are the
    // caller's PVE ACL answers for the document addressed (`docs/DESIGN.md`
    // §4), `tags` resolve a prefix's selector, `node` picks its overrides.

    /// `GET /meta/version` -> `{ token }`, one hash over every document and
    /// prefix file.
    #[export]
    pub fn api_version() -> Result<api::ApiVersion, api::ApiError> {
        api::version(&open_store())
    }

    /// `GET /meta/prefixes` -> without `$tags`, every prefix file as
    /// declared; with `$tags`/`$node`, those reaching the guest with
    /// `enforce`/`hidden`/`schema` already resolved. Either way, a row per
    /// file that failed to load, named with `error` set (`docs/DESIGN.md` §1).
    #[export]
    pub fn api_prefixes(
        node: Option<&str>,
        tags: Option<Vec<String>>,
    ) -> Result<Vec<api::PrefixEntry>, api::ApiError> {
        api::prefixes(open_store().registry()?, node, tags.as_deref())
    }

    /// `GET /meta/schemas` -> `{ prefix }`, the prefix file format in its
    /// own schema dialect, for the editor to render as a typed tree.
    #[export]
    pub fn api_schemas() -> Result<pve_meta_core::model::Value, Error> {
        Ok(api::schemas())
    }

    /// Dies with `400:` unless `$id` is one [`api::parse_id`] accepts: a
    /// vmid or `prefixes/<name>`. `PVE::API2::Ext::Meta::lock_domain_for`
    /// checks with it, before a lock is named after the id.
    #[export]
    pub fn check_id(id: &str) -> Result<(), api::ApiError> {
        api::parse_id(id).map(|_| ())
    }

    /// `GET /meta/access` -> `{ read, write }` for one document, exactly
    /// `$acl`'s own answers. `$id` is only parsed, for a 400 on a garbage id.
    #[export]
    pub fn api_access(id: &str, acl: CallerAcl) -> Result<api::ApiAccess, api::ApiError> {
        api::parse_id(id)?;
        api::access(&open_store(), &acl)
    }

    /// `GET /meta/guests`. `$guests` is the vmlist rows Perl already has —
    /// `[{vmid, node, type, name, tags, read}]`; Rust never reads `.vmlist`
    /// or a guest config itself.
    #[export]
    pub fn api_list_guests(
        guests: Vec<GuestInput>,
        has: Option<&str>,
    ) -> Result<Vec<api::GuestListEntry>, api::ApiError> {
        api::list_guests(&open_store(), &guests, has)
    }

    /// `GET /meta/guests/{vmid}` and the registry documents' `GET` (`$id` is
    /// a vmid or `prefixes/<name>`). `$comments` keeps the comment keys a
    /// read otherwise leaves out.
    #[export]
    pub fn api_get(
        id: &str,
        view: Option<&str>,
        format: &str,
        acl: CallerAcl,
        comments: Option<bool>,
    ) -> Result<api::ApiViewDocument, api::ApiError> {
        api::get_document(&open_store(), id, view, format, comments.unwrap_or(false), &acl)
    }

    /// `PUT /meta/guests/{vmid}` and the registry documents' `PUT`.
    /// `$payload` is the client's `data` (JSON) or `text` (YAML), decoded
    /// once in Rust. `$force` stores past an enforcing prefix's schema
    /// mismatch; `$comments` keeps a replace's own subtree notes. The
    /// caller must already hold the document's `cfs_lock_domain` lock — the
    /// digest check here is not itself a cross-node lock.
    #[export]
    #[allow(clippy::too_many_arguments)] // matches the PUT endpoint's parameters 1:1
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

    /// `DELETE /meta/guests/{vmid}` and the registry documents' `DELETE`.
    /// Removes the current document only, never a snapshot copy.
    #[export]
    pub fn api_delete(
        id: &str,
        view: Option<&str>,
        digest: Option<&str>,
        acl: CallerAcl,
    ) -> Result<api::ApiPutResult, api::ApiError> {
        api::delete_document(&open_store(), id, view, digest, &acl)
    }
}
