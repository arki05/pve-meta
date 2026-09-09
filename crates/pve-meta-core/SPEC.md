# pve-meta-core — specification

Pure-Rust, platform independent library: the document model, formats, patch engine,
registry, file store and API layer behind `pve-meta`. Its only consumer is
`crates/pve-meta-perl` (`PVE::RS::Meta`), which adds the perlmod bindings and nothing
else — there is no daemon and no CLI. No networking, no async, no PVE-specific crates.
Must build and pass tests on macOS and Linux.

`docs/DESIGN.md` (revision 6) is the source of truth for behaviour; where it and this
document disagree, DESIGN.md wins. This file describes only what exists: revision 5's
§10 deletions are gone from the code and from here.

## 1. Data model (`model.rs`)

* `pub type Value = serde_json::Value;` built with the `preserve_order` feature, so
  objects are ordered maps (insertion order = human order).
* A **document** is a `Value::Object` at the top level.
* **One lint**, `pub fn lint(doc: &Value) -> Vec<Lint>` (`Lint { path: Path, msg: String }`),
  run on the planned document and nowhere else:
  1. top level must be an object;
  2. no `null` anywhere (absent means unset);
  3. every object key must match `^[A-Za-z0-9_@!-]+$` — no dots (they separate path
     segments); `@` and `!` are permitted and are not path separators;
  4. **comment keys**: a key ending in `__` is a comment key. Its value must be a
     string. `__` alone documents the containing map; `foo__` documents sibling `foo`
     (the sibling need not exist);
  5. numbers: integers and finite floats only (NaN/inf rejected by serde_json anyway).
* There is no second variant. Revision 4 had four (`lint`, `lint_relaxed`,
  `lint_relaxed_at`, `lint_at`) plus a before/after finding-set subset check, because
  the lint had been narrowed by privilege. Nothing narrows it now, so a payload is
  never linted on its own — it is linted where it lands, as part of the document.
* **No key is reserved in any document.** Access-control data lives outside documents
  entirely (§7), so `scopes` is ordinary user data and an authid-shaped key is not
  special anywhere.
* `pub fn get_path<'a>(doc: &'a Value, path: &Path) -> Option<&'a Value>`; arrays are
  indexed by numeric segments.
* `pub const COMMENT_SUFFIX: &str = "__";` `pub fn is_comment_key(k: &str) -> bool`.

## 2. Paths (`path.rs`)

* `pub struct Path(Vec<String>)` — segments. Parse from dotted `a.b.c` and from slash form
  `/a/b/c` or `a/b/c` (`Path::parse(&str)`); `Display` is dotted; empty path = document
  root.
* `is_prefix_of(&self, other: &Path) -> bool` (root is a prefix of everything), `push`,
  `parent`, `join`, `new`, `segments()`, `last()`.
* Segment validation: the same charset as keys; numeric segments (array indices) are
  allowed when addressing arrays.
* `is_prefix_of` is a **pure structural predicate**. The one comment-key aliasing rule a
  scope needs (`p` covers `p__`) lives in `scopes.rs`, not here.

## 3. Patches (`patch.rs`)

Merge-patch semantics (RFC 7386) with explicit delete:

* A patch is a `Value::Object`. Applying: for each `(k, v)` in the patch — if `v` is an
  object and the target has an object at `k`, recurse; if `v` is `null`, delete `k`;
  otherwise set `k = v` (replacing whatever was there, including objects).
* `pub fn apply_patch(doc: &mut Value, patch: &Value) -> Vec<Touched>` where
  `Touched { path: Path, op: Set | Delete }`. The touched set is the **minimal** set of
  paths that changed: recursion yields leaf paths; a replaced/deleted subtree yields its
  root path; setting a key to the value it already has yields nothing; deleting a missing
  key yields nothing.
* `pub fn diff(old: &Value, new: &Value) -> Vec<Touched>` — the same notion of touched
  paths for a full-document replace: recurse into objects that exist on both sides; a key
  only in `new` → `Set` at that path; only in `old` → `Delete`; differing non-object
  values (including arrays, which are atomic) → `Set`.
