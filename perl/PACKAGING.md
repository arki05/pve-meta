# Packaging notes for `PVE::API2::Ext::Meta` (package `pve-meta`)

This directory (`perl/`) holds `PVE::API2::Ext::Meta` (`perl/PVE/API2/Ext/Meta.pm`),
the native `/meta/...` API module described in `docs/DESIGN.md` §6. It ships in the
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

## The prefix drop-directories

`docs/DESIGN.md` §3 puts prefixes (what a prefix is) outside the documents, in these
directories:

* `/usr/share/pve-meta/prefixes/` — packaged prefixes, one file per prefix; **the
  file name is the prefix** (`docs/DESIGN.md` §3). The root `Makefile`'s `install:`
  target creates this directory empty: pve-meta ships no live prefix of its own,
  and it is the ecosystem seam an operator's own `.deb` drops a prefix file into.
  `examples/prefixes/traefik.yaml` documents the format instead, installed under
  `/usr/share/doc/pve-meta/examples/`.
* `/etc/pve/meta.d/prefixes/` — cluster-wide overrides on pmxcfs. `debian/pve-meta.postinst`
  creates it (guarded on `/etc/pve/local`, i.e. only on a node that has joined a cluster);
  nothing packages files into it.

A cluster prefix file overrides the packaged file of the same name; a prefix file's own
`nodes:` map overrides its `schema`/`enforce`/`hidden` for a guest on that node, inside
the same file (`docs/DESIGN.md` §3). Both directories are read per request and neither is
required to exist.

## Registration (handled by pve-ext, not this file)

`docs/DESIGN.md` §8: `pve-meta` depends on `pve-ext`. `PVE::API2::Ext` (package
`pve-ext`, loaded via one dpkg-diverted `use PVE::API2::Ext;` line in
`/usr/share/perl5/PVE/API2.pm`) scans `/usr/share/perl5/PVE/API2/Ext/*.pm` at
pvedaemon/pveproxy startup, `require`s each file, and registers it in the API root at
the path its `ext_path` class method declares. `PVE::API2::Ext::Meta::ext_path`
returns `'meta'`, so simply installing this file (above) is enough to reach it at
`/api2/json/meta/...` after a `pvedaemon`/`pveproxy` restart — no patch, no
`dpkg-divert`, no `pve-manager` diff.
