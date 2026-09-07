# pve-meta-core — specification

Pure-Rust, platform independent library: the document model, formats, patch engine and
the file store used by the daemon, the CLI and the Perl bindings. No networking, no
async, no PVE-specific crates. Must build and pass tests on macOS and Linux.

## 1. Data model (`model.rs`)

* `pub type Value = serde_json::Value;` built with the `preserve_order` feature, so
  objects are ordered maps (insertion order = human order).
* A **document** is a `Value::Object` at the top level.
* Rules (checked by `pub fn lint(doc: &Value) -> Vec<Lint>`; `Lint { path: Path, msg: String }`):
  1. top level must be an object;
  2. no `null` anywhere (absent means unset);
  3. every object key must match `^[A-Za-z0-9_-]+$` (valid bare key in TOML/YAML/JSON, no dots since dots separate path segments);
  4. **comment keys**: a key ending in `__` is a comment key. Its value must be a string. `__` alone documents the containing map; `foo__` documents sibling `foo` (the sibling need not exist).
  5. numbers: integers and finite floats only (NaN/inf rejected by serde_json anyway).
* `pub fn strip_comments(doc: &mut Value)` removes all comment keys recursively (objects inside arrays too).
* `pub fn namespaces(doc: &Value) -> Vec<String>`: top-level non-comment keys, in order.
* `pub fn get_path<'a>(doc: &'a Value, path: &Path) -> Option<&'a Value>`; arrays are indexed by numeric segments.
* `pub const COMMENT_SUFFIX: &str = "__";` `pub fn is_comment_key(k: &str) -> bool`.

## 2. Paths (`path.rs`)

* `pub struct Path(Vec<String>)` — segments. Parse from dotted `a.b.c` and from slash form `/a/b/c` or `a/b/c` (`Path::parse(&str)`); `Display` is dotted; empty path = document root.
* `is_prefix_of(&self, other: &Path) -> bool` (root is a prefix of everything), `push`, `parent`, `join`, `segments()`, `last()`.
* Segment validation: same regex as keys, except that numeric segments (array indices) are allowed when addressing arrays.

## 3. Patches (`patch.rs`)

Merge-patch semantics (RFC 7386) with explicit delete:

* A patch is a `Value::Object`. Applying: for each `(k, v)` in the patch — if `v` is an object and the target has an object at `k`, recurse; if `v` is `null`, delete `k`; otherwise set `k = v` (replacing whatever was there, including objects).
* `pub fn apply_patch(doc: &mut Value, patch: &Value) -> Vec<Touched>` where `Touched { path: Path, op: Set | Delete }`. The touched set is the **minimal** set of paths that changed: recursion yields leaf paths; a replaced/deleted subtree yields its root path; setting a key to the value it already has yields nothing. Deleting a missing key yields nothing.
* `pub fn diff(old: &Value, new: &Value) -> Vec<Touched>` — same notion of touched paths for a full-document replace: recurse into objects that exist on both sides; a key only in `new` → `Set` at that path; only in `old` → `Delete`; differing non-object values (including arrays, which are atomic) → `Set`.
* `pub fn make_patch(old: &Value, new: &Value) -> Value` — a merge patch that transforms `old` into `new` (removed keys become `null`). `apply_patch(old, make_patch(old,new)) == new` must hold (property test).
* Patches are validated with the same key rules as documents (except that `null` is allowed as a delete marker).

## 4. Formats (`format.rs`)

`pub enum Format { Yaml, Toml, Json }` with `ext()` (`yaml`, `toml`, `json`), `from_ext(&str)` (also accepts `yml`), `Display`, `FromStr`, `ALL`.

* `pub fn parse(format, text: &str) -> Result<Value, Error>`; after parsing, `lint` is run and any lint → `Error::Lint(Vec<Lint>)`. Format-specific rejections:
  * TOML: datetime values → error (message names the path). Use `toml_edit::DocumentMut` → walk items into `Value` (integers → i64, floats → f64, arrays, inline tables, tables, arrays of tables).
  * YAML: parse with `serde_yaml_ng` into `Value`. Additionally scan with `saphyr-parser` events (or an equivalent tokenizer) and reject anchors, aliases and explicit tags (`!`), and reject non-string keys. Reject YAML `~`/empty-value nulls via the generic no-null rule. Numbers stay numbers, `true/false` stay booleans; the YAML 1.1 words `yes/no/on/off` must remain strings (verify what serde_yaml_ng does and add a test — if it converts them, that is a bug to work around).
  * JSON: `serde_json::from_str`; comments not allowed.
* `pub fn dump(format, doc: &Value) -> String` — canonical dump, always ends with a single `\n`, preserves key order:
  * YAML: block style, 2-space indent, no document markers, strings quoted only when needed (serde_yaml_ng default is fine).
  * TOML: nested objects become `[a.b]` tables (never inline tables) in document order; arrays of objects become `[[a.b]]` arrays of tables; scalars/arrays of scalars are key = value lines. Comment keys are ordinary keys. Build via `toml_edit` so that `dump` output re-parses to an identical `Value`.
  * JSON: `serde_json::to_string_pretty` (2 spaces) + newline.
* Round-trip property tests: for a set of sample documents, `parse(f, dump(f, doc)) == doc` for every format, and cross-format conversion preserves order.

## 5. Edit engine (`edit.rs`)

`pub fn apply_patch_text(format, text: &str, patch: &Value) -> Result<EditResult, Error>` with `EditResult { text: String, value: Value, touched: Vec<Touched> }`.

