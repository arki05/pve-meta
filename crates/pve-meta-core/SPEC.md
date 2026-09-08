# pve-meta-core — specification

Pure-Rust, platform independent library: the document model, formats, patch engine, file
store and API layer behind `pve-meta`. Its only consumer is `crates/pve-meta-perl`
(`PVE::RS::Meta`), which adds the perlmod bindings and nothing else — there is no daemon
and no CLI. No networking, no async, no PVE-specific crates. Must build and pass tests on
macOS and Linux.

`docs/DESIGN.md` is the source of truth for behaviour; §8 and §9 record the binding
decisions from the 2026-09-07 and 2026-09-08 reviews that this document was reconciled
against.

## 1. Data model (`model.rs`)

* `pub type Value = serde_json::Value;` built with the `preserve_order` feature, so
  objects are ordered maps (insertion order = human order).
* A **document** is a `Value::Object` at the top level.
* Rules (checked by `pub fn lint(doc: &Value) -> Vec<Lint>`; `Lint { path: Path, msg: String }`):
  1. top level must be an object;
  2. no `null` anywhere (absent means unset);
  3. every object key must match `^[A-Za-z0-9_@!-]+$` — no dots (they separate path
     segments); `@` and `!` are allowed so a PVE authid (`user@realm!tokenid`) can be a
     key, which the datacenter document's `scopes` map needs. Inside the **top-level
     `scopes` map** a full PVE authid is accepted too (`model::is_authid`, dots
     included): `PVE::AccessControl`'s `$userid_or_token_regex` transliterated,
     `^[^\s:/]+@[A-Za-z][A-Za-z0-9.\-_]+(?:![A-Za-z][A-Za-z0-9.\-_]+)?$`. Lint only
     *relaxes* there; the shape rule for a real `scopes` map is `parse_scopes`, at
     write time (`docs/DESIGN.md` §9);
  4. **comment keys**: a key ending in `__` is a comment key. Its value must be a string.
     `__` alone documents the containing map; `foo__` documents sibling `foo` (the
     sibling need not exist).
  5. numbers: integers and finite floats only (NaN/inf rejected by serde_json anyway).
* `pub fn lint_relaxed(doc: &Value) -> Vec<Lint>` — the same recursive rules without
  rule 1, for a *view*'s value (which may be an array or a scalar at its own root).
* `pub fn strip_comments(doc: &mut Value)` removes all comment keys recursively (objects
  inside arrays too).
* `pub fn top_level_keys(doc: &Value) -> Vec<String>`: top-level non-comment keys, in
  order — the wire API's `keys` field. (Named after the wire contract: DESIGN explicitly
  disclaims the word "namespaces".)
* `pub fn get_path<'a>(doc: &'a Value, path: &Path) -> Option<&'a Value>`; arrays are
  indexed by numeric segments.
* `pub const COMMENT_SUFFIX: &str = "__";` `pub fn is_comment_key(k: &str) -> bool`.
* `pub const SCOPES_KEY: &str = "scopes";` `pub fn is_authid(k: &str) -> bool` — the one
  reserved top-level key and the rule for its own keys.

## 2. Paths (`path.rs`)

* `pub struct Path(Vec<String>)` — segments. Parse from dotted `a.b.c` and from slash form
  `/a/b/c` or `a/b/c` (`Path::parse(&str)`); `Display` is dotted; empty path = document
  root.
* `is_prefix_of(&self, other: &Path) -> bool` (root is a prefix of everything), `push`,
  `parent`, `join`, `new`, `segments()`, `last()`.
* Segment validation: the same charset as keys; numeric segments (array indices) are
  allowed when addressing arrays.
* `is_prefix_of` is a **pure structural predicate**. The comment-key aliasing a scope
  needs (`p` covers `p__`) lives in `scopes.rs`, not here.

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
* `pub fn lint_patch(patch: &Value) -> Vec<Lint>` — the document key rules, except that
  `null` is allowed anywhere as the delete marker (and as a comment key's value, meaning
  "delete this note").

There is no `make_patch`: nothing in the API or the lifecycle hooks builds a patch from
two documents.

## 4. Formats (`format.rs`)

`pub enum Format { Yaml, Json }` with `ext()` (`yaml`, `json`), `from_ext(&str)` (also
accepts `yml`), `Display`, `FromStr`, `ALL`.

