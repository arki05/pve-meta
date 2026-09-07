# pve-meta — design (revision 4)

This document replaces `API.md`, `NATIVE-API-SPEC.md`, `UI-SPEC.md` and `DAEMON-SPEC.md`.
It describes the one feature this project builds, the two generic seams it needs in
Proxmox VE, and nothing else.

## 1. The feature

Every guest (vmid) and the datacenter have one **document**: a nested key-value tree
stored as YAML in `/etc/pve/meta/<vmid>.yaml` (`datacenter.yaml`). A document is the
JSON data model with ordered maps, no nulls, and **comment keys**: a key ending in `__`
is a human note about its sibling (`host__` documents `host`, a bare `__` documents the
map). Comment keys are ordinary data and travel with the document everywhere.

A caller reads or writes the document through a **view**: a key-path prefix. A view of
`traefik` is the subtree under `traefik`, returned with the prefix stripped, in JSON or as
YAML text. Views are also the unit of access: a principal (PVE user or API token) can be
granted read or read-write on a prefix, and then sees and edits only that part of every
document. That is the whole feature: a per-guest key-value tree, and prefix-scoped views
of it. Nothing about namespaces, operators, or forms.

## 2. Access

Two grant paths, evaluated per request:

1. **PVE ACLs** (full access): `VM.Audit` on `/vms/<vmid>` reads the whole document,
   `VM.Config.Options` writes it. Datacenter document: `Sys.Audit` / `Sys.Modify` on `/`.
2. **Scopes** (partial access): entries in the datacenter document, keyed by authid:

   ```yaml
   scopes:
     svc@pve!traefik:
       - prefix: traefik
         mode: rw
       - prefix: netbird
         mode: ro
   ```

   A scope grants that principal the prefix on **every** guest document (no per-vmid
   scoping in this revision), without needing any VM privilege. Scopes never restrict a
   principal that already has full access through ACLs.

Reading a view `P` requires read on `P` (ACL, or a scope whose prefix is a prefix of `P`).
Writing requires write on every touched path. A read without `view` returns the union of
the caller's readable subtrees (full document for ACL holders). A non-existent document
is an empty document with digest `""`; there is no explicit create.

Only principals with `Sys.Modify` on `/` may edit `scopes` (it lives in the datacenter
document, so that rule falls out of the datacenter write permission; additionally any
write touching `scopes` is refused for scope-granted principals).

## 3. API (native, `/api2/json/meta`, served by pveproxy/pvedaemon)

Reads run in pveproxy, writes are `protected` and run in pvedaemon. Parameters follow
PVE conventions (form/JSON parameters; nested values are JSON-encoded strings).

| Method | Path | Params | Returns |
|---|---|---|---|
| GET | `/meta/guests` | `has` (prefix filter) | `[{ vmid, node, type, name, digest, keys: [top-level keys visible to the caller] }]` — every guest in the vmlist, `digest: ""` when no document |
| GET | `/meta/guests/{vmid}` | `view` (prefix, optional), `format` = `json` (default) or `yaml`, `comments` (default 1) | `{ vmid, view, digest, data }` or `{ vmid, view, digest, text }` |
| PUT | `/meta/guests/{vmid}` | `view` (optional), exactly one of `data` (JSON string) or `text` (YAML) — the format follows from which one is given, `mode` = `replace` (default: the view's subtree is replaced by the payload) or `merge` (merge-patch; `null` deletes), `digest` (expected file digest, optional), `dry_run` | `{ vmid, view, digest, touched: [{ path, op: set|delete }...] }`; 409 on digest mismatch, 403 if any touched path is outside the caller's write scopes, 400 on invalid content |
| DELETE | `/meta/guests/{vmid}` | `view` (optional), `digest` | removes the subtree (or the whole document) |
| GET/PUT/DELETE | `/meta/datacenter` | same as guests | same shapes with `id: "datacenter"` |
| GET | `/meta/access` | — | the caller's effective grants: `{ full: [vmids or "*"], scopes: [{prefix, mode}] }` (what the UI's "view as" offers) |
| GET | `/meta/version` | — | `{ token }` — content hash over the store; poll it |

Implementation: `perl/PVE/API2/Ext/Meta.pm` is a thin `PVE::RESTHandler` over the Rust
core through the perlmod bindings (`PVE::RS::Meta`): view extraction, prefix stripping,
merge/replace, touched-path computation, YAML/JSON rendering and digesting all happen
in Rust; Perl does parameters, permissions and scope lookup.

