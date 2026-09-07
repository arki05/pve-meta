# libpve-meta-rs-perl — `PVE::RS::Meta` specification

Rust cdylib exposing the store's lifecycle operations to PVE's Perl code through
`perlmod`, packaged exactly like upstream `libpve-rs-perl`. Reference guide with
file:line citations: the research report
`/private/tmp/claude-501/-Users-arki-Documents-proxmox/afff609e-fb1e-4d06-8a92-09f2b57a5c90/scratchpad/research-perlmod.md`
(perlmod macros, genpackage.pl, Makefile/debian layout, gotchas such as `libperl-dev`).
Upstream sources: `.../scratchpad/upstream/perlmod/` and `.../proxmox-perl-rs/pve-rs/`.
The Perl call sites that will invoke these functions are planned in
`docs/LIFECYCLE-PATCHES.md` (read it if it exists; the function signatures below are the
contract either way).

## Crate

`crates/pve-meta-perl` (package name `pve-meta-rs`, `[lib] crate-type = ["cdylib"]` →
`libpve_meta_rs.so`), edition 2021, dependencies: `pve-meta-core` (path), `anyhow`,
`perlmod = { version = "0.14", features = ["exporter"] }` (crates.io has it; if
resolution fails use git `https://git.proxmox.com/git/perlmod.git` pinned to
`d85d4ebdd13c1dcb469e15eb0ce7b22640418b8e`), `serde`, `serde_json`. It is a member of
the root workspace but must be **excluded from the default `cargo test --workspace`
on macOS** if it does not build there (perl headers) — gate with a cargo feature or
just document that it builds on the Linux host only; the Linux build host has
`libperl-dev` installed.

## Perl API (`#[perlmod::package(name = "PVE::RS::Meta", lib = "pve_meta_rs")]`)

All functions take the vmid as an integer and die with a readable message on error
(`anyhow::Error` → Perl `die`). The store root defaults to `/etc/pve/meta` and can be
overridden with the env var `PVE_META_ROOT` (used by tests).

* `on_snapshot($vmid, $snapname)` → copies the document to the snapshot file; no-op if
  the guest has no document. Returns 1 if a copy was made, 0 otherwise.
* `on_rollback($vmid, $snapname)` → restores the snapshot copy over the current document;
  if no snapshot copy exists but a current document does, the current document is
  removed (the guest had no metadata when the snapshot was taken). Returns a string:
  `restored`, `removed`, `none`.
* `on_delsnap($vmid, $snapname)` → removes the snapshot copy. Returns 1/0.
* `on_clone($vmid, $newid)` → copies the document (not the snapshots) to the new vmid;
  dies if the target document already exists. Returns 1/0.
* `on_destroy($vmid)` → removes the document and all its snapshot copies. Returns the
  number of files removed.
* `export_for_backup($vmid)` → `undef` if no document, else a string
  `"#pve-meta-format: <ext>\n" + raw text`. (The header line lets restore pick the
  extension; it is stripped on import. For TOML/YAML/JSON a leading `#` line is a
  comment anyway.)
* `import_from_backup($vmid, $string)` → parses the header, writes the document with that
  format (replacing any existing document for that vmid), validates it through the core
  parser/lint first (dies on invalid content). Returns 1.
* `list_snapshots($vmid)` → array ref of snapshot names.
* `has_document($vmid)` → 1/0.
* `version()` → the crate version string (for the health endpoint / `pve-meta health`).

## Build & packaging

* `crates/pve-meta-perl/Makefile` modelled on `pve-rs/Makefile`: generates
  `PVE/RS/Meta.pm` and `Proxmox/Lib/PVEMeta.pm` with
  `perl <perlmod-checkout>/perlmod-bin/genpackage.pl --lib=pve_meta_rs --lib-tag=pvemeta --lib-package=Proxmox::Lib::PVEMeta --lib-prefix=PVEMeta PVE::RS::Meta`
  (vendor `genpackage.pl` into `crates/pve-meta-perl/genpackage.pl` — it is a
  single Perl script, license per the perlmod repo — so the build needs no perlmod-bin
  package), builds the cdylib with cargo, and has an `install` target placing
  `libpve_meta_rs.so` in `$(DESTDIR)$(PERL_INSTALLVENDORARCH)/auto/` and the `.pm` files
  in `$(DESTDIR)$(PERL_INSTALLVENDORLIB)/…` (paths from `perl -MConfig`).
* Perl-side test: `crates/pve-meta-perl/test/basic.pl` run by `make check`: uses the
  sed-patched loader trick from the research report so it loads
  `target/release/libpve_meta_rs.so` directly, sets `PVE_META_ROOT` to a temp dir,
  writes a YAML document there, and exercises every function including
  export/import round trip and error → `die` behaviour (`eval {}` + `$@` checks).
* The Debian package `libpve-meta-rs-perl` is produced by the root `debian/` of the
  pve-meta source package as a second binary package (add it to `debian/control`:
  `Package: libpve-meta-rs-perl`, `Architecture: any`, `Depends: ${shlibs:Depends},
  ${misc:Depends}, ${perl:Depends}`, description "pve-meta guest lifecycle hooks for
  PVE (Rust, perlmod)"), installed by the root Makefile's `install` target calling
  `$(MAKE) -C crates/pve-meta-perl install`. Coordinate: the root Makefile/debian files
  may be owned by another agent — if they exist, edit minimally and re-read before
  editing; if they do not exist yet, create only the parts you need and say so.
