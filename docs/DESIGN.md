# pve-meta — specification

What is true of the code now. Where code and this document disagree, the code is wrong.
The reasons behind the rules, and what was tried before them, live in
[`decisions/`](decisions/README.md); this file does not repeat them.

## 1. Threat model

A trusted, single-administrator environment: a homelab cluster. The principals are the
administrator and the service API tokens that same administrator issued. pve-meta holds
configuration intent, not secrets.

Scopes are a **blast-radius limiter**, not an adversarial boundary. In scope: a scoped
principal does not read or write document content outside its granted prefixes; a
concurrent write does not silently lose another's update. Out of scope: key-name
disclosure through errors, digests or listings; any defence against a principal the
administrator deliberately issued a token to. pve-meta gates nothing but access to
metadata documents and never restricts a PVE permission the platform itself grants.

## 2. Documents

Every guest (vmid) has one **document**: `/etc/pve/meta/<vmid>.yaml`, a nested
key-value tree in the JSON data model — strings, numbers, booleans, arrays, maps — with
two rules on top:

* **Ordered maps, no nulls.** Key insertion order is kept on disk as a courtesy to
  whoever wrote the file; absent means unset. Order is not a value: two maps with the
  same keys and values in different orders are the same document, a reordering stages
  nothing in the editor, and no lookup, selector or permission depends on it. A
  reordering typed in Text mode is written only when applied from Text mode.
* **Comment keys.** A key ending in `__` is a string note about its sibling (`host__`
  documents `host`; a bare `__` documents the containing map). Comment keys are
  ordinary data; the UI shows them as the row's description.

Object keys match `^[A-Za-z0-9_@!-]+$`. No key is reserved. There is no datacenter-level
document; a `datacenter.yaml` an earlier release left behind is a stray file the store
ignores.

A **view** is a key-path prefix into a document (dotted, any depth, through maps only).
A view of `traefik` is that subtree with the prefix stripped. Views are also the unit of
access. Array members are not addressable.

Snapshot copies are `<vmid>.<snapname>.yaml` beside the document; they are not documents
and are never reachable through the API.

## 3. Prefixes — what a prefix is

`/etc/pve/meta.d/prefixes/<prefix>.yaml`, with packaged defaults under
`/usr/share/pve-meta/prefixes/<prefix>.yaml`; a cluster file overrides the packaged file
of the same name. **The file name is the prefix**: `homelab.docker.yaml` declares
`homelab.docker`. Names are dotted segments of the key charset, at most 128 bytes.

```yaml
description: Traefik dynamic configuration   # optional
selector: { tag: traefik }                   # required: { all: true } or { tag: <t> }
enforce: true                                # optional, default false
hidden: false                                # optional, default false
schema:                                      # optional, PVE::JSONSchema dialect
  type: object
  properties:
    spec:
      type: object
      properties:
        host: { type: string, format: dns-name, description: Public host name }
        port: { type: integer, minimum: 1, maximum: 65535, default: 80 }
```

* A prefix names **no principal**. Declaring one is useful on its own.
* The **selector** decides which guests the prefix reaches, and therefore where its
  declared-but-unset rows appear and where `enforce` applies. Tag membership is the
  guest's PVE tags from the cluster's cached guest properties.
* **Most-specific wins; schemas never merge.** The governing prefix of a path is the
  longest declared prefix that contains it. A parent's schema for a key a child prefix
  owns is shadowed, silently. A prefix with no `schema` still governs its subtree.
