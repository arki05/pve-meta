# pve-meta

A small, boring, permission-aware, structured (nested) key-value metadata store for
Proxmox VE guests and the datacenter. Metadata lives in human-readable sidecar files
(YAML by default, TOML or JSON per file) inside `/etc/pve`, so it replicates and
fails over with the rest of the cluster config for free, participates in guest
snapshot/clone/destroy/backup like any other piece of guest state, and is reachable
through an API that behaves exactly like the rest of Proxmox's own `/api2` — because
it *is* the rest of Proxmox's own `/api2`: `pve-meta` registers a native
`PVE::API2::Meta` module served by pveproxy/pvedaemon on port 8006, a Yew/wasm editor
embedded as a "Metadata" tab on every guest and the datacenter, and a set of
reversible `dpkg-divert` patches that wire it into the surrounding PVE packages. It
knows nothing about what anyone stores in it. See [`../docs/VISION.typ`](../docs/VISION.typ)
for the full design rationale and roadmap.

## Status

Working, tested end-to-end on **PVE 9.2.11** in a nested/disposable lab
(`pvemeta-node1`), including a real headless-Chromium pass over the injected
"Metadata" tab and a full lifecycle-hook round trip (snapshot, rollback, clone,
destroy, vzdump backup/restore for both a container and a VM). **Not yet used in
production, and not yet installed from a published apt repository** — the
distribution pipeline (`docs/DISTRIBUTION.md`) is built and dry-run tested, but no
key has been published yet. Treat this as a v1 in the "trusted-lab" sense described
below: the native API enforces normal PVE object permissions, but there is no
namespace-claims authorization yet — see "Known limitations".

## Architecture

```
 ┌────────────────────────────────────────────────────────────────┐
 │  Browser: PVE web UI (ExtJS, pvemanagerlib.js)                  │
 │  "Metadata" tab on every guest + Datacenter panel               │
 │  (injected by pve-meta-patch: one <script> in index.html.tpl,   │
 │   one PVE.panel.Config.prototype.initComponent patch)           │
 │      │ same-origin iframe: /pve2/js/pve-meta-ui/index.html      │
 └──────┼───────────────────────────────────────────────────────────┘
        │  PVEAuthCookie / CSRFPreventionToken (shared with the PVE UI)
        ▼
 ┌────────────────────────────────────────────────────────────────┐
 │  pveproxy / pvedaemon — port 8006, node's real TLS cert          │
 │  PVE::API2::Meta  (perl/PVE/API2/Meta.pm)                        │
 │    reads  → pveproxy (www-data)      /api2/json/meta/...         │
 │    writes → pvedaemon (root)         VM.Audit / VM.Config.Options│
 └──────┼───────────────────────────────────────────────────────────┘
        │  perlmod bindings — PVE::RS::Meta (crates/pve-meta-perl)
        ▼
 ┌────────────────────────────────────────────────────────────────┐
 │  pve-meta-core (Rust): document model, YAML/TOML/JSON formats,   │
 │  merge-patch engine, atomic file store, snapshot/clone/destroy   │
 └──────┼───────────────────────────────────────────────────────────┘
        │  reads/writes
        ▼
 /etc/pve/meta/<vmid>.<ext>            (pmxcfs — replicated, no node affinity)
 /etc/pve/meta/datacenter.<ext>
 /etc/pve/meta/<vmid>.<snapname>.<ext>

 also driving pve-meta-core, independently of the API above:
   • pve-meta-lifecycle-patch — dpkg-divert patches into pve-container / qemu-server /
     libpve-guest-common-perl / vzdump, calling PVE::RS::Meta on snapshot, rollback,
     delsnap, clone, destroy and backup export/import
```

Everything below the ExtJS layer is a thin Perl shim over Rust: `PVE::API2::Meta`'s
methods are one or two lines each, calling into `PVE::RS::Meta::api_*` functions
(`crates/pve-meta-perl`) that wrap `pve-meta-core` — one document model, one patch
engine, one set of format writers, backing both the native API and the lifecycle
hooks.

