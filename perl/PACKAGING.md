# Packaging notes for `PVE::API2::Ext::Meta` (package `pve-meta`)

This directory (`perl/`) holds `PVE::API2::Ext::Meta` (`perl/PVE/API2/Ext/Meta.pm`),
the native `/meta/...` API module described in `docs/DESIGN.md` §5. It ships in the
`pve-meta` binary package (the same one as the UI dist), **not** as its own package and
**not** as part of `libpve-meta-rs-perl` (that package is `PVE::RS::Meta` only, see
`crates/pve-meta-perl/PACKAGING.md`).

## Install target

Already wired into the root `Makefile`'s `install:` target — nothing to add:

```make
install -D -m 0644 perl/PVE/API2/Ext/Meta.pm $(DESTDIR)$(PREFIX)/share/perl5/PVE/API2/Ext/Meta.pm
```

Final installed path: **`/usr/share/perl5/PVE/API2/Ext/Meta.pm`**.

## Runtime dependency

`PVE::API2::Ext::Meta` does `use PVE::RS::Meta;` — the `pve-meta` package's control
file needs a `Depends: libpve-meta-rs-perl (= ${binary:Version})` (or a loosened
version constraint if the two packages' versions can drift; they are built from the
same source package here, so an exact match is simplest) alongside its existing
`Depends:` line, so the two packages install together.

## The prefix and grant drop-directories

`docs/DESIGN.md` §3 puts prefixes (what a prefix is) and permissions (who may touch one)
outside the documents, in three directories:

* `/usr/share/pve-meta/prefixes/` — packaged prefixes, one file per prefix; **the
  file name is the prefix** (`docs/DESIGN.md` §3.1). The root `Makefile`'s `install:`
  target already creates this directory and copies `prefixes/*.yaml` into it, tolerating
  an empty checkout (a missing directory is not an error). This repository's
  `prefixes/traefik.yaml` documents the format rather than granting anything — a
  prefix names no principal at all — so packaging that one file is optional, but the
  directory itself is the ecosystem seam: an operator's own `.deb` drops its prefix
  file in here.
* `/etc/pve/meta.d/prefixes/` — cluster-wide overrides on pmxcfs. `debian/pve-meta.postinst`
  creates it (guarded on `/etc/pve/local`, i.e. only on a node that has joined a cluster);
  nothing packages files into it.
* `/etc/pve/meta.d/permissions/` — permissions, created by the same postinst. **Cluster-only: there
  is deliberately no packaged permissions directory** (`docs/DESIGN.md` §3.2). An operator's
  `.deb` may ship a prefix, which is a declaration, but must never ship its own grant,
  which would be self-registration; dpkg cannot write into pmxcfs, so that rule is
  enforced by where the files live rather than by a check.

A cluster prefix file overrides the packaged file of the same name; all of these are
read per request and none is required to exist.

## Registration (handled by pve-ext, not this file)

`docs/DESIGN.md` §7: `pve-meta` depends on `pve-ext`. `PVE::API2::Ext` (package
`pve-ext`, loaded via one dpkg-diverted `use PVE::API2::Ext;` line in
`/usr/share/perl5/PVE/API2.pm`) scans `/usr/share/perl5/PVE/API2/Ext/*.pm` at
pvedaemon/pveproxy startup, `require`s each file, and registers it in the API root at
the path its `ext_path` class method declares. `PVE::API2::Ext::Meta::ext_path`
returns `'meta'`, so simply installing this file (above) is enough to reach it at
`/api2/json/meta/...` after a `pvedaemon`/`pveproxy` restart — no patch, no
`dpkg-divert`, no `pve-manager` diff, unlike the revision-3 design this superseded.

## Verified manually

Installed by hand on the lab node (`pvemeta-node1`, PVE 9.2.11) as
`cp perl/PVE/API2/Ext/Meta.pm /usr/share/perl5/PVE/API2/Ext/Meta.pm` (that directory,
and `pve-ext`'s `use PVE::API2::Ext;` line in a dpkg-diverted `PVE/API2.pm`, already
existed from a separate `pve-ext` install), then `systemctl restart pvedaemon
pveproxy`. `GET /api2/json/ext/modules` confirmed `PVE::API2::Ext::Meta` auto-loaded
at path `meta`; the full `/meta/...` tree (version, access, guests, datacenter, and the
single registration listing revision 6 has since split into `prefixes` and `permissions`)
was then exercised through the real pveproxy on port 8006 via
`Authorization: PVEAPIToken=...` headers for both a full-access principal and a scoped
one -- including the tag selector on and off, merge/replace/delete semantics, digest
409, `dry_run`, an unparsable document and the GC (revision 5, 2026-09-08).
