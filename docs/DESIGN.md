# pve-meta — design

The contract the code is held to. It states the model, the invariants and the non-goals;
edge cases live in tests, named for what they pin. A behaviour that is neither an
invariant here nor a test is not a feature.

## 1. What it is, and for whom

One YAML document of metadata per guest, in the cluster filesystem, with an API, a CLI,
an editor tab and lifecycle hooks, so that integrations (a reverse proxy, a compose
runner, a GPU picker) keep their per-guest configuration next to the guest.

The environment is a **trusted, single-administrator cluster**. Whoever reaches the API
with a token is the administrator or a service the administrator runs. Security code
exists only to stop accidents.

**Invariants** (each names the accident it prevents):

1. A write carries a digest and is refused on mismatch: no lost update.
2. Writes run under `cfs_lock_domain("pve-meta-<id>")`, API and CLI alike, and files are
   written by rename: no torn file, no interleaved writers.
3. No pmxcfs, no answer: an unmounted `/etc/pve` is an error (503), never an empty store.
4. An I/O error is never an absence; only *not found* is.
5. Reading never fails on a document's content; a file that cannot be parsed is shown
   as text and repaired only by a whole-document write: no narrow write wipes a
   hand-edited file.
6. YAML is read strictly (no anchors, aliases, tags, complex keys): a rewrite never
   silently changes what a hand-written file meant.
7. A registry file that does not load is listed with its error, never dropped.
8. An error names the path it refused.
9. Metadata never breaks a guest operation: every lifecycle hook is `eval`-wrapped.

**Non-goals.** No principals beyond PVE's ACLs; no defence against a token holder or a
process inside a guest; no information hiding; no graceful handling of files written
out of band beyond refusing them; no rule proven in more than one test suite; no
abstraction for a second consumer that does not exist.

## 2. Documents

`/etc/pve/meta/<vmid>.yaml`: a tree in the JSON data model with **ordered maps and no
nulls** (absent means unset; order is kept on disk and is not a value). Keys match
`^[A-Za-z0-9_@!-]+$`. Snapshot copies are `<vmid>.<snapname>.yaml` beside it and are not
reachable through the API.

A **view** is a dotted key path through maps; the view `traefik` is that subtree.
Array members are not addressable.

**Notes.** A key ending in `__` is a string note on its sibling (`host__` on `host`; a
bare `__` on the map). Notes are data. A read leaves them out unless it asks with
`comments=1`. A `replace` that did not ask keeps the stored `k__` of every map key `k`
it keeps; inside lists nothing is kept. Deleting `k` deletes `k__`.

## 3. Prefixes

`/etc/pve/meta.d/prefixes/<prefix>.yaml`, shadowing a packaged
`/usr/share/pve-meta/prefixes/<prefix>.yaml` of the same name. The file name is the
prefix (`homelab.docker.yaml` declares `homelab.docker`).

```yaml
description: GPU assignment           # optional
selector: { tag: gpu }                # required: { all: true } | { tag: <t> }
enforce: false                        # optional
hidden: false                         # optional
schema: { type: object, properties: { ... } }   # optional, PVE::JSONSchema dialect
nodes:                                # optional: per-node overrides
  pve1: { schema: { ... } }           # any of schema, enforce, hidden; replaces, never merges
```

* **Most specific wins, twice.** For a guest on node `n`, a file's `nodes.n` fields
  replace its top-level ones. The prefix governing a path is the longest declared prefix
  containing it. Schemas never merge.
* The **selector** decides which guests a prefix reaches (tags come from the cluster's
  guest properties). Dialect: `type`, `properties`, `items`, `description`, `default`, `enum`,
  `minimum`, `maximum`, `format`, and the hints `multiline`, `hidden`, `enforce`; the
  two flags inherit down the schema, an explicit value winning. An unknown `type` does
  not load; an unknown keyword passes through.
* `enforce: true` makes the schema a rule for writes (§5); otherwise it is advisory and
  the editor marks mismatches. `hidden: true` offers no declared-but-unset row.
* A prefix file is itself a document, id `prefixes/<name>`, through the same read/write
  machinery. Writes land in the cluster directory, and the result must load as a prefix.
  The packaged file is never written or removed: a `DELETE` of a prefix that has no
  cluster file is a 404, and deleting a view of one writes the cluster file that shadows
  it, packaged content minus the view.
  `GET /meta/schemas` describes the file format to the editor.

## 4. Access

