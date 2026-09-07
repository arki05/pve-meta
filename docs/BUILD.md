# Building pve-meta

## Toolchain

Development and CI happen on a Debian 13 (trixie) host with a [rustup](https://rustup.rs/)
toolchain installed under `~/.cargo/bin` (rustc 1.98 at the time of writing) — **not** the
`cargo`/`rustc` Debian packages. `pve-meta-perl` (the perlmod bindings) needs `libperl-dev`
headers and only builds on Linux; `pve-meta-core` is pure Rust and also compiles on macOS.
Use the Linux build host for anything else:

```sh
rsync -az --exclude target --exclude .git --exclude dist ./ pve-meta-build:/root/pve-meta/
ssh pve-meta-build 'export PATH=$HOME/.cargo/bin:$PATH; cd /root/pve-meta && cargo build -p pve-meta-rs --release'
```

## Plain (non-packaged) build

```sh
make build   # builds crates/pve-meta-perl in release mode
make ui      # trunk build in ui/ (falls back to a placeholder if trunk is not installed)
make install DESTDIR=/some/root PREFIX=/usr
```

## Debian package

The `.deb`s are built with `dpkg-buildpackage`, but **without** relying on Debian's own
`cargo`/`rustc` packages — `debian/rules` calls `make`, and the `Makefile` resolves `cargo` as
`~/.cargo/bin/cargo` when present (the rustup toolchain), falling back to a plain `cargo` on
`$PATH` otherwise. Because `debian/control`'s `Build-Depends` intentionally does **not** list
`cargo`/`rustc` (they would resolve to a toolchain we don't want to build with), build with `-d`
to skip the build-dependency check:

```sh
# one-time, if missing:
sudo apt install dpkg-dev debhelper

make deb   # == dpkg-buildpackage -b -us -uc -d
```

This produces two binary packages, `../pve-meta_<version>_<arch>.deb` and
`../libpve-meta-rs-perl_<version>_<arch>.deb` (dpkg-buildpackage places the artifacts in the
parent directory). Install both with `dpkg -i ../pve-meta_*.deb ../libpve-meta-rs-perl_*.deb`.

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
