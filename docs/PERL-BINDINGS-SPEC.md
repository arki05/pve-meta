# libpve-meta-rs-perl — `PVE::RS::Meta` specification

> **Scope.** This file specifies the exports, the boundary and the crate's build and
> packaging. The behaviour behind the `api_*` functions is specified by `DESIGN.md` §3–§6
> and `crates/pve-meta-core/SPEC.md`. Where this file and DESIGN.md disagree, DESIGN.md
> wins.

Rust cdylib exposing the store to PVE's Perl code through `perlmod`, packaged exactly
like upstream `libpve-rs-perl`.

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

The bindings crate is deliberately **thin**: it owns the `#[perlmod::package]` glue,
`open_store()` (the `$PVE_META_ROOT` lookup, default `/etc/pve/meta`) and
`open_registry()` (`registry::load_default()`), and nothing else. Every decision — the
document model, the views, the registry, and in particular the write authorization —
lives in `pve-meta-core`, where it can be unit-tested on any machine.

## The boundary: native structures, one string

`DESIGN.md` §5: **grants, guest lists and results cross as native Perl hashes and
arrays.** perlmod does serde-based conversion of native Perl structures; that is its
purpose. The single exception is the client-supplied `data` parameter, which is a JSON
string because that is what the REST parameter is, decoded once in Rust.

Revision 4 passed grants, view data and guest lists as JSON *strings*, which cost an
encode plus a decode per request and created a bug class: `Meta.pm`'s `_grants_json` was
a hand-built JSON string, because `encode_json` renders Perl's `1`/`0` as JSON numbers
and serde wanted `true`/`false`. `_grants_json`, `_inflate_view`, `api::parse_grants`,
`GuestInput::grants`-as-string and `ApiViewDocument::data_json` are all gone.

**perlmod converts a Perl scalar to a Rust `bool` by truthiness** — `1`, `"1"` and any
non-empty string are true; `0`, `""` and `undef` are false. `test/basic.pl` asserts all
six cases, because that is the property that replaced the hand-built JSON.

The caller crosses as one hash:

```perl
{ authid => 'scoped@pve!t1', read => 1, write => 0, tags => ['traefik'] }
```

`read`/`write` are the PVE ACL answers for the document being addressed (`VM.Audit` /
`VM.Config.Options` on `/vms/<vmid>`; `Sys.Audit` / `Sys.Modify` on `/` for the
datacenter document) and `tags` are that guest's PVE tags, which resolve the
registrations' selectors. Rust computes the caller's scopes from it; Perl never builds a
grant list.

## Perl API (`#[perlmod::package(name = "PVE::RS::Meta", lib = "pve_meta_rs")]`)

All functions die with a readable message on error (`anyhow::Error` → Perl `die`).

### Snapshot hooks

The **only** lifecycle hooks (`DESIGN.md` §6). Called from one patched file,
`PVE/AbstractConfig.pm` (package `libpve-guest-common-perl`). They run inside PVE's own
guest locks and copy whole files; they do not consult grants. Their signatures are
unchanged from revision 4.

* `on_snapshot($vmid, $snapname)` → copies the document to the snapshot file; no-op if
  the guest has no document. Returns 1 if a copy was made, 0 otherwise.
* `on_rollback($vmid, $snapname)` → restores the snapshot copy over the current document;
  if no snapshot copy exists but a current document does, the current document is
  removed (the guest had no metadata when the snapshot was taken). Returns a string:
  `restored`, `removed`, `none`.
* `on_delsnap($vmid, $snapname)` → removes the snapshot copy. Returns 1/0.

