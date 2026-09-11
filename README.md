# pve-meta

A structured metadata store for Proxmox VE. Every guest (vmid) gets one nested key-value
document, stored as YAML under `/etc/pve/meta` and replicated by pmxcfs like the rest of
the cluster config. It is reachable through a native API on port 8006
(`/api2/json/meta`), a tree editor on every guest's **Metadata** tab, and a local CLI for
hook scripts. A schema can describe any part of a document, and an API token can be
scoped to a part of it.

Three packages: `pve-ext` (a generic extension layer for PVE, its own package), and
`pve-meta` + `libpve-meta-rs-perl` (this project). The rules live in one Rust crate,
`pve-meta-core`, which the API calls through perlmod and the editor runs as wasm, so
there is one implementation of every rule.

## What it looks like

A document, `/etc/pve/meta/105.yaml`:

```yaml
backup:
  schedule: "03:00"
  schedule__: local time, cron-ish, interpreted by whatever reads this key
  retention: 7
traefik:
  spec: { host: web.example, port: 8080 }
```

A key ending in `__` documents its sibling; the editor shows it as the row's
description. A caller reads or writes through a **view**, a dotted key-path prefix:
`GET /meta/guests/105?view=traefik` returns the `traefik` subtree and nothing else.

A **prefix** says what a prefix is — `/etc/pve/meta.d/prefixes/traefik.yaml`, or a
packaged default under `/usr/share/pve-meta/prefixes/`; the file name is the prefix:

```yaml
description: Traefik dynamic configuration
selector: { tag: traefik }   # or { all: true }: which guests it reaches
enforce: true                # optional: refuse an API write that breaks the schema
schema:                      # optional, PVE::JSONSchema dialect
  type: object
  properties:
    spec:
      type: object
      properties:
        host: { type: string, format: dns-name }
        port: { type: integer, minimum: 1, maximum: 65535, default: 80 }
```

A **permission** says who may touch one — `/etc/pve/meta.d/permissions/traefik.yaml`,
cluster-only, so a package can declare a prefix but never grant itself access:

```yaml
authid: svc@pve!traefik
rules:
  - { prefix: traefik, mode: rw, selector: { tag: traefik } }
```

Full read of a guest's document is `VM.Audit` on the guest, full write is
`VM.Config.Options`; a permission adds a prefix on the guests its selector matches. A
write is authorized by what it changes, not by the view it names. All of this, precisely,
is [`docs/DESIGN.md`](docs/DESIGN.md); why it is so is [`docs/decisions/`](docs/decisions/README.md).

## API

`GET /meta/version` (poll it), `GET /meta/guests`, `GET|PUT|DELETE /meta/guests/{vmid}`
with `view`, `format`, `mode=replace|merge`, `digest`, `dry_run`, `force`;
`GET /meta/access`; `GET /meta/prefixes`, `GET /meta/permissions` and the two files as
documents at `/meta/prefixes/{name}` and `/meta/permissions/{name}`; `GET /meta/schemas`.
The table is in `docs/DESIGN.md` §8. Every write that changes a file leaves a syslog line
tagged `pve-meta audit:`.

## The CLI

`/usr/sbin/pve-meta`, for root on the node — no ticket, no pveproxy, so it works in a
hook script during boot:

```sh
host=$(pve-meta get 105 traefik.spec.host)          # a scalar prints bare; exit 2 = not there
pve-meta get 105 traefik                             # structure prints YAML
pve-meta set 105 traefik --data '{"spec":{"host":"web.example"}}'
pve-meta merge 105 traefik --text 'spec: {port: 8080}'
pve-meta delete 105 traefik.spec.port
pve-meta ls --orphans && pve-meta rm 999500          # files whose guest is gone
```

Writes go through the same code as the API (lint, digest check, enforced schemas,
audit) under the same cluster lock, and skip only permissions. See
`examples/maintenance-hook.pl`.

## The editor

