# pve-meta — design (revision 6)

Revision 6 splits revision 5's "operator registration" into a **namespace** and a
**grant** (§3, §12); everything else is revision 5's. Revision 5 superseded revision 4 (`DESIGN-rev4.md`, kept for the record) and the
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

## 3. Namespaces and grants

Two concepts, in two drop directories. They were one — an "operator registration" —
until revision 6; see §12 for why splitting them was the point.

### 3.1 Namespaces — what a prefix is

`/etc/pve/meta.d/namespaces/<prefix>.yaml`, with packaged defaults in
`/usr/share/pve-meta/namespaces/<prefix>.yaml` (a cluster file overrides the packaged
file of the same name).

**The filename is the prefix.** `traefik.yaml` declares `traefik`;
`homelab.docker.yaml` declares `homelab.docker`. There is no `prefix:` field, so one
namespace is exactly one prefix and "who declares `traefik`?" is `ls`. A prefix segment
is `[A-Za-z0-9_@!-]+` and dots are only separators, so a namespace filename can never
contain a slash, never start with a dot, and never escape its directory — the identity
is safe by construction rather than by validation.

```yaml
# /etc/pve/meta.d/namespaces/traefik.yaml
description: Traefik dynamic configuration
selector: { tag: traefik }   # or { all: true }; room for { pool: name } later
schema:                      # optional, PVE::JSONSchema dialect for the subtree
  type: object
  properties:
    spec:
      type: object
      properties:
        host: { type: string, description: Public host name }
        port: { type: integer, minimum: 1, maximum: 65535, optional: 1, default: 80 }
```

A namespace names **no principal**. Declaring that a prefix exists and has a shape is
useful with no operator, no token and no automation anywhere near it — a structured
notes field with a schema is a complete use of this system.

**Most-specific wins; schemas never merge.** For a document path, the governing
namespace is the one with the **longest** declared prefix that covers it; no other
namespace contributes to that path. So with both `homelab` and `homelab.docker`
declared, `homelab.notes` is governed by `homelab` and `homelab.docker.compose` by
`homelab.docker` — including `homelab`'s own `properties.docker`, which is shadowed
rather than merged. Merging two schemas is a rabbit hole (it is what `allOf`/`$ref`
exist for), and where a parent and child namespace have different owners it would mean
two owners fighting over one key.

A parent that declares a key a child namespace owns is **not** rejected: files are
parsed independently, and a cross-file check would trade that for nothing. It is shadowed silently: the UI shows the child's schema and never the parent's.

The selector decides which guests a namespace reaches, and therefore where its
declared-but-unset rows appear. Nowhere else.

### 3.2 Grants — who may touch a prefix

`/etc/pve/meta.d/grants/<name>.yaml`. Cluster-only: **there is deliberately no packaged
grants directory.**

```yaml
# /etc/pve/meta.d/grants/traefik.yaml
authid: svc@pve!traefik
grants:
  - prefix: traefik
    mode: rw                 # ro | rw
    selector: { tag: traefik }
```

That absence is a mechanism, not an omission. An operator's own `.deb` *should* be able
to ship a namespace — a schema is a declaration. It must never be able to ship its own
grant, because that is self-registration, which is privilege escalation. dpkg cannot
write into pmxcfs, so "an operator declares what it expects; only an administrator
grants it" is enforced by where the files live rather than by a rule someone has to
remember. For the same reason **nothing registers itself over the API**: writing either
directory requires `Sys.Modify` on `/`.

**Grants nest by containment, additively** — the opposite of schemas, deliberately. A
grant on `homelab` covers `homelab.docker`, because "you may write `homelab`" not
implying its subtree would be surprising. Schemas shadow because they describe shape and
shape has one owner; grants accumulate because they describe permission and permission is
a union. Those two rules cannot both live on one object, which is the concrete reason
this is two concepts and not one.

### 3.3 Rules common to both

* Files are parsed strictly and independently; a malformed file is skipped with a
  warning and contributes nothing. It never affects another file.
