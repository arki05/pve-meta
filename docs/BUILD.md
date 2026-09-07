# Building pve-meta

## Toolchain

Development and CI happen on a Debian 13 (trixie) host with a [rustup](https://rustup.rs/)
toolchain installed under `~/.cargo/bin` (rustc 1.98 at the time of writing) — **not** the
`cargo`/`rustc` Debian packages. `pve-meta-api`/`pve-metad`/`pve-meta-cli` depend on
`proxmox-sys`/`openssl`-linking crates that do not build on macOS; only `pve-meta-core` compiles
there. Use the Linux build host for anything else:

```sh
rsync -az --exclude target --exclude .git --exclude dist ./ pve-meta-build:/root/pve-meta/
ssh pve-meta-build 'export PATH=$HOME/.cargo/bin:$PATH; cd /root/pve-meta && cargo build -p pve-metad -p pve-meta-cli --release'
```

## Plain (non-packaged) build

```sh
make build   # cargo build --release -p pve-metad -p pve-meta-cli
make ui      # trunk build in ui/ (falls back to a placeholder if trunk is not installed)
make install DESTDIR=/some/root PREFIX=/usr
```

## Debian package

The `.deb` is built with `dpkg-buildpackage`, but **without** relying on Debian's own
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

This produces `../pve-meta_<version>_<arch>.deb` (dpkg-buildpackage places the artifact in the
parent directory). Install it with `dpkg -i ../pve-meta_*.deb`; `dh_installsystemd` enables and
starts `pve-metad.service` automatically (it's a `Type=notify` unit, `After=pve-cluster.service`
since `/etc/pve` is a pmxcfs FUSE mount that must already be up).

## Smoke test

`scripts/smoke.sh` exercises a running daemon over HTTP(S) with `curl`. See that script's header
comment for the required environment variables (`PVE_META_URL` plus either a ticket/CSRF pair or
an API token).