A **Metadata** tab on every LXC/QEMU guest: one tree of the document, with the keys the
governing prefixes declare shown greyed with their defaults. Edits are staged and one
Apply writes them; a Tree | Text toggle and an "Edit selection as text" window give
Monaco over the same document. Schema mismatches are marked, and Apply asks for a "Save
anyway" tick before storing one. The Datacenter panel's Metadata tab lists the prefixes
and permissions and edits them in the same editor. `ui-extjs/` is plain JavaScript with
no build step; the rules it needs come from `crates/pve-meta-wasm` (`docs/WASM-CORE.md`).

## Lifecycle

One patched file, `PVE/AbstractConfig.pm` (`libpve-guest-common-perl`), carries the
hooks: a document is cleared when a vmid is created afresh, removed when the guest is
destroyed, and copied, restored and removed with snapshots. Migration needs nothing.
Clone and backup are not carried: back up `/etc/pve`. See `docs/LIFECYCLE-PATCHES.md`.

## How it plugs into PVE

Everything that touches pve-manager goes through `pve-ext`'s three seams: an API-module
loader (one line in `PVE/API2.pm`), a UI-page loader (one `<script>` line in
`index.html.tpl`), and `pve-ext-patch`, which applies, verifies, removes and reports
dpkg-diverted patches from TOML manifests and re-applies them when the patched package
is reshipped. pve-meta ships two page manifests and one patch manifest.

## Install

There is no published apt repository yet (`docs/DISTRIBUTION.md` documents the
pipeline). Build the three `.deb`s (below) and install in dependency order:

```sh
dpkg -i pve-ext_*.deb
dpkg -i libpve-meta-rs-perl_*.deb pve-meta_*.deb
```

`pve-meta`'s `postinst` applies the lifecycle patch (dpkg-divert, gated on `perl -c`),
creates `/etc/pve/meta.d/{prefixes,permissions}` when `/etc/pve` is mounted, and
restarts `pvedaemon`/`pveproxy`. The patch step never fails install; a trigger re-runs it
when `libpve-guest-common-perl` reships the file. `pve-ext-patch status` shows what is
applied. `dpkg -r pve-meta` restores the pristine file and leaves `/etc/pve/meta/*`
alone: that is guest data.

## Building

Debian 13 with a rustup toolchain (`wasm32-unknown-unknown` target added), `libperl-dev`,
and `npm` to vendor Monaco. `make build` builds the perlmod crate and the editor's
`.wasm`; `make ui` fetches Monaco; `make deb` builds all three packages into the parent
directory. Only `pve-meta-core` and `pve-meta-wasm` build on macOS. Gates: `make check`
(clippy, rustdoc, the wasm smoke suite), `make test`, `make check-perl` (shellcheck and
`perl -c` against stub PVE modules), `make -C crates/pve-meta-perl check` (the Perl
boundary suite, Linux only). `docs/BUILD.md` has the details and the safe way to replace
the installed `.so` on a live node.

## Repository layout

| Path | What |
|---|---|
| `crates/pve-meta-core` | The rules: model, views, prefixes, permissions, shape, edit set, store, API layer |
| `crates/pve-meta-perl` | `PVE::RS::Meta`, the perlmod bindings |
| `crates/pve-meta-wasm` | The same core for the browser |
| `perl/PVE/API2/Ext/Meta.pm` | The REST module |
| `bin/pve-meta` | The CLI |
| `ui-extjs/` | The editor and its tests |
| `pve-ext/` | The extension layer (own package) |
| `pages/`, `patches/`, `prefixes/` | Page manifests, the lifecycle patch, packaged example prefixes |
| `docs/` | `DESIGN.md` (the spec), `decisions/`, `BUILD.md`, `DISTRIBUTION.md`, `LIFECYCLE-PATCHES.md`, `WASM-CORE.md` |
| `scripts/` | apt-repo publishing, the ceiling watcher, Perl stubs for `perl -c` |

## License

AGPL-3.0-or-later for this project's own code; the vendored Monaco carries its own
licenses. See `debian/copyright`.
