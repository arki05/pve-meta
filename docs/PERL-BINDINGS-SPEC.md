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
`open_store()` (the `$PVE_META_ROOT` lookup, default `/etc/pve/meta`),
`open_prefixes()` (`registry::load_prefixes_default()`) and `open_grants()`
(`registry::load_grants_default()`), and nothing else. Both drop directories are read per
request — they are tiny, pmxcfs caches them, and a stale grant is a wrong answer about
who may write. Every decision — the document model, the views, the prefix and grant
rules, and in particular the write authorization — lives in `pve-meta-core`, where it can
be unit-tested on any machine.

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
datacenter document) and `tags` are that guest's PVE tags, which resolve the grants' and
prefixes' selectors (`DESIGN.md` §3). Rust computes the caller's scopes from it; Perl
never builds a grant list.

## Perl API (`#[perlmod::package(name = "PVE::RS::Meta", lib = "pve_meta_rs")]`)

All functions die with a readable message on error (`anyhow::Error` → Perl `die`).

### Lifecycle hooks

All of them (`DESIGN.md` §6), called from one patched file, `PVE/AbstractConfig.pm`
(package `libpve-guest-common-perl`). They run inside PVE's own guest locks and move
whole files; they do not consult grants.

* `on_snapshot($vmid, $snapname)` → copies the document to the snapshot file; no-op if
  the guest has no document. Returns 1 if a copy was made, 0 otherwise.
* `on_rollback($vmid, $snapname)` → restores the snapshot copy over the current document;
  if no snapshot copy exists but a current document does, the current document is
  removed (the guest had no metadata when the snapshot was taken). Returns a string:
  `restored`, `removed`, `none`.
* `on_delsnap($vmid, $snapname)` → removes the snapshot copy. Returns 1/0.
* `on_create($vmid)` → clears any document **and** snapshot copies left at `$vmid`.
  Returns the number of files removed. Called from `create_and_lock_config`, and **only
  when its `$allow_existing` is false** — that is when the `check_vmid_unused` inside it
  has just asserted the vmid was free, so anything still there is a leftover. A restore
  *over* an existing guest keeps its document: a backup does not carry one, so clearing
  would be data loss.
* `on_destroy($vmid)` → the same purge, called from `destroy_config` after the guest
  config's own `unlink` succeeds. Returns the number of files removed; idempotent.

The two are the same operation with different call sites, and are named separately so a
warning says which path ran. `on_create` is the one a periodic sweep could never be: a
vmid destroyed and recreated between two sweeps is never *missing* from the vmlist, so a
sweep never nominates it and the new guest inherits the old document permanently.

**Removed in revision 5** (`DESIGN.md` §10): `on_clone`, `export_for_backup`,
`import_from_backup`, `list_snapshots`, `has_document` and `api_grants`. Clone and backup
are not carried ("metadata lives in `/etc/pve`; back up `/etc/pve`");
`list_snapshots`/`has_document` existed for the orphan machinery, which is gone with the
orphan concept. Revision 6's `api_grants` is a different function under the same name:
that one returned the *caller's* computed grants for Perl to forward, this one lists the
grant **files** (`DESIGN.md` §3.2).

### Garbage collection (manual only)

With create and destroy hooked, nothing runs this on a timer. It stays as the broom for
the one case the hooks cannot see — a guest config removed out of band — and an
administrator runs `/usr/libexec/pve-meta/gc` by hand.

Rust never reads `/etc/pve/.vmlist` itself; the caller passes the vmlist in, as a native
array ref of integers.

* `gc_candidates($vmids)` → the stale vmids: everything the store holds a file for that
  is not in `$vmids`. Removes nothing.
* `gc_purge($vmid, $live)` → removes `$vmid`'s document **and every snapshot copy**, but
  only if `$live` still does not contain it. Returns the number of files removed, `0` if
  the guest is live again. **Dies if `$live` is empty.**
* `gc($vmids)` → the unvalidated whole sweep: every stale vmid purged in one pass,
  holding no per-vmid lock. Returns the number of files removed.

**Two locks, two phases** — `/usr/libexec/pve-meta/gc` (and any other caller) must use
`gc_candidates` + `gc_purge`, not `gc`. Writes serialize under
`cfs_lock_domain("pve-meta-$vmid")` and a GC pass under `cfs_lock_domain('pve-meta-gc')`,
which are disjoint: a guest destroyed, recreated at the same vmid and given fresh
metadata by a `PUT` that already answered `200` has that document deleted by a sweep
whose vmlist read predates it. So the candidate list only nominates, and each vmid is
purged under **its own write lock**, against a vmlist re-read inside it:

```perl
sub live_vmids {
    PVE::Cluster::cfs_update();
    my $vmlist = PVE::Cluster::get_vmlist() || {};
    return sort { $a <=> $b } keys %{ $vmlist->{ids} // {} };
}

PVE::Cluster::cfs_lock_domain('pve-meta-gc', 30, sub {
    my @vmids = live_vmids();
    die "refusing to gc with an empty vmlist\n" if !@vmids;
    for my $vmid (@{ PVE::RS::Meta::gc_candidates(\@vmids) }) {
        my $n = PVE::Cluster::cfs_lock_domain("pve-meta-$vmid", 30, sub {
            my @fresh = live_vmids();
            die "refusing to gc $vmid with an empty vmlist\n" if !@fresh;
            return PVE::RS::Meta::gc_purge($vmid, \@fresh);
        });
        # Best-effort per candidate: a busy lock must not stop the sweep.
        if (my $err = $@) { syslog('warning', "skipping %d: %s", $vmid, $err); next; }
    }
});
die $@ if $@;
```