## The document format

Every document — one per guest (`vmid`) plus one for the datacenter — is a nested
map of strings, numbers, booleans, arrays and maps: the plain JSON data model, with
two rules layered on top:

* **Ordered maps, no nulls.** Key insertion order is part of the document and
  survives every write; new keys append. Absent means unset — there is no `null`.
* **Comment keys.** A key ending in `__` documents its sibling; a bare `__`
  documents the containing map. They are plain data (not real file comments), so
  they survive every format and every client, and are stripped from API reads
  unless `?comments=1` is passed.

Format is a per-file choice, selected by the file extension (`.yaml`/`.toml`/`.json`
under `/etc/pve/meta/`); **YAML is the default**. Write fidelity differs per format:
TOML keeps real comments and formatting via `toml_edit` (minimal-diff edits, like
`cargo` does to `Cargo.toml`); YAML and JSON are canonical dumps — order and comment
keys survive, but real `#`/`//`-style file comments and original quoting do not.

Example `/etc/pve/meta/105.yaml`:

```yaml
backup:
  schedule: "03:00"
  schedule__: local time, cron-ish, interpreted by whatever reads this key
  retention: 7
```

Any top-level key is its own namespace; the UI renders each as its own collapsible
panel.

## Install

There is no published apt repository yet — `docs/DISTRIBUTION.md` documents the
full pipeline (a static, GPG-signed repo on Cloudflare R2), but no signing key has
been published. Once it is:

```sh
# placeholder — see docs/DISTRIBUTION.md for the real script/URL once published
curl -fsSL https://apt.<domain>/scripts/apt-repo/client-setup.sh | bash -s -- --url https://apt.<domain>
apt update
apt install pve-meta
```

Until then, build the two `.deb`s yourself (see "Building" below) and install them
directly:

```sh
dpkg -i pve-meta_*.deb libpve-meta-rs-perl_*.deb
```

`pve-meta` depends on `pve-manager (>= 9.0)` and `libpve-meta-rs-perl`
(`debian/control`). Installing/configuring it runs `postinst`, which:

1. Runs `pve-meta-patch apply` — diverts `pve-manager`'s `index.html.tpl` and
   `pvemanagerlib.js` (via `dpkg-divert`), re-renders them with the "Metadata" tab
   loader script inserted, installs the loader at
   `/usr/share/pve-manager/js/pve-meta-loader.js`.
2. Runs `pve-meta-lifecycle-patch apply` — diverts the eight Perl files that need a
   `PVE::RS::Meta` call site or the `PVE::API2::Meta` registration
   (`PVE/AbstractConfig.pm`, `PVE/API2/{LXC,Qemu}.pm`, `PVE/LXC/Create.pm`,
   `PVE/VZDump/{LXC,QemuServer}.pm`, and `PVE/API2.pm` itself), applies the
   corresponding `pve-manager-patches/lifecycle/*.diff` with `patch -p1`, gates
   installation on `perl -c` reporting `syntax OK`.
3. Restarts `pvedaemon` and `pveproxy` so the patched files (and the newly
   registered `PVE::API2::Meta` module) are picked up.

Both patch steps are **best-effort per file** and never fail the package
install/configure — a mismatched anchor on some future point release degrades to a
warning, not a broken `apt upgrade`. `interest-noawait` dpkg triggers
(`debian/pve-meta.triggers`) re-run both tools automatically whenever `pve-manager`,
`pve-container`, `qemu-server` or `libpve-guest-common-perl` reship any of the
patched files, so the patches survive their upgrades too.

Check what's actually applied:

```sh
pve-meta-patch status              # web UI tab injection: diverted? patched? loader present?
pve-meta-lifecycle-patch status    # all 7 lifecycle files: diverted? patched? in sync with the shipped diff?
pve-meta-lifecycle-patch verify    # dry-run every diff without changing anything
```

### Uninstall