* Prefixes are non-empty. `authid` is a PVE user or token id.
* A **selector** restricts to guests: `all`, or `tag: <t>` (the guest carries the PVE
  tag), read from the cluster's cached guest properties. Adding the tag is the
  deliberate act of including that guest. It is *not* enforced by PVE — see §1: this is
  a selector, not a permission boundary.
* Grants apply to **guest documents only**. The datacenter document is governed by ACLs
  alone.
* A grant on prefix `p` covers the subtree `p` and the sibling comment key `p__`. That
  is the only comment-key rule.

### 3.4 Effective access for one request

* `full_read` = `VM.Audit` on `/vms/<vmid>`, `full_write` = `VM.Config.Options`
  (datacenter: `Sys.Audit` / `Sys.Modify` on `/`).
* `scopes` = the union of grant entries whose `authid` is the caller and whose selector
  matches the guest.
* Reading view `P` requires full read or a scope covering `P`; writing requires full
  write or a `rw` scope covering every touched path; a write to the root view requires
  full write. A read by a caller with no grant at all is 403. Authorization is decided
  from the request and a plan computed against a copy, never from a diff of stored data.

**PVE ACLs cannot express this**, which is why grants are ours and not
`pveum acl modify /meta/traefik`. `PVE::AccessControl::check_path` is a hardcoded
whitelist (`/`, `/access/*`, `/nodes/*`, `/pool/*`, `/sdn/*`, `/storage/*`,
`/vms/[1-9][0-9]{2,}`, `/mapping/*`); `/meta/*` is not in it and the API refuses it
(`400 invalid ACL path '/meta/traefik'`, verified live). The whitelist is enforced in
exactly one place, `PVE::API2::ACL::update_acl`, and the `user.cfg` *parser* only calls
`normalize_path` — so a hand-written entry would load and evaluate. That is a trap, not
an opening: unsupported, invisible to the Permissions UI, and one upstream edit from
breaking silently. Making it legitimate would mean patching a fourth package.

### 3.5 The registry files are documents too

A namespace or grant file is addressed as a document: `namespaces/<name>` and
`grants/<name>` are ids like `100` and `datacenter`, reachable at
`/meta/namespaces/{name}` and `/meta/grants/{name}` with the same `view`, `format`,
`mode`, `digest` and `dry_run` the other two take. That is not an aesthetic choice: the
editor's tree, its markers, its diff, the digest compare-and-swap and the version poll
are all written against *a document*, and the alternative was a second read/write path
beside the first — the shape of every wrong-result bug this project has had.

Four things are specific to them:

* **Writes land in the cluster directory**, always. Editing a namespace that a package
  shipped creates `/etc/pve/meta.d/namespaces/<name>.yaml` and leaves the packaged file
  alone; a `DELETE` removes only the cluster file, so it is a *revert* to the packaged
  namespace rather than a removal, and a following `GET` returns the packaged one again.
  The compare-and-swap is checked against the file the caller actually read, so
  overriding a packaged file is an ordinary write and not a spurious 409.
* **The result must parse as what it claims to be.** The loader deliberately skips a
  malformed file (§3.3), which is exactly why a write that produced one must not answer
  200: the namespace would silently disappear. `parse_namespace`/`parse_grant` — the
  loader's own parsers, not a copy — gate every write, including a `dry_run` and a
  narrow `DELETE ?view=authid`. The ordinary document lint (§4) applies on top, to the
  `schema:` subtree as much as anywhere else — a property name the lint refuses is a
  property no document could ever hold — so a registry file that already contains
  something it rejects is repaired the same way any document is: one whole-document
  replace. (That is not hypothetical. The lab's `homelab.docker` namespace held
  `compose: { type: string, description: The compose file, as text }`, where the unquoted
  comma inside a flow mapping had silently made a second key `as text: null`. The loader
  never looked inside `schema`, so nothing had complained for weeks.)