**YAML is the only on-disk format** (`docs/DESIGN.md` §8). JSON exists solely as a wire
format for a view's `data`. TOML — on-disk support, `convert`, `settings.default_format`
and the format-preserving `toml_edit` edit engine — was removed: none of it was reachable
from the shipped API, and it was the only way to end up with a document whose formatting
an edit would then silently destroy.

* `pub fn parse(format, text: &str) -> Result<Value, Error>`; after parsing, `lint` is run
  and any lint → `Error::Lint(Vec<Lint>)`. Format-specific rejections:
  * YAML: parse with `serde_yaml_ng` into `Value`. Additionally scan with `saphyr-parser`
    events and reject anchors, aliases and explicit tags (`!`), and reject non-string
    keys. YAML `~`/empty-value nulls are rejected by the generic no-null rule. Numbers
    stay numbers, `true`/`false` stay booleans; the YAML 1.1 words `yes/no/on/off` remain
    strings (tested).
  * JSON: `serde_json::from_str`; comments and trailing commas not allowed.
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
  object stores an **empty map**; it does not delete the key (`docs/DESIGN.md` §8).
  A root prefix replaces the whole document and applies the full `lint`.
* `merge(doc, prefix, patch) -> Result<Vec<Touched>>` — RFC 7386 merge relative to the
  prefix, `null` deletes. A non-object at the prefix is merged over as `{}`.
* `remove(doc, prefix) -> Result<Vec<Touched>>` — deletes the subtree (empties the
  document at the root). This, not `replace` with `{}`, is what `DELETE` uses.
* `filter(doc, readable_prefixes) -> Value` — the "no view" read: the union of the
  prefixes, unstripped, in the document's own order, carrying along every comment key
  the same prefixes cover (`scopes::covers`, i.e. exactly what `Grants::can_read`
  answers, in either authoring order). A map's bare `__` documents the whole map and so
  travels only where the map itself is readable; `p__` travels with a fully readable `p`
  (`docs/DESIGN.md` §9). **Invariant:** `filter` never emits a comment key the same
  grant's `can_read` refuses as a view.
* `render(value, format) -> String`, `parse(text, format)` (a replace payload: no nulls),
  `parse_patch(text, format)` (a merge payload: `null` is the delete marker).

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

## 7. Scopes and grants (`scopes.rs`)

* `Grants { full_read, full_write, scopes: Vec<Scope> }`, `Scope { prefix: Path, mode: Ro|Rw }`
  — exactly the `grants_json` shape crossing the Perl/Rust boundary. Perl computes it
  from PVE ACLs plus the datacenter document's `scopes` map.
* `can_read(path)` / `can_write(path)` / `readable_prefixes()` / `check_write(&[Touched])`.
* A scope on `p` also covers the sibling comment key `p__` at the same depth
  (`docs/DESIGN.md` §8: comment keys follow their subject). The bare `__` map comment has
  no subject and is not aliased: a scope on `p` covers `p`, `p__` and everything below
  `p`, and nothing above it (`docs/DESIGN.md` §9). `covers` is `pub(crate)`, because
  `view::filter` must give the same answer on the read side.
* `check_write(&[])` is vacuously `Ok`, so it is **not** a security boundary on its own;
  see §9.
* `check_write` treats the `scopes` map as **one path**: a touched path inside it is
  checked, and reported, as `scopes` itself (`docs/DESIGN.md` §9), so a 403 never names
  another principal's authid.
* `parse_scopes(dc)` — the strict, whole-map parse used at *write* time: `Error::
  InvalidScopes` naming the offender for a malformed entry, a key that is not a PVE
  authid, an **empty prefix** (whole-document access comes from PVE ACLs, never from a
  scope), or a prefix addressing inside `scopes` (it is an opaque leaf).
* `scopes_for(dc, authid) -> Vec<Scope>` — the lenient per-principal read, which
  **cannot fail**: a `scopes` value that is not a map, and any entry that `parse_scopes`
  would reject (whoever owns it), is skipped with a `warn!` and grants nothing. This
  lookup runs on every guest request, so an error here would be a cluster-wide outage
  for every principal including the administrator who has to repair it
  (`docs/DESIGN.md` §9).