`dpkg -r pve-meta` (or `apt remove pve-meta`) runs `prerm`, which calls
`pve-meta-patch remove` and `pve-meta-lifecycle-patch remove` **before** the
package's own files are deleted — each restores the pristine file via
`dpkg-divert --remove --rename` (falling back to `cp -a` from the diverted backup if
`dpkg-divert` itself fails), so the host is left byte-for-byte at stock `pve-manager`
Perl/JS/HTML. `/etc/pve/meta/*` documents themselves are left untouched by
uninstalling the package — they are guest data, not package state.

## Usage

### `pvesh` / curl, against the native API

Because `PVE::API2::Meta` is a regular PVE API module, `pvesh` works against it like
any other tree:

```sh
pvesh get /meta/version
pvesh get /meta/guests/105
pvesh set /meta/guests/105 -patch '{"backup":{"schedule":"03:00"}}'
```

Or with a PVE API token over HTTPS on port 8006 (`pveum user token add ...`):

```sh
curl -sk \
  -H "Authorization: PVEAPIToken=root@pam!meta=xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx" \
  https://pve1:8006/api2/json/meta/guests/105

curl -sk -X PUT \
  -H "Authorization: PVEAPIToken=root@pam!meta=xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx" \
  --data-urlencode 'patch={"backup":{"retention":7}}' \
  https://pve1:8006/api2/json/meta/guests/105
```

`patch` (and any other object-valued parameter) is passed as a JSON-encoded string,
same as every other PVE API endpoint that takes structured input. See
`docs/API.md` for the full endpoint table.

### UI

A **Metadata** tab appears on every LXC/QEMU guest's config panel and on the
Datacenter panel — form view first (one collapsible panel per namespace, schema-
driven fields where a schema is registered), source view second (raw YAML/TOML/JSON
with convert/verify/diff-before-apply). It's also reachable standalone, same origin
as the PVE UI, no separate login:

```
https://<node>:8006/pve2/js/pve-meta-ui/index.html?vmid=105&theme=light
https://<node>:8006/pve2/js/pve-meta-ui/index.html?dc=1
```

## Permissions

The native module enforces ordinary PVE object permissions — there is no separate
account/ACL system of its own:

| Scope | Read | Write |
|---|---|---|
| A guest's document (`/meta/guests/{vmid}`, `.../subtree`, `.../snapshots`) | `VM.Audit` on `/vms/{vmid}` | `VM.Config.Options` on `/vms/{vmid}` (`clone` additionally needs `VM.Clone`) |
| Datacenter document, registry, inventory | `Sys.Audit` on `/` | `Sys.Modify` on `/` |

Identity is a PVE API token or a browser ticket — `pveum` issues and revokes both;
`pve-meta` has no accounts of its own.

## Known limitations

* **QEMU VMs with at least one disk cannot carry the metadata blob inside a
  vzdump/PBS backup.** Both the PBS and VMA backup paths for a disk-having VM go
  through QEMU's own QMP `backup` command, whose fixed parameter set
  (`config-file`/`firewall-file`) is compiled into `pve-qemu-kvm` and can't be
  extended by a Perl-only patch. Container backups (PBS and local) are unaffected —
  `pct`/vzdump's LXC path never touches QMP and accepts an arbitrary list of named
  blobs — and diskless VMs and third-party backup-provider plugins that implement
  the new optional `archive_get_meta_config` method also carry metadata through.
  See `docs/LIFECYCLE-PATCHES.md` §4 for the full code-path analysis and the
  considered (and rejected) workarounds.
* **The ExtJS "Metadata" tab injection is only as durable as the PVE releases it was
  tested against.** It works by patching a live prototype method
  (`PVE.panel.Config.prototype.initComponent`) and matching literal ExtJS class
  names — robust to code motion inside those files, not to a future rename.
  `ceilings.toml` records the last version of `pve-manager`/`pve-container`/
  `qemu-server`/`libpve-guest-common-perl` this was verified against; a scheduled CI
  job (`.github/workflows/watch-pve.yml`, `scripts/watch-pve/check.sh`) dry-runs
  every patch against each new upstream release and opens a PR (bump the ceiling) or
  an issue (patch needs attention) — this is advisory, not a dependency pin, so
  `apt dist-upgrade` is never blocked by it.
