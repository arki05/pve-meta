# pve-meta — design (revision 5)

Revision 5 supersedes revision 4 (`DESIGN-rev4.md`, kept for the record) and the
review-driven decisions in its §8–§9. It follows `../DIRECTION.md` (2026-09-08) with two
deviations recorded in §11. It is the single authority for the code; where code and this
document disagree, the code is wrong.

## 1. Threat model

This is a trusted, single-administrator environment: a homelab cluster. The principals
are the administrator and the service API tokens that same administrator issued.
pve-meta holds configuration intent, not secrets. Scopes are a **blast-radius limiter**,
not an access-control system: their job is that an operator with a bug writes garbage
into its own prefix instead of everyone's, and that a principal sees the part of a
document it cares about. They must be correct; they are not adversarially hardened.

In scope: a scoped principal does not read or write document content outside its
granted prefixes; a concurrent write does not silently lose another's update.
Out of scope: key-name disclosure through errors, digests or listings; any defence
against a principal the administrator deliberately issued a token to. pve-meta gates
nothing but access to metadata documents; it never restricts a PVE permission the
platform itself grants.

## 2. The feature

Every guest (vmid) and the datacenter have one **document**: a nested key-value tree
stored as YAML in `/etc/pve/meta/<vmid>.yaml` (`datacenter.yaml`). A document is the JSON
data model with ordered maps and no nulls. A key ending in `__` is a **comment key**: a
string note about its sibling (`host__` documents `host`, a bare `__` documents the
map). Comment keys are ordinary data; the UI renders them as the description of the
row they document. No key is reserved in any document.

A caller reads or writes a document through a **view**: a key-path prefix (dotted, any
depth, through maps only). A view of `traefik` is the subtree under `traefik`, returned
with the prefix stripped. Views are also the unit of access.

## 3. Registrations, scopes, grants

Access-control data lives **outside** the documents, one file per principal, in a drop
directory: `/etc/pve/meta.d/operators/<name>.yaml` (cluster-wide, pmxcfs) with packaged
defaults in `/usr/share/pve-meta/operators/<name>.yaml` (a cluster file overrides the
packaged file of the same name). One format serves scopes and operator registration:

