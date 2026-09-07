# Packaging notes for `libpve-meta-rs-perl` (`PVE::RS::Meta`)

This crate (`crates/pve-meta-perl`, Cargo package `pve-meta-rs`) ships as a **second
Debian binary package**, `libpve-meta-rs-perl`, built from the same `pve-meta` source
package as the main `pve-meta` binary. The root `Makefile` and `debian/control` already
exist (owned by the coordinator/another agent), so this file documents exactly what was
added there and what is still outstanding, rather than duplicating full file contents.

## What was already applied (minimal, additive edits)

* **`debian/control`**:
  * Added `libperl-dev` to the source stanza's `Build-Depends` (needed by perlmod's
    `build.rs`, which compiles `glue.c` against Perl's `CORE` headers -- see
    `docs/PERL-BINDINGS-SPEC.md` / the research report's "biggest gotcha").
  * Appended a new binary package stanza:

    ```
    Package: libpve-meta-rs-perl
    Architecture: any
    Depends: ${shlibs:Depends}, ${misc:Depends}, ${perl:Depends}
    Description: pve-meta guest lifecycle hooks for PVE (Rust, perlmod)
     PVE::RS::Meta, a Perl binding (via perlmod) to pve-meta's guest metadata
     store, exposing snapshot/rollback/delsnap/clone/destroy lifecycle hooks and
     vzdump backup/restore export/import for use from PVE's own Perl code
     (pve-container, qemu-server, pve-guest-common).
    ```

* **Root `Makefile`**:
  * `build:` now also runs `$(MAKE) -C crates/pve-meta-perl BUILD_MODE=release`
    (builds `PVE/RS/Meta.pm` + `Proxmox/Lib/PVEMeta.pm` via `genpackage.pl` and
    `cargo build --release -p pve-meta-rs`).
  * `install:` now also runs `$(MAKE) -C crates/pve-meta-perl install DESTDIR=$(DESTDIR)`,
    which installs:
    * `$(DESTDIR)$(PERL_INSTALLVENDORARCH)/auto/libpve_meta_rs.so`
    * `$(DESTDIR)$(PERL_INSTALLVENDORLIB)/PVE/RS/Meta.pm`
    * `$(DESTDIR)$(PERL_INSTALLVENDORLIB)/Proxmox/Lib/PVEMeta.pm`

    (`PERL_INSTALLVENDORARCH`/`PERL_INSTALLVENDORLIB` come from `perl -MConfig`, e.g.
    `/usr/lib/x86_64-linux-gnu/perl5/5.40/auto` and `/usr/share/perl5` on Debian 13.)

## What is still outstanding (needs a coordinator decision)

`debian/rules` currently builds a **single** binary package:

```make
override_dh_auto_build:
	$(MAKE) build ui

override_dh_auto_install:
	$(MAKE) install DESTDIR=$(CURDIR)/debian/pve-meta PREFIX=/usr
```

With the `install:` edit above, this puts the perl-binding files inside
`debian/pve-meta/...` too -- i.e. **still one package** on disk, even though
`debian/control` now declares two. To actually split them into
`debian/pve-meta/` vs. `debian/libpve-meta-rs-perl/`, pick one:

1. **Per-package install override** (smallest diff): keep `override_dh_auto_install`
   installing the main package as-is, and add a second install call for the perl
   binding with its own `DESTDIR`:

   ```make
   override_dh_auto_install:
   	$(MAKE) install DESTDIR=$(CURDIR)/debian/pve-meta PREFIX=/usr
   	$(MAKE) -C crates/pve-meta-perl install DESTDIR=$(CURDIR)/debian/libpve-meta-rs-perl
   ```

   and then remove the `$(MAKE) -C crates/pve-meta-perl install ...` line from the root
   Makefile's own `install:` target (added above) so it isn't installed into
   `debian/pve-meta` *as well* -- or leave both and add `debian/pve-meta.install`
   (see option 2) to explicitly exclude the perl-binding paths from the `pve-meta`
   package's file list. Option 1 alone (without a `.install` file) is simplest and
   sufficient as long as `debian/pve-meta`'s `install:` invocation no longer also
   installs the perl bits into the same tree.

2. **`debian/tmp` + per-package `.install` files** (more idiomatic dh, bigger diff):
   change `override_dh_auto_install` to install everything once into
   `$(CURDIR)/debian/tmp`, then add `debian/pve-meta.install` and
   `debian/libpve-meta-rs-perl.install` files listing which paths under
   `debian/tmp` go to which package. Standard debhelper multi-binary pattern, but
   touches the main package's install path too.

Either approach also needs `override_dh_auto_build` to actually build this crate
before `dh_auto_install` runs (already covered if the root Makefile's `build:` target
change above is kept, since `dh_auto_build` calls `$(MAKE) build ui`).

`override_dh_auto_test` in `debian/rules` explicitly skips Rust tests during the
packaging build already (pre-existing comment: no guaranteed network access with
`dpkg-buildpackage -d`); `PVE::RS::Meta`'s Perl test suite
(`crates/pve-meta-perl/test/basic.pl`, run via `make check` inside the crate) is
likewise **not** wired into `dh_auto_test` and should stay a manual/CI step, matching
upstream `pve-rs` (its own `test/README` notes the same: Perl tests are not run during
the `.deb` build).

## Cargo workspace

`crates/pve-meta-perl` (package `pve-meta-rs`) is a member of the root workspace but
excluded from `[workspace] default-members` (see root `Cargo.toml`), since it only
builds where `libperl-dev` + a `perl` interpreter are installed (confirmed: it does
**not** build on macOS, which lacks Perl's `CORE` headers). `cargo build`/`cargo test`
run bare at the workspace root skip it; `cargo build -p pve-meta-rs` /
`cargo test -p pve-meta-rs` (or an explicit `--workspace`) still target it explicitly.

`perlmod` is **not currently on crates.io** (confirmed 2026-09-07: the crates.io API
returns 404 for it, despite some documentation suggesting otherwise) -- this crate
depends on it via git, pinned to the commit named in
`docs/PERL-BINDINGS-SPEC.md`/the research report:

```toml
perlmod = { git = "https://git.proxmox.com/git/perlmod.git", rev = "d85d4ebdd13c1dcb469e15eb0ce7b22640418b8e", features = ["exporter"] }
```

If `libpve-meta-rs-perl` is ever built the "Debian way" (via `dh-cargo`/debcargo,
`librust-*-dev` packages), this git dependency would need to become a path/vendored
dependency instead -- not done here since the rest of this workspace's crates (see
`crates/pve-meta-cli/Cargo.toml`, `crates/pve-meta-core/Cargo.toml`) are also built via
a plain rustup + `cargo build`, not `dh-cargo`, and `debian/control`'s existing
`Build-Depends` (just `debhelper-compat (= 13)`) confirms that's the project's chosen
build model.