* There is no `lint_patch`: the one lint judges the document the patch produces.
  **Invariant, still enforced:** no patch may produce a document `lint` rejects. An
  object patch value is therefore always *applied*, never stored verbatim — against an
  absent container a patch of nothing but deletes is a no-op that creates nothing, and
  against an existing scalar or array the value becomes a map (a change in itself,
  reported at the container's own path). A patch key that only ever deletes never
  reaches the document at all.

There is no `make_patch`: nothing in the API or the hooks builds a patch from two
documents.

## 4. Formats (`format.rs`)

`pub enum Format { Yaml, Json }` with `ext()` (`yaml`, `json`), `from_ext(&str)` (also
accepts `yml`), `Display`, `FromStr`, `ALL`.

**YAML is the only on-disk format.** JSON exists solely as a wire format for a view's
`data`. There is no TOML.

* `pub fn parse(format, text: &str) -> Result<Value, Error>`; after parsing, `lint` is run
  and any lint → `Error::Lint(Vec<Lint>)`. Format-specific rejections:
  * YAML: parse with `serde_yaml_ng` into `Value`. Additionally scan with `saphyr-parser`
    events and reject anchors, aliases and explicit tags (`!`), and reject non-string
    keys. YAML `~`/empty-value nulls are rejected by the generic no-null rule. Numbers
    stay numbers, `true`/`false` stay booleans; the YAML 1.1 words `yes/no/on/off` remain
    strings (tested).
  * JSON: `serde_json::from_str`; comments and trailing commas not allowed.
* `parse_raw(format, text)` is the same without the lint (crate-internal): the store's
  tolerant read, the view payload parsers and the namespace/grant parsers all use it.
* `pub fn dump(format, doc: &Value) -> String` — canonical dump, always ends with a single
  `\n`, preserves key order:
  * YAML: block style, 2-space indent, no document markers (serde_yaml_ng defaults).
  * JSON: `serde_json::to_string_pretty` (2 spaces) + newline.
  * A document is always rewritten canonically from its value: free-form comments in the
    previous text are lost. Comment *keys* are ordinary data and survive.
* Round-trip property tests: `parse(f, dump(f, doc)) == doc` for both formats, and
  cross-format conversion preserves order.

## 5. Digest (`digest.rs`)

`pub fn digest(bytes: &[u8]) -> String` — lowercase hex SHA-256 of the raw file bytes.

## 6. Views (`view.rs`)

A *view* is a `Path` prefix addressing **only through maps**, never through an array or a
scalar (`Error::InvalidPath` otherwise).

* `extract(doc, prefix) -> Option<Value>` — the subtree, prefix stripped.
* `replace(doc, prefix, subtree) -> Result<Vec<Touched>>` — wholesale replace. An empty
  object stores an **empty map**; it does not delete the key. A root prefix replaces the
  whole document.
* `merge(doc, prefix, patch) -> Result<Vec<Touched>>` — RFC 7386 merge relative to the
  prefix, `null` deletes. A non-object at the prefix is merged over as `{}`.
* `remove(doc, prefix) -> Result<Vec<Touched>>` — deletes the subtree (empties the
  document at the root). This, not `replace` with `{}`, is what `DELETE` uses.
* `filter(doc, readable_prefixes) -> Value` — the "no view" read: the union of the
  prefixes, unstripped, in the document's own order. **One pass, one predicate**: a key
  is emitted exactly when `scopes::covers` says a prefix covers it, which is exactly what
  `Grants::can_read` answers for it. That one rule is all comment keys need — `p__`
  travels with a readable `p`, a map's bare `__` documents the whole map and travels only
  where the map does. Revision 4's dedicated second pass for comment-key visibility is
  gone. A document that is **not a map at the top level** is the empty document here (no
  non-root prefix can address any part of it); a root prefix returns it unchanged from
  the early return.
* `render(value, format) -> String`, `parse(text, format)` (a replace payload),
  `parse_patch(text, format)` (a merge payload; the only rule is "must be an object",
  since a non-object patch would apply as a silent no-op).
* **Nothing in this module lints.** A payload is not a document until it has been spliced
  in, so the one lint runs on the planned document in `api.rs`, where its findings name
  real document paths.

Two invariants the write-authorization boundary depends on:

* **The touched list is complete.** Every operation reports at least one touched path
  whenever it changes the document — including the corners where the content diff is
  empty because the content is an empty map (creating `a: {}` where `a` was absent;
  removing an existing `a: {}`). Nothing may create or destroy structure with a vacuous
  `touched: []`.
* **A no-op merge mutates nothing.** Intermediate maps are materialised only once the
  patch is known to write something.

Touched paths are reported *below* the view prefix. Where a whole subtree disappears at
once the report names that subtree's path rather than every leaf under it, which only
makes the write check stricter (every reported path is an ancestor of what it stands
for).

## 7. Namespaces and grants (`registry.rs`)

Two concepts in two drop directories, both **outside** the documents, one file each,
parsed strictly and independently (`docs/DESIGN.md` §3). Revision 6 split revision 5's
single "operator registration" in two; nothing here carries the *operator* concept any
more — this crate knows namespaces and grants (`docs/DESIGN.md` §12).

`Namespace { prefix, description?, selector, schema? }`,
`Grant { name, authid, description?, grants: Vec<GrantEntry> }`,
`GrantEntry { prefix, mode, selector }`.

**Namespaces — what a prefix is** (`docs/DESIGN.md` §3.1):

* `NAMESPACE_PACKAGED_DIR = "/usr/share/pve-meta/namespaces"` — packaged defaults,
  dropped in by an operator's own `.deb`;
* `NAMESPACE_CLUSTER_DIR = "/etc/pve/meta.d/namespaces"` — cluster-wide (pmxcfs),
  **overriding the packaged file of the same name**;
* `NAMESPACE_DIRS_ENV = "PVE_META_NAMESPACE_DIRS"` — a colon-separated override, lowest
  precedence first, for the tests and `test/basic.pl`.

**The file name is the prefix** — there is no `prefix:` field to disagree with it. A
prefix segment is `[A-Za-z0-9_@!-]+` and dots are only separators, so a namespace file
name can never contain a slash, never start with a dot and never escape its directory.
A namespace names **no principal**.

```yaml
# namespaces/traefik.yaml — declares the prefix `traefik`
description: Traefik dynamic configuration
selector: { tag: traefik }   # or { all: true }
schema:                      # optional, PVE::JSONSchema dialect, passed through
  type: object
```

**Grants — who may touch a prefix** (`docs/DESIGN.md` §3.2):

* `GRANT_CLUSTER_DIR = "/etc/pve/meta.d/grants"` — cluster only. **There is deliberately
  no packaged grants directory**: an operator's `.deb` may ship a namespace (a
  declaration) but must never ship its own grant, which would be self-registration, and
  dpkg cannot write into pmxcfs;