Three things in that shape are not optional. `cfs_update()` before every vmlist read: a
fresh process that has not refreshed sees an empty vmlist. The **empty-vmlist guard**,
because an empty vmlist means "every document is stale" — `gc_purge` refuses one too, so
the guard sits on the destructive call and not only on its caller — `gc` (the
whole-sweep form) refuses one too, so no export of this module can be handed an empty
vmlist. And the explicit **`$@` check after each `cfs_lock_domain`**, which catches its
callback's `die` and re-raises it by assigning `$@`: wrapping the call in an `eval {}`
instead clears `$@` on the way out and swallows the failure silently. The outer check
dies (a sweep that cannot take its own lock has done nothing); the inner one warns and
moves to the next candidate, because one document whose write lock is busy must not stop
the sweep from reaching the rest — the next run picks it up.

Nesting `"pve-meta-$vmid"` inside `'pve-meta-gc'` cannot deadlock: a writer only ever
takes the per-document lock, never the GC one, so there is no lock-order cycle. The
datacenter document is never a guest and is never removed.

### Build identification

* `version()` → the crate version string. Used by `test/basic.pl` and available to
  operators for checking which build a running pvedaemon/pveproxy has loaded.

### `api_*` functions

Thin wrappers over `pve_meta_core::api` (see `crates/pve-meta-core/SPEC.md` §10 for the
authorization rules and `DESIGN.md` §5 for the endpoints). They die with
`"NNN: message"` (an HTTP status prefix) which `PVE::API2::Ext::Meta::_call` turns into a
`PVE::Exception`.

* `api_version($detail)` → `{ token, changed }`, plus `documents` (`[{ id, digest }]`,
  sorted) when `$detail` is true.
* `api_prefixes()` → every prefix, as native hashes, **sorted most-specific first**
  — the order that resolves which one governs a path (`DESIGN.md` §3.1):
  `[{ prefix, description?, selector, schema? }]`. The prefix is the file's name; a
  prefix names no principal, so there is no `authid` on it.
* `api_grants()` → every grant, as native hashes:
  `[{ name, authid, description?, grants: [{ prefix, mode, selector }] }]`. Read from
  `/etc/pve/meta.d/grants` only — there is deliberately no packaged grants directory
  (`DESIGN.md` §3.2).
* In both, `selector` is spelled as the file spells it (`{all => 1}` /
  `{tag => '<name>'}`), and a malformed file is skipped with a warning and does not
  appear — independently per directory.
* `api_access($id, $acl)` → `{ read, write, scopes }` for one document, with the grants'
  selectors already resolved against `$acl->{tags}`. `$id` is a vmid or
  `"datacenter"`; scopes are always empty for the latter.
* `api_list_guests($authid, $guests, $has)` → one row per guest the caller can read
  anything of. `$guests` is the array of vmlist rows Perl already has,
  `[{vmid, node, type, name, tags, read, write}]`, as a native array of hashes.
  `node`/`name`/`tags` come back only for guests the caller has `VM.Audit` on.
* `api_get($id, $view, $format, $acl)` → `{ id, view, digest, data | text, parse_error? }`.
  `data` is a native structure. A document whose content could not be recovered — it does
  not parse, it is above the read cap, or it parses to something that is not a mapping —
  answers with the raw `text` plus `parse_error` for `format=yaml` and a full reader, and
  **422** otherwise, including for everyone when the bytes were never read at all
  (`DESIGN.md` §4). Such a document is only ever repairable by a root `replace` or a root
  `DELETE`; anything narrower is a 400 that says so.
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
  directly, sets `PVE_META_ROOT`, `PVE_META_PREFIX_DIRS` and `PVE_META_GRANT_DIRS` to
  temp dirs, and exercises every export: the three snapshot hooks and their `die`
  behaviour; that the removed exports really are gone; `gc` (a stale document plus both
  its snapshot copies,
  idempotence, never the datacenter document) and the two-phase `gc_candidates` +
  `gc_purge` (a document written after the candidate snapshot survives the re-check, an
  empty `$live` is refused, a vmid that really is gone is purged with its snapshots);
  the perlmod truthiness conversion in all
  six shapes; the `api_*` contract with native hash arguments and native results
  (including that integers, floats, booleans and lists survive the boundary);
  `api_prefixes` listing every prefix sorted longest-prefix-first (the file name
  being the prefix, and no `authid` on any of them) and `api_grants` listing a grant by
  its file name with every entry's prefix, mode and native-hash selector, and no schema;
  tag selectors on and off, on `access`, on reads and on writes; a read-only scope; the root
  view needing full write; an empty merge creating nothing; the one comment-key rule;
  scopes never reaching the datacenter document; a malformed file in *either* drop
  directory being skipped without disturbing a valid one, including a prefix file
  whose name is not a valid prefix; `api_list_guests` gating node/name/tags on
  `VM.Audit` and `has` filtering on visible data; the one lint for both a full and a
  scoped caller with no redaction; unrecoverable documents in all three of their causes
  (yaml+`parse_error`, 422, root replace and root DELETE as the two repairs, nothing
  narrower); the read cap, including that an oversized document is listed and polled
  without being read and is repairable against the identity the listing reported; that a
  write changing nothing rewrites nothing and moves neither `token` nor `changed`; that a
  document another caller removed is 404-shaped rather than a 500; and
  digest/dry_run/delete semantics.
* The Debian package `libpve-meta-rs-perl` is the second binary package of the `pve-meta`
  source package; see `crates/pve-meta-perl/PACKAGING.md`.
