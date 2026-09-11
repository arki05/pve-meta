# Distribution: releases, the apt repository, and the ceiling watcher

How packages get from a tag to a host running `apt update`, and how the "tested
ceiling" for the patched upstream Proxmox packages stays current.

## 1. Releases

`.github/workflows/build.yml` builds and tests every push on **amd64 and arm64** --
native builds in a `debian:trixie` container on each architecture, because
`libpve-meta-rs-perl` is a compiled Perl module and each architecture builds its own.
The two `Architecture: all` packages (`pve-meta`, `pve-ext`) are taken from the amd64
job alone, so one file name never means two different files.

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

## 3. Ceiling watcher

The patched packages carry a *tested ceiling*, not a dependency pin, so
`apt dist-upgrade` is never blocked (see "Upgrade gating" below).
`ceilings.toml` at the repo root holds it:

```toml
[tested]
pve-manager = "9.2.11"
libpve-guest-common-perl = "6.0.5"
```

Every 6 hours (`.github/workflows/watch-pve.yml`, cron `17 */6 * * *`, plus manual
`workflow_dispatch`) `scripts/watch-pve/check.sh`:

1. fetches the Proxmox no-subscription `Packages` index for `trixie`;
2. for each of the two tracked packages, compares its current version there (via
   `dpkg --compare-versions`) against `ceilings.toml`'s ceiling;
3. for every package strictly newer than its ceiling: downloads the `.deb`, extracts it
   with `dpkg-deb -x`, and runs `pve-ext-patch --root <extracted> verify <manifest>`
   (see `pve-ext/README.md` for what `verify` does: a dry-run of every entry's diff
   against the pristine copy it can find under `--root`, never touching anything):
   - **pve-manager**: verifies pve-ext's own manifest,
     `pve-ext/patches/pve-manager.toml` (the `index.html.tpl`/`PVE/API2.pm` hooks);
   - **libpve-guest-common-perl**: verifies `patches/lifecycle.toml` (pve-meta's one
     guest-lifecycle diff — one file carries the whole lifecycle, see
     `docs/LIFECYCLE-PATCHES.md`), filtered down first to that package's own `[[file]]`
     entries (a no-op today, kept for a future entry);
4. packages whose checks all pass get **one PR** bumping their `ceilings.toml` entries;
   packages with any failure get **one GitHub issue** (label `pve-upgrade`) carrying the
   full `pve-ext-patch verify` output, plus a `TODO(llm-fix)` block marking the hand-off
   to a planned (not yet implemented) LLM-assisted fix flow that would hand the failing
   diff to an LLM run to draft a fix PR for human review.
   Both checks are best-effort deduplicated against already-open PRs/issues with the
   same exact title, so a repeated 6-hourly run doesn't spam.

### Escape hatch (Upgrade gating)

The watcher is advisory, never a gate on anything:

* A failed verify only opens an issue; it does not block `build.yml`, does not touch
  `ceilings.toml`, and does not stop anyone from installing the newer upstream package
  by hand -- there is no `Depends:` pin on any tracked package's version, only a
  `Depends: pve-manager (>= 9.0)`-style floor (see `debian/control`,
  `pve-ext/debian/control`).
* If the automated verify has a false negative (e.g. it needs a check this script
  can't perform), a human just edits the failing diff under `pve-ext/patches/` or
  `patches/lifecycle/`, confirms locally, and hand-edits `ceilings.toml` in a normal PR
  -- the watcher's own PRs are not special, just the same file anyone can bump.
* Local dry run before trusting a real run:
  ```sh
  PVE_META_DRY_RUN=1 scripts/watch-pve/check.sh
  ```
  prints exactly what it would download/check/bump/file, and makes no GitHub API calls
  (it also skips them automatically if `gh` isn't installed).
* `workflow_dispatch` lets you trigger a check on demand instead of waiting up to 6h.

