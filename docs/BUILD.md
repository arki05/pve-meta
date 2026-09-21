# Building pve-meta

## Toolchain

Development and CI happen on a Debian 13 (trixie) host with a [rustup](https://rustup.rs/)
toolchain installed under `~/.cargo/bin` (the version `rust-toolchain.toml` pins, which
rustup installs on first use) — **not** the
`cargo`/`rustc` Debian packages. `pve-meta-perl` (the perlmod bindings) needs `libperl-dev`
headers and only builds on Linux; `pve-meta-core` and `pve-meta-wasm` are pure Rust and
also compile on macOS. Use the Linux build host for anything else:

```sh
rsync -az --exclude target --exclude .git --exclude dist ./ pve-meta-build:/root/pve-meta/
ssh pve-meta-build 'export PATH=$HOME/.cargo/bin:$PATH; cd /root/pve-meta && cargo build -p pve-meta-rs --release'
```

### The `wasm32-unknown-unknown` target

The editor's rules are `pve-meta-core` compiled for the browser (`crates/pve-meta-wasm`,
`docs/WASM-CORE.md`), and `make build` and `make check` both depend on `make wasm`, so
the toolchain needs the wasm32 standard library as well as the host's. rustup's
`--profile minimal` (and a fresh `rustup-init`) installs the host target only, and building
for a target that is not installed is a hard error, not a skip:

```
error[E0463]: can't find crate for `core`
  = note: the `wasm32-unknown-unknown` target may not be installed
```

Install it once:

```sh
rustup target add wasm32-unknown-unknown          # rustup toolchain (the build host, CI)
apt install libstd-rust-dev-wasm32                # a distro rustc instead, if you must
```

That is the whole requirement. The `.wasm` is a plain `cargo build --target
wasm32-unknown-unknown --profile wasm` (the `[profile.wasm]` in the root `Cargo.toml`)
behind a hand-written five-export ABI: no `wasm-bindgen`, no `wasm-pack`, no `wasm-opt`,
no npm, nothing whose version has to match the crate's. `.github/workflows/build.yml`
adds the target right after installing rustup; `docs/WASM-CORE.md` records the ABI.

## Plain (non-packaged) build

```sh
make wasm    # target/wasm32-unknown-unknown/wasm/pve_meta_wasm.wasm, the editor's core
make build   # make wasm, then crates/pve-meta-perl in release mode
make ui      # fetches Monaco into ui-extjs/monaco/vs (skipped, with a warning, without npm)
make install DESTDIR=/some/root PREFIX=/usr
```

`make install` ships the `.wasm` as
`/usr/share/pve-manager/js/pve-meta-extjs/pve-meta-core.wasm`, next to the editor that
loads it, and fails if it is missing — unlike Monaco, it is not optional: an editor without
its core cannot read a document. `make check` runs clippy, rustdoc and, when `node` is
present, `ui-extjs/testing/smoke.js`, which instantiates that same built `.wasm`.

## The gates

Nothing is wired into `dh_auto_test` (`debian/rules` skips it: a packaging build has
no guaranteed network for crates.io), so these run by hand and in CI, before the
packages are built:

```sh
make test                                        # every pve-meta-core, pve-meta-wasm and pve-meta-guest-files test; runs on macOS
cargo test -p pve-meta-core                      # the core crate alone
make check                                       # rustdoc -D warnings, clippy, make wasm, the smoke suite
node ui-extjs/testing/smoke.js                   # the editor's offline suite against the built .wasm (after make wasm)
make -C crates/pve-meta-perl check               # test/basic.pl over the built .so (Linux only)
```

The Rust suites take their paths from the environment — `PVE_META_ROOT` for the store,
`PVE_META_PREFIX_DIRS` (colon-separated, lowest precedence first) for the prefix
registry directory — and `test/basic.pl` sets both to temp dirs, so nothing here
touches `/etc/pve`. A store rooted under `/etc/pve` refuses every operation unless
`/etc/pve/local` is a symlink (pmxcfs is mounted); `PVE_META_CLUSTER_MARKER` names the
symlink to check instead, for any root, and set to nothing checks none. `test/basic.pl`
points it at a temp path to exercise the refusal; the Rust suites use
`MetaStore::with_cluster_marker`.

## Debian package

The second binary package, `libpve-meta-rs-perl`, keeps its own notes in
`crates/pve-meta-perl/PACKAGING.md`: what it needs from `debian/control` and
`debian/rules`, and how the two packages' files are kept apart. This section is how the
packages are built and installed.