## 4. Guest lifecycle

Metadata is part of the guest: snapshot, rollback, delete-snapshot, clone, destroy and
container backup/restore carry it, through one-line calls to `PVE::RS::Meta` inserted
into pve-container, qemu-server, libpve-guest-common and vzdump (see
`LIFECYCLE-PATCHES.md`). QEMU backups through QEMU's own backup command cannot embed
the blob (fixed parameter set); that is a documented gap.

## 5. Two generic seams in Proxmox VE (package `pve-ext`)

Everything that touches pve-manager or the PVE UI goes through one small, reusable
extension layer, so that adding a page or an API module never needs another patch:

* **API modules.** One line in `PVE/API2.pm` (`use PVE::API2::Ext;`) loads
  `PVE::API2::Ext`, which scans `/usr/share/perl5/PVE/API2/Ext/*.pm`, `require`s each
  module and registers it in the API root at the path the module declares
  (`sub ext_path { 'meta' }`). Modules are plain `PVE::RESTHandler` subclasses.
* **UI pages.** One `<script>` line in `index.html.tpl` loads `pve-ext-loader.js`, which
  fetches `GET /api2/json/ext/pages` (served by `PVE::API2::Ext` from
  `/usr/share/pve-ext/pages/*.json`) and adds one tab per manifest to the declared
  targets (`lxc`, `qemu`, `node`, `dc`) as a same-origin iframe:

  ```json
  { "id": "pve-meta", "title": "Metadata", "iconCls": "fa fa-tags",
    "targets": ["lxc", "qemu", "dc"],
    "url": "/pve2/js/pve-meta-ui/index.html?{query}",
    "requires": { "vms": ["VM.Audit"], "dc": ["Sys.Audit"] } }
  ```

  Placeholders `{vmid}`, `{node}`, `{type}`, `{theme}` and `{query}` are substituted by
  the loader; a tab is only added when the logged-in user has the listed privileges
  (checked against the UI's capability map, `PVE.Utils`/`Proxmox.UserName` caps).
* **Managed patches.** `pve-ext-patch` applies, verifies, removes and reports a set of
  dpkg-diverted file patches described by manifests in `/usr/share/pve-ext/patches/*.toml`
  (file, owning package, diff, marker). pve-ext ships its own two-line manifest; pve-meta
  ships the lifecycle manifest. Triggers on the patched paths re-apply after upgrades;
  `perl -c` and template checks gate installation; `remove` restores pristine files.

pve-meta depends on pve-ext. The lifecycle patches are pve-meta's own manifest.

## 6. The editor page

A single page, `/pve2/js/pve-meta-ui/index.html?vmid=<id>|dc=1&theme=…`, built with
pwt exactly the way PDM composes a page (see `design/PDM-DESIGN-LANGUAGE.md`), containing:

* a header line with the document identity (`105 wiki (lxc, node1)` / `Datacenter`),
* a toolbar: **View as** (a dropdown of the prefixes the caller may see: the top-level
  keys plus the caller's scopes; "whole document" first), **Reload**, **Apply**,
  **Discard**,
* a full-height **Monaco** editor with YAML mode showing exactly what
  `GET …?view=<selected>&format=yaml` returns; editing is enabled when the caller may
  write that view; Apply sends `PUT …?view=<selected>&format=yaml&mode=replace` with
  the digest after showing a diff confirmation dialog; a 409 shows a "changed on
  server" notice with Reload,
* a status line for errors (server message verbatim).

No forms, no schema, no namespace buttons. Monaco is loaded from files shipped in the
package and mounted into a container the pwt page provides; its theme follows pwt's
light/dark state.

## 7. Repository layout after this revision

```
crates/pve-meta-core     document model, YAML/JSON (TOML stays supported on disk), views, merge, digest, store
crates/pve-meta-perl     PVE::RS::Meta: lifecycle + api_* functions used by the API module
perl/PVE/API2/Ext/Meta.pm
ui/                      the editor page (pwt + Monaco)
patches/lifecycle/       the Perl diffs + manifest
pve-ext/                 the extension layer (its own Debian package; may move to its own repo)
debian/, Makefile        two packages: pve-meta, libpve-meta-rs-perl (plus pve-ext)
```
