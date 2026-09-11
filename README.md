# pve-meta

A structured metadata store for Proxmox VE. Every guest (vmid) gets one nested
key-value document, stored under `/etc/pve/meta` and replicated by pmxcfs like the rest
of the cluster config. The document is reachable through a native API on port 8006
(`PVE::API2::Ext::Meta`) and edited through a tree editor embedded as a "Metadata" tab
on every guest; the Datacenter panel's "Metadata" tab lists the prefixes and
permissions that describe and govern those documents. Three packages: `pve-ext`
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

## Views, prefixes and permissions

A caller reads or writes the document through a **view**: a key-path prefix, dotted, any
depth, through maps only. A view of `backup` is the `backup` subtree, returned with the
prefix stripped. Views are also the unit of access.

Two things live outside the documents, in two drop directories.

**A prefix says what a prefix is** — `/etc/pve/meta.d/prefixes/<prefix>.yaml`, with
packaged defaults under `/usr/share/pve-meta/prefixes/<prefix>.yaml` (a cluster file
overrides a packaged one of the same name). **The file name is the prefix**, so
`traefik.yaml` declares `traefik` and `homelab.docker.yaml` declares `homelab.docker`;
there is no `prefix:` field for the two to disagree about.

```yaml
# /usr/share/pve-meta/prefixes/traefik.yaml
description: Traefik dynamic configuration
selector: { tag: traefik }   # or { all: true }; room for { pool: name } later
enforce: true                # optional: refuse a write that breaks the schema below
schema:                      # optional, PVE::JSONSchema dialect for the subtree
  type: object
  properties:
    spec:
      type: object
      properties:
        host: { type: string, description: Public host name }
        port: { type: integer, minimum: 1, maximum: 65535, default: 80 }
```

A prefix names **no principal**. Declaring that a prefix exists and has a shape is
useful with no operator, no token and no automation anywhere near it — a structured
notes field with a schema is a complete use of this system.

**A permission file says who may touch one** — `/etc/pve/meta.d/permissions/<name>.yaml`:

```yaml
authid: svc@pve!traefik
rules:
  - prefix: traefik
    mode: rw                 # ro | rw
    selector: { tag: traefik }
```

Permissions are **cluster-only: there is deliberately no packaged permissions directory.** An
operator's own package may ship a prefix, because a schema is a declaration; it must
never ship its own, because that is self-registration. dpkg cannot write into
pmxcfs, so "an operator declares what it expects, only an administrator grants it" is
enforced by where the files live rather than by a rule someone has to remember.

* Files are parsed strictly and independently; a malformed file is skipped with a
  warning and costs no other file anything.
* A **selector** restricts either to guests: `all`, or `tag: <t>` — the guest carries
  that PVE tag. Tag membership comes from the cluster's cached guest properties. Adding
  the tag is the deliberate, manual act of including that guest; pve-meta does not
  enforce anything about the tag itself, it only filters by it.
* A rule on prefix `p` also covers the sibling comment key `p__` — the only
  comment-key access rule.

**The two nest by opposite rules, deliberately.** Prefixes: *most-specific wins, and
schemas never merge* — with both `homelab` and `homelab.docker` declared,
`homelab.docker.compose` is governed by the child alone and the parent's own
`properties.docker` is shadowed, not combined. Permissions: *containment, additive* — a rule
on `homelab` covers `homelab.docker`, because "you may write `homelab`" not implying its
subtree would be surprising. Shape has one owner, so it shadows; permission is a union,
so it adds. Those two rules cannot live on one object, which is why this is two
concepts and not one (`docs/DESIGN.md` §12).

Grants for a caller on a guest document:

* Full read = `VM.Audit` on `/vms/<vmid>`; full write = `VM.Config.Options`.
* Scopes = the union of rules whose `authid` is the caller and whose selector
  matches the guest.
* Reading view `P` needs full read or a scope covering `P`. A caller with no rule at all
  gets 403 on read.
* **A write is authorized by what it changes, not by what it is addressed to**: every
  path the plan touches — values changed, keys added, keys removed — needs full write or
  a `rw` scope. So one write may span two granted prefixes even though the view covering
  both is the document root. On top of that a write needs read access to the view it
  names (otherwise the check is a read oracle) and *some* write permission on the
  document (key order is not a path, so a pure reordering touches nothing). A document
  that cannot be read back is the exception: repairing it as a whole needs full write,
  because there is no stored content to check the change against.

This is a blast-radius limiter, not a security boundary against an adversary — see
`docs/DESIGN.md` §1 for the threat model.

## API

Native, `/api2/json/meta`, served by pveproxy/pvedaemon. Reads run in pveproxy, writes
are `protected` and run in pvedaemon.