* **No grant ever reaches them.** These documents get no scopes at all, so an operator
  holding `rw` on a prefix cannot edit the grant that gave it that prefix, nor the
  namespace that declares it. Self-registration is refused by there being no way to
  express it. Read is open to every authenticated user, matching the two list endpoints,
  which return the same content; writing is `Sys.Modify` on `/`.
* **They move the version token.** The poll walks the registry directories as well as
  the store, so an editor open on a guest notices a namespace change within one tick.
  A file shadowed by a higher-precedence one still moves the token while contributing no
  document of its own: over-notifying a poll costs a reload, under-notifying it leaves a
  stale UI.

## 4. Documents on the wire

* Booleans in `data` are rendered as `1`/`0`, the PVE API convention (perlmod and PVE's JSON encoder both do this); a namespace schema's declared type disambiguates them in the UI, and `format=yaml` carries exact types for clients that need them.
* Reads: `data` (JSON object, unordered) or `text` (YAML, the file's own text for the
  root view, a canonical dump for a sub-view). Key order is preserved in the file and is
  not a wire contract; the UI sorts.
* Writes: `data` (JSON string) or `text` (YAML) with `mode=replace` (the view's subtree
  is replaced; `{}` stores an empty map) or `mode=merge` (merge-patch, `null` deletes; a
  write that touches nothing changes nothing — and rewrites nothing, so it does not move
  `changed` either). One lint runs on the planned document;
  the write is refused with a 400 that names the offending path. `digest` is the
  optional expected file digest (409 on mismatch); `dry_run=1` plans and validates
  without writing.
* Unrecoverable file — it does not parse, it is above the store's 4 MiB read cap, or it
  parses to something that is not a mapping (`null` from an empty or comment-only file, a
  scalar, a list): `format=yaml` returns the raw text plus `parse_error` where the bytes
  were read at all (so an administrator can repair it); everything else returns 422
  naming the condition. It is always reported, never rendered as an empty document. A
  root `replace` or a root `DELETE` by a full writer repairs it, and **nothing narrower
  is allowed** — a narrower write plans against the empty document, so it would silently
  discard the file. This is a per-document condition, never cluster-wide.
* Writes run under `PVE::Cluster::cfs_lock_domain("pve-meta-<id>")` with the digest
  check inside the lock; files are written atomically with node/pid/seq-unique temp
  names. Reads are unlocked, so a file can vanish under one: that is a 404 (or, in a
  listing or a poll, a skipped entry), never a 500, and a `DELETE` of a document someone
  else already removed succeeds. Errors name paths; there is no disclosure filtering.

## 5. API (native, `/api2/json/meta`, served by pveproxy/pvedaemon)

| Method | Path | Params | Returns |
|---|---|---|---|
| GET | `/meta/version` | `detail` | `{ token, changed }` — content hash over the store; poll it. With `detail`, also `documents: [{ id, digest }]` (sorted) so a caller that saw the token move knows which documents to re-read instead of re-listing. Snapshot copies move `token` but are not documents and are not listed. Digests are unfiltered (§1). |
| GET | `/meta/guests` | `has` (prefix) | `[{ vmid, node, type, name, tags: [..], digest }]` for every guest in the vmlist the caller can read something of; `node`/`name`/`tags` only with `VM.Audit`; `digest: ""` when no document |
| GET | `/meta/guests/{vmid}` | `view`, `format` = `json` (default) or `yaml` | `{ id, view, digest, data }` or `{ id, view, digest, text, parse_error? }` |
| PUT | `/meta/guests/{vmid}` | `view`, `data` or `text`, `mode`, `digest`, `dry_run` | `{ id, view, digest, touched: [{ path, op }] }` |
| DELETE | `/meta/guests/{vmid}` | `view`, `digest` | removes the subtree, or the whole document |
| GET/PUT/DELETE | `/meta/datacenter` | same | same with `id: "datacenter"` |
| GET | `/meta/access` | `vmid` or `dc=1` | `{ read, write, scopes: [{ prefix, mode }] }` for that document (selectors already resolved); without either, the caller's datacenter read/write |
| GET | `/meta/namespaces` | — | `[{ prefix, description?, selector, schema? }]`, sorted most-specific first — every namespace, readable by every authenticated user |
| GET | `/meta/grants` | — | `[{ name, authid, grants: [{ prefix, mode, selector }] }]` — every grant, readable by every authenticated user |
| GET/PUT/DELETE | `/meta/namespaces/{name}` | same as a document | the namespace **file** as a document, with `id: "namespaces/<name>"`. Read is open like the listing; write is `Sys.Modify` on `/`. A `PUT` whose result would not parse as a namespace is a 400, never a 200 (§3.5) |
| GET/PUT/DELETE | `/meta/grants/{name}` | same | the grant file, `id: "grants/<name>"`, same rules |

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
* **Create and destroy**: two hooks in the same patched file. `create_and_lock_config`
  calls `PVE::RS::Meta::on_create($vmid)` — but **only when `$allow_existing` is false**,
  i.e. when the `PVE::Cluster::check_vmid_unused` inside it has just asserted the vmid was
  free — which clears any document and snapshot copies left at that vmid. `destroy_config`
  calls `PVE::RS::Meta::on_destroy($vmid)` after the config's own `unlink` succeeds, which
  removes the document and its snapshot copies. Both are `eval`-wrapped and warn: metadata
  never breaks a guest operation.

  Those two methods are the whole lifecycle. Every destroy path in `pve-container` and
  `qemu-server` funnels through `destroy_config` (primary destroy, create/restore failure
  cleanup, clone failure cleanup, remote-migration abort — 12 call sites), and every
  creation path through `create_and_lock_config` (create, restore, clone target, CLI,
  both remote-migration inbound paths — 6 call sites). Neither adds a patched *file*:
  `AbstractConfig.pm` is already patched for the snapshot trio.

  **The create hook is what a periodic sweep cannot be.** A sweep nominates vmids missing
  from the vmlist, so a guest destroyed and recreated at the same vmid between two sweeps
  is never stale from its point of view, and the new guest inherits the old document
  permanently. Clearing on create closes that window rather than narrowing it, and doubles
  as the backstop for a destroy that never ran because its node was down.

  Migration needs nothing: `/etc/pve/meta/<vmid>.yaml` is flat and cluster-wide, like
  `/etc/pve/firewall/<vmid>.fw`; only the guest config is node-scoped and gets
  `move_config_to_node`'d.

  A **manual broom** remains at `/usr/libexec/pve-meta/gc` (`PVE::RS::Meta::gc_candidates`
  + `gc_purge`, two-phase under `cfs_lock_domain`) for the one case the hooks cannot see:
  a config removed out of band. **Nothing runs it on a timer.** There is no orphan concept
  in the API.
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

The page shows one tree of the document the caller can see. Rows are the union of the
keys present and the keys the governing namespaces declare (declared-but-unset rows are
greyed with their default and a "set" action). Rows carry a folder icon for maps and a
leaf icon for values, next to the expander. Columns:

* **Key**.
* **Value**, edited through the row editor (textfield, number, checkbox, combobox for
  enums, arrays as one text leaf); opened by Edit, double-click or Enter. A schema's
  `minimum`/`maximum` bound the number editor and its `format` (a `PVE::JSONSchema`
  format name) validates the field: `ip`, `ipv4`, `ipv6`, `CIDR`, `CIDRv4`, `CIDRv6`,
  `mac-addr`, `dns-name`, `address`, `email` — the same set in both implementations,
  wired to proxmoxlib's own vtypes in `ui-extjs`. **A format neither knows constrains
  nothing**: this is an affordance so a human is told before the round trip, never an
  authority — the server's one lint is that (§4), and an operator writing through the
  API is not policed by it. There is deliberately no `pattern`/regex: a format is a
  name PVE already defines and validates, a regex is one more dialect to own.
* **Description**: the row's comment key (`k__`) if present, else nothing; the schema's
  description is the tooltip.
* **Access**: every grant whose prefix covers the row, `rw` ones by name, `ro` ones
  muted with "(ro)"; tooltip with selectors. Several principals may read a subtree;
  "access" is about who writes and who subscribes, not ownership.

Toolbar: Add, Edit, Remove (targeting the selection: Add into the selected map, or the
parent of a selected leaf, or the root), **Edit selection as text** (enabled with a
selection; Monaco on that subtree, YAML/JSON view toggle, diff-confirmed apply), Reload,
and at the right end a **Tree | Text** toggle that swaps the panel body in place between
the tree and a full-document Monaco editor with Apply (diff dialog, root replace with the
digest) and Discard; leaving Text while dirty asks first. Text mode validates the buffer **as it is typed**: a
YAML syntax error is one Error marker on the line the parser reports (Monaco's own JSON
language service already does this for the JSON view), and every schema finding is a
Warning marker on its key's line, with the key's declared type, format, range, default
and description on hover. A document's own comment keys need no hover; they are ordinary
lines in the YAML. Grammar markers are YAML-only — the line index is a YAML scan — so the
JSON view keeps syntax validation and loses the schema squiggles. **Nothing here blocks
anything**: markers are advisory, and a document that does not match the schema is applied
through the same diff dialog as any other, with a warning banner listing what does not fit
above the diff and Apply gated on an explicit "Save anyway" tick — one decision, taken with
the diff it is about on screen, rather than an alert to dismiss before a second window. The server's one lint decides
what is storable (§4); an operator whose schema has drifted from what a document
legitimately holds must not be able to lock the administrator out of editing it.

**Format** re-dumps the buffer canonically in whichever language is showing (two-space
indent, no folding, key order preserved), and refuses a buffer that does not parse rather
than mangling it. The **YAML | JSON** toggle is presentation only and says so: coming back
to YAML restores the server's own text whenever the document is unchanged, because
js-yaml and `serde_yaml` lay the same document out differently and re-dumping made a view
toggle report unsaved changes. The diff dialog sets `ignoreTrimWhitespace: false` —
Monaco defaults it to `true`, which hid exactly the indentation-only changes that shape
produces, leaving a confirm dialog that showed nothing while Apply was enabled. A muted "Scoped write access"
or "Read-only" label appears next to the toggle only when the caller is restricted. No
per-row action icons. Editability is per row from `/meta/access`; a row edit is
`PUT ?view=<path>&mode=replace` with the scalar, delete is `DELETE ?view=<path>`, the
digest is sent and a 409 reloads. The version poll refreshes the tree and the grants,
never while an editor is open.

**Who sees this page.** The manifest requires `VM.Audit` (`Sys.Audit` for the
datacenter), and that is the whole audience: PVE's own resource tree lists a guest only
to a caller holding `VM.Audit` on it (`PVE::API2::Cluster::resources`), so a principal
holding nothing but grants has no guest to open the tab on, whatever the manifest says.
Tag selectors are therefore resolved against `GET /meta/guests`' `tags` (§5) and nothing
else, and no server-resolved fallback is needed for a caller this page can have. Grants
lose nothing by that: they bind server-side, on the API a scope-only principal actually
uses. Inside the tab a caller with `VM.Audit` but not `VM.Config.Options` still edits
exactly the rows its `rw` grants cover -- that is the "Scoped write access" label.

`ui-extjs/` is the implementation: plain JavaScript, `Ext.tree.Panel` with columns,
mounted as a native tab through the `script`/`xtype` manifest form (§7). Session, CSRF,
theme and i18n come from the PVE UI, so none of it is reimplemented; there is no iframe,
no wasm and no build step beyond vendoring Monaco. YAML is a vendored js-yaml, used for
presentation only (the YAML/JSON toggle and the diff's original side) — the server stays
the authority, and an Apply sends the buffer back as `text`.

A second implementation in pwt/Yew was built to the same specification and compared on
the lab; it was removed once the choice was made (git tag `pwt-ui-removed`). It cost
~4,900 lines of Rust and 259 crates against ~2,200 lines of JavaScript for the same page,
and its only structural advantage — that it never parses YAML itself — was answered by
vendoring a real parser with a property test. What it was genuinely better at, native
unit tests, is the thing `ui-extjs/testing/` has to keep earning.

## 9. Repository layout

```
crates/pve-meta-core     document model, views, namespaces+grants+selectors, lint, api layer, store, gc
crates/pve-meta-perl     PVE::RS::Meta: snapshot hooks, gc, api exports (native perlmod conversion)
perl/PVE/API2/Ext/Meta.pm
namespaces/              packaged example namespaces (none required)
patches/                 lifecycle.toml + libpve-guest-common-perl_AbstractConfig.pm.diff (one file)
pve-ext/                 the extension layer (own package)
ui-extjs/                the editor tab (plain JS, native ExtJS panel)
debian/, Makefile        packages: pve-ext, pve-meta, libpve-meta-rs-perl
```

## 10. What revision 5 deletes

Reserved `scopes` key and every rule keyed on it (opaque-leaf addressing, touched-path
collapsing, `check_scopes_write`, authid key lint); the strict/lenient scope parser
split; the per-request datacenter read for grants; `WriteGate`, `lint_at`/`lint_relaxed*`,
the lint-finding subset check; `may_name` and all message redaction; the `keys` wire
field and the UI's YAML key scanner; the comment-key access machinery (`covers` aliasing
beyond the one sibling rule, bare-`__` prefix rule, mid-path rejection, `filter`'s comment
pass); orphan listing/deletion/access rules; the clone and backup hooks and their diffs
(destroy came back as a hook in `AbstractConfig`, §6, together with a new create
hook — what went is the GC *timer*, not the destroy hook); JSON-string crossings for grants, guest lists and results
(`_grants_json`, `_inflate_view`, `parse_grants`, `data_json`); the "View as" selector.

## 12. Why revision 6 splits the registration

Revision 5 had one object doing two jobs, and the type said so: `authid` was
**mandatory**, so a schema could not be declared without naming a principal. To say
"a `hass` prefix exists and looks like this" you had to invent an operator to own it.

Three pieces of evidence that it was one concept too few, all of them found in use
rather than in review:

* **The lab config had already split it by hand.** One file carried the *grammar* with
  `selector: {all: true}`, another carried the *access* with `selector: {tag: traefik}` —
  same prefix, two files, because one object could not express both cleanly.
* **A real bug came out of it.** Declared-but-unset rows were driven by a *grant's*
  grammar, so a broadly-scoped principal painted one operator's rows onto every guest in
  the cluster. With the schema on the namespace, the selector that governs rows is the
  namespace's and that bug is not expressible.
* **Two files naming the same authid silently unioned their scopes.** Nobody decided
  that; it is what happens when identity is a field rather than the file.

And the rule that settles it: **schemas shadow, grants accumulate** (§3.1, §3.2). Two
opposite nesting semantics cannot live on one object. Revision 5's did — which is why
overlapping grammars unioned their findings and why a path covered by two schemas got
whichever the iteration reached last.

What the split buys beyond correctness is that the store's vocabulary loses the word
*operator* entirely. It knows namespaces and grants. An operator is an installer — a
package that drops a namespace, has an administrator issue a grant, and creates an LXC
with credentials injected. Nothing at runtime needs the concept, so nothing in the core
carries it.

## 11. Deviations from DIRECTION.md, with reasons

* **Lifecycle is snapshot-only, not zero.** Rollback restoring metadata was an explicit
  product decision; it costs one patched file in the least-churned package.
* **The pwt implementation was not dropped by fiat — it was compared first.** DIRECTION
  §5.4 argued for switching on the premise that `Ext.tree.Panel` had no pwt equivalent;
  it does (`DataTable` + `TreeStore`, used by PDM). Both were built to this §8 and judged
  on the lab. ExtJS won on size and build surface, not on the doc's original argument,
  and pwt was then removed (§8, git tag `pwt-ui-removed`).
