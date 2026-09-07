# Packaging notes for `PVE::API2::Meta` (package `pve-meta`)

This directory (`perl/`) holds `PVE::API2::Meta` (`perl/PVE/API2/Meta.pm`), the native
PVE API module described in `docs/NATIVE-API-SPEC.md`. It ships in the `pve-meta`
binary package (the same one as the CLI and UI dist), **not** as its own package and
**not** as part of `libpve-meta-rs-perl` (that package is `PVE::RS::Meta` only, see
`crates/pve-meta-perl/PACKAGING.md`).

Like that file, this one documents what needs to be added to the root `Makefile` /
`debian/` (owned by the coordinator) rather than editing them directly.

## Install target

Add to the root `Makefile`'s `install:` target:

```make
install -d -m755 $(DESTDIR)$(PERL_INSTALLVENDORLIB)/PVE/API2
install -m644 perl/PVE/API2/Meta.pm $(DESTDIR)$(PERL_INSTALLVENDORLIB)/PVE/API2/Meta.pm
```

(`PERL_INSTALLVENDORLIB` is the same `perl -MConfig -e 'print $Config{installvendorlib}'`
variable `crates/pve-meta-perl/Makefile` already defines — e.g.
`/usr/share/perl5` on Debian 13 — reuse it rather than redefining it, if the root
Makefile does not already have it in scope for another reason.)

Final installed path: **`/usr/share/perl5/PVE/API2/Meta.pm`** (matches
`docs/NATIVE-API-SPEC.md`'s "Packaging" section).

## Runtime dependency

`PVE::API2::Meta` does `use PVE::RS::Meta;` — the `pve-meta` package's control file
needs a `Depends: libpve-meta-rs-perl (= ${binary:Version})` (or a loosened version
constraint if the two packages' versions can drift; they are built from the same
source package here, so an exact match is simplest) alongside its existing
`Depends:` line, so the two packages install together. It is otherwise **inert**
until also registered in `PVE::API2` — see the next section — so installing
`pve-meta` alone (without applying `pve-manager-patches/lifecycle/pve-manager_API2.pm.diff`)
is safe and does not risk pveproxy failing to start over a missing module.

## Registration (separate mechanism, not this file's job)

`PVE::API2::Meta` is *installed* by the `pve-meta` package (per above) but *registered*
into the live API tree by the lifecycle-patch tool's `pve-manager_API2.pm.diff`
(`pve-manager-patches/lifecycle/`, applied via `dpkg-divert` + `patch -p1` against
`/usr/share/perl5/PVE/API2.pm`, package `pve-manager`) — see
`pve-manager-patches/lifecycle/API2-ADDITION.md` for exactly what needs to be merged
into that tool. `postinst`/the trigger already re-run that tool on `pve-manager`
upgrades (`interest-noawait /usr/share/perl5/PVE/API2.pm` in `debian/pve-meta.triggers`),
so installing/upgrading `pve-meta` alone does **not** re-apply the API2.pm patch by
itself — only a `pve-meta` postinst run or a `pve-manager` upgrade trigger does. If the
`pve-meta` postinst doesn't already call the lifecycle tool unconditionally on every
`pve-meta` install (it should, to cover a fresh install where `pve-manager` isn't being
upgraded), add that call there too.

## Verified manually (see this agent's own report for the full transcript)

Installed by hand on the lab node (`pvemeta-node1`, PVE 9.2.11) as:
`install -Dm644 perl/PVE/API2/Meta.pm /usr/share/perl5/PVE/API2/Meta.pm`, plus the
`dpkg-divert`+`patch -p1` sequence against `/usr/share/perl5/PVE/API2.pm` described in
`pve-manager-patches/lifecycle/API2-ADDITION.md`, then `systemctl restart pvedaemon
pveproxy`. Confirmed reachable at `/api2/json/meta/...` through the real pveproxy on
port 8006, via cookie+ticket auth, an `Authorization: PVEAPIToken=...` header, and
`pvesh`.