* `GRANT_DIRS_ENV = "PVE_META_GRANT_DIRS"` — the same colon-separated override.

```yaml
# grants/traefik.yaml
authid: svc@pve!traefik
description: Traefik dynamic-configuration provider
grants:
  - prefix: traefik            # any dotted path, nested allowed
    mode: rw                   # ro | rw
    selector: { all: true }    # or { tag: traefik }
```

* `parse_namespace(name, text) -> Result<Namespace>` and
  `parse_grant(name, text) -> Result<Grant>` — **strict**: `deny_unknown_fields`
  throughout, so neither file can express the other's job (a grant entry with a `schema`
  and a namespace with an `authid` are both errors). A namespace's `name` must be a valid
  **non-root** prefix and must not contain a slash; a grant's `authid` must satisfy
  `is_authid` (`PVE::AccessControl`'s `$userid_or_token_regex` transliterated), each
  grant prefix must parse and be **non-empty** (whole-document access comes from PVE
  ACLs, never from a grant), `mode` is `ro`/`rw`, and `selector` is exactly one of
  `{all: true}` / `{tag: <name>}`. Room is left in the format for `{pool: <name>}`; it is
  deliberately not implemented, so it is currently an unknown field.
* `load_namespaces(dirs) -> Vec<Namespace>` and `load_grants(dirs) -> Vec<Grant>` — read
  `*.yaml` from each directory in order, later directories overriding earlier ones **by
  file name**. A missing directory is not an error. **A malformed file is skipped with a
  `warn!` and contributes nothing** — files are independent, so one operator's typo can
  never take another's grants or another's schema away. That independence is the whole
  reason this data left `datacenter.yaml`.
* `load_namespaces` returns them **sorted longest-prefix-first** (ties by name), which is
  the order `governing` relies on; `load_grants` returns them sorted by file name.
* `namespace_dirs()` / `grant_dirs()` — the constants above, or the env override.
  `load_namespaces_default()` / `load_grants_default()` are those composed with the
  matching loader.
* `governing(namespaces, path, tags) -> Option<&Namespace>` — **most-specific wins;
  schemas never merge** (`docs/DESIGN.md` §3.1). The governing namespace is the one with
  the longest prefix covering `path` among those whose selector matches; no other
  contributes to that path. Given the load order it is the first match.
* `applicable(namespaces, tags) -> Vec<&Namespace>` — the namespaces reaching a guest
  carrying `tags`, most-specific first.
* `scopes_for(grants, authid, tags) -> Vec<Scope>` — the union of the entries of every
  grant file for that authid whose selector matches the guest's tags. **Grants accumulate
  by containment** (`docs/DESIGN.md` §3.2): a grant on `homelab` covers `homelab.docker`,
  because `scopes::covers` is prefix containment. That is deliberately the opposite of
  `governing` — shape has one owner so it shadows, permission is a union so it adds, and
  the two rules cannot live on one object.
* `Selector` serializes as it is written (`{all: true}` / `{tag: <name>}`), so
  `GET /meta/namespaces` and `GET /meta/grants` hand the UI the same shape an
  administrator edits.

Tag membership is a **selector pve-meta implements itself** — PVE has no tag ACL. Adding
the tag is the deliberate act of including that guest.

## 8. Resolved access (`scopes.rs`)

* `Grants { full_read, full_write, scopes: Vec<Scope> }`, `Scope { prefix: Path, mode: Ro|Rw }`
  — one request's *resolved* answer, not the grant files of §7. Built by `api::grants`
  from the ACL answers Perl passes plus `registry::scopes_for`; it is never deserialized
  from a wire string.
* `can_read(path)` / `can_write(path)` / `readable_prefixes()` / `check_write(&[Touched])`
  / `is_empty()`.
* `covers(prefix, path)`: plain prefix containment, plus the **one** comment-key rule — a
  scope on `p` also covers the sibling comment key `p__` at the same depth. The bare `__`
  map comment has no subject and is not aliased. `covers` is `pub(crate)` because
  `view::filter` must give the same answer on the read side.
* `check_write(&[])` is vacuously `Ok`, so it is **not** a security boundary on its own;
  the API layer's up-front `can_write(view)` is (§10).
* Scopes apply to guest documents only. `api::grants` returns an empty scope list for
  `DocId::Datacenter`, which is what keeps a grant from being able to reach it.

## 9. Store (`store.rs`)

`pub struct MetaStore { root: PathBuf }` — root is `/etc/pve/meta` in production, a
tempdir in tests. Directory created on demand.

Naming: `DocId::Guest(u32)` ↔ `<vmid>.yaml`; `DocId::Datacenter` ↔ `datacenter.yaml`;
guest snapshot ↔ `<vmid>.<snapname>.yaml` (snapname matches `^[A-Za-z][A-Za-z0-9_-]*$` and
is never a format extension). `pub const DISK_FORMAT: Format = Format::Yaml`.

All writes are atomic: write `.<name>.tmp.<hostname>.<pid>.<seq>` in the same directory,
then `rename`. The hostname and the per-call counter matter because `/etc/pve/meta` is one
pmxcfs directory shared by every node.

API (all synchronous, `Result<_, Error>`):

* `locate(id) -> Option<PathBuf>`
* `read(id) -> Document { id, path, raw, value, parse_error, digest, mtime }`
  (`Error::NotFound` if absent, `Error::TooLarge` above `MAX_READ_BYTES`, `Error::Parse`
  for bytes that are not UTF-8 at all). **Reads never lint and never fail on the
  document's own content**: the parse is `format::parse_raw`, and text it rejects yields
  `parse_error: Some(msg)` with the empty document as `value` and the file's real bytes
  and digest. Content can only be invalid out of band (a hand-edited file, pmxcfs
  replication), and failing on it would make a typo unrepairable through the API.
* `digest_of(id) -> Option<String>` — the content identity without parsing (or keeping)
  the document, so a write can still carry a compare-and-swap precondition against a file
  `read` refuses to read.
* `stored_vmids() -> Vec<u32>` — every vmid the store holds any file for, document or
  snapshot copy, ascending. The store's half of the GC (§10); a vmid whose *only* file is
  a snapshot copy is found too.
* `check_precondition(id, expected_digest)` — the compare-and-swap rule, without writing.
* `put_raw(id, text, expected_digest) -> PutResult { document, touched }` — full text
  replace, creating the document if absent. **One gate, for every caller**: the new text
  is parsed and linted, so nothing this store writes can ever fail `model::lint`.
  (`WriteGate` and `put_raw_gated` are gone with the privilege-narrowed lint.) The
  existing content is parsed leniently — a syntax error, or a file above the read cap,
  diffs as the empty document — since it is only read to diff against and must not be
  able to block a repair. `touched = diff(old, new)`; text is stored as given (only
  normalised to end with a single newline).
* `delete(id) -> bool` — the **current document only**. Idempotent: `false` if there was
  nothing to remove.
* `purge(vmid) -> usize` — the document *and every snapshot copy*, idempotent, returning
  the number of files removed. Only the GC calls this; the REST `DELETE` calls `delete`.
* `snapshot(vmid, name)` (no-op `Ok(false)` without a document), `rollback(vmid, name)`
  (→ `RollbackOutcome::Restored | RemovedNoSnapshot | NoOp`), `delete_snapshot(vmid,
  name) -> bool` (idempotent), `list_snapshots(vmid) -> Vec<String>`.
* `version() -> StoreVersion { token, changed }` — token = SHA-256 over the sorted list of
  `(file name, content identity)`, computed from the files' actual bytes on **every**
  call. There is no `(mtime, len)` cache: the bindings build a fresh store per request so
  it could never hit, and pmxcfs's mtime granularity cannot distinguish two same-length
  writes in one tick so it would be unsound if it did. Documents are tiny.
* Size limits: `WARN_BYTES = 256 KiB`, `MAX_BYTES = 512 KiB` (writes) and
  `MAX_READ_BYTES = 4 MiB` (reads) → `Error::TooLarge`. The read cap bounds what an
  out-of-band file can cost; it is eight times the write cap, so nothing this store wrote
  can ever hit it. A file above it is never parsed and stays replaceable. `MAX_BYTES` is
  a **backstop, not the operative limit for an API write**: pveproxy refuses a body of
  about that size first (measured: 520 000 B through, 530 000 B → "for data too large",
  HTTP 501). It bounds a hand-written or replicated file being rewritten, and the pmxcfs
  size budget.
* **One identity function, `identify(path)`, and the read cap applies to all of it.**
  `digest_of`, `check_precondition`/`put_raw`'s precondition and `version`'s per-file
  entry all go through it, so the digest a caller reads out of a `GET` is the string
  their next `PUT`'s precondition is compared against. Below the cap it is the SHA-256 of
  the bytes; above it, a domain-separated SHA-256 over `(len, mtime)` — a surrogate, so
  that a single multi-megabyte file dropped in out of band is not read and hashed by
  every 5 s poll and every listing. It still moves when the file does, which is all the
  token and the compare-and-swap need of it.
* **A file that is not there is never an `Error::Io`.** Reads are unlocked, writes hold
  `pve-meta-<id>` and the GC holds `pve-meta-gc` — disjoint domains — so a file can
  vanish between any two syscalls. `read` answers `NotFound` (no `locate`-then-read
  pair), `delete`/`delete_snapshot` are idempotent, and the directory walks (`version`,
  `stored_vmids`) skip an entry that disappeared under them. There is no
  `exists()`-then-act pair left in the module.

The digest precondition is enforced **here and only here**: `check_digest` treats `None`
as "no precondition" and `Some("")` as matching a *missing* document, which is what makes
the documented GET-then-PUT create flow work (a missing document reports digest `""`).

Serialising concurrent writers is not this layer's job — `PVE::API2::Ext::Meta` runs each
write inside `PVE::Cluster::cfs_lock_domain("pve-meta-<id>", 10, …)`.

## 10. API layer (`api.rs`)

The request-shaped layer `PVE::RS::Meta`'s `api_*` functions export (`docs/DESIGN.md` §5).
It lives in this crate, not in the bindings crate, because it is the project's security
boundary and must be unit-testable without `libperl-dev` and without a cluster.

* `pub struct CallerAcl { authid, read, write, tags }` — the caller as Perl computes it,
  a **native Perl hash** on the wire; `read`/`write` arrive as ordinary scalars and are
  converted by truthiness.
* `pub struct GuestInput { vmid, node, type, name, tags, read, write }` — one vmlist row,
  likewise native.
* `grants(grants, doc_id, acl) -> Grants` — the ACL answers plus, for a guest,
  `registry::scopes_for(grants, acl.authid, acl.tags)`. Empty scopes for the datacenter.
* `version(store, detail)`, `access(grants, doc_id, acl)`, `grants_list(grants)`,
  `namespaces_list(namespaces)`,
  `list_guests(store, grants, authid, guests, has)`,
  `get_document(store, grants, id, view, format, acl)`,
  `put_document(store, grants, id, view, format, payload, mode, digest, dry_run, acl)`,
  `delete_document(store, grants, id, view, digest, acl)`,
  `gc_candidates(store, vmids) -> Vec<u32>`, `gc_purge(store, vmid, live) -> usize`,
  `gc(store, vmids) -> usize`.
* Errors are `anyhow::Error`s whose `Display` is `"NNN: message"` (an HTTP status prefix);
  `PVE::API2::Ext::Meta::_call` parses that and re-raises through `PVE::Exception`.
  **Messages name paths.** There is no `may_name`, no redaction, no counting of hidden
  findings: key-name disclosure is out of scope under the threat model
  (`docs/DESIGN.md` §1).

**Wire contract.** Everything crosses as native hashes and arrays. The single exception
is the client-supplied `data` parameter, a JSON string because that is what the REST
parameter is, decoded once here. `_grants_json`, `_inflate_view`, `parse_grants` and
`data_json` are gone; so is the `keys` field, since key order is no longer a wire
contract — the tree UI sorts.

**Write authorization** — decided from the request, never from a diff:

0. a write against a document whose content could not be recovered is refused with 400
   unless it *replaces the file whole* — a root `replace` or a root `DELETE`, both of
   which already require `full_write`. The value planned against is the empty document,
   so anything narrower (a root `merge` included) would silently discard the file. **One
   condition, three causes**: the file does not parse (or is not UTF-8), it is above the
   read cap, or it parses to something that is not a mapping — `null` from an empty or
   comment-only file, a scalar, a list. Keying this on the parse alone let the third
   through;
1. `can_write(view)` must hold before anything is computed, and a caller without
   `full_write` may not write the root view at all;
2. the mutation is planned against a **clone** and every path the plan touches is checked
   with `check_write`;
3. the planned document is linted — **once**, the same way for every caller, before the
   `dry_run` branch so a dry run validates exactly what the write validates. The 400
   names the offending path;
4. only then is the planned value written.

**Read authorization**: a caller with no readable prefix at all gets 403, not an empty
document with the real digest (which would be a change-detection oracle). A caller with a
*partial* grant does get the whole document's digest — scoped writers need it for
compare-and-swap PUTs.

**Unrecoverable documents** (`docs/DESIGN.md` §4) are a per-document condition, never
cluster-wide — nothing reads `datacenter.yaml` on a guest request any more.
`format=yaml` for a caller with `full_read` answers 200 with the file's raw `text` plus
`parse_error`, so an administrator can repair it; `format=json`, any caller without full
read, and everyone when the bytes were never read at all (above the cap) gets **422**
naming the condition and the two repairs. It is always *reported*, never rendered as an
empty document: the file is there, and a caller has to know that before writing over it.
A root `replace` or `DELETE` repairs it.

**A write that changes nothing writes nothing.** When the plan touches no path *and* the
canonical dump equals the bytes already on disk, the write is skipped: rewriting the file
would advance its mtime and so `version()`'s `changed`, while the content `token`
correctly does not move. Both halves are required — `touched: []` alone also describes
the repair of a document that reads back as empty because it is unrecoverable.

**View addressing** (`parse_view`) parses a path and does nothing else. No key is
reserved, no segment is special, and there is no mid-path comment-key rejection: a view
through a comment key would materialise a map at `q__`, and the one lint on the planned
document is what refuses that — for every caller and every verb, with no second rule to
keep in step.

One unreadable document never denies a *listing*: `list_guests` reads tolerantly, so a
guest whose document does not parse (or is above the read cap) is listed with its real
digest, rather than 400-ing `GET /meta/guests` for every principal.

`list_guests` takes the vmlist rows *from Perl*. This crate never opens
`/etc/pve/.vmlist` or a guest config: there is one reader of each per request, and
guest-config parsing is not re-implemented in a second language. `node`, `name` and
`tags` are returned only to a caller with `VM.Audit` on that guest.

**The GC is two-phase, because its phases are locked differently.** Writes serialize
under `cfs_lock_domain("pve-meta-<vmid>")` and a GC pass under
`cfs_lock_domain('pve-meta-gc')` — disjoint, so a whole-sweep GC deletes a document
written after its vmlist snapshot was taken (destroy 999500, recreate a guest there, PUT
its metadata; the GC pass already in flight purges the fresh document with the PUT
answered 200). So `gc_candidates(store, vmids)` only *nominates* — the stored vmids
absent from the vmlist — and `libexec/gc` purges them **one at a time**, each under that
vmid's own write lock, calling `gc_purge(store, vmid, live)` with a vmlist re-read inside
that lock. `gc_purge` re-checks the vmid against `live` and refuses an empty `live`
outright, so the guard is on the destructive call rather than only in its one caller.

`gc(store, vmids)` is the unvalidated whole sweep those two replace: candidates plus an
immediate `purge` of each, holding no per-vmid lock. It stays because it is what the
two-phase path is tested against, and because it is the one place an empty `vmids`
legitimately means "purge every guest document" (`test/basic.pl`). Together they replace
the destroy hook *and* the entire orphan concept — there is no orphan listing, no orphan
grant rule and no orphan delete anywhere. The datacenter document is never a guest.

## 11. Errors (`error.rs`)

`thiserror` enum `Error`: `Parse { format, msg }`, `Lint(Vec<Lint>)`,
`DigestMismatch { expected, actual }`, `NotFound(DocId)`, `TooLarge { size, max }`,
`InvalidPath(String)`, `InvalidName(String)`, `Registration(String)`,
`Io(#[from] std::io::Error)`, `Other(#[from] anyhow::Error)`. `Display` messages are
suitable for an HTTP API response body.

## 12. Tests

Unit tests per module plus `tests/` integration tests using `tempfile`. Required
scenarios:

* **model**: each lint rule; comment keys accepted; the lint names the offending path;
  no key is reserved in any document;
* **patch**: apply/diff properties (arrays, nested deletes, no-op sets); no patch
  produces a document the lint rejects; a delete-only patch key never reaches the
  document;
* **format**: round trips for both formats, order preservation, YAML `yes/no` stay
  strings, YAML anchors/aliases/tags rejected, JSON comments rejected;
* **views**: `replace` with `{}` stores an empty map and round-trips; `remove` deletes; a
  no-op `merge` mutates nothing at any depth; creating or removing an empty map still
  reports a touched path; `filter` keeps a comment key in either authoring order and
  never emits one the same grant's `can_read` refuses; a list- or scalar-rooted document
  filters to nothing; `parse` does not lint the payload, and `parse_patch` requires an
  object and nothing else;
* **registry**: a namespace's prefix is its file name, dotted and any depth; a file name
  that is not a valid prefix is refused; strict parsing on both kinds rejects an unknown
  field, a missing/ambiguous selector, a bad authid, an empty prefix and a bad mode; a
  grant has no schema and a namespace has no authid; a cluster file overrides the
  packaged one of the same name wholesale while unrelated files survive; a malformed file
  is skipped and costs no other file anything; namespaces load most-specific first and
  `governing` takes the first match, respecting the selector; grants accumulate by
  containment, which is the opposite of namespaces, with the selector still gating;
  `Selector` serializes as it is written; a missing directory is not an error for either
  kind; `is_authid` matches `PVE::AccessControl`'s shape;
* **scopes**: a scope covers its own `p__` and nothing else's; `readable_prefixes`;
  `check_write` reports the first denied path; an empty touched list is vacuously ok;
* **store**: create/put_raw/delete/purge, digest mismatch, `Some("")` against a missing
  document, snapshots/rollback, `delete` leaves snapshots alone and `purge` removes them,
  an api-delete-then-rollback end-to-end case, version token changes on write and not on
  read and distinguishes same-length writes, no temp files left behind; an out-of-band
  invalid document is still readable and still repairable; a document whose *syntax* is
  broken reads as `parse_error` + the empty value + the real digest; a file above the
  read cap is `TooLarge` on read, is never read by `version`/`digest_of`, and is still
  replaceable against the identity they report; `delete` is idempotent and says whether
  it removed anything; a file that vanished between syscalls is `NotFound`, not
  `Error::Io`, and `version` skips it under a concurrent deleter; there is one write gate
  and it is the document lint; `stored_vmids` covers documents and snapshot copies
  alike;
* **api**: selectors resolve against the guest's tags and scopes never apply to the
  datacenter document; a zero-grant (and a wrongly-scoped) token cannot create structure
  through an empty merge or a `{}` replace at any prefix, and cannot write the root view;
  a 403 names the path it refused; the one lint runs on the planned document for every
  caller and names the offending path whoever asks, including for a view through a
  comment key; `dry_run` and the real write agree and a dry run never writes;
  merge-with-null deletes and `{}` replace stores an empty map end to end; a no-grant
  read is 403 while a partial grant still gets the digest; a scoped read sees only its
  own prefixes and never the bare `__`; a full read of the root view returns the file's
  own text; an unparseable document is yaml+`parse_error` / 422 / root-repairable; a
  document above the read cap is refused on read, listed without taking the listing down,
  and repairable through the digest the listing reported; a document that parses to
  `null`, a scalar or a list is the same condition — reported, not rendered, repairable
  only as a whole — and cannot be used as an oracle through `?has=`; a write that changes
  nothing does not rewrite the file; a document that vanished mid-request is 404-shaped,
  never a 500; `list_guests` gates node/name/tags on `VM.Audit`; `access` reports
  resolved scopes; a grant for another authid grants nothing; `grants_list` returns every
  grant; `gc` removes documents and
  snapshot copies whose vmid is gone, is idempotent, and never touches the datacenter
  document; `gc_purge` keeps a vmid the re-read vmlist has and refuses an empty one.
