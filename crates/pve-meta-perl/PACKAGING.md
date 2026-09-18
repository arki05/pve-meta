# Packaging notes for `libpve-meta-rs-perl` (`PVE::RS::Meta`)

This crate (`crates/pve-meta-perl`, Cargo package `pve-meta-rs`) ships as a **second
Debian binary package**, `libpve-meta-rs-perl`, built from the same `pve-meta` source
package as the main `pve-meta` binary. The root `Makefile`, `debian/control` and
`debian/rules` are owned elsewhere in the tree, so this file records what this crate
needs from them rather than duplicating their contents. How the packages are built,
gated and installed is `docs/BUILD.md`.

## What is wired up

* **`debian/control`**:
  * Added `libperl-dev` to the source stanza's `Build-Depends` (needed by perlmod's
    `build.rs`, which compiles `glue.c` against Perl's `CORE` headers -- the one
    build-time gotcha of perlmod, and why this crate does not build on macOS).
  * Appended a new binary package stanza:

    ```
    Package: libpve-meta-rs-perl
    Architecture: any
    Depends: ${shlibs:Depends}, ${misc:Depends}, ${perl:Depends}
    Description: pve-meta store bindings for PVE (Rust, perlmod)
     PVE::RS::Meta, a Perl binding (via perlmod) to pve-meta's guest metadata
     store: the lifecycle hooks called from libpve-guest-common-perl, the backup
     hooks called from qemu-server and pve-container, and the api_* functions
     behind PVE::API2::Ext::Meta.
    ```

    (`debian/control` is owned by the packaging work, not by this crate; the
    stanza above is what this crate's contents ask for. The lifecycle hooks the
    bindings export — create, destroy, snapshot, rollback, delsnap — and the
    restore side of backup are called from one patched file in
    `libpve-guest-common-perl`; the backup side, `export_for_backup`, from one
    patched file each in `qemu-server` and `pve-container`.)

* **Root `Makefile`**:
  * `build:` runs `$(MAKE) -C crates/pve-meta-perl BUILD_MODE=release`
    (builds `PVE/RS/Meta.pm` + `Proxmox/Lib/PVEMeta.pm` via `genpackage.pl` and
    `cargo build --release -p pve-meta-rs`) — after its `wasm` prerequisite, which
    builds `crates/pve-meta-wasm` for `wasm32-unknown-unknown`. That target has to
    be installed on the build host (`rustup target add wasm32-unknown-unknown`; see
    `docs/BUILD.md`) or `make build` fails before this crate is reached. The `.wasm`
    is the **`pve-meta`** package's file, not this one's — it ships as
    `/usr/share/pve-manager/js/pve-meta-extjs/pve-meta-core.wasm` next to the editor
    — but the two share one `cargo`, so the target is a build-host requirement of
    the source package as a whole.
* **`crates/pve-meta-perl`'s own `install:` target** (invoked by `debian/rules` with its
  own `DESTDIR`, see below -- *not* from the root `Makefile`'s `install:`, which builds
  only the `pve-meta` package's tree) installs:
    * `$(DESTDIR)$(PERL_INSTALLVENDORARCH)/auto/libpve_meta_rs.so`
    * `$(DESTDIR)$(PERL_INSTALLVENDORLIB)/PVE/RS/Meta.pm`
    * `$(DESTDIR)$(PERL_INSTALLVENDORLIB)/Proxmox/Lib/PVEMeta.pm`

    (`PERL_INSTALLVENDORARCH`/`PERL_INSTALLVENDORLIB` come from `perl -MConfig`, e.g.
    `/usr/lib/x86_64-linux-gnu/perl5/5.40/auto` and `/usr/share/perl5` on Debian 13.)

## Two binary packages from one source: how the split is done

`debian/rules` implements **option 1**, a per-package install override, and says so by
name:

```make
override_dh_auto_build:
	$(MAKE) build ui

override_dh_auto_install:
	$(MAKE) install DESTDIR=$(CURDIR)/debian/pve-meta PREFIX=/usr
	$(MAKE) -C crates/pve-meta-perl install DESTDIR=$(CURDIR)/debian/libpve-meta-rs-perl
```

The root `Makefile`'s own `install:` target deliberately does **not** invoke this crate's
`install:`, so the perl-binding files land in `debian/libpve-meta-rs-perl/` only and never
in the `pve-meta` package's tree as well. `override_dh_auto_build` runs `make build`,
whose first line is `$(MAKE) -C crates/pve-meta-perl BUILD_MODE=release`, so the cdylib and
the generated `.pm` files exist before `dh_auto_install` runs.

`override_dh_auto_test` in `debian/rules` skips Rust tests during the packaging build
(no guaranteed network access under `dpkg-buildpackage -d`); `PVE::RS::Meta`'s Perl test
suite (`crates/pve-meta-perl/test/basic.pl`, run via `make check` inside the crate) is
likewise not wired into `dh_auto_test` and stays a manual/CI step, matching upstream
`pve-rs`.

## Cargo workspace

`crates/pve-meta-perl` (package `pve-meta-rs`) is a member of the root workspace but
excluded from `[workspace] default-members` (see root `Cargo.toml`), since it only
builds where `libperl-dev` + a `perl` interpreter are installed (confirmed: it does
**not** build on macOS, which lacks Perl's `CORE` headers). `cargo build`/`cargo test`
run bare at the workspace root skip it; `cargo build -p pve-meta-rs` /
`cargo test -p pve-meta-rs` (or an explicit `--workspace`) still target it explicitly.

`perlmod` is not on crates.io, so this crate depends on it via git, pinned to the
commit upstream `pve-rs` 0.15.3 uses:

```toml
perlmod = { git = "https://git.proxmox.com/git/perlmod.git", rev = "d85d4ebdd13c1dcb469e15eb0ce7b22640418b8e", features = ["exporter"] }
```

If `libpve-meta-rs-perl` is ever built the "Debian way" (via `dh-cargo`/debcargo,
`librust-*-dev` packages), this git dependency would need to become a path/vendored
dependency instead -- not done here since the workspace's other crates
(`pve-meta-core`, `pve-meta-wasm`) are also built via a plain rustup + `cargo build`, not
`dh-cargo`, and `debian/control`'s `Build-Depends` confirms that's the project's chosen
build model. (The wasm32 target would then be `libstd-rust-dev-wasm32` in
`Build-Depends`; with rustup it is a `rustup target add`, which is why it is not there.)