* **No claims/authorization enforcement yet.** The native API enforces normal PVE
  object permissions (above), but nothing yet stops an authorized writer from
  touching a namespace another client claims — the "trusted-lab" framing from
  `docs/VISION.typ`. Every write's touched-path set is already computed and returned
  in the response, which is the seam claims enforcement slots into later.
* **YAML and JSON writes are canonical dumps, not format-preserving edits.** Order
  and comment keys (`key__`) survive every write in every format; real `#`-style
  file comments and original quoting/whitespace survive only in TOML (via
  `toml_edit`). Hand-edit a YAML/JSON file in `/etc/pve/meta/` and the next API
  write will re-dump it canonically.

## Building

Build host: Debian 13 (trixie), a [rustup](https://rustup.rs/) toolchain under
`~/.cargo/bin` (not the distro `cargo`/`rustc` packages — `debian/control`
deliberately excludes them from `Build-Depends`), `libperl-dev` for the perlmod
crate, and, for the UI, `trunk`/`grass`/`wasm-opt` (the wasm toolchain doesn't build
on macOS at all — only `pve-meta-core` compiles there; develop the rest on the Linux
build host and `rsync` over).

```sh
make build          # builds crates/pve-meta-perl (the libpve-meta-rs-perl cdylib)
make ui             # trunk build in ui/ (falls back to a placeholder page if trunk is missing)
make deb            # == dpkg-buildpackage -b -us -uc -d
```

`make deb` produces two binary packages from one source package (see
`debian/control`): `../pve-meta_<version>_<arch>.deb` and
`../libpve-meta-rs-perl_<version>_<arch>.deb`. See `docs/BUILD.md` for the exact
rsync/ssh incantation for the Linux build host.

## Repo layout

| Path | What |
|---|---|
| `crates/pve-meta-core` | Document model, YAML/TOML/JSON formats, merge-patch engine, file store — pure Rust, builds on macOS and Linux |
| `crates/pve-meta-perl` | `PVE::RS::Meta` — perlmod bindings exposing lifecycle hooks and the `api_*` functions to Perl |
| `perl/PVE/API2/Meta.pm` | The native `PVE::API2::Meta` REST module, thin over `PVE::RS::Meta` |
| `ui/` | The Yew/`pwt`/wasm editor SPA (Form + Source views), served by pveproxy from `/pve2/js/pve-meta-ui/` |
| `pve-manager-patch/` | `pve-meta-patch` — injects the "Metadata" tab into the PVE web UI |
| `pve-manager-patches/lifecycle/` | The eight Perl diffs + `pve-meta-lifecycle-patch`, wiring snapshot/clone/destroy/backup hooks and registering `PVE::API2::Meta` |
| `debian/` | The `pve-meta` source package: `control`, `rules`, triggers, `postinst`/`prerm`/`postrm` |
| `docs/` | `API.md`, `NATIVE-API-SPEC.md`, `UI-SPEC.md`, `PERL-BINDINGS-SPEC.md`, `LIFECYCLE-PATCHES.md`, `BUILD.md`, `DISTRIBUTION.md` |
| `scripts/apt-repo/` | Static signed-apt-repo build/publish scripts (Cloudflare R2) |
| `scripts/watch-pve/` | The ceiling-watcher CI job |
| `ceilings.toml` | Tested-ceiling versions for the four patched upstream packages |
| `Makefile`, `Cargo.toml` | Top-level build orchestration and Rust workspace |

## License

AGPL-3.0-or-later (see `debian/copyright`; every crate in the workspace inherits
`license.workspace = true` from the root `Cargo.toml`).

---

Built largely with [Claude Code](https://claude.com/claude-code), in a
pair-programming style — architecture and review by a human, most of the
implementation, testing and documentation drafted by Claude across many sessions.
