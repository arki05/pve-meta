# Distribution: signed apt repo, CI, and the ceiling watcher

This document covers everything under `.github/workflows/`, `scripts/apt-repo/`,
`scripts/watch-pve/`, and `ceilings.toml` at the repo root: how packages get from a
`make deb` build to a host running `apt update`, and how the "tested ceiling" for the
patched upstream Proxmox packages stays current.

The design rationale in one paragraph: pve-meta patches stock Proxmox VE files it does
not control the release cadence of (see `docs/DESIGN.md` §5, "Managed patches"). A
signed, static apt repo (§1-§5 below) is the simplest way to distribute the resulting
`.deb`s without asking every install to build from source; a scheduled "ceiling
watcher" (§6) is the cheapest way to find out *before* a user's `apt dist-upgrade` that
a new upstream point release moved an anchor our diffs depend on, without ever
*blocking* that upgrade — see "Upgrade gating" at the end of §6. `pve-ext-patch` itself
(the tool the ceiling watcher drives) is documented in `pve-ext/README.md`; the
research that led to pve-meta's own guest-lifecycle manifest is in
`docs/LIFECYCLE-PATCHES.md` (lifecycle is snapshot-only: one file, `pve-ext-patch` +
`patches/lifecycle.toml`).

## 1. Repo layout

The apt tree is static files in a bucket, nothing server-side:

```
<bucket root>/
  pve-meta.asc                                  armored public signing key
  pool/main/<letter>/<source-pkg>/<file>.deb     every version ever published (append-only)
  dists/trixie/Release                           unsigned metadata
  dists/trixie/Release.gpg                       detached signature over Release
  dists/trixie/InRelease                         clearsigned Release (apt's preferred fetch)
  dists/trixie/main/binary-amd64/Packages{,.gz}

  private/                                       identical shape, gated by a Worker (§5)
    pve-meta.asc
    pool/main/<letter>/<source-pkg>/<file>.deb
    dists/trixie/...
```

`<letter>` follows the Debian pool convention: the package's first letter, or the first
four characters for anything starting with `lib` (e.g. `pve-meta` -> `pool/main/p/`,
`libpve-meta-rs-perl` -> `pool/main/libp/`).

Only `amd64` + `trixie` exist; there is no need for more (PVE 9 targets trixie only).

### The three scripts

* **`scripts/apt-repo/build-repo.sh <deb-dir> <repo-dir>`** — builds/updates the tree
  above in `<repo-dir>`, locally, from a directory of `.deb` files. Public packages go
  in `<deb-dir>/*.deb`, private ones in `<deb-dir>/private/*.deb`. It never wipes
  `<repo-dir>/pool` — if `R2_*` env vars are set it pulls the bucket's current `pool/`
  down first (via `rclone`), then only *adds* new `.deb` files (a name collision with
  different bytes is a hard error, not a silent overwrite). It regenerates
  `Packages`/`Packages.gz`/`Release`/`Release.gpg`/`InRelease` from the full pool every
  run (cheap: these are small index files, not the pool itself) and self-checks the
  signature with `gpgv` before exiting.
* **`scripts/apt-repo/publish-r2.sh <repo-dir>`** — syncs that tree to R2 with `rclone`.
  Uploads `pool/` and the keyring first, `dists/` last, and within `dists/`, the
  `Packages`/`Packages.gz` indexes before `Release`/`InRelease`/`Release.gpg` — so a
  client can never fetch a signed manifest that names files which aren't there yet.
  Sets `Content-Type` explicitly per file kind (R2's extension-based guess gets the
  extension-less `Packages`/`Release`/`InRelease` wrong).
* **`scripts/apt-repo/client-setup.sh`** — run as root on a PVE host to consume the
  repo (see §4).

`scripts/apt-repo/lib.sh` holds helpers shared by the first two (rclone config
generation from env, GPG key import, sign args) — not meant to be run directly.

## 2. Required secrets

Set these as GitHub Actions repository secrets (`Settings -> Secrets and variables ->
Actions`):

