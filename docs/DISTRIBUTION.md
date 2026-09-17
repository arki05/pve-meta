# Distribution: releases and the apt repository

How packages get from a tag to a host running `apt update`, and how an admin finds out
if a patched upstream package no longer matches our patch.

## 1. Releases

`.github/workflows/build.yml` builds and tests every push on **amd64 and arm64** --
native builds in a `debian:trixie` container on each architecture, because
`libpve-meta-rs-perl` is a compiled Perl module and each architecture builds its own;
`pve-meta-publish` is a compiled binary too. The two `Architecture: all` packages
(`pve-meta`, `pve-ext`) are taken from the amd64 job alone, so one file name never means
two different files.

A tag `v<version>` runs the same build and then publishes:

1. The tag must equal `debian/changelog`'s upstream version (`v0.1.0` for `0.1.0-1`;
   `debian/rules` already checks that Cargo.toml agrees). A mismatch fails the release.
2. The `.deb`s become the assets of a GitHub Release for the tag. **A file name that
   already exists in an earlier release is never uploaded again**: `pve-ext` keeps its
   own version and is rebuilt by every release, but only a new pve-ext version is
   published. `gh release upload` runs without `--clobber`, so an asset already on the
   tag is left alone too.
3. The workflow asks `apt.arki05.com` to index the release now rather than at its
   nightly run, through a `workflow_dispatch` on that repository. This is the one
   secret this repository needs, and it is optional: `APT_REPO_DISPATCH_TOKEN`, a
   fine-grained PAT scoped to `arki05/apt.arki05.com` with **Actions: read and write**
   and nothing else. `workflow_dispatch` rather than `repository_dispatch`, because the
   latter needs **Contents: write**, which is push access to that repository's code.
   Without the token the release is picked up by the nightly run.

To cut a release: bump `[workspace.package] version` in `Cargo.toml` and add a
`debian/changelog` entry with the same upstream version (`dch -v 0.2.0-1`); bump
`pve-ext/debian/changelog` only if pve-ext changed; commit; `git tag v0.2.0`; push the
tag.

## 2. The apt repository

`apt.arki05.com` (repository `arki05/apt.arki05.com`) is a signed, static Debian
repository on Cloudflare R2, shared by every package under that account. It holds the
R2 and GPG credentials; package repositories hold none. Its workflow **pulls**: it lists
every GitHub Release of every repository in its `packages.txt` (`arki05/pve-meta` is
one), downloads the `.deb` assets, lays out the pool, builds indices for amd64 and
arm64 with `dpkg-scanpackages --multiversion` (old versions stay installable), signs
`Release`, and syncs to the bucket -- pool first and never deleted, indices next,
signed `Release` last.

**Published files are immutable.** The apt repository commits the SHA-256 of every
`.deb` it has ever published to its own `manifest.sha256`. On every run it re-hashes
what it fetches: a known file that hashes the same is left alone, an unknown file is
recorded and published, and a known file that hashes **differently fails the run** --
something rewrote a release asset after publication, and the good copy in the bucket
is neither overwritten nor re-signed. The release workflow's "never upload an
existing file name" rule above exists so that a rebuild can never trip this by
accident.

A host consumes it with:

```sh
curl -fsSL https://apt.arki05.com/pubkey.asc \
    | gpg --dearmor -o /etc/apt/keyrings/arki05.gpg
echo "deb [signed-by=/etc/apt/keyrings/arki05.gpg] https://apt.arki05.com trixie main" \
    > /etc/apt/sources.list.d/arki05.list
apt update && apt install pve-meta
```

`pve-meta` depends on `pve-ext` and `libpve-meta-rs-perl`, so apt installs all three.
`pve-meta-publish` is not pulled in; `apt install pve-meta-publish` adds it.

## 3. Upgrade safety

There is no scheduled check against upstream Proxmox releases; nothing pins or blocks
`apt dist-upgrade` on `pve-manager`, `qemu-server`, `pve-container` or
`libpve-guest-common-perl` (only a `Depends: pve-manager (>= 9.0)`-style floor, see
`debian/control`, `pve-ext/debian/control`).

Instead, the managed patches tell the admin at the moment it matters. `pve-meta`'s
`debian/triggers` declares `interest-noawait` on the files its lifecycle patch touches,
so upgrading any of the three packages above fires `pve-meta.postinst`'s `triggered`
case in the same `apt` transaction, which re-runs `pve-ext-patch apply
pve-meta-lifecycle`. If the patch no longer applies cleanly, `apply` restores the
pristine file, never leaves one patched against stale content, and logs the failure to
syslog and to `<root>/run/pve-ext-patch/failed` rather than only to a postinst's stderr.
`pve-ext/README.md` ("Managed patches") owns the mechanism; `pve-ext-patch verify` runs
the same check by hand, without writing anything, for a human confirming a fix.