* The schema dialect the editor consumes: `type`, `properties`, `description`,
  `default`, `enum`, `minimum`, `maximum`, `format` (a PVE::JSONSchema format name,
  validated by proxmoxlib's own vtype in the editor), the editor hints `multiline` and
  `hidden`, and `enforce`. Nothing in pve-meta requires a key to be present; a `default`
  is written only by an explicit action.
* **`hidden: true`** on a schema node: the editor offers no declared-but-unset row at
  that path. It never hides a key that is set — a stored value has its row from the
  document, typed and described by the schema as ever — and it never reaches
  validation. On the prefix itself it hides every declared key below; the prefix's own
  row stays, since it is the declaration that something lives there.
* **`enforce: true`** makes the schema a rule for API writes (§7). Off, a schema is
  advisory: the editor marks mismatches and asks for a tick.
* **Both flags are inherited** down the schema node by node, an explicit setting on a
  node winning at any depth; the prefix-level field is the root default. So
  `enforce: true` on the prefix with `enforce: false` on a passthrough subtree, or
  `hidden: true` on the prefix with `hidden: false` on the two keys worth offering.
  Either flag may be spelled `true`/`false` or `1`/`0`, the idiom `optional` and
  `multiline` already use.

## 4. Permissions — who may touch a prefix

`/etc/pve/meta.d/permissions/<name>.yaml`. **Cluster-only: there is no packaged
permissions directory**, so an operator's package can declare a prefix but never grant
itself access.

```yaml
authid: svc@pve!traefik        # a PVE user or token id
description: ...               # optional
rules:                         # optional; a file with none grants nothing
  - prefix: traefik            # non-empty
    mode: rw                   # ro | rw
    selector: { tag: traefik } # required
```

* **Permissions accumulate by containment.** A rule on `homelab` covers
  `homelab.docker`. A rule on `p` also covers the sibling comment key `p__`; that is the
  only comment-key access rule.
* Permissions apply to **guest documents only**.
* Both directories are parsed strictly (`deny_unknown_fields`) and independently. A
  malformed file contributes nothing, is logged, and **is still listed** by `GET
  /meta/prefixes` and `GET /meta/permissions` as `{ name|prefix, origin, error }`, so it
  can be found and repaired.

## 5. Effective access for one request

* `full_read` = `VM.Audit` on `/vms/<vmid>`; `full_write` = `VM.Config.Options`.
* `scopes` = the union of rules whose `authid` is the caller and whose selector matches
  the guest.
* A read of view `P` needs full read or a scope covering `P`. A caller with no read
  access at all gets 403. A view-less read returns the union of the caller's readable
  subtrees, in document order.
* **A write is authorized by what it changes.** The mutation is planned against a copy
  of the stored document, and every path the plan touches — values changed, keys added,
  keys removed — needs full write or an `rw` scope. Two request-shaped checks come
  first: the caller must be able to read the view it names, and must hold some write
  permission on the document. `full_write` short-circuits both. Key order is not a
  path, so a pure reordering touches nothing.
* A document that **cannot be read back** (§7) is repaired only by a root `replace` or a
  root `DELETE`, and only with full write.
* Registry documents (§6) get no scopes: read is open to every authenticated user,
  write is `Sys.Modify` on `/`.

## 6. Registry files are documents

A prefix or permission file is addressed as a document with id `prefixes/<name>` or
`permissions/<name>`, through the same `view`, `format`, `mode`, `digest`, `dry_run`
machinery as a guest document. Four things are specific to them:

* **Writes land in the cluster directory.** Editing a packaged prefix creates the
  cluster override; deleting the override reverts to the packaged file.
* **The result must parse as its kind**, using the loader's own parser
  (`registry::parse_prefix` / `parse_permission`), on every write including `dry_run`
  and a narrow `DELETE ?view=`. A 200 must never make a file the loader would skip.
* **No permission reaches them** (§5).
* **They move the version token** (§8), including a shadowed packaged file.

`GET /meta/schemas` returns the two file formats as schemas in the same dialect
(`metaschema`), so the editor renders a registry document as a typed tree. It is an
affordance, not the validator; a test keeps its required keys equal to the parser's.

## 7. Documents on the wire

* Reads: `format=json` returns `data`, a native structure (unordered, booleans as
  `1`/`0` per PVE convention); `format=yaml` returns `text`, the file's own text for a
  full reader's root view and a canonical dump otherwise. The editor reads YAML.
* Writes: `data` (a JSON string) or `text` (YAML), with `mode=replace` (the view's
  subtree replaced; `{}` stores an empty map) or `mode=merge` (RFC 7386 merge patch,
  `null` deletes). `digest` is the compare-and-swap precondition (409 on mismatch;
  `""` matches a missing document). `dry_run=1` plans and validates without writing. A
  write that changes nothing rewrites nothing.
* **One lint** runs on the planned document: top level is a map, no nulls, keys match
  the charset, comment keys are strings. A failure is a 400 naming the path.
* **Enforced schemas.** If a prefix that reaches the guest says `enforce: true` (for
  itself, or on a schema node under it: §3), a write that would leave the enforced part
  of its subtree not matching the schema — for the findings the write introduces or
  touches, never for what was already wrong elsewhere — is a 422 naming the paths,
  unless the request carries `force=1`. Anyone who may write may force. `format:`
  checks are never enforced.
* **Unrecoverable file** — not valid YAML, above the 4 MiB read cap, or not a map:
  `format=yaml` for a full reader returns the raw `text` plus `parse_error`; everything
  else is a 422. A root `replace` or root `DELETE` repairs it; nothing narrower is
  accepted. Per document, never cluster-wide.
* YAML is read strictly — no anchors, aliases, explicit tags or complex keys; the YAML
  1.1 words `yes`/`no`/`on`/`off` stay strings — and written canonically (block style,
  two-space indent, key order kept). Free-form `#` comments survive only until a write.
* Sizes: writes above 512 KiB are refused (pveproxy refuses a body of about that size
  first); reads above 4 MiB are never parsed and are identified by a surrogate over size
  and mtime.
* Writes run under `PVE::Cluster::cfs_lock_domain("pve-meta-<id>")` with the digest
  check inside the lock; files are written atomically. Reads are unlocked: a file that
  vanishes under one is a 404, never a 500.
* Every write that changes a file logs one syslog line at `info`, tagged
  `pve-meta audit:`, with the authid, document, view, mode, touched count and digest.

## 8. API

Native, `/api2/json/meta`, served by pveproxy (reads) and pvedaemon (writes,
`protected`). No `proxyto`: any node answers.

| Method | Path | Params | Returns |
|---|---|---|---|
| GET | `/meta/version` | `detail`, `id` | `{ token, changed }`; with `detail`, `documents: [{ id, digest }]`. With `id`, the token covers that document plus the registry directories only. Tokens of different scope are not comparable |
| GET | `/meta/guests` | `has` | `[{ vmid, node, type, name, tags, digest }]` for every guest the caller can read something of; `node`/`name`/`tags` only with `VM.Audit` |
| GET | `/meta/guests/{vmid}` | `view`, `format` | `{ id, view, digest, data \| text, parse_error? }` |
| PUT | `/meta/guests/{vmid}` | `view`, `data`/`text`, `mode`, `digest`, `dry_run`, `force` | `{ id, view, digest, touched: [{ path, op }] }` |
| DELETE | `/meta/guests/{vmid}` | `view`, `digest` | same shape |
| GET | `/meta/access` | `id` | `{ read, write, scopes: [{ prefix, mode }], tags }` for that document; `tags` only with `VM.Audit`. Without `id`: the registry's answer (`read` always, `write` = `Sys.Modify`) |
| GET | `/meta/prefixes` | — | `[{ prefix, description?, selector, enforce, hidden, schema?, origin, overrides }]`, most-specific first, plus `{ prefix, origin, error }` for a file that did not load |
| GET | `/meta/permissions` | — | `[{ name, authid, description?, rules, origin, overrides }]`, plus `{ name, origin, error }` rows |
| GET/PUT/DELETE | `/meta/prefixes/{name}`, `/meta/permissions/{name}` | as a document | the file as a document, id `prefixes/<name>` |
| GET | `/meta/schemas` | — | `{ prefix, permission }` |

A vmid absent from the vmlist is 404 for GET, PUT and DELETE. Errors from Rust are
`"NNN: message"`, re-raised by Perl as a `PVE::Exception`; there is no second error
vocabulary. Everything crosses the Perl/Rust boundary as native structures except the
client's `data` string.

Implementation: `perl/PVE/API2/Ext/Meta.pm` does parameters, PVE ACL checks, the
vmlist, guest tags and the per-document lock, and calls `PVE::RS::Meta::api_*`;
`pve_meta_core::api` does everything else. The three get/put/delete families are
generated from one spec.

## 9. Guest lifecycle

One patched file, `PVE/AbstractConfig.pm` (`libpve-guest-common-perl`), managed by
`pve-ext-patch`:

* `create_and_lock_config` calls `on_create` when PVE has just asserted the vmid was
  unused, clearing any document and snapshot copies left there.
* `destroy_config` calls `on_destroy` after the config's own unlink, removing the
  document and its snapshot copies. Every destroy path funnels through it.
* `snapshot`, `rollback`, `delsnapshot` copy, restore and remove
  `<vmid>.<snapname>.yaml`. Rollback to a snapshot that had no document removes the
  live document.

All hooks are `eval`-wrapped and warn; metadata never breaks a guest operation.
Migration needs nothing (the document is cluster-wide). Clone and backup are not
carried; back up `/etc/pve`.

There is no sweeper. A guest config removed out of band leaves an orphan; `pve-meta ls
--orphans` lists them and `pve-meta rm <vmid>` removes one under the document's lock
with the vmlist re-read inside it.

## 10. The CLI

`/usr/sbin/pve-meta`, for root on the node: no ticket, no pveproxy, so it works in a
hook script during boot.

| Command | Does |
|---|---|
| `get <id> [<view>] [--format yaml\|json]` | a scalar prints bare, structure prints YAML; exit 2 when not there |
| `set <id> [<view>] --data\|--text\|--file [--digest] [--dry-run] [--force]` | the API's `PUT mode=replace`, under the document's lock |
| `merge ...` | the same with `mode=merge` |
| `delete <id> [<view>] [--digest]` | the API's `DELETE`; exit 2 when nothing was there |
| `ls [--orphans] [--format plain\|json]` | document ids; with `--orphans`, vmids with files but no guest |
| `rm <vmid>` | remove an orphan's files; refuses a live guest |

Writes go through the same Rust functions as the API — lint, digest, enforced schemas,
audit line — and skip only permissions, because root can already write the file.

## 11. Extension seams (`pve-ext`)

A separate package with three generic seams, each a one-line patch of pve-manager
applied by `pve-ext-patch`:

* **API modules**: `PVE::API2::Ext` mounts every `PVE/API2/Ext/*.pm` at the path its
  `ext_path` declares.
* **UI pages**: `pve-ext-loader.js` fetches `GET /ext/pages` and adds one tab per
  manifest, as an iframe (`url`) or a native panel (`script` + `xtype`), gated by
  `requires`.
* **Managed patches**: TOML manifests describing dpkg-diverted file patches, with
  apply, verify, remove and status, re-applied by a trigger when the patched package
  is reshipped.

pve-meta ships two page manifests (guest, datacenter) over one script and one patch
manifest (lifecycle).

## 12. The editor

`ui-extjs/pve-meta-tree.js`: plain JavaScript, a native `Ext.tree.Panel`, no build
step. The rules it needs — codec, key charset, coverage, governing prefix, schema
findings, staged edits — are `pve-meta-core` compiled to wasm
(`crates/pve-meta-wasm`, see `WASM-CORE.md`); the editor reimplements none of them.

* **Guest tab**: one tree of the document the caller can see. Rows are the union of
  keys present and keys the governing prefixes declare and do not hide (§3);
  declared-but-unset rows are greyed with their default and a **Set to default** action.
  Columns: key, value, description (the row's comment key), access (every rule covering
  the row).
* **Edits are staged** and one **Apply** writes them as a single `replace` at the
  narrowest view covering every staged path (a single delete is a `DELETE`). Staged
  rows render like a pending PVE config change. **Revert** drops them.
* **Tree | Text** switches between the tree and a Monaco buffer of the same planned
  document; switching back turns the typed text into staged edits, and asks first
  when the buffer changed only layout, which the tree cannot hold. Apply from Text
  sends the buffer, so a `#` comment or a reordering typed there reaches the file.
  **Edit selection as text** opens Monaco on one subtree of the planned document and
  stages the parsed value on OK, like the row editor; a comment typed there is not
  kept. Both text editors share Format and a YAML/JSON view toggle. **Diff** shows what
  Apply would write against what is stored, from either view: a buffer verbatim, a
  document rendered.
* Schema findings mark rows amber, bubble up to ancestors, and squiggle the text buffer.
  Apply stops to show the diff only when the edit introduces a finding; a **Save
  anyway** tick then applies with `force=1`, and enforced findings are labelled.
* A document that does not parse opens in Text mode only.
* A version poll (`GET /meta/version?id=`) refreshes the tree, never while anything
  is staged or an editor is open. Every write carries the digest; a 409 reloads.
* **Datacenter tab**: two grids, Prefixes and Permissions, with origin, selector,
  schema and enforced columns. Editing a row opens the file in the same document
  editor. **New** creates the smallest file the loader accepts, or a service token
  (a `pve` user with no password, one token with privilege separation off, and an
  empty permission file naming the token). **Declare Key** writes one
  `schema.properties.<key>`; **Add Rule** appends to `rules`.
* The page manifest requires `VM.Audit` (`Sys.Audit` for the datacenter tab).

## 13. Repository layout

```
crates/pve-meta-core     model, paths, formats, patch, view, registry, scopes, shape, edit, store, api
crates/pve-meta-perl     PVE::RS::Meta: lifecycle hooks, stored_vmids, the api_* exports
crates/pve-meta-wasm     the core for the browser, behind a JSON-string ABI
perl/PVE/API2/Ext/Meta.pm  the REST module
bin/pve-meta             the CLI
pages/                   the two page manifests
prefixes/                packaged example prefixes
patches/                 the lifecycle patch manifest and diff
pve-ext/                 the extension layer (own package)
ui-extjs/                the editor and its offline and headless tests
scripts/perl-stubs/      stub PVE modules so perl -c runs anywhere
testdata/                the canonical YAML fixture both suites pin
debian/, Makefile        packaging
docs/decisions/          why the rules above are what they are
```
