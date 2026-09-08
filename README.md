# pve-meta

A structured metadata store for Proxmox VE. Every guest (vmid) and the datacenter get
one nested key-value document, stored under `/etc/pve/meta` and replicated by pmxcfs
like the rest of the cluster config. The document is reachable through a native API on
port 8006 (`PVE::API2::Ext::Meta`) and edited through a Monaco-based editor embedded as
a "Metadata" tab on every guest and on the Datacenter panel. Three packages: `pve-ext`
(a generic extension layer for PVE), and `pve-meta` + `libpve-meta-rs-perl` (this
project, a consumer of it).

## The document model

A document is the plain JSON data model — strings, numbers, booleans, arrays, maps —
with two rules on top: **ordered maps, no nulls** (key insertion order is part of the
document; absent means unset), and **comment keys** (a key ending in `__` documents its
sibling, a bare `__` documents the containing map; comment keys are ordinary data and
are stripped from API reads unless `comments=1` is passed).

Example `/etc/pve/meta/105.yaml`:

```yaml
backup:
  schedule: "03:00"
  schedule__: local time, cron-ish, interpreted by whatever reads this key
  retention: 7
```

## Views and scopes

A caller reads or writes the document through a **view**: a key-path prefix. A view of
`backup` is the `backup` subtree, returned with the prefix stripped. Views are also the
unit of access, via two grant paths:

1. **PVE ACLs** (full access) — `VM.Audit`/`VM.Config.Options` on `/vms/<vmid>` for a
   guest document, `Sys.Audit`/`Sys.Modify` on `/` for the datacenter document.
2. **Scopes** (partial access) — entries in the datacenter document, keyed by authid:

   ```yaml
   scopes:
     svc@pve!backup-agent:
       - prefix: backup
         mode: rw
       - prefix: monitoring
         mode: ro
   ```

Three rules govern scopes:

* A scope grants that authid the listed prefix on **every** guest document (no
  per-vmid scoping in this revision), without needing any VM privilege.
* A scope never restricts a principal who already has full access through ACLs.
* Only a principal with `Sys.Modify` on `/` may edit `scopes` itself.

Reading a view `P` requires read on `P` (an ACL, or a scope whose prefix is a prefix of
`P`); writing requires write on every path the write touches. A non-existent document
is an empty document with digest `""` — there is no explicit create.

## API

Native, `/api2/json/meta`, served by pveproxy/pvedaemon. Reads run in pveproxy, writes
are `protected` and run in pvedaemon.

| Method | Path | Params | Returns |
|---|---|---|---|
| GET | `/meta/guests` | `has` (prefix filter) | `[{ vmid, node, type, name, digest, keys: [top-level keys visible to the caller] }]` — every guest in the vmlist, `digest: ""` when no document |
| GET | `/meta/guests/{vmid}` | `view` (prefix, optional), `format` = `json` (default) or `yaml`, `comments` (default 1) | `{ id, view, digest, keys, data }` or `{ id, view, digest, keys, text }` — `keys` is the ordered list of top-level keys of the returned value; `data` is an unordered JSON object |
| PUT | `/meta/guests/{vmid}` | `view` (optional), exactly one of `data` (JSON string) or `text` (YAML) — the format follows from which one is given, `mode` = `replace` (default: the view's subtree is replaced by the payload) or `merge` (merge-patch; `null` deletes), `digest` (expected file digest, optional), `dry_run` | `{ vmid, view, digest, touched: [{ path, op: set|delete }...] }`; 409 on digest mismatch, 403 when the write touches a path outside the caller's write grants (including the view itself, and any write to `scopes` for a scope-granted principal — see "Permissions"), 400 on invalid content |
| DELETE | `/meta/guests/{vmid}` | `view` (optional), `digest` | removes the subtree (or the whole document) |
| GET/PUT/DELETE | `/meta/datacenter` | same as guests | same shapes with `id: "datacenter"` |
| GET | `/meta/access` | `vmid` or `dc=1` (optional) | `{ read, write, scopes: [{prefix, mode}] }` for that document; without either, the caller's scopes and datacenter read/write (what the UI's "View as" offers) |
| GET | `/meta/version` | — | `{ token, changed }` — content hash over the store and the newest mtime; poll it |