## 8. Store (`store.rs`)

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
* `read(id) -> Document { id, path, raw, value, digest, mtime }` (`Error::NotFound` if
  absent). `value` is **unstripped** — comment stripping is the API layer's job.
  **Reads never lint** (`docs/DESIGN.md` §9): the parse is `format::parse_raw`, so
  whatever is on disk comes back. Only a syntax error is an error. Content can only be
  invalid out of band (a hand-edited file, a restored backup, pmxcfs replication), and
  linting it on the way in made one bad key in `datacenter.yaml` a cluster-wide outage
  that also blocked its own repair.
* `guest_ids() -> Vec<u32>` — every vmid with a live document, ascending (not snapshot
  copies, not the datacenter document). The store's half of orphan detection; Perl owns
  the vmlist and does the comparison (`docs/DESIGN.md` §9).
* `check_precondition(id, expected_digest)` — the compare-and-swap rule, without writing.
* `put_raw(id, text, expected_digest) -> PutResult { document, touched }` — full text
  replace, creating the document if absent. The *new* text is parsed and linted — this is
  the write-time gate; the existing content is parsed leniently, since it is only read to
  diff against and must not be able to block a repair. `touched = diff(old, new)`; text
  is stored as given (only normalised to end with a single newline).
* `delete(id)` — the **current document only**. `Error::NotFound` if absent.
* `destroy(vmid)` — the document *and every snapshot copy*, idempotent. Only the
  `on_destroy` lifecycle hook calls this; the REST `DELETE` calls `delete`
  (`docs/DESIGN.md` §8).
* `snapshot(vmid, name)` (no-op `Ok(false)` without a document), `rollback(vmid, name)`
  (→ `RollbackOutcome::Restored | RemovedNoSnapshot | NoOp`), `delete_snapshot(vmid,
  name)` (idempotent), `list_snapshots(vmid) -> Vec<String>`.
* `version() -> StoreVersion { token, changed }` — token = SHA-256 over the sorted list of
  `(file name, content digest)`, hashed from the files' actual bytes on **every** call.
  There is no `(mtime, len)` cache: the bindings build a fresh store per request so it
  could never hit, and pmxcfs's mtime granularity cannot distinguish two same-length
  writes in one tick so it would be unsound if it did. Documents are tiny.
* Size limits: `WARN_BYTES = 256 KiB`, `MAX_BYTES = 512 KiB` → `Error::TooLarge`.
  `MAX_BYTES` is a **backstop, not the operative limit for an API write**: pveproxy
  refuses a body of about that size first (measured: 520 000 B through, 530 000 B →
  "for data too large", HTTP 501). It bounds the writers that do not go through
  pveproxy — `import_from_backup`, and a hand-written or replicated file being
  rewritten — and the pmxcfs size budget.

The digest precondition is enforced **here and only here**: `check_digest` treats `None`
as "no precondition" and `Some("")` as matching a *missing* document, which is what makes
the documented GET-then-PUT create flow work (a missing document reports digest `""`).

Serialising concurrent writers is not this layer's job — `PVE::API2::Ext::Meta` runs each
write inside `PVE::Cluster::cfs_lock_domain("pve-meta-<id>", 10, …)`.

## 9. API layer (`api.rs`)

The request-shaped layer `PVE::RS::Meta`'s `api_*` functions export
(`docs/DESIGN.md` §3). It lives in this crate, not in the bindings crate, because it is
the project's security boundary and must be unit-testable without `libperl-dev` and
without a cluster. Every function takes a `&MetaStore`; the bindings supply one rooted at
`$PVE_META_ROOT`.

* `version(store)`, `grants(store, authid)`,
  `list_guests(store, guests_json, has, orphans)`,
  `get_document(store, id, view, format, comments, grants_json)`,
  `put_document(store, id, view, format, payload, mode, digest, dry_run, grants_json)`,
  `delete_document(store, id, view, digest, grants_json)`.
* Errors are `anyhow::Error`s whose `Display` is `"NNN: message"` (an HTTP status prefix);
  `PVE::API2::Ext::Meta::_call` parses that and re-raises through `PVE::Exception`.

**Write authorization** (`docs/DESIGN.md` §8) — decided from the request, never from a
diff:

1. `can_write(view)` must hold before anything is computed, and a caller without
   `full_write` may not write the root view at all;
2. the mutation is planned against a **clone** and every path the plan touches is checked
   with `check_write`;
3. the whole planned document is linted (so `dry_run` validates exactly what the write
   validates);