| Method | Path | Params | Returns |
|---|---|---|---|
| GET | `/meta/version` | `detail`, `id` | `{ token, changed }` — content hash over the store; poll it. With `id`, over that one document plus the registry directories instead |
| GET | `/meta/guests` | `has` (prefix) | `[{ vmid, node, type, name, tags, digest }]` for every guest in the vmlist the caller can read something of; `node`/`name`/`tags` only with `VM.Audit`; `digest: ""` when no document |
| GET | `/meta/guests/{vmid}` | `view`, `format` = `json` (default) or `yaml` | `{ id, view, digest, data }` or `{ id, view, digest, text, parse_error? }` |
| PUT | `/meta/guests/{vmid}` | `view`, `data` or `text`, `mode` = `replace` or `merge`, `digest`, `dry_run`, `force` | `{ id, view, digest, touched }`; 409 on digest mismatch, 403 outside the caller's permissions, 400 on invalid content, 422 when the write would break a prefix's schema that says `enforce: true` and `force` is not set |
| DELETE | `/meta/guests/{vmid}` | `view`, `digest` | removes the subtree, or the whole document |
| GET | `/meta/access` | `id` (any document id) | `{ read, write, scopes, tags }` for that document, selectors already resolved; `tags` are the guest's PVE tags (`VM.Audit` only, empty otherwise); without `id`, the caller's read/write on the registry (`read` always, `write` = `Sys.Modify` on `/`) |
| GET | `/meta/prefixes` | — | `[{ prefix, description?, selector, enforce, schema? }]`, most-specific prefix first — what each prefix is and where it applies. A file that did not parse is listed too, as `{ prefix, origin, error }` and nothing else, so it can be found and repaired instead of silently ceasing to exist |
| GET | `/meta/permissions` | — | `[{ name, authid, description?, rules: [{ prefix, mode, selector }] }]` — who may touch which prefix; drives the Access column. A file that did not parse is listed as `{ name, origin, error }`; it grants nothing |
| GET/PUT/DELETE | `/meta/prefixes/{name}`<br>`/meta/permissions/{name}` | same as a document | the file itself as a document (`id: "prefixes/<name>"`). Writes land in the cluster directory, never over a packaged file, and are refused if the result would not parse as a prefix definition/permission file. `Sys.Modify` on `/` to write |
| GET | `/meta/schemas` | — | `{ prefix, permission }` — the two registry file formats described as schemas, which is what lets the editor show a prefix file as a typed tree |

PUT and DELETE 404 for a vmid absent from the vmlist; GET of such a vmid is 404 too.
`data` is a JSON-encoded string parameter; permissions and guest lists cross the Perl/Rust
boundary as native hashes/arrays, not JSON strings.

`perl/PVE/API2/Ext/Meta.pm` is a thin `PVE::RESTHandler` over the Rust core
(`pve_meta_core::api`) through the perlmod bindings (`PVE::RS::Meta`).

## The editor

A **Metadata** tab appears on every LXC/QEMU guest's config panel, and one on the
Datacenter panel for the registry.

On a guest it is one tree of the document the caller can see. Rows are the union of the
keys present and the keys the governing prefix definition declares; an unset declared key
renders greyed with its default and a **Set to default** button, which is the only thing
that ever writes one. Columns: key, value (an editor chosen by the value's shape — inline
for a scalar, a text box for a string with newlines, Monaco for a map or an array of
maps), description (the row's own `k__` comment key) and access (every rule whose prefix
covers the row). A row whose value does not match its schema is marked amber in place.

A schema is advisory: the editor marks a value that does not match it and asks for a
"Save anyway" tick before applying, but the server stores whatever passes its one lint.
A prefix that says `enforce: true` turns that tick into the rule: an API write that would
leave the prefix's subtree not matching its schema, for the paths the write changed, is
refused with a 422 unless the request carries `force=1`, which is what the tick sends.
It is available to anyone who may write, so a drifted schema never locks anyone out; it
makes a mismatch a deliberate act. Format checks (`ipv4`, `dns-name`, ...) are never
enforced, since only the editor has PVE's validators for them.

Edits are **staged**, not written one key at a time: the tree shows the document as it
would be, a staged row renders like a pending PVE config change (the stored value, then
the pending one beneath it in `darkorange`), and **Apply** sends the lot as one
`PUT ?view=<narrowest covering path>&mode=replace`, diff-confirmed. That is what makes a
change like "this prefix applies to a tag rather than to every guest" possible at all —
dropping `all` and adding `tag` are each refused on their own, because a definition's
selector is exactly one of the two. Editability is per row, from `/meta/access`.