The `.deb`s are built with `dpkg-buildpackage`, but **without** relying on Debian's own
`cargo`/`rustc` packages — `debian/rules` calls `make`, and the `Makefile` resolves `cargo` as
`~/.cargo/bin/cargo` when present (the rustup toolchain), falling back to a plain `cargo` on
`$PATH` otherwise. Because `debian/control`'s `Build-Depends` intentionally does **not** list
`cargo`/`rustc` (they would resolve to a toolchain we don't want to build with) — nor,
for the same reason, the wasm32 target, which on the rustup toolchain is a `rustup target
add` and not a package — build with `-d` to skip the build-dependency check:

```sh
# one-time, if missing:
sudo apt install dpkg-dev debhelper

make deb   # builds pve-ext (make -C pve-ext deb) and this source's own three
           # binaries (dpkg-buildpackage -b -us -uc -d), and collects every
           # artifact in the parent directory
```

This produces four binary packages: `../pve-ext_<version>_all.deb`,
`../pve-meta_<version>_all.deb`, `../libpve-meta-rs-perl_<version>_<arch>.deb` and
`../pve-meta-guest-files_<version>_<arch>.deb` (plus `-dbgsym` packages for the two compiled
ones; dpkg-buildpackage places artifacts in the
parent directory, and the root `deb` target moves pve-ext's own output there too, since
a plain `dpkg-buildpackage` run from `pve-ext/` would otherwise drop them one level
short, in this repo's own top directory). `pve-meta` depends on `pve-ext`, so install (or
upgrade) it first — apt resolves the order for you either way:

```sh
apt install ../pve-ext_*.deb ../libpve-meta-rs-perl_*.deb ../pve-meta_*.deb
# or: dpkg -i ../pve-ext_*.deb ../libpve-meta-rs-perl_*.deb ../pve-meta_*.deb && apt-get -f install
apt install ../pve-meta-guest-files_*.deb    # optional: the guest-files daemon (docs/DESIGN.md §8)
```

`libpve-meta-rs-perl` ships `activate-noawait pve-api-updates`
(`debian/libpve-meta-rs-perl.triggers`, same as upstream `libpve-rs-perl`), so `pve-manager`
restarts pvedaemon/pveproxy after the `.so` changes and the daemons actually pick the new
library up.

## Installing onto a live node

**Never write through the installed `libpve_meta_rs.so`.** Every running pveproxy/pvedaemon
has that file `mmap`ed (`Proxmox/Lib/PVEMeta.pm` → `DynaLoader::dl_load_file`), and its text
pages are file-backed and never copied on write, so rewriting the bytes of that inode replaces
the code inside every running daemon. Nothing calls into the library after startup, so the
daemons keep working — until they exit, when ld.so's `_dl_fini` runs the library's
`DT_FINI_ARRAY` destructor (the only code of ours that runs at shutdown) out of a different
build and the process dies:

```
pveproxy worker[21973]: segfault at bf ip ... error 6 in libpve_meta_rs.so[...]
pvedaemon.service: Main process exited, code=killed, status=11/SEGV
```

`dpkg -i` (unpacks to `.dpkg-new`, then renames) and `make install` (installs to a temporary
name and `mv`s it into place) are both safe. A hand-rolled deploy is only safe if it replaces
the destination inode too:

```sh
# safe: new inode, running daemons keep the old one until they exit
scp libpve_meta_rs.so node1:/tmp/
ssh node1 'mv -f /tmp/libpve_meta_rs.so \
    /usr/lib/x86_64-linux-gnu/perl5/5.40/auto/libpve_meta_rs.so && \
    systemctl restart pvedaemon pveproxy'

# UNSAFE: writes through the existing inode -> SIGSEGV in every daemon at exit
scp libpve_meta_rs.so node1:/usr/lib/x86_64-linux-gnu/perl5/5.40/auto/
cp libpve_meta_rs.so /usr/lib/x86_64-linux-gnu/perl5/5.40/auto/
rsync --inplace ...
```

`rsync` without `--inplace` and GNU `install`(1) (which unlinks the destination first) are
also safe.

Note on reloads: `deb-systemd-invoke reload-or-try-restart pveproxy.service` re-executes
the master (it maps the new library immediately) and re-forks workers, but workers
that are serving a request drain first and keep the old library mapped for a few
seconds. Wait until `pgrep -f 'pveproxy worker'` shows only new PIDs before testing a
freshly installed `.so`; `systemctl restart` skips the drain.