4. a datacenter write that touches `scopes` additionally requires `full_write` — the
   access-control map is admin-only whatever the scopes say, since `check_write` asks
   only whether the *path* is covered and a scope covering `scopes` makes that true
   (`docs/DESIGN.md` §9) — and is then validated with `parse_scopes`;
5. only then is the planned value written.

**Read authorization**: a caller with no readable prefix at all gets 403, not an empty
document with the real digest (which would be a change-detection oracle). A caller with a
*partial* grant does get the whole document's digest — scoped writers need it for
compare-and-swap PUTs.

**403 messages** never name a path the caller cannot read: the offending path is included
only when the caller has read access to its parent.

**View addressing**: `scopes` is an opaque leaf. `?view=scopes` addresses the whole map;
anything deeper is a 400 explaining why (its keys are authids, which may contain the path
separator). Entries are added and removed by writing the map, `mode=merge` with `null` to
delete one (`docs/DESIGN.md` §9).

`list_guests` takes the vmlist rows *from Perl* (`[{vmid, node, type, name, grants}]`,
`grants` a JSON string). This crate never opens `/etc/pve/.vmlist` or a guest config:
there is one reader of each per request, and guest-config parsing is not re-implemented in
a second language. `node` and `name` are returned only to a caller with `VM.Audit` on that
guest. With `orphans` (the caller has datacenter read) the list also contains every
document whose vmid is *not* among those rows, marked `orphan: 1` and with no
`node`/`type`/`name` — there is no guest to take them from (`docs/DESIGN.md` §9).

## 10. Errors (`error.rs`)

`thiserror` enum `Error`: `Parse { format, msg }`, `Lint(Vec<Lint>)`,
`DigestMismatch { expected, actual }`, `NotFound(DocId)`,
`TooLarge { size, max }`, `InvalidPath(String)`, `InvalidName(String)`,
`InvalidScopes(String)`, `Io(#[from] std::io::Error)`, `Other(#[from] anyhow::Error)`.
`Display` messages are suitable for an HTTP API response body. (There is no `Conflict`
variant: it described a multi-format store that no longer exists, and was never
constructed.)

## 11. Tests

Unit tests per module plus `tests/` integration tests using `tempfile`. Required
scenarios:

* lint catches each rule; comment keys accepted and stripped; `top_level_keys` listing;
* patch apply/diff properties on a handful of documents (arrays, nested deletes, no-op
  sets);
* format round trips for both formats, order preservation, YAML `yes/no` stay strings,
  YAML anchors/aliases/tags rejected, JSON comments rejected;
* views: `replace` with `{}` stores an empty map and round-trips; `remove` deletes; a
  no-op `merge` mutates nothing at any depth; creating or removing an empty map still
  reports a touched path; `filter` keeps a comment key in either authoring order;
  `parse_patch` accepts `null` where `parse` rejects it;
* scopes: a scope covers its own `p__` and nothing else's; anything malformed — the
  container, another principal's entry, the caller's own — is skipped by `scopes_for`
  while the strict parse still rejects the document; an empty prefix, a prefix inside
  `scopes`, and a non-authid key are all rejected by `parse_scopes`; `check_write`
  collapses a path inside `scopes` to `scopes`;
* store: create/put_raw/delete/destroy, digest mismatch, `Some("")` against a missing
  document, snapshots/rollback, `delete` leaves snapshots alone and `destroy` removes
  them, an api-delete-then-rollback end-to-end case, version token changes on write and
  not on read and distinguishes same-length writes, no temp files left behind; an
  out-of-band invalid document is still readable and still repairable while the
  write-time lint is unchanged; `guest_ids` lists live documents only;
* api: a zero-grant (and a wrongly-scoped) token cannot create structure through an empty
  merge or a `{}` replace at any prefix, cannot write the root view, and cannot learn
  values or key structure from a 403; `dry_run` and the real write agree; merge-with-null
  deletes end to end; malformed `scopes` are refused at write time; a no-grant read is
  403; `keys` carries the document order; **no scope of any breadth can write `scopes`**
  (PUT, DELETE and `dry_run`, with a root-prefix and with a `scopes`-prefix rw scope);
  a single `scopes` entry is not addressable as a view; an out-of-band invalid document
  is readable and repairable by a full-ACL caller; a scoped read never carries a bare
  `__`; orphan documents are listed only with datacenter read.
