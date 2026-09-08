# libpve-meta-rs-perl — `PVE::RS::Meta` specification

> **Scope.** This file specifies the *lifecycle* functions and the crate's build and
> packaging. The `api_*` functions are specified by `DESIGN.md` §3 (and §8/§9, the binding
> decisions from the 2026-09-07 and 2026-09-08 reviews); their implementation lives in
> `pve_meta_core::api` and is specified by `crates/pve-meta-core/SPEC.md` §9. Where this
> file and DESIGN.md disagree, DESIGN.md wins.

Rust cdylib exposing the store's lifecycle operations to PVE's Perl code through
`perlmod`, packaged exactly like upstream `libpve-rs-perl`.

## Crate

`crates/pve-meta-perl` (package name `pve-meta-rs`, `[lib] crate-type = ["cdylib"]` →
`libpve_meta_rs.so`), edition 2021, dependencies: `pve-meta-core` (path), `anyhow`,
`serde`, `serde_json`, and `perlmod` with the `exporter` feature. `perlmod` is **not** on
crates.io (checked 2026-09-07: the API returns 404), so it is a git dependency pinned to
`d85d4ebdd13c1dcb469e15eb0ce7b22640418b8e` — the commit upstream `pve-rs` 0.15.3 uses.

It is a member of the root workspace but excluded from `default-members`, because
perlmod's `build.rs` compiles against Perl's `CORE` headers and therefore needs
`libperl-dev` and a `perl` interpreter: it builds on the Linux build host, not on macOS.
`cargo build`/`cargo test` run bare at the workspace root skip it; `cargo build -p
pve-meta-rs` (or an explicit `--workspace`) still targets it.

The bindings crate is deliberately **thin**: it owns the `#[perlmod::package]` glue and
`open_store()` (the `$PVE_META_ROOT` lookup, default `/etc/pve/meta`), and nothing else.
Every decision — the document model, the views, and in particular the write
authorization — lives in `pve-meta-core`, where it can be unit-tested on any machine.

## Perl API (`#[perlmod::package(name = "PVE::RS::Meta", lib = "pve_meta_rs")]`)

All functions take the vmid as an integer and die with a readable message on error
(`anyhow::Error` → Perl `die`).

### Guest lifecycle hooks

Called from the patched PVE Perl code (`patches/lifecycle/`, `docs/LIFECYCLE-PATCHES.md`).
They run inside PVE's own guest locks and copy whole files; they do not consult grants.

* `on_snapshot($vmid, $snapname)` → copies the document to the snapshot file; no-op if
  the guest has no document. Returns 1 if a copy was made, 0 otherwise.
* `on_rollback($vmid, $snapname)` → restores the snapshot copy over the current document;
  if no snapshot copy exists but a current document does, the current document is
  removed (the guest had no metadata when the snapshot was taken). Returns a string:
  `restored`, `removed`, `none`.
* `on_delsnap($vmid, $snapname)` → removes the snapshot copy. Returns 1/0.
* `on_clone($vmid, $newid)` → copies the document (not the snapshots) to the new vmid,
  **overwriting** any document `$newid` already has, mirroring
  `PVE::Firewall::Helpers::clone_vmfw_conf`'s unlink-then-copy, soft-fail clone: a stale
  target document left over from an earlier destroy race must not block a clone. If
  `$vmid` has no document this is a no-op that still removes a stale `$newid` document,
  so the failed clone leaves `$newid` clean. Returns 1/0.
  (`LIFECYCLE-PATCHES.md` §3.4 offered dying on an existing target as the alternative;
  overwriting is what the code does and what this spec now says.)
* `on_destroy($vmid)` → removes the document and all its snapshot copies. Returns the
  number of files removed. This is the **only** caller of the cascading
  `MetaStore::destroy`: the REST API's `DELETE` removes the current document only
  (`DESIGN.md` §8).
* `export_for_backup($vmid)` → `undef` if no document, else a string
  `"#pve-meta-format: yaml\n" + raw text`. (The header records the format; it is stripped
  on import, and a leading `#` line is a YAML comment anyway.)
