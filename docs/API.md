# pve-meta HTTP API (v1, trusted-lab)

Served natively by PVE (pveproxy/pvedaemon) on port **8006** as the API module
`PVE::API2::Meta` (see `docs/NATIVE-API-SPEC.md`). All paths are under `/api2/json`.
Bodies are ordinary PVE request parameters (form-encoded or JSON); object-valued
parameters (`patch`) are passed as JSON-encoded strings. Shapes follow PVE conventions: responses are `{"data": ...}`, errors are HTTP 4xx/5xx with a
plain-text or `{"errors": {...}}` body, optimistic concurrency uses a `digest`
parameter (like `pvesh`), and `GET` never mutates.

## Authentication (v1)

One of:

* Cookie `PVEAuthCookie` (or `__Host-PVEAuthCookie`) carrying a PVE ticket. Verified
  locally against `/etc/pve/authkey.pub` (and `authkey.pub.old`), 2h validity like PVE.
  Non-GET requests additionally require the `CSRFPreventionToken` header (same rule as
  pveproxy) — the UI gets it from the PVE login it already has.
* Header `Authorization: PVEAPIToken=<user>@<realm>!<tokenid>=<uuid>`. In v1 the token
  is parsed for identity but **not** verified (trusted lab). Identity is recorded in
  the log line of every write.

No per-path *claims* authorization in v1 (i.e. nothing yet stops an authorized caller
from writing to a namespace/prefix another operator claims) — the touched-path set of
every write is computed and returned in the response, which is the seam where claims
enforcement slots in later. Ordinary PVE object permissions **are** enforced by the
native `PVE::API2::Meta` module, though: guest endpoints require the caller's usual
`VM.Audit` (read) / `VM.Config.Options` (write; `clone` additionally requires
`VM.Clone`) on `/vms/{vmid}`, and datacenter/registry/inventory endpoints require
`Sys.Audit` (read) / `Sys.Modify` (write) on `/` — see `docs/NATIVE-API-SPEC.md`'s
endpoint table for the exact mapping. The standalone `pve-metad` daemon, in contrast,
has no ACL layer at all (any caller who can authenticate at all may read or write
anything) — that daemon is the one this whole "trusted-lab" framing was written for.

## Documents

A guest document is the metadata of one guest (`vmid`); the datacenter document is
cluster-wide. Representation on the wire:

```json
{
  "id": "105",                    // or "datacenter"
  "format": "yaml",               // yaml | toml | json
  "digest": "<sha256 hex of raw file bytes>",
  "mtime": 1757265409,            // unix seconds
  "data": { "traefik": { "spec": { "host": "wiki.arkenberg.eu" } } }
}
```

`data` has comment keys (`key__`, `__`) **stripped** unless `comments=1` is passed.

## Endpoints

| Method | Path | Purpose |
|---|---|---|
| GET | `/meta/version` | `{ "token": "...", "changed": <unix> }`. Cheap; poll it every few seconds. `wait`/`since` are accepted but only the optional standalone daemon long-polls; the native module returns immediately. |
| GET | `/meta/health` | `{ "store": {"root": "/etc/pve/meta", "files": n, "bytes": n}, "hooks": {...}, "version": "<daemon version>" }` |
| GET | `/meta/inventory` | Guests from `/etc/pve/.vmlist`, enriched with the guest name read from the guest config (`name:` for qemu, `hostname:` for lxc): `[{ "vmid": 105, "node": "n1", "type": "lxc", "name": "wiki", "has_meta": true, "format": "yaml" }]` |
| GET | `/meta/guests` | List guest documents: `[{ "vmid", "node", "type", "format", "digest", "mtime", "size", "namespaces": ["traefik", ...] }]`. Filter with `has=<namespace>` (top-level key or dotted prefix present). |
| GET | `/meta/guests/{vmid}` | The document (above). `comments=1` keeps comment keys. `raw=1` adds `"raw": "<file text>"`. |
| GET | `/meta/guests/{vmid}/subtree?path=traefik.spec` | Subtree at a dotted path (the router cannot match variable-depth paths, so the path is a query parameter) → `{ "data": {...}, "digest": "..." }`. 404 if absent. |
| PUT | `/meta/guests/{vmid}` | **Patch.** Body `{ "patch": { ... }, "digest": "<optional expected>", "dry_run": false }`. Merge-patch semantics, `null` deletes. Creates the document (in the default format) if it does not exist. Returns the new document plus `"touched": [{"path": "traefik.spec.host", "op": "set"}]`. With `dry_run=1` nothing is written and the would-be result (document text + touched) is returned; a patch that would fail against a real write (digest mismatch, lint/parse error, or a top-level delete against a document that does not exist) fails identically under `dry_run` rather than silently reporting a fabricated empty result. 409 on digest mismatch, 400 on lint/parse errors, 404 if a top-level key is deleted from a document that does not exist. |
| PUT | `/meta/guests/{vmid}/raw` | **Full text replace.** Body `{ "content": "<text>", "format": "<optional, switches extension>", "digest": "<optional>", "dry_run": false }`. Returns document + `touched` (derived by diffing old and new). `dry_run=1` validates and diffs without writing. |
| POST | `/meta/guests/{vmid}/convert` | Body `{ "format": "toml", "digest": "<optional>" }`. Re-dumps in the new format (file comments lost, comment keys kept). |
| DELETE | `/meta/guests/{vmid}` | Remove the document and its snapshot copies. |
| GET | `/meta/guests/{vmid}/snapshots` | `["before-upgrade", ...]` |
| GET/PUT/DELETE | `/meta/datacenter`, `/meta/datacenter/raw`, `/meta/datacenter/subtree?path=`, `POST /meta/datacenter/convert` | Same as guests for the datacenter document. |
| GET | `/meta/registry` | Convenience view of `datacenter.operators`: `[{ "name", "claims": [{"prefix","scope"}], "schemas": {"<prefix>": {...json schema...}} }]`. |
| GET | `/meta/schemas/{vmid}` | The JSON schemas applicable to a guest document, keyed by namespace prefix (from the registry). Used by the UI form generator. |

Lifecycle endpoints (used by the Perl hooks via CLI, exposed for completeness):

| POST | `/meta/guests/{vmid}/snapshot` | `{ "name" }` |
| POST | `/meta/guests/{vmid}/rollback` | `{ "name" }` |
| DELETE | `/meta/guests/{vmid}/snapshots/{name}` | |
| POST | `/meta/guests/{vmid}/clone` | `{ "newid" }` |

## Datacenter document conventions

```yaml
settings:
  default_format: yaml        # format for newly created documents
operators:
  traefik:
    claims:
      - prefix: traefik
        scope: rw
    schemas:
      traefik: { "$schema": "...", "type": "object", ... }
    description: Traefik router provider
```

## CLI

`pve-meta` runs the same handlers in-process against `/etc/pve/meta` (no daemon needed):

```
pve-meta list [--has traefik]
pve-meta get 105 [traefik.spec] [--comments] [--output-format json|yaml|text]
pve-meta set 105 traefik.spec.host=wiki.arkenberg.eu [more k=v ...]
pve-meta patch 105 '{"traefik":{"spec":{"port":8080}}}'
pve-meta delete 105 traefik.spec.port          # delete a path
pve-meta raw 105                               # print file text
pve-meta edit 105                              # $EDITOR round trip → put raw
pve-meta convert 105 toml
pve-meta remove 105                            # remove the document
pve-meta version
pve-meta datacenter get|set|patch|raw|edit|convert
pve-meta snapshot 105 <name> | rollback 105 <name> | delsnap 105 <name> | clone 105 106 | destroy 105
```
