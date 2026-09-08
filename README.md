# pve-meta

A structured metadata store for Proxmox VE. Every guest (vmid) and the datacenter get
one nested key-value document, stored under `/etc/pve/meta` and replicated by pmxcfs
like the rest of the cluster config. The document is reachable through a native API on
port 8006 (`PVE::API2::Ext::Meta`) and edited through a tree editor embedded as a
"Metadata" tab on every guest and on the Datacenter panel. Three packages: `pve-ext`
(a generic extension layer for PVE), and `pve-meta` + `libpve-meta-rs-perl` (this
project, a consumer of it).

## The document model

A document is the plain JSON data model — strings, numbers, booleans, arrays, maps —
with two rules on top: **ordered maps, no nulls** (key insertion order is part of the
file; absent means unset), and **comment keys** (a key ending in `__` documents its
sibling, a bare `__` documents the containing map; comment keys are ordinary data, shown
by the UI as the row's description rather than as a row of their own).

Example `/etc/pve/meta/105.yaml`:

```yaml
backup:
  schedule: "03:00"
  schedule__: local time, cron-ish, interpreted by whatever reads this key
  retention: 7
```

## Views, registrations and scopes

A caller reads or writes the document through a **view**: a key-path prefix, dotted, any
depth, through maps only. A view of `backup` is the `backup` subtree, returned with the
prefix stripped. Views are also the unit of access.

Access control lives outside the documents, one file per principal, in a drop
directory: `/etc/pve/meta.d/operators/<name>.yaml` (cluster-wide, pmxcfs), with packaged
defaults under `/usr/share/pve-meta/operators/<name>.yaml` — a cluster file overrides a
packaged file of the same name. An operator's own package drops its file into the
packaged location; an administrator overrides or adds one directly under
`/etc/pve/meta.d/operators/`. One format serves both registration and scopes:

```yaml
# /usr/share/pve-meta/operators/traefik.yaml
authid: svc@pve!traefik
description: Traefik dynamic-configuration provider
scopes:
  - prefix: traefik            # any dotted path, nested allowed
    mode: rw                   # ro | rw
    selector: { all: true }    # or { tag: traefik }; room for { pool: name } later
    grammar:                   # optional, PVE::JSONSchema dialect for the subtree
      type: object
      properties:
        spec:
          type: object
          properties:
            host: { type: string, description: Public host name }
            port: { type: integer, minimum: 1, maximum: 65535, optional: 1, default: 80 }
```

Rules:

* Files are parsed strictly and independently; a malformed file is skipped with a
  warning and grants nothing to anyone else.
* A **selector** restricts a scope to guests: `all`, or `tag: <t>` — the guest carries
  that PVE tag. Tag membership comes from the cluster's cached guest properties. Adding
  the tag is the deliberate, manual act of granting the operator that guest; pve-meta
  does not enforce anything about the tag itself, it only filters by it.
* Scopes apply to guest documents only; the datacenter document is governed by ACLs
  alone.
* A scope on prefix `p` also covers the sibling comment key `p__` — the only comment-key
  access rule.

Grants for a caller on a guest document:

* Full read = `VM.Audit` on `/vms/<vmid>`; full write = `VM.Config.Options` (datacenter:
  `Sys.Audit` / `Sys.Modify` on `/`).
* Scopes = the union of scope entries from every registration whose `authid` is the
  caller and whose selector matches the guest.
* Reading view `P` needs full read or a scope covering `P`; writing needs full write or
  a `rw` scope covering every path the write touches; a write to the root view needs
  full write. A caller with no grant at all gets 403 on read.

This is a blast-radius limiter, not a security boundary against an adversary — see
`docs/DESIGN.md` §1 for the threat model.

## API

Native, `/api2/json/meta`, served by pveproxy/pvedaemon. Reads run in pveproxy, writes
are `protected` and run in pvedaemon.

| Method | Path | Params | Returns |
|---|---|---|---|
| GET | `/meta/version` | — | `{ token, changed }` — content hash over the store; poll it |
| GET | `/meta/guests` | `has` (prefix) | `[{ vmid, node, type, name, tags, digest }]` for every guest in the vmlist the caller can read something of; `node`/`name`/`tags` only with `VM.Audit`; `digest: ""` when no document |
| GET | `/meta/guests/{vmid}` | `view`, `format` = `json` (default) or `yaml` | `{ id, view, digest, data }` or `{ id, view, digest, text, parse_error? }` |
| PUT | `/meta/guests/{vmid}` | `view`, `data` or `text`, `mode` = `replace` or `merge`, `digest`, `dry_run` | `{ id, view, digest, touched }`; 409 on digest mismatch, 403 outside the caller's grants, 400 on invalid content |
| DELETE | `/meta/guests/{vmid}` | `view`, `digest` | removes the subtree, or the whole document |
| GET/PUT/DELETE | `/meta/datacenter` | same as guests | same shapes with `id: "datacenter"` |
| GET | `/meta/access` | `vmid` or `dc=1` | `{ read, write, scopes }` for that document, selectors already resolved; without either, the caller's own datacenter read/write |
| GET | `/meta/operators` | — | `[{ name, authid, description, scopes }]` — every registration, readable by any authenticated user (it drives the UI's ownership column) |

PUT and DELETE 404 for a vmid absent from the vmlist; GET of such a vmid is 404 too.
`data` is a JSON-encoded string parameter; grants and guest lists cross the Perl/Rust
boundary as native hashes/arrays, not JSON strings.

`perl/PVE/API2/Ext/Meta.pm` is a thin `PVE::RESTHandler` over the Rust core
(`pve_meta_core::api`) through the perlmod bindings (`PVE::RS::Meta`).

## The editor

A **Metadata** tab appears on every LXC/QEMU guest's config panel and on the
Datacenter panel: one tree of the document the caller can see. Rows are the union of
the keys present and the keys any applicable grammar declares (an unset declared key
renders greyed out, with its default, description and a "set" action). Columns: key,
value (an inline editor by type), owner (which registration's scope covers the row, from
`/meta/operators`). A row edit is a minimal `PUT ?view=<path>&mode=replace`; add is the
same at a new path; delete is `DELETE ?view=<path>`. Editability is per row, from
`/meta/access`. Monaco is the escape hatch: edit a subtree as YAML/JSON text, with a
diff-confirmed apply.

The tab is `ui-extjs/`: plain JavaScript, a native `Ext.tree.Panel` mounted through
pve-ext's `script`+`xtype` manifest form, so session, CSRF, theme and i18n all come from
the PVE UI. No iframe, no wasm, no build step beyond vendoring Monaco. A second
implementation in pwt/Yew was built to the same spec and compared on the lab before being
removed — `docs/DESIGN.md` §8 and §11, git tag `pwt-ui-removed`.

## Lifecycle

Metadata follows a guest through create, destroy, snapshot, rollback and
delete-snapshot: **one** patched file, `PVE/AbstractConfig.pm`
(`libpve-guest-common-perl`), calling `PVE::RS::Meta::on_create`/`on_destroy`/
`on_snapshot`/`on_rollback`/`on_delsnap`. Migration needs no hook — the document is
flat and cluster-wide, so it does not move. Clone and backup are not carried:

* **Create and destroy** are hooks in the same patched file: `create_and_lock_config`
  clears any metadata left at a vmid when PVE has just asserted it was unused (so a
  recreated vmid never inherits the old guest's document), and `destroy_config` removes
  the document and its snapshot copies once the guest config itself is gone. Both are
  best-effort and warn; metadata never breaks a guest operation. `/usr/libexec/pve-meta/gc`
  stays as a **manual** broom for a config removed out of band — nothing runs it on a
  timer.
* **Clone and backup are not carried.** Documented instead: metadata lives in
  `/etc/pve`, so back up `/etc/pve`. See `docs/LIFECYCLE-PATCHES.md` for the reasoning
  (a disk-having QEMU VM's backup path has no room for a third blob, so a partial
  guarantee here would be worse than an honest one).

## How it plugs into PVE

Everything that touches pve-manager or the PVE UI goes through `pve-ext`'s three
generic seams, so pve-meta itself patches nothing directly:

* **API modules.** One line in `PVE/API2.pm` loads `PVE::API2::Ext`, which scans
  `/usr/share/perl5/PVE/API2/Ext/*.pm` at startup and mounts each module at the path it
  declares — `perl/PVE/API2/Ext/Meta.pm` ends up at `/api2/json/meta`.
* **UI pages.** One `<script>` line in `index.html.tpl` loads `pve-ext-loader.js`, which
  fetches `GET /api2/json/ext/pages` and adds one tab per manifest to its declared
  targets — a same-origin iframe (`url`) or a native ExtJS panel loaded once and
  instantiated in place (`script` + `xtype`). pve-meta ships one, `pages/pve-meta.json`,
  in the `script`+`xtype` form.
* **Managed patches.** `pve-ext-patch` applies, verifies, removes and reports a set of
  dpkg-diverted file patches described by TOML manifests; pve-meta ships one
  (`patches/lifecycle.toml`) for the snapshot/rollback/delete-snapshot hook.

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

Installing/configuring `pve-meta` runs its `postinst`: `pve-ext-patch apply
pve-meta-lifecycle` (dpkg-diverts `PVE/AbstractConfig.pm`, applies the diff, gates on
`perl -c` reporting `syntax OK`), creates `/etc/pve/meta.d/operators` when `/etc/pve`
is mounted, and restarts `pvedaemon`/`pveproxy`. The API module and the UI tabs need no
action — pve-ext discovers
the API module at process startup and re-reads page manifests on every `/ext/pages`
request; `pve-ext`'s own `postinst` applies the two-file manifest those two seams
depend on. The patch step is best-effort and never fails install/configure; a trigger
re-runs it whenever `libpve-guest-common-perl` reships the patched file. Check what's
applied with `pve-ext-patch status`.

### Uninstall

`dpkg -r pve-meta` runs `prerm`, which calls `pve-ext-patch remove pve-meta-lifecycle`
**before** the package's own files are deleted, restoring the pristine file via
`dpkg-divert --remove --rename`. `/etc/pve/meta/*`
documents are left untouched — they're guest data, not package state.

## Permissions

The native module enforces ordinary PVE object permissions; there is no separate
account system of its own:

| Scope | Read | Write |
|---|---|---|
| A guest's document | `VM.Audit` on `/vms/<vmid>` | `VM.Config.Options` on `/vms/<vmid>` |
| Datacenter document | `Sys.Audit` on `/` | `Sys.Modify` on `/` |

A registration (above) additionally grants a prefix, read-only or read-write, on the
guests its selector matches, to a principal that may hold no VM privilege at all.

## Building

Build host: Debian 13 (trixie) with a [rustup](https://rustup.rs/) toolchain under
`~/.cargo/bin` (not the distro `cargo`/`rustc` packages), `libperl-dev` for the perlmod
crate, and `npm` to vendor Monaco (only `pve-meta-core` builds on macOS — develop the
rest on Linux and `rsync` over). `make build` builds `crates/pve-meta-perl`, `make ui`
vendors Monaco into `ui-extjs/vendor/vs`, and `make deb` builds
`pve-ext` (its own source package) plus `pve-meta` and `libpve-meta-rs-perl`, dropping
all three `.deb`s next to each other in the parent directory. See `docs/BUILD.md` for
the exact rsync/ssh incantation and the safe way to replace the installed `.so` on a
live node.

## Repo layout

| Path | What |
|---|---|
| `crates/pve-meta-core` | Document model, views, registrations/scopes/selectors, lint, api layer, store, gc — pure Rust |
| `crates/pve-meta-perl` | `PVE::RS::Meta` — perlmod bindings: lifecycle hooks, gc, the `api_*` functions |
| `perl/PVE/API2/Ext/Meta.pm` | The native API module, thin over `PVE::RS::Meta` |
| `operators/` | Packaged example registrations (none required) |
| `ui-extjs/` | The editor tab: plain JS, a native `Ext.tree.Panel` |
| `pve-ext/` | The extension layer: API-module loader, UI-page loader, `pve-ext-patch` (own package) |
| `patches/lifecycle/` | The one guest-lifecycle diff (snapshot/rollback/delete-snapshot) + `lifecycle.toml` manifest |
| `pages/` | The "Metadata" tab's page manifest |
| `libexec/gc` | Manual GC broom; no timer runs it (see `docs/DESIGN.md` §6) |
| `debian/` | The `pve-meta` source package: `control`, triggers, systemd units, `postinst`/`prerm` |
| `docs/` | `DESIGN.md` (authoritative), `design/`, `BUILD.md`, `DISTRIBUTION.md`, `LIFECYCLE-PATCHES.md`, `PERL-BINDINGS-SPEC.md` |
| `scripts/apt-repo/` | Signed apt repo build/publish scripts (Cloudflare R2) |
| `scripts/watch-pve/` | The ceiling-watcher CI job |
| `ceilings.toml` | Tested-ceiling versions for the two patched upstream packages |

## License

AGPL-3.0-or-later for this project's own code (every crate inherits
`license.workspace = true` from the root `Cargo.toml`); a handful of vendored editor
assets (Monaco, js-yaml) carry their own upstream MIT/
Apache-2.0/OFL-1.1 licenses — see `debian/copyright` for the full, per-file breakdown.

---

Built with [Claude Code](https://claude.com/claude-code).