PVE's ACLs and nothing else. Guest documents: read is `VM.Audit` on `/vms/<vmid>`, write
is `VM.Config.Options`. Prefix files: read for any authenticated user, write is
`Sys.Modify` on `/`. The CLI is root and checks nothing.

## 5. Reads and writes

* Read: `format=json` returns `data` (native, booleans `1`/`0` as PVE spells them);
  `format=yaml` returns `text`. `digest` is the file's.
* Write: `data` (JSON string) or `text` (YAML); `mode=replace` (the view's subtree) or
  `mode=merge` (RFC 7386, `null` deletes); `digest` is the precondition (409; `""`
  matches a missing document); `dry_run=1` plans without writing. A write that changes
  nothing rewrites nothing. The result lists `touched: [{ path, op }]`.
* One lint on the planned document: top level is a map, no nulls, keys in the charset,
  notes are strings. 400, naming the path.
* If an enforcing prefix reaches the guest, a write whose *own* changes break its schema
  is a 422 naming the paths, unless `force=1`. `format:` is never enforced.
* A document that does not parse, is not a map, or is above the 4 MiB read cap is
  returned as `text` with `parse_error` to a YAML read and is a 422 otherwise; only a
  root `replace` or `DELETE` is accepted for it. Writes above 512 KiB are refused.
* Every write that changes a file logs one `pve-meta audit:` syslog line.

## 6. API — `/api2/json/meta`

| Method | Path | Params |
|---|---|---|
| GET | `/version` | — → `{ token }`, moved by any document or prefix file changing |
| GET | `/guests` | `has` → `[{ vmid, node, type, name, tags, digest }]` the caller may read |
| GET/PUT/DELETE | `/guests/{vmid}` | `view`, `format`, `comments`; writes add `data`\|`text`, `mode`, `digest`, `dry_run`, `force` |
| GET | `/prefixes` | `id` → every file, or with a vmid the prefixes reaching that guest with its node's overrides applied; files that did not load are rows with `error` |
| GET/PUT/DELETE | `/prefixes/{name}` | as a document |
| GET | `/access` | `id` → `{ read, write }` |
| GET | `/schemas` | — → `{ prefix }` |

Perl (`perl/PVE/API2/Ext/Meta.pm`) does parameters, ACLs, vmlist, tags, node and the
lock, and calls `PVE::RS::Meta`; `pve_meta_core::api` does the rest. Rust errors are
`"NNN: message"`. A vmid absent from the vmlist is 404.

## 7. Lifecycle and backup

One managed patch (`pve-ext-patch`) of `PVE/AbstractConfig.pm` and the two vzdump
plugins. Create clears leftovers for the vmid; destroy removes the document and its
snapshot copies; snapshot, rollback and delsnapshot copy, restore and remove
`<vmid>.<snapname>.yaml`. A backup carries the document as one marked block in the
archive's copy of the guest's notes; the restore's first config write imports it and
strips it, and a restore *over* an existing guest whose backup carried no block removes
the document it replaced — after a restore the guest is the backup; `pve-meta scan-notes` picks up a block a restore left behind. Migration needs
nothing. Clone is not carried. There is no sweeper: `pve-meta ls --orphans` and
`pve-meta rm <vmid>`. Details: `LIFECYCLE.md`.

## 8. The rest

* **CLI** `/usr/sbin/pve-meta` (root, no ticket, works during boot): `get`, `set`,
  `merge`, `delete`, `ls`, `rm`, `scan-notes`, through the same Rust functions and the
  same lock as the API. `--help` is the reference.
* **Editor** (`ui-extjs/`): a guest tab with a tree of the document and a Monaco text
  view, and a datacenter tab listing prefix files. An edit is one write with the digest,
  as everywhere in PVE; a 409 reloads.
* **pve-ext** (own package): mounts the API module, adds the tabs, applies the patches.
* **pve-meta-guest-files** (own package): writes views of a container's document into it as
  files. `GUEST-FILES.md`. **Deprecated in 0.3.3, removed in 0.4**: pve-meta has no
  effects on guests; a consumer reads its document over the API with a scoped token, or a
  separate plugin manages what it writes (decision 029).

## 9. Planned, not in 0.2.0

Node and datacenter documents (metadata on a node or the cluster, reached by prefixes
with a target kind), so integrations keep node-level state such as managed GPUs. `DocId`
stays an enum for it. `#` comments in place of note keys, once view writes can splice
text.