PUT and DELETE 404 for a vmid absent from the vmlist; GET does not (an absent guest's document simply reads as empty, digest `""`).

`perl/PVE/API2/Ext/Meta.pm` is a thin `PVE::RESTHandler` over the Rust core through the
perlmod bindings (`PVE::RS::Meta`): view extraction, prefix stripping, merge/replace,
touched-path computation, YAML/JSON rendering and digesting all happen in Rust.

## The editor

A **Metadata** tab appears on every LXC/QEMU guest's config panel and on the
Datacenter panel, and the same page is reachable standalone, same origin as the PVE UI:

```
https://<node>:8006/pve2/js/pve-meta-ui/index.html?vmid=105&theme=light
https://<node>:8006/pve2/js/pve-meta-ui/index.html?dc=1&theme=light
```

The page: a header line with the document's identity, a **View as** dropdown (the
prefixes the caller may see, plus "whole document"), Reload/Apply/Discard, and a
full-height Monaco editor in YAML mode. Editing is enabled only when the caller may
write the selected view. Apply shows a diff confirmation dialog, then sends the write
with the last-seen digest; a 409 (changed on the server since) shows a conflict notice
with Reload. That toolbar and the editor are the whole page.

## Lifecycle

Metadata is part of the guest: snapshot, rollback, delete-snapshot, clone, destroy and
container backup/restore all carry it, through one-line calls to `PVE::RS::Meta`
inserted into pve-container, qemu-server, libpve-guest-common and vzdump. QEMU VMs with
at least one disk are the one gap — both backup paths for those go through QEMU's own
QMP `backup` command, whose fixed parameter set can't carry a third blob, so a
disk-having VM's metadata is not included in its vzdump/PBS backup (see
`docs/LIFECYCLE-PATCHES.md` §4).

## How it plugs into PVE

Everything that touches pve-manager or the PVE UI goes through `pve-ext`'s three
generic seams, so pve-meta itself patches nothing directly:

* **API modules.** One line in `PVE/API2.pm` loads `PVE::API2::Ext`, which scans
  `/usr/share/perl5/PVE/API2/Ext/*.pm` at startup and mounts each module at the path it
  declares — `perl/PVE/API2/Ext/Meta.pm` ends up at `/api2/json/meta`.
* **UI pages.** One `<script>` line in `index.html.tpl` loads `pve-ext-loader.js`, which
  fetches `GET /api2/json/ext/pages` and adds one tab per manifest (`pages/pve-meta.json`)
  to its declared targets as a same-origin iframe.
* **Managed patches.** `pve-ext-patch` applies, verifies, removes and reports a set of
  dpkg-diverted file patches described by TOML manifests; pve-meta ships one
  (`patches/lifecycle.toml`) for the seven guest-lifecycle files.

pve-meta depends on pve-ext; the lifecycle patch is pve-meta's own manifest.

## Install

There is no published apt repository yet — `docs/DISTRIBUTION.md` documents the
pipeline (a static, GPG-signed repo on Cloudflare R2), but no key has been published.
Until then, `make deb` builds all three `.deb`s (see "Building" below); install in
dependency order:

```sh
dpkg -i pve-ext_*.deb
dpkg -i pve-meta_*.deb libpve-meta-rs-perl_*.deb
```