**Removed in revision 5** (`DESIGN.md` §10): `on_clone`, `on_destroy`,
`export_for_backup`, `import_from_backup`, `list_snapshots`, `has_document` and
`api_grants`. Destroy is a GC; clone and backup are not carried ("metadata lives in
`/etc/pve`; back up `/etc/pve`"); `list_snapshots`/`has_document` existed for the orphan
machinery, which is gone with the orphan concept.

### Garbage collection

* `gc($vmids)` → removes every document **and snapshot copy** whose vmid is not in
  `$vmids`, an array ref of integers — the vmlist the caller passes in. Returns the
  number of files removed. Rust never reads `/etc/pve/.vmlist` itself.

  Run by a systemd timer on every node. The caller holds the cluster lock and passes a
  vmlist it has actually refreshed:

  ```perl
  PVE::Cluster::cfs_update();
  my $ids = (PVE::Cluster::get_vmlist() || {})->{ids} || {};
  my $vmids = [ map { int($_) } keys %$ids ];
  die "refusing to gc against an empty vmlist\n" if !@$vmids;
  my $removed = PVE::Cluster::cfs_lock_domain("pve-meta-gc", 10, sub {
      return PVE::RS::Meta::gc($vmids);
  });
  ```

  The `cfs_update()` and the empty-vmlist guard are not optional: a fresh process that
  has not refreshed sees an empty vmlist, and an empty vmlist means "every document is
  stale". The datacenter document is never a guest and is never removed.

* `version()` → the crate version string. Used by `test/basic.pl` and available to
  operators for checking which build a running pvedaemon/pveproxy has loaded.

### `api_*` functions

Thin wrappers over `pve_meta_core::api` (see `crates/pve-meta-core/SPEC.md` §10 for the
authorization rules and `DESIGN.md` §5 for the endpoints). They die with
`"NNN: message"` (an HTTP status prefix) which `PVE::API2::Ext::Meta::_call` turns into a
`PVE::Exception`.

* `api_version()` → `{ token, changed }`.
* `api_operators()` → every registration, as native hashes:
  `[{ name, authid, description, scopes: [{ prefix, mode, selector, grammar? }] }]`.
  `selector` is spelled as the file spells it (`{all => 1}` / `{tag => '<name>'}`).
  A malformed registration file is skipped with a warning and does not appear.
* `api_access($id, $acl)` → `{ read, write, scopes }` for one document, with the
  registrations' selectors already resolved against `$acl->{tags}`. `$id` is a vmid or
  `"datacenter"`; scopes are always empty for the latter.
* `api_list_guests($authid, $guests, $has)` → one row per guest the caller can read
  anything of. `$guests` is the array of vmlist rows Perl already has,
  `[{vmid, node, type, name, tags, read, write}]`, as a native array of hashes.
  `node`/`name`/`tags` come back only for guests the caller has `VM.Audit` on.
* `api_get($id, $view, $format, $acl)` → `{ id, view, digest, data | text, parse_error? }`.
  `data` is a native structure. A document that does not parse answers with the raw
  `text` plus `parse_error` for `format=yaml` and a full reader, and **422** otherwise
  (`DESIGN.md` §4).
* `api_put($id, $view, $format, $payload, $mode, $digest, $dry_run, $acl)` →
  `{ id, view, digest, touched }`. `$payload` is the one string crossing.
* `api_delete($id, $view, $digest, $acl)` → the same shape. Removes the current document
  only — never a snapshot copy.

`api_put` and `api_delete` re-check the digest precondition, but only the caller's
`PVE::Cluster::cfs_lock_domain("pve-meta-<id>", 10, …)` makes the read-modify-write atomic
across nodes (`DESIGN.md` §4) — the Perl API module is responsible for holding it.

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
  directly, sets `PVE_META_ROOT` and `PVE_META_OPERATOR_DIRS` to temp dirs, and exercises
  every export: the three snapshot hooks and their `die` behaviour; that the seven
  removed exports really are gone; `gc` (a stale document plus both its snapshot copies,
  idempotence, never the datacenter document); the perlmod truthiness conversion in all
  six shapes; the `api_*` contract with native hash arguments and native results
  (including that integers, floats, booleans and lists survive the boundary); tag
  selectors on and off, on `access`, on reads and on writes; a read-only scope; the root
  view needing full write; an empty merge creating nothing; the one comment-key rule;
  scopes never reaching the datacenter document; a malformed registration file being
  skipped without disturbing a valid one; `api_list_guests` gating node/name/tags on
  `VM.Audit` and `has` filtering on visible data; the one lint for both a full and a
  scoped caller with no redaction; parse failures (yaml+`parse_error`, 422, root replace
  and root DELETE as the two repairs); the read cap; and digest/dry_run/delete semantics.
* The Debian package `libpve-meta-rs-perl` is the second binary package of the `pve-meta`
  source package; see `crates/pve-meta-perl/PACKAGING.md`.
