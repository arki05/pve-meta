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
| GET | `/meta/guests` | `has` (prefix filter) | `[{ vmid, node, type, name, digest, keys: [top-level keys visible to the caller], orphan }]` — every guest in the vmlist the caller can read something of, `digest: ""` when no document; `node`/`name` only with `VM.Audit`; documents whose vmid is no longer in the vmlist are listed with `orphan: 1` (and `node`/`type`/`name` null) for callers with datacenter read |
| GET | `/meta/guests/{vmid}` | `view` (prefix, optional), `format` = `json` (default) or `yaml`, `comments` (default 1) | `{ id, view, digest, keys, data }` or `{ id, view, digest, keys, text }` — `keys` is the ordered list of top-level keys of the returned value; `data` is an unordered JSON object. A document whose stored text does not parse answers 200 with `parse_error` (the parser's message), empty `data`/`text`, no `keys`, its real `digest`, and — for a caller with full read — `raw`, the text to repair from (§9) |
| PUT | `/meta/guests/{vmid}` | `view` (optional), exactly one of `data` (JSON string) or `text` (YAML) — the format follows from which one is given, `mode` = `replace` (default: the view's subtree is replaced by the payload) or `merge` (merge-patch; `null` deletes), `digest` (expected file digest, optional), `dry_run` | `{ vmid, view, digest, touched: [{ path, op: set|delete }...] }`; 409 on digest mismatch, 403 if any touched path is outside the caller's write scopes, 400 on invalid content |
| DELETE | `/meta/guests/{vmid}` | `view` (optional), `digest` | removes the subtree (or the whole document) |
| GET/PUT/DELETE | `/meta/datacenter` | same as guests | same shapes with `id: "datacenter"` |
| GET | `/meta/access` | `vmid` or `dc=1` (optional) | `{ read, write, scopes: [{prefix, mode}] }` for that document; without either, the caller's scopes and datacenter read/write; for an orphan vmid, `read`/`write` are `Sys.Audit`/`Sys.Modify` on `/` and `scopes` is empty (§9), matching GET and DELETE; 404 for a vmid that is neither a guest nor an orphan |
| GET | `/meta/version` | — | `{ token, changed }` — content hash over the store and the newest mtime; poll it |

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
* a status line for errors (server message verbatim),
* a fallback rule: if the selected view stops being one of the caller's options (the
  key was deleted, or the covering scope was revoked), the editor falls back to the
  whole document, silently when the buffer is clean and otherwise through the same
  confirmation dialog Discard, Reload and view switching use; the page re-reads its
  access grants on Reload and whenever the store version changes.

No forms, no schema, no namespace buttons. Monaco is loaded from files shipped in the
package and mounted into a container the pwt page provides; its theme follows pwt's
light/dark state.

## 7. Repository layout after this revision

```
crates/pve-meta-core     document model, YAML on disk / JSON on the wire, views, merge, scopes, the
                         authorization-checked api layer (pve_meta_core::api), digest, store
crates/pve-meta-perl     PVE::RS::Meta: perlmod exports of the lifecycle hooks and the api layer
perl/PVE/API2/Ext/Meta.pm
ui/                      the editor page (pwt + Monaco)
patches/lifecycle/       the Perl diffs + manifest
pve-ext/                 the extension layer (its own Debian package; may move to its own repo)
debian/, Makefile        two packages: pve-meta, libpve-meta-rs-perl (plus pve-ext)
```

## 8. Decisions from the 2026-09-07 review (binding)

These resolve the under-specified corners the review found (`REVIEW-2026-09-07.md`).

* **Authorization is decided from the request, never from a diff.** A write must satisfy
  `can_write(view)` before anything is computed, and every path the planned mutation
  touches is checked on a *plan* computed against a copy; the stored document is only
  modified after the check passes. A caller without full write may not write the root
  view. 403 messages never name a path the caller cannot read.
* **Reads require a grant.** A principal with neither an ACL nor a scope covering
  anything in a document gets 403, not an empty document. `GET /meta/guests` lists
  only guests the caller can read something of; `name` and `node` are included only
  with `VM.Audit` on that guest.
* **`merge` with `null` deletes; `replace` with `{}` stores an empty map.** Deleting a
  view is `DELETE …?view=`. Payload lint allows `null` only as a merge delete marker.
  A merge that touches nothing changes nothing (no container creation).
* **Comment keys follow their subject.** A scope on prefix `p` also covers the sibling
  comment key `p__`; a view of `p` contains only the subtree of `p`.
* **`GET /meta/access`** takes an optional `vmid` and returns `{ read, write, scopes }`
  for that guest (or for the datacenter with `dc=1`); without either it returns the
  caller's scopes and whether they have datacenter read/write.
* **Key order is guaranteed in YAML text only.** `data` (JSON) is an unordered object
  on the wire; clients that care about order use `format=yaml`.
* **`scopes` is validated at write time** (400 naming the entry) and read leniently:
  a malformed entry for another principal is skipped with a warning and never denies
  service to anyone else.
* **Writes are serialised.** Every API write runs under `PVE::Cluster::cfs_lock_domain`
  keyed by document id, with the digest check inside the lock. Lifecycle hooks run
  inside PVE's own guest locks and copy files; a rollback may replace an in-flight edit,
  which the editor detects through the digest.
* **The API never creates documents for guests that do not exist** (404 from the vmlist),
  and `DELETE` of a document removes only the current document; snapshot copies are
  handled exclusively by the lifecycle hooks.
* **YAML only on disk.** TOML/JSON on-disk support, `convert`, format settings, the
  format-preserving edit engine and other unreachable core code are removed. Digest
  and version are computed from file content on every call (no mtime cache).
* **Extension registration happens after the core.** `PVE::API2::Ext` registers its
  modules from an explicit call at the end of `PVE/API2.pm`, skips paths that already
  exist, and never dies.
* **Managed patches: no fuzz, no stacking.** `patch -F0`; a manifest whose file is already
  diverted for another manifest is refused with a clear error (documented limitation).
* **Service restarts** in maintainer scripts go through `deb-systemd-invoke
  reload-or-try-restart`.

## 9. Decisions from the 2026-09-08 review (binding)

* **`scopes` is admin-only, whatever the scope grants.** A write that touches `scopes`
  requires `full_write` on the datacenter document; no scope (including a broad one)
  can grant it. Scope prefixes must be non-empty: full access is only ever granted
  through PVE ACLs, never through an empty-prefix scope.
* **`scopes` keys are PVE authids** (validated with the authid rule, dots allowed) and the
  `scopes` map is an opaque leaf for path addressing: a view may target `scopes` as a
  whole, never a single entry. Entries are added or removed by writing the map.
* **Reads never lint, and never fail on a document's own content.** The store reads and
  returns what is on disk. Text that does not parse as YAML at all is reported *per
  document* — `parse_error` plus the empty value, and `raw` for a caller with full read —
  never as an error, because `api::grants` reads `datacenter.yaml` on every guest request
  and both write handlers read a document before planning: a fatal parse was a
  cluster-wide 400 for every principal, root included, that also blocked its own repair.
  An unparseable (or oversized) document is repaired by replacing it whole — `PUT` with no
  `view` and `mode=replace`, or `DELETE` — and every narrower write against it is refused
  with 400, since the value planned against is the empty document and a narrower write
  would silently discard the file. A malformed `scopes` container or entry, and an
  unparseable `datacenter.yaml`, are skipped with a warning and grant nothing.
* **Strict lint runs only on the content being written.** A caller who may replace the
  whole document is gated on the whole document; anyone else is gated on the subtree
  their view writes, so one out-of-band bad key elsewhere denies nobody. A 400 from lint
  obeys the same disclosure rule as a 403: findings the caller may read are named, the
  rest are counted.
* **A document that is not a map at the top level is the empty document for a scope-only
  reader** and is returned as-is to a full reader; the write gate refuses to store it
  again. A view prefix addresses only through maps, so no scope covers any part of such
  a document.
* **`scopes` is a reserved top-level key in every document**, not only the datacenter one:
  one key rule (PVE authids, dots allowed), one addressing rule (an opaque leaf — `touched`
  reports `scopes`, never `scopes.<authid>`), one scope-prefix rule, everywhere. Only the
  datacenter document's `scopes` map *means* anything, and only there does a write to it
  additionally require `Sys.Modify`.
* **A scope prefix is non-empty, never inside `scopes`, and never a bare `__`** — checked
  both where the datacenter document is written and where a `grants` value crosses into
  the core. A bare `__` documents the map it sits in and is readable only where that map
  is; `prefix: a__` (the note about `a`) stays legal.
* **Documents have a read size cap** (4 MiB, eight times the write limit): a larger file
  is refused with a clear error rather than parsed and hashed on every request, and can
  still be replaced or deleted whole.
* **The bare `__` comment of a map is visible only when the map itself is readable**
  (a scope on `p` covers `p`, `p__` and everything below `p`, nothing above it).
* **Orphans** (documents whose vmid is no longer in the vmlist) are listed by
  `GET /meta/guests` with `orphan: true` for callers with datacenter read, and may be
  deleted by callers with datacenter write. Nothing else may write them. An orphan's
  read/write authority is computed **purely from `/`** (`Sys.Audit` / `Sys.Modify`): there
  is no guest, so a `/vms/<vmid>` ACL left behind by a destroy that never ran
  `remove_vm_access` confers nothing, and scopes do not apply. `GET /meta/access?vmid=`
  answers for an orphan with exactly those two flags and an empty `scopes` — the same
  answer its GET and DELETE act on — rather than 404.
* **`GET /meta/access` and the single-document GET** are documented as implemented:
  `{ read, write, scopes }` with optional `vmid`/`dc`; documents return `id`, `view`,
  `digest`, ordered `keys`, and `data` or `text`. `GET /meta/version` returns
  `{ token, changed }`.
* **lintian runs in every `deb` target.**