* YAML and JSON: `parse` → `apply_patch` → `lint` → `dump`. (Ordered canonical dump; comment loss accepted in v1 — documented.)
* TOML: **format preserving**. Parse into `toml_edit::DocumentMut` and apply the patch on the document tree:
  * setting a scalar/array at an existing key replaces only the value (keep key formatting, keep trailing comment where toml_edit keeps decor);
  * new keys are appended at the end of their table; if the table does not exist, create explicit `[a.b]` tables (not dotted keys, not inline tables); a new nested object becomes a new table appended after the parent's existing sub-tables;
  * objects inside arrays → array of tables when the existing item is an array of tables, otherwise an inline array of inline tables;
  * `null` removes the key (or the whole table); removing the last key of an explicit table removes the table header too;
  * unchanged parts of the document must be byte-identical (tests: comments before/after untouched keys survive, a header comment survives, changing one value changes exactly one line).
  * Compute `touched` by applying the same patch to the parsed `Value` (`apply_patch`), and assert in tests that `parse(Toml, result.text) == value`.

## 6. Digest (`digest.rs`)

`pub fn digest(bytes: &[u8]) -> String` — lowercase hex SHA-256 of the raw file bytes.

## 7. Store (`store.rs`)

`pub struct MetaStore { root: PathBuf }` — root is `/etc/pve/meta` in production, a tempdir in tests. All writes are atomic: write `.<name>.tmp.<pid>` in the same directory, then `rename` (pmxcfs supports rename). Directory created on demand (`create_dir_all`).

Naming: `DocId::Guest(u32)` ↔ `<vmid>.<ext>`; `DocId::Datacenter` ↔ `datacenter.<ext>`; guest snapshot ↔ `<vmid>.<snapname>.<ext>` (snapname matches `^[A-Za-z][A-Za-z0-9_-]*$`, which cannot collide with an extension because extensions are a closed set and snapshot names cannot be `yaml|yml|toml|json`). Exactly one format file may exist per DocId — `Error::Conflict` if several are found.

API (all synchronous, `Result<_, Error>`):

* `locate(id) -> Option<Located { path, format }>`
* `read(id) -> Document { id, format, path, raw: String, value: Value, digest: String, mtime: SystemTime }` (`Error::NotFound` if absent). `value` is **unstripped** — comment stripping is the caller's (API layer's) job via `model::strip_comments`.
* `patch(id, patch: &Value, expected_digest: Option<&str>) -> Document` — creates the document (in `default_format()`) if it does not exist and the patch has no deletes at the top; `Error::DigestMismatch { expected, actual }` if the digest differs; applies through the edit engine; enforces size limits; writes atomically; returns the new document.
* `put_raw(id, text: &str, format: Option<Format>, expected_digest: Option<&str>) -> PutResult { document, touched: Vec<Touched> }` — full text replace. `format` may switch the extension (write new file then remove old). Text is parsed and linted; `touched = diff(old, new)`. Text is stored as given (only normalised to end with a single newline).
* `convert(id, to: Format, expected_digest) -> Document` — `dump(to, value)` (comments in the old text are lost, comment keys survive), write new, remove old.
* `delete(id)`; for guests also removes all snapshot files.
* `list_guests() -> Vec<GuestEntry { vmid, format, digest, mtime, size }>` sorted by vmid, ignoring snapshot files and tmp files.
* `snapshot(vmid, name)`, `rollback(vmid, name)`, `delete_snapshot(vmid, name)`, `list_snapshots(vmid) -> Vec<String>`, `clone(vmid, newid)` (also copies nothing else; fails if target exists), `destroy(vmid)` = delete. `snapshot` of a guest without a document is a no-op returning Ok(false). `rollback` when the snapshot file does not exist but the current document does: remove the current document (the guest had no metadata at snapshot time) — return what happened as an enum.
* `default_format() -> Format` — reads `settings.default_format` (string) from the datacenter document if present, else `Yaml`.
* `version() -> StoreVersion { token: String, changed: SystemTime }` — token = digest over the sorted list of `(file name, mtime_ns, len)`; cheap, no file reads. Used for polling/long-poll.
* Size limits: `WARN_BYTES = 256 KiB`, `MAX_BYTES = 512 KiB` → `Error::TooLarge`.

## 8. vmlist (`vmlist.rs`)

`pub fn parse_vmlist(text: &str) -> Result<VmList>` for `/etc/pve/.vmlist`, shape:
```json
{ "version": 4913, "ids": { "100": { "node": "arkantos", "type": "lxc", "version": 4919 }, ... } }
```
→ `VmList { version: u64, guests: BTreeMap<u32, GuestInfo { node: String, kind: GuestKind (Qemu|Lxc), version: u64 }> }`. `pub fn read_vmlist(path) -> Result<VmList>`.

## 9. Errors (`error.rs`)

`thiserror` enum `Error`: `Parse { format, msg }`, `Lint(Vec<Lint>)`, `DigestMismatch { expected, actual }`, `NotFound(DocId)`, `Conflict(String)`, `TooLarge { size, max }`, `InvalidPath(String)`, `InvalidName(String)`, `Io(#[from] std::io::Error)`, `Other(#[from] anyhow::Error)`. Implement `Display` messages that are suitable for an HTTP API response body.

## 10. Tests

Unit tests per module plus `tests/` integration tests using `tempfile`. Required scenarios:
* lint catches each rule; comment keys accepted and stripped; namespaces listing;
* patch apply/diff/make_patch properties on a handful of documents (including arrays, nested deletes, no-op sets);
* format round trips for all three formats, cross-format order preservation, YAML `yes/no` stay strings, TOML datetime rejected, YAML anchors rejected;
* TOML edit preservation (comments/blank lines untouched, single line change, append, delete-table);
* store: create/patch/put_raw/convert/delete, digest mismatch, conflict on two formats, snapshots/rollback/clone/destroy, list_guests ignores snapshot files, default_format from datacenter doc, version token changes on write and not on read.