| Secret | Used by | What it is |
|---|---|---|
| `APT_GPG_KEY_ID` | build-repo.sh | The signing key's long id or fingerprint |
| `APT_GPG_PRIVATE_KEY` | build-repo.sh | Armored private key (imported into a throwaway keyring in CI) |
| `APT_GPG_PASSPHRASE` | build-repo.sh | Only if the key has one (unattended CI keys normally don't) |
| `R2_ACCOUNT_ID` | build-repo.sh, publish-r2.sh | Cloudflare account id (part of the S3 endpoint hostname) |
| `R2_ACCESS_KEY_ID` | build-repo.sh, publish-r2.sh | R2 API token access key |
| `R2_SECRET_ACCESS_KEY` | build-repo.sh, publish-r2.sh | R2 API token secret |
| `R2_BUCKET` | build-repo.sh, publish-r2.sh | Bucket name |

`GITHUB_TOKEN` (automatic) needs `contents: write`, `issues: write`,
`pull-requests: write` for `watch-pve.yml` (already set in the workflow's
`permissions:` block) — no extra secret needed there.

## 3. One-time setup

### 3.1 GPG signing key

```sh
gpg --quick-generate-key "pve-meta apt repo <julius@rkenberg.de>" rsa4096 sign 2y
gpg --list-secret-keys --with-colons | awk -F: '/^sec/{print $5}'   # -> APT_GPG_KEY_ID
gpg --armor --export-secret-keys <KEY_ID>                            # -> APT_GPG_PRIVATE_KEY secret
gpg --armor --export <KEY_ID>                                        # published as pve-meta.asc
```

Give it a real expiry (`2y` above) and calendar-remind yourself to rotate it before then
-- an expired signing key doesn't corrupt anything already published, but a fresh
`InRelease` signed with an expired key fails `apt update` for every client, so rotate
*before* expiry, re-publish, and update `APT_GPG_KEY_ID`/`APT_GPG_PRIVATE_KEY`. Keep the
old public key around in `pve-meta.asc` (armored files can hold more than one key)
during the overlap window if you want zero-downtime rotation; not automated here.

Passphrase-less is the simplest choice for a key that only ever signs from CI runners
that don't tty-prompt (`build-repo.sh` uses `--pinentry-mode loopback --batch`); if you
do give it a passphrase, also set `APT_GPG_PASSPHRASE`.

### 3.2 R2 bucket + custom domain

1. Cloudflare dashboard -> R2 -> **Create bucket** (any name; matches `R2_BUCKET`).
2. **Settings -> Public access -> Custom Domains -> Connect Domain** -> e.g.
   `apt.<domain>`. An R2 public bucket bound to a custom domain like this needs no
   separate CDN/Pages config and has free egress, and it's what makes
   `https://apt.<domain>/pool/...` resolve directly to bucket objects.
3. DNS: Cloudflare creates the CNAME for you when the domain is on Cloudflare; confirm
   it under the zone's DNS tab.
4. **R2 -> Manage API tokens -> Create API token** scoped to just this bucket
   (Object Read & Write is enough; the account id shown alongside the token is
   `R2_ACCOUNT_ID`) -> the generated key pair is `R2_ACCESS_KEY_ID` /
   `R2_SECRET_ACCESS_KEY`.
5. `client-setup.sh`'s default URL (`https://apt.example.com`) is a placeholder --
   always pass `--url https://apt.<domain>` (or set `PVE_META_APT_URL`).

Do **not** enable public access on the bucket for the whole thing if you plan to use
the private prefix trick below unmodified -- see §5: the public-bucket + custom-domain
setup happens once, and the private prefix is layered on top with a Worker, not a
second bucket.

### 3.3 GitHub Actions

Nothing to enable beyond the secrets in §2 -- `build.yml` triggers on push to `main`
(builds + uploads `.deb` artifacts only) and on tags (also builds+publishes the repo);
`watch-pve.yml` is cron + `workflow_dispatch`.

## 4. What a user runs

```sh
curl -fsSL https://apt.<domain>/scripts/apt-repo/client-setup.sh | bash -s -- --url https://apt.<domain>
```

(or copy the script over and run it locally -- it's plain bash, no curl-pipe
requirement, just convenient). It:

1. installs the armored public key to `/etc/apt/keyrings/pve-meta.asc`;
2. writes a deb822 `/etc/apt/sources.list.d/pve-meta.sources`:
   ```
   Types: deb
   URIs: https://apt.<domain>
   Suites: trixie
   Components: main
   Signed-By: /etc/apt/keyrings/pve-meta.asc
   ```
3. with `--private`, appends a second stanza for `https://apt.<domain>/private` and
   writes `/etc/apt/auth.conf.d/pve-meta.conf` (mode 0600) with a `machine
   <host>/private` basic-auth line -- placeholders unless `--private-user`/
   `--private-pass` are given; apt reads `auth.conf.d` natively, no extra config;
4. runs `apt update` (skip with `--no-update`).

## 5. Private prefix

Private packages (e.g. personal theming/boot-logo debs not meant for public
distribution) live at the same bucket under `private/`, built and published by the
exact same two scripts (they
already know about `private/` -- see §1). The bucket itself has no separate ACL for
that prefix; a tiny Cloudflare Worker in front of the custom domain gates it:

```js
// Cloudflare Worker, routed at apt.<domain>/private/* (leave everything else
// unrouted so it falls through to the R2 custom domain's own public serving).
export default {
  async fetch(request, env) {
    const auth = request.headers.get("Authorization") || "";
    const [scheme, encoded] = auth.split(" ");
    const valid =
      scheme === "Basic" &&
      encoded &&
      atob(encoded) === `${env.APT_PRIVATE_USER}:${env.APT_PRIVATE_PASS}`;

    if (!valid) {
      return new Response("Unauthorized", {
        status: 401,
        headers: { "WWW-Authenticate": 'Basic realm="pve-meta private apt"' },
      });
    }

    // Bind the same R2 bucket to this Worker (Settings -> Bindings) and serve
    // the object with the "private/" prefix stripped from the request path,
    // OR simply fetch through to the public custom domain -- either works;
    // an R2 binding avoids a second network hop.
    const url = new URL(request.url);
    const key = url.pathname.replace(/^\/private\//, "");
    const obj = await env.APT_BUCKET.get(key);
    if (!obj) return new Response("Not found", { status: 404 });
    return new Response(obj.body, {
      headers: { "Content-Type": obj.httpMetadata?.contentType || "application/octet-stream" },
    });
  },
};
```

Set `APT_PRIVATE_USER`/`APT_PRIVATE_PASS` as Worker secrets (`wrangler secret put`);
issue a client credential by handing out those same two values via
`client-setup.sh --private --private-user ... --private-pass ...` (or letting the user
edit `/etc/apt/auth.conf.d/pve-meta.conf`'s placeholders by hand). This is a sketch, not
a deployed Worker -- routing config, `wrangler.toml`, and the R2 binding name are left
to whoever stands it up.

## 6. Ceiling watcher

The patched packages carry a *tested ceiling*, not a dependency pin, so
`apt dist-upgrade` is never blocked (see "Upgrade gating" at the end of this section).
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
     guest-lifecycle diff — lifecycle is snapshot-only, see
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

## 7. Local dry-run recipe (no real credentials needed)

Shell/syntax checks, no network or Linux required:

```sh
bash -n scripts/apt-repo/*.sh scripts/watch-pve/*.sh
shellcheck scripts/apt-repo/*.sh scripts/watch-pve/*.sh   # brew install shellcheck
```

Full repo build + apt-acceptance test needs Linux tools (`dpkg-deb`, `apt-ftparchive`/
`dpkg-scanpackages`, `gpg`, `gpgv`) -- use the Linux build host if not available
locally:

```sh
ssh pve-meta-build "apt-get install -y apt-utils gnupg rclone"   # once

# 1. a throwaway signing key (never use this for anything real)
export GNUPGHOME=$(mktemp -d)
gpg --batch --quiet --passphrase '' --quick-generate-key \
    "test <test@example.com>" rsa3072 sign never
export APT_GPG_KEY_ID=$(gpg --list-secret-keys --with-colons | awk -F: '/^sec/{print $5}')

# 2. a dummy .deb (dpkg-deb -b needs a DEBIAN/control, not a real package)
mkdir -p /tmp/dummy/DEBIAN
cat > /tmp/dummy/DEBIAN/control <<EOF
Package: pve-meta-dummy
Version: 0.1.0-1
Architecture: amd64
Maintainer: Test <test@example.com>
Description: dummy
 dummy
EOF
mkdir -p /tmp/debs
dpkg-deb --build --root-owner-group /tmp/dummy /tmp/debs/pve-meta-dummy_0.1.0-1_amd64.deb

# 3. build the tree (no R2 env set -> skips the bucket pull, local-only)
bash scripts/apt-repo/build-repo.sh /tmp/debs /tmp/repo
# build-repo.sh already gpgv-verifies both Release.gpg files itself and fails
# loudly if either doesn't check out.

# 4. prove a real apt would accept it: serve locally and point apt at it
#    without touching the host's real /etc/apt (all Dir::Etc:: overrides)
( cd /tmp/repo && python3 -m http.server 8899 --bind 127.0.0.1 & )
mkdir -p /tmp/apttest
gpg --armor --export "$APT_GPG_KEY_ID" > /tmp/apttest/pve-meta.asc
cat > /tmp/apttest/pve-meta.sources <<EOF
Types: deb
URIs: http://127.0.0.1:8899
Suites: trixie
Components: main
Signed-By: /tmp/apttest/pve-meta.asc
EOF
apt-get -o Dir::Etc::sourcelist=/tmp/apttest/pve-meta.sources \
        -o Dir::Etc::sourceparts=/dev/null -o Dir::Etc::trusted=/dev/null \
        -o Dir::Etc::trustedparts=/dev/null \
        -o Dir::State::lists=/tmp/apttest/lists \
        -o Dir::Cache::archives=/tmp/apttest/archives \
        update   # -> "Fetched ... InRelease ... Packages", no signature errors
```

The watcher can be exercised the same way, live, with real upstream data and no
credentials at all -- it only needs `gh` (and its side effects) once a check fails or
passes:

```sh
PVE_META_DRY_RUN=1 scripts/watch-pve/check.sh
```

### What was actually verified while building this

* `bash -n` and `shellcheck` clean on every script in `scripts/apt-repo/` and
  `scripts/watch-pve/`; `actionlint` clean on both workflow files.
* `build-repo.sh` end to end on the Linux build host with a throwaway GPG key and two
  hand-built dummy `.deb`s (one public, one under `private/`): produced a correctly
  laid out tree, and its own `gpgv` self-check passed for both the public and private
  `Release.gpg`.
* Ran it a second time with an additional package version: the pool kept **both**
  versions (append, not replace); re-running with unchanged inputs was a no-op
  (idempotent, still re-signs and re-verifies).
* A real `debian:13` (trixie) `apt-get update` against the built tree, served over
  local HTTP with all `/etc/apt` paths overridden (no changes to the host's real apt
  config): succeeded, fetched `InRelease` + `Packages` with a valid signature.
* Tamper test: corrupting `Packages.gz` without re-signing `Release` made the same
  `apt-get update` fail with `Hash Sum mismatch` / unexpected file size, i.e. a bad
  actor (or a broken publish step) cannot silently serve tampered indexes.
* `publish-r2.sh`: verified it fails fast with a clear error when `R2_*` env is unset,
  and that with fake credentials it writes a correctly-shaped rclone config (endpoint
  `https://<account>.r2.cloudflarestorage.com`) and fails only at the network layer
  (TLS handshake to a nonexistent account id) -- the config/plumbing is exercised, the
  actual upload is not (needs real R2 credentials).
* `client-setup.sh`: ran against the local test repo above (with `/etc/apt` path
  substituted for a scratch directory instead of the real filesystem, since it insists
  on running as root against real paths otherwise) -- fetched the real key, wrote a
  correct two-stanza deb822 `.sources` file with `--private`, and wrote
  `/etc/apt/auth.conf.d/pve-meta.conf` at mode `0600` with the given credentials.
* `scripts/watch-pve/check.sh`: run for real against the live Proxmox no-subscription
  index (no credentials needed for this part) with `PVE_META_DRY_RUN=1`, on the
  `pve-ext-patch verify`-based mechanism described in §6 above (re-verified after
  fixing the tool references that used to point at a `pve-manager-patches/lifecycle/`
  layout that never existed in this repo). Confirmed correct version parsing and
  `dpkg --compare-versions` behavior (including a tricky `~` pre-release version).
  Then, with ceilings temporarily lowered in a scratch copy of the repo on the build
  host (never the real `ceilings.toml`) to force the "newer version" branch: it
  downloaded and extracted real `pve-container`, `qemu-server`,
  `libpve-guest-common-perl`, and `pve-manager` packages from the live Proxmox repo and
  ran the real `pve-ext-patch verify` checks -- observed an actual **pass** for
  `qemu-server` 9.0.10, `libpve-guest-common-perl` 6.0.2, and an older `pve-manager`
  9.0.0~10 (against pve-ext's own `pve-manager.toml` manifest -- the `PVE::API2::Ext`
  registration-ordering diff from §5 of `docs/DESIGN.md` still applies cleanly even
  that far back), and an actual **fail** for `pve-container` 6.0.10, whose
  `API2/LXC.pm` hunk #1 no longer applies against that older release (the diffs in
  this repo were generated against 6.1.13/6.1.14), with correct per-package pass/fail
  aggregation and correct per-package manifest filtering (no "no pristine source
  found" noise for the two packages' files not present in each other's extracted
  tree) and correct output (no `gh` calls attempted -- confirmed `gh` isn't even
  installed on the test host).

### What still needs real credentials / hasn't been exercised

* `publish-r2.sh`'s actual upload to R2 (needs `R2_*` secrets), and therefore the
  content-type headers landing correctly on real objects.
* The `pull_existing_pool` step in `build-repo.sh` actually pulling from a populated
  bucket (only exercised against a bucket that doesn't exist, which it handles by
  starting fresh).
* The GitHub side of `check.sh` (`gh pr create` / `gh issue create` / label creation) --
  the dry-run path was exercised thoroughly; the real `gh` calls were not run against a
  real repo from this environment.
* The Cloudflare Worker in §5 is a sketch, not deployed or tested.
* `build.yml`'s actual run in GitHub Actions (the `debian:trixie` container steps,
  rustup/trunk/grass install, `make deb`) -- not runnable from here since it depends on
  `crates/`, `debian/`, and the root `Makefile` other agents are concurrently writing;
  reviewed against the current `Makefile`/`debian/rules`/`debian/control` for shape
  (multiple binary `.deb`s from one source package are handled by the artifact-glob
  step, which copies every `.deb` it finds).