On the Datacenter panel it is two sub-tabs, **Prefixes** and **Permissions** — two
grids over `/meta/prefixes` and `/meta/permissions`, with columns a tree could not show (which guests a prefix reaches,
whether it carries a schema, and whether the file is a package's or yours on top of one).
Editing a row opens that file in the same document editor, because a prefix definition is
a document like any other. **Create Service Token** on the Permissions list makes the
principal an operator needs — a `pve` user that cannot log in, one token on it, and a
permission file naming that token with no rules — and nothing else; **Add Rule** fills in
what it may touch.

A **local CLI** for hook scripts, `pve-meta get <vmid> [<view>]`, reads `/etc/pve/meta`
directly: no ticket, no token, no pveproxy, so it works during boot. `pve-meta ls`
lists documents (`--orphans`: only those whose guest is gone), and `pve-meta rm <vmid>`
removes an orphan's files under the document's write lock. A scalar prints bare,
anything with structure prints YAML, and exit status 2 means "not there" — which is what
lets a hook script tell "nothing configured" from "something is broken". See
`examples/maintenance-hook.pl`, which refuses to start a guest its metadata says is under
maintenance: no operator, no token, no daemon.

The tab is `ui-extjs/`: plain JavaScript, a native `Ext.tree.Panel` mounted through
pve-ext's `script`+`xtype` manifest form, so session, CSRF, theme and i18n all come from
the PVE UI. No iframe. The rules it needs -- the YAML codec, key names, access, which
prefix governs a path, what a schema says, what staged edits do -- are the server's own
crate compiled for the browser (`crates/pve-meta-wasm`, a plain `cargo build` for
wasm32, no wasm-bindgen), so the editor reimplements none of them; see
`docs/WASM-CORE.md`. A second implementation in pwt/Yew was built to the same spec and
compared on the lab before being removed — `docs/DESIGN.md` §8 and §11, git tag
`pwt-ui-removed`.

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
  best-effort and warn; metadata never breaks a guest operation. The one case the hooks
  cannot see is a guest config deleted out of band: `pve-meta ls --orphans` lists what
  that left behind and `pve-meta rm <vmid>` removes it. Nothing sweeps on a timer.
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
  instantiated in place (`script` + `xtype`). pve-meta ships two in the
  `script`+`xtype` form, `pages/pve-meta.json` (guests) and `pages/pve-meta-dc.json`
  (the registry lists on the Datacenter panel), over one script — a manifest carries a
  single `xtype`, and the two tabs are different panels.
* **Managed patches.** `pve-ext-patch` applies, verifies, removes and reports a set of
  dpkg-diverted file patches described by TOML manifests; pve-meta ships one
  (`patches/lifecycle.toml`) for the five guest-lifecycle hooks.

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
`perl -c` reporting `syntax OK`), creates `/etc/pve/meta.d/{prefixes,permissions}` when `/etc/pve`
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
| A prefix or permission file | every authenticated user | `Sys.Modify` on `/` |

A permission file (above) additionally grants a prefix, read-only or read-write, on the
guests its selector matches, to a principal that may hold no VM privilege at all.

## Building

Build host: Debian 13 (trixie) with a [rustup](https://rustup.rs/) toolchain under
`~/.cargo/bin` (not the distro `cargo`/`rustc` packages) with the `wasm32-unknown-unknown`
target added, `libperl-dev` for the perlmod crate, and `npm` to vendor Monaco (only
`pve-meta-core` and `pve-meta-wasm` build on macOS — develop the rest on Linux and
`rsync` over). `make build` builds `crates/pve-meta-perl` and the editor's `.wasm`
(`make wasm` on its own), `make ui` fetches Monaco into `ui-extjs/monaco/vs`, and
`make deb` builds
`pve-ext` (its own source package) plus `pve-meta` and `libpve-meta-rs-perl`, dropping
all three `.deb`s next to each other in the parent directory. See `docs/BUILD.md` for
the exact rsync/ssh incantation and the safe way to replace the installed `.so` on a
live node.

## Repo layout

| Path | What |
|---|---|
| `crates/pve-meta-core` | Document model, views, prefixes/permissions/selectors, lint, api layer, store — pure Rust |
| `crates/pve-meta-perl` | `PVE::RS::Meta` — perlmod bindings: lifecycle hooks, the `api_*` functions |
| `crates/pve-meta-wasm` | The same core for the browser: a JSON-string ABI over `wasm32-unknown-unknown`, loaded by the editor |
| `perl/PVE/API2/Ext/Meta.pm` | The native API module, thin over `PVE::RS::Meta` |
| `prefixes/` | Packaged example prefixes (none required) |
| `ui-extjs/` | The editor tab: plain JS, a native `Ext.tree.Panel` |
| `pve-ext/` | The extension layer: API-module loader, UI-page loader, `pve-ext-patch` (own package) |
| `patches/` | `lifecycle.toml`, the managed-patch manifest, and `lifecycle/` holding the one guest-lifecycle diff it names |
| `pages/` | The "Metadata" tab's two page manifests (guest, and the registry lists on the Datacenter panel) |
| `debian/` | The `pve-meta` source package: `control`, triggers, systemd units, `postinst`/`prerm` |
| `docs/` | `DESIGN.md` (authoritative), `design/`, `BUILD.md`, `DISTRIBUTION.md`, `LIFECYCLE-PATCHES.md`, `WASM-CORE.md` |
| `scripts/apt-repo/` | Signed apt repo build/publish scripts (Cloudflare R2) |
| `scripts/watch-pve/` | The ceiling-watcher CI job |
| `ceilings.toml` | Tested-ceiling versions for the two patched upstream packages |

## License

AGPL-3.0-or-later for this project's own code (every crate inherits
`license.workspace = true` from the root `Cargo.toml`); the vendored editor assets
(Monaco) carry their own upstream MIT/Apache-2.0/OFL-1.1 licenses — see
`debian/copyright` for the full, per-file breakdown.

---

Built with [Claude Code](https://claude.com/claude-code).