```yaml
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
  warning and grants nothing. Prefixes are non-empty. `authid` is a PVE user or token id.
* A **selector** restricts a scope to guests: `all`, or `tag: <t>` (the guest carries the
  PVE tag). Tag membership is read from the cluster's cached guest properties. Adding the
  tag is the deliberate act of granting the operator that guest.
* Scopes apply to **guest documents only**. The datacenter document is governed by ACLs
  alone.
* A scope on prefix `p` covers the subtree `p` and the sibling comment key `p__`. That
  is the only comment-key rule.

Grants for a caller on a guest document:

* `full_read` = `VM.Audit` on `/vms/<vmid>`, `full_write` = `VM.Config.Options`
  (datacenter: `Sys.Audit` / `Sys.Modify` on `/`).
* `scopes` = the union of scope entries from registrations whose `authid` is the caller
  and whose selector matches the guest.
* Reading view `P` requires full read or a scope covering `P`; writing requires full
  write or a `rw` scope covering every touched path; a write to the root view requires
  full write. A read by a caller with no grant at all is 403. Authorization is decided
  from the request and a plan computed against a copy, never from a diff of stored data.

## 4. Documents on the wire

* Booleans in `data` are rendered as `1`/`0`, the PVE API convention (perlmod and PVE's JSON encoder both do this); a grammar's declared type disambiguates them in the UI, and `format=yaml` carries exact types for clients that need them.
* Reads: `data` (JSON object, unordered) or `text` (YAML, the file's own text for the
  root view, a canonical dump for a sub-view). Key order is preserved in the file and is
  not a wire contract; the UI sorts.
* Writes: `data` (JSON string) or `text` (YAML) with `mode=replace` (the view's subtree
  is replaced; `{}` stores an empty map) or `mode=merge` (merge-patch, `null` deletes; a
  merge that touches nothing changes nothing). One lint runs on the planned document;
  the write is refused with a 400 that names the offending path. `digest` is the
  optional expected file digest (409 on mismatch); `dry_run=1` plans and validates
  without writing.
* Unparsable file: `format=yaml` returns the raw text plus `parse_error` (so an
  administrator can repair it); `format=json` returns 422 with the parse error; a root
  `replace` by a full writer repairs it. This is a per-document condition, never
  cluster-wide.
* Writes run under `PVE::Cluster::cfs_lock_domain("pve-meta-<id>")` with the digest
  check inside the lock; files are written atomically with node/pid/seq-unique temp
  names. Errors name paths; there is no disclosure filtering.

## 5. API (native, `/api2/json/meta`, served by pveproxy/pvedaemon)

| Method | Path | Params | Returns |
|---|---|---|---|
| GET | `/meta/version` | — | `{ token, changed }` — content hash over the store; poll it |
| GET | `/meta/guests` | `has` (prefix) | `[{ vmid, node, type, name, tags: [..], digest }]` for every guest in the vmlist the caller can read something of; `node`/`name`/`tags` only with `VM.Audit`; `digest: ""` when no document |
| GET | `/meta/guests/{vmid}` | `view`, `format` = `json` (default) or `yaml` | `{ id, view, digest, data }` or `{ id, view, digest, text, parse_error? }` |
| PUT | `/meta/guests/{vmid}` | `view`, `data` or `text`, `mode`, `digest`, `dry_run` | `{ id, view, digest, touched: [{ path, op }] }` |
| DELETE | `/meta/guests/{vmid}` | `view`, `digest` | removes the subtree, or the whole document |
| GET/PUT/DELETE | `/meta/datacenter` | same | same with `id: "datacenter"` |
| GET | `/meta/access` | `vmid` or `dc=1` | `{ read, write, scopes: [{ prefix, mode }] }` for that document (selectors already resolved); without either, the caller's datacenter read/write |
| GET | `/meta/operators` | — | `[{ name, authid, description, scopes: [{ prefix, mode, selector, grammar? }] }]` — all registrations, readable by every authenticated user |

PUT and DELETE return 404 for a vmid that is not in the vmlist; GET of such a vmid is
404 too. Reads run in pveproxy, writes are `protected` (pvedaemon). Parameters follow
PVE conventions; `data` is a JSON-encoded string parameter.

Implementation: `perl/PVE/API2/Ext/Meta.pm` is a thin `PVE::RESTHandler` over
`pve_meta_core::api` through the perlmod bindings. **Grants, guest lists and results
cross the Perl/Rust boundary as native hashes/arrays**, never as JSON strings; only the
client-supplied `data` parameter is a JSON string, decoded once in Rust.

## 6. Guest lifecycle

* **Snapshots**: `pct/qm snapshot`, `rollback` and `delsnapshot` copy, restore and remove
  `/etc/pve/meta/<vmid>.<snapname>.yaml` through one patched file,
  `PVE/AbstractConfig.pm` (package libpve-guest-common-perl), calling
  `PVE::RS::Meta::on_snapshot/on_rollback/on_delsnap`.
* **Destroy**: no hook. A GC (`PVE::RS::Meta::gc`, run by a systemd timer on every node
  under the cluster lock) removes documents and snapshot copies whose vmid is no longer
  in the vmlist. There is no orphan concept in the API.
* **Clone and backup**: not carried. Documented: "metadata lives in `/etc/pve`; back up
  `/etc/pve`". The QEMU backup command cannot embed foreign blobs, so a partial guarantee
  is not offered.

## 7. Extension seams (`pve-ext`)

Unchanged in substance: one `<script>` line in `index.html.tpl` loading the page loader,
two lines at the end of `PVE/API2.pm` calling `PVE::API2::Ext->register_all`, the
manifest-driven `pve-ext-patch`, and page manifests in `/usr/share/pve-ext/pages/`.
**New:** a page manifest may declare either `url` (a same-origin iframe) or `script` +
`xtype` (a native ExtJS panel class defined by that script and instantiated as the tab).
Both substitute the same placeholders; `requires` gating applies to both.

## 8. The UI: one tree of the document

The page shows one tree of the document the caller can see: rows are the union of the
keys present and the keys the applicable grammars declare (declared-but-unset rows are
greyed with default and description and a "set" action). Columns: key, value (inline
editor by type: string, integer/number, boolean, enum; arrays as one text leaf),
owner (the registration whose scope covers the row, from `/meta/operators`, plus the
selector). Comment keys are shown as the row description, not as rows. Editability is
per row from `/meta/access`. A row edit is `PUT ?view=<path>&mode=replace` with the
scalar; add is the same at a new path; delete is `DELETE ?view=<path>`. The digest is
sent and 409 reloads. The version poll refreshes the tree and the grants.

Monaco has two jobs: "edit subtree as text" (YAML/JSON toggle, presentation only) with
diff-confirmed apply, and the diff dialog itself.

Two implementations are built and compared on the lab cluster, then one is kept:

* `ui/` — pwt/Yew, `DataTable` over a `TreeStore` (the PDM pattern), same-origin iframe.
* `ui-extjs/` — plain JavaScript, `Ext.tree.Panel` with columns, a native tab through the
  `script`/`xtype` manifest form; session, CSRF, theme and i18n come from the PVE UI.

Both ship as page manifests ("Metadata", "Metadata (ExtJS)") during the comparison.

## 9. Repository layout

```
crates/pve-meta-core     document model, views, scopes+selectors+registrations, lint, api layer, store, gc
crates/pve-meta-perl     PVE::RS::Meta: snapshot hooks, gc, api exports (native perlmod conversion)
perl/PVE/API2/Ext/Meta.pm
operators/               packaged example registrations (none required)
patches/                 lifecycle.toml + libpve-guest-common-perl_AbstractConfig.pm.diff (one file)
pve-ext/                 the extension layer (own package)
ui/  ui-extjs/           the two editor implementations
debian/, Makefile        packages: pve-ext, pve-meta, libpve-meta-rs-perl
```

## 10. What revision 5 deletes

Reserved `scopes` key and every rule keyed on it (opaque-leaf addressing, touched-path
collapsing, `check_scopes_write`, authid key lint); the strict/lenient scope parser
split; the per-request datacenter read for grants; `WriteGate`, `lint_at`/`lint_relaxed*`,
the lint-finding subset check; `may_name` and all message redaction; the `keys` wire
field and the UI's YAML key scanner; the comment-key access machinery (`covers` aliasing
beyond the one sibling rule, bare-`__` prefix rule, mid-path rejection, `filter`'s comment
pass); orphan listing/deletion/access rules; the destroy, clone and backup hooks and
their six diffs; JSON-string crossings for grants, guest lists and results
(`_grants_json`, `_inflate_view`, `parse_grants`, `data_json`); the "View as" selector.

## 11. Deviations from DIRECTION.md, with reasons

* **Lifecycle is snapshot-only, not zero.** Rollback restoring metadata was an explicit
  product decision; it costs one patched file in the least-churned package.
* **The pwt implementation is not dropped by fiat.** pwt has a tree grid (`DataTable` +
  `TreeStore`, used by PDM), which was the doc's premise for switching; both
  implementations are built and judged on the lab instead.