Installing/configuring `pve-meta` runs its `postinst`, which runs
`pve-ext-patch apply pve-meta-lifecycle` (dpkg-diverts the seven lifecycle files,
applies the corresponding diffs, gates on `perl -c` reporting `syntax OK`) and restarts
`pvedaemon`/`pveproxy`. The API module and the UI tab need no action — pve-ext
discovers the API module at process startup and re-reads the page manifest on every
`/ext/pages` request; `pve-ext`'s own `postinst` applies the two-file manifest that
those two seams depend on. Both patch steps are best-effort per file and never fail
install/configure; triggers re-run them whenever `pve-manager`, `pve-container`,
`qemu-server` or `libpve-guest-common-perl` reship a patched file. Check what's applied with `pve-ext-patch status`.

### Uninstall

`dpkg -r pve-meta` runs `prerm`, which calls `pve-ext-patch remove pve-meta-lifecycle`
**before** the package's own files are deleted, restoring every pristine file via
`dpkg-divert --remove --rename`. `/etc/pve/meta/*` documents are left untouched —
they're guest data, not package state.

## Permissions

The native module enforces ordinary PVE object permissions; there is no separate
account system of its own:

| Scope | Read | Write |
|---|---|---|
| A guest's document | `VM.Audit` on `/vms/<vmid>` | `VM.Config.Options` on `/vms/<vmid>` |
| Datacenter document | `Sys.Audit` on `/` | `Sys.Modify` on `/` |

A scope (above) additionally grants a prefix, read-only or read-write, on every guest
document to a principal with no VM privilege at all; only `Sys.Modify` on `/` may edit
`scopes`.

## Building

Build host: Debian 13 (trixie) with a [rustup](https://rustup.rs/) toolchain under
`~/.cargo/bin` (not the distro `cargo`/`rustc` packages), `libperl-dev` for the perlmod
crate, and, for the UI, `trunk`/`grass`/`wasm-opt` (only `pve-meta-core` builds on macOS
— develop the rest on Linux and `rsync` over). `make build` builds `crates/pve-meta-perl`,
`make ui` runs `trunk build` in `ui/`, and `make deb` builds `pve-ext` (its own source
package) plus `pve-meta` and `libpve-meta-rs-perl`, dropping all three `.deb`s next to
each other in the parent directory. See `docs/BUILD.md` for the exact rsync/ssh
incantation and the safe way to replace the installed `.so` on a live node.

## Repo layout

| Path | What |
|---|---|
| `crates/pve-meta-core` | Document model, YAML on-disk format, merge-patch engine, file store — pure Rust |
| `crates/pve-meta-perl` | `PVE::RS::Meta` — perlmod bindings: lifecycle hooks and the `api_*` functions |
| `perl/PVE/API2/Ext/Meta.pm` | The native API module, thin over `PVE::RS::Meta` |
| `ui/` | The editor page (pwt + Monaco, compiled to wasm) |
| `pve-ext/` | The extension layer: API-module loader, UI-page loader, `pve-ext-patch` (own package) |
| `patches/lifecycle/` | The seven guest-lifecycle Perl diffs + `lifecycle.toml` manifest |
| `pages/pve-meta.json` | The "Metadata" tab's page manifest |
| `debian/` | The `pve-meta` source package: `control`, triggers, `preinst`/`postinst`/`prerm`/`postrm` |
| `docs/` | `DESIGN.md` (authoritative), `design/`, `BUILD.md`, `DISTRIBUTION.md`, `LIFECYCLE-PATCHES.md`, `PERL-BINDINGS-SPEC.md` |
| `scripts/apt-repo/` | Signed apt repo build/publish scripts (Cloudflare R2) |
| `scripts/watch-pve/` | The ceiling-watcher CI job |
| `ceilings.toml` | Tested-ceiling versions for the four patched upstream packages |

## License

AGPL-3.0-or-later for this project's own code (every crate inherits
`license.workspace = true` from the root `Cargo.toml`); a handful of vendored editor
assets (pwt's stylesheets, Font Awesome, Monaco) carry their own upstream MIT/
Apache-2.0/OFL-1.1 licenses — see `debian/copyright` for the full, per-file breakdown.

---

Built with [Claude Code](https://claude.com/claude-code).