* `import_from_backup($vmid, $string)` → parses the header and writes the document,
  replacing any existing document for that vmid; validated through the core parser/lint
  first, dies on invalid content. A blob already in the on-disk format is restored
  byte-for-byte; a blob from an older build naming another format is parsed in that
  format and **re-dumped as YAML**. Returns 1.

  That asymmetry is deliberate and is the only place a non-YAML format is still read
  (`DESIGN.md` §8 is about the *store*: YAML only on disk). A blob is a backup, possibly
  years old and written by a build that had TOML/JSON on disk; refusing it would make an
  old archive unrestorable, while converting it ends the asymmetry at the file — nothing
  but YAML is ever stored.

  A die here is safe for the caller: `PVE::API2::LXC::create_vm` sets
  `$destroy_config_on_error = 1` unconditionally before the restore hook runs
  (`API2/LXC.pm:539`), so a rejected import leaves no half-restored guest behind.
* `list_snapshots($vmid)` → array ref of snapshot names.
* `has_document($vmid)` → 1/0.
* `version()` → the crate version string. Used by `test/basic.pl` and available to
  operators for checking which build a running pvedaemon/pveproxy has loaded.

### `api_*` functions

Thin wrappers over `pve_meta_core::api` (see `crates/pve-meta-core/SPEC.md` §9 for the
authorization rules and `DESIGN.md` §3 for the endpoints): `api_version`, `api_grants`,
`api_list_guests`, `api_get`, `api_put`, `api_delete`. They die with
`"NNN: message"` (an HTTP status prefix) which `PVE::API2::Ext::Meta::_call` turns into a
`PVE::Exception`.

`api_grants($authid)` never fails on a malformed `scopes` map: anything it cannot parse
is skipped with a warning and grants nothing, because this lookup runs on every guest
request (`DESIGN.md` §9). `api_list_guests($guests_json, $has, $orphans)` takes a third
argument: with it (the caller has `Sys.Audit` on `/`) the result also lists documents
whose vmid is not among the rows Perl passed in, marked `orphan => 1`.

`api_put` and `api_delete` re-check the digest precondition, but only the caller's
`PVE::Cluster::cfs_lock_domain("pve-meta-<id>", 10, …)` makes the read-modify-write atomic
across nodes (`DESIGN.md` §8) — the Perl API module is responsible for holding it.

## Build & packaging

* `crates/pve-meta-perl/Makefile` modelled on `pve-rs/Makefile`: generates
  `PVE/RS/Meta.pm` and `Proxmox/Lib/PVEMeta.pm` with
  `perl genpackage.pl --lib=pve_meta_rs --lib-tag=pvemeta --lib-package=Proxmox::Lib::PVEMeta --lib-prefix=PVEMeta PVE::RS::Meta`
  (`genpackage.pl` is vendored into the crate, so the build needs no perlmod-bin package),
  builds the cdylib with cargo, and has an `install` target placing `libpve_meta_rs.so` in
  `$(DESTDIR)$(PERL_INSTALLVENDORARCH)/auto/` and the `.pm` files in
  `$(DESTDIR)$(PERL_INSTALLVENDORLIB)/…` (paths from `perl -MConfig`).
* Perl-side test: `crates/pve-meta-perl/test/basic.pl`, run by `make check`. It uses the
  sed-patched loader trick so it loads `target/{debug,release}/libpve_meta_rs.so`
  directly, sets `PVE_META_ROOT` to a temp dir, and exercises every function — the
  lifecycle hooks including the export/import round trip and `die` behaviour, and the
  `api_*` contract including the reviews' regressions (no structure creation through an
  empty merge, no path names in a 403 the caller cannot read, merge-with-null, `{}`
  replace, write-time `scopes` validation, the empty-digest create flow; and from the
  2026-09-08 pass: no scope may write `scopes`, an empty scope prefix is refused on write
  and grants nothing on disk, an out-of-band invalid document stays readable and
  repairable, a non-map `scopes` grants nothing, a scoped read never carries the bare
  `__`, a single `scopes` entry is not addressable while a dotted authid is a valid key,
  and orphan documents are listed and deletable).
* The Debian package `libpve-meta-rs-perl` is the second binary package of the `pve-meta`
  source package; see `crates/pve-meta-perl/PACKAGING.md`.
