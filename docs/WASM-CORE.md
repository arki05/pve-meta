# The core in the browser

A decision record. The editor's rules — the YAML codec, the key charset, who may touch
a path, which prefix governs one, what a schema says about a value, and what staged
edits do to a document — are `pve-meta-core`, compiled for `wasm32-unknown-unknown`
(`crates/pve-meta-wasm`) and asked through a hand-written JSON-string ABI. Nothing of
them is implemented in JavaScript any more. This was built as a spike on a branch
(`968cf82`, `fdee33e`, `e1a9baa`), reviewed adversarially, answered in a fourth commit
(`04b0a4d`), and merged; the behaviour it fixed is `docs/DESIGN.md` §8 ("The browser
reimplements nothing", "The editor's YAML is the store's YAML — by construction"), and
this file keeps what that section does not need: what was rejected, what it costs, what
got harder, and what is still open.

## 1. The problem

The editor held a second implementation of twelve rules the server already had, in
js-yaml plus a pile of predicates, and every disagreement between the two was a bug
the editor showed and its diff attributed to whatever was being edited. Read closely
they were not twelve independent duplicates but shards of five concepts, split not only
across languages but across files on each side:

| concept | was, in Rust | was, in JavaScript |
|---|---|---|
| the codec | `format::parse`/`dump`, `view::parse` | js-yaml with six settings, `yamlLoad`/`yamlDump`/`parseBuffer`/`dumpBuffer`/`renderBuffer`/`originalInLang`/`sameDocument` |
| the path rules | `path::is_valid_segment`, `registry::is_valid_file_name` | a regex in `keyPathError`, another regex in `Meta.pm`, and nothing at all on the New dialog's name field despite a comment saying there was |
| who may touch a path | `scopes::Effective` (`can_read`/`can_write`/`has_any_write`) over `covers` | `Utils.covers`, `Utils.hasAnyWrite`, and `can_write` inlined into `editableFor` |
| what describes a document | `registry::governing` (dead: no caller), `load_prefixes`'s sort, `Selector::matches` | `applicablePrefixes` (selector), `bySpecificity` (sort), `governing`, `containsPath`, `Lint.applicable`, and the walks in `Lint.findings`, `Lint.schemaIndex` and `addGrammar`, each pruning by `governing` on its own |
| what staged edits do | `view::replace`/`remove`, `patch::diff` | `setAtPath`, `deleteAtPath`, `applyPending`, `diffDocuments`, `changedPaths`, `writeView`, `stage`'s subsumption, `pendingUnder` |

So the question was never "how do we call Rust from the browser" — that part is
mechanical — but "what are the things the browser would be calling". Naming them was
most of the work, and it changed the Rust side as much as the JavaScript side.

## 2. What was built

### `shape::Shape` — what describes a document

New module. A `Shape` is the prefixes that reach one document, most-specific first,
built once from the registry's definitions (or the API's listing) and the guest's tags,
and asked `governing(path)`, `schema_at(path)`, `schema_index()`, `findings(doc)`.

It absorbed the selector filter, the specificity sort (one comparator,
`registry::by_specificity`, which the API listing uses too, so the order a client sees
is the order the server would resolve), the dead `registry::governing`, and the three
JavaScript walks that each re-derived "stop where a more specific prefix governs". The
schema lint turned out not to be a separate step: once `Shape` owned the walk,
`findings` was sixty lines.

The registry meta-schema (a prefix or permission file described as a document) used to
be a special case in three places — `grammarSplit` refused to mix it with prefixes,
`Lint.findings` walked it "with no owner", `documentEntries` branched on `!ns.prefix`.
It is `Shape::rooted(schema)` now: a `Declared` at the empty path, which is a prefix of
everything and the least specific of all, so it governs the whole document with no
branch anywhere below. The special case was an artefact of not having the type.

`Shape` deliberately does not use `covers`. Schemas use plain containment and do *not*
alias `p__`; that is the doc comment on the module, and the test table has the case:
with prefixes `a` and `a__` both declared, `a__.note` is governed by `a__`, and with
only `a` declared, `a__` is governed by nothing. Two opposite nesting rules are two types
(`Shape` and `Effective`), and each one's doc says why it is not the other.

At the call sites: `grammarFor` + `grammarSplit` + `applicablePrefixes` + `Lint.applicable`
became `shapeFor(docId)`, which returns a Shape. `addGrammar` (an 85-line recursive walk
with a governing check per child) became `addShape`, a flat loop over the core's
already-pruned index.

### `edit::EditSet` — what staged edits do

New module. An `EditSet` is the list the editor's `me.pending` always was, given the
operations the editor performed on it by hand: `apply` (the planned document),
`between` (the edits that turn one document into another, with the self-check that a
pure reordering stages nothing), `write_view` (the narrowest view covering every staged
path, moved up one level when a delete sits exactly there), and `changed_paths`. A
first version also had `stage` (an edit at `p` drops everything staged under `p`),
`under` and `discard_under`, for a set that was appended to; they went once the editor
derived the set from the planned document with `between` instead of maintaining it as
a log, which is what fixed staging the value a row already had and A->B->A leaving two
edits that described nothing.

The important property is what `apply` is made of: a `set` is `view::replace` and a
`delete` is `view::remove` — the two operations a `PUT ?view=` and a `DELETE ?view=`
perform on the server. The editor's prediction of a write and the store's execution of
it are the same function rather than two that agreed. The old `setAtPath` silently
turned a scalar intermediate into a map; `view::replace` refuses to address through a
scalar. The refusal is unreachable from a row edit or a text diff (neither produces such
a path), and if it ever becomes reachable it will be an error rather than a silent
rewrite.

The self-check in `between` needed something `Value` lacked: `serde_json::Value`'s `==`
compares maps as sets, which is the rule everywhere: a reordering touches no path and
is the same document (`docs/decisions/007`); `same` on the wasm side is that equality.
`JSON.stringify` equality was doing that job in JavaScript without anyone having said so.

### What the browser holds

Two objects and two faces, over five concepts in the core:

- **`PVE.meta.Shape`** owns its listing and tags and caches what the core derives from
  them alone: the applicable prefix names, the schema index, and each `governing`
  answer. Only `findings(doc)` crosses every time, because only it takes a document.
  The panel keeps one Shape per document (`shapeFor`) and validates the cache against
  its inputs *by identity* — every load replaces `prefixes`, `tags` or `schemas` with a
  new object — rather than clearing it at the right moment, because a cache that must
  be told is a cache that is stale the first time someone forgets. (A `forgetShapes()`
  hook in `reload` was the first draft; the test suite's stubs, which copy a panel with
  `Object.assign`, found the staleness within a minute.)
- **`PVE.meta.EditSet`** owns the staged list. `between` and `empty` make one, and
  nothing changes one in place: the panel derives a new set whenever the planned
  document changes. `apply` and `writeView` ask the core.
- **`Codec`** and **`Access`** stay stateless faces: a codec has no state, and an
  `Effective` is one `GET /meta/access` answer the panel already holds.

The first version had `Shape.make` return a bag of closures and `Edits` as free
functions over a plain array. The review was right that this was two concepts on the
Rust side and RPC stubs on the browser side — and that the lack of an object is why
`declared()` and `hasSchema()` each re-asked the core for the same prefix list on every
render.

### `scopes::Effective` — who may touch a path

Already the right type. `covers` became `pub`; nothing else changed in the module. The
editor's `Access.canWrite(access, path)` is `Effective::can_write` over the
`GET /meta/access` answer, and `editableFor` is one line. The Access column's "which
rules reach this guest" question got its own function, `registry::rules_reaching`, and
`scopes_for` (the server's per-principal version) is that narrowed by authid — one
place decides reach.

### The codec — `format`

Unchanged except that `Error::Parse` now carries a `Location`. The editor's marker on
the offending line was the one thing it took from js-yaml that the server's message did
not carry; now the parse error itself says where (serde_yaml_ng's own location for the
YAML parse, saphyr's marker for the safety scan, serde_json's for JSON). saphyr counts
lines from 1 and columns from 0; the first draft added one to both.

### The path rules — `path`

`is_valid_segment` is public and defined by `invalid_char`, so the editor can name the
character it refuses ("a space is not allowed in a key") without restating the charset.
`registry::is_valid_file_name` reaches the New dialog's name field, and carries the
whole rule: the Perl schema for `{name}` had a `maxLength => 128` beside its pattern,
and only the pattern had a Rust twin, so the first version let the dialog accept a name
the server 400s. `MAX_FILE_NAME_LEN` is in Rust with the charset (the name becomes
`<name>.yaml` on disk) and the Perl schema keeps both as the friendly-400 mirror.

### The adapter — `crates/pve-meta-wasm`

The one place that knows the wire. The API hands the browser Perl-encoded JSON (`true`
is `1`; a listed file that failed carries `error` and nothing else), and turning that
into `Effective`, `Shape` and `Permission` happens in the `Args` type of this crate and
nowhere in the core. `Selector::from_wire` is the exception, in the core, because the
rule it applies ("exactly one of `all: true` or `tag`") is `parse_selector`'s and must
stay one rule.

## 3. What stayed in JavaScript, and why

- **Rows.** `entry`, `addData`, `documentEntries`, `buildTree`, `toNodes`: the tree is
  presentation. `addShape` is the only row code that consults the core, and it consults
  an index rather than walking a schema.
- **Marker placement.** `Markers.lineIndex` is a YAML *scan*, not a parser, that maps
  document paths to line numbers in the buffer for Monaco (§8, open). `hoverText` is
  gettext.
- **The `format:` check.** A schema's `format` is a `PVE::JSONSchema` format name, and
  the editor validates it with proxmoxlib's own vtype for that name (DESIGN §3). The
  core does not know what `ipv4` means and should not learn — that would be a third
  implementation of PVE's formats. So `Shape::findings` returns one list of `Report`s —
  findings, and `FormatCheck { path, format, value }` entries it could not judge — sorted
  by path once, in Rust, and `PVE.meta.Shape.findings` resolves the format entries *in
  place* through `Utils.checkFormat`. (Two lists re-sorted in JavaScript by string order
  was the first version; string order disagrees with `Path`'s segment order on `a.b`
  versus `a-c`.) This is a deliberate leak with a type on it.
- **`valueAt`, `rollUp`, `sameValue`, `itemSummary`, `editorKind`, `editorFor` …**:
  render-time lookups and UI choices with no server twin.
- **The Perl regex** in `perl/PVE/API2/Ext/Meta.pm` for registry names. It is a
  `PVE::JSONSchema` `pattern`, evaluated before Rust is called (§8, open). **This is the
  one duplicate left.**

## 4. The build: chosen, and rejected

**Chosen: a hand-written JSON-string ABI over a plain `cargo build`.** Five exports:

```
pm_alloc(len) -> ptr        pm_free(ptr, len)
pm_call(ptr, len) -> len    pm_output() -> ptr
pm_abi() -> u32
```

A request is `{"fn": name, "args": [...]}`; a response is `{"ok": value}` or
`{"err": {message, line?, column?}}`. The JavaScript glue (`PVE.meta.Core.call`) is
twenty lines: encode, copy in, call, re-read `memory.buffer` (a call may grow it), copy
out, decode. The output buffer belongs to the module and is reused; the caller frees
only its request. `pm_abi` is checked on attach, so a `.wasm` and a script from different
builds fail loudly rather than mis-read each other.

**Rejected: wasm-bindgen.** It was not close. Every function here takes and returns
documents — a `Value`, a path string, an edit list, a listing — so JSON is the natural
type of the interface, and wasm-bindgen's ergonomics (typed exports, a generated `.js`)
would have bought a generated file to ship and a `wasm-bindgen-cli` whose version must
equal the crate's, in a Debian build that has neither. The cost of the chosen shape is
that argument checking is by hand (`Args` does it, and reports "argument 2 must be a
string" rather than a type error) and that there is one dispatch `match` to keep in step
with the JavaScript faces. Both are tested.

**Rejected: wasm-opt.** Measured: it saves 15% raw and 2% gzipped. Not worth a build
dependency.

What the build needs is one toolchain target: `rustup target add
wasm32-unknown-unknown` on the build host, or Debian's `libstd-rust-dev-wasm32` with a
distro `rustc` (`docs/BUILD.md`). No npm, no bindgen. `make wasm` is one cargo
invocation with a `[profile.wasm]` (size optimisation, LTO, one codegen unit,
`panic = "abort"`, stripped). `make build` and `make check` depend on it; `make install`
ships the file next to the editor and fails if it is missing, because an editor without
its core cannot read a document. CI (`.github/workflows/build.yml`) installs rustup with
`--profile minimal`, which is the host target only, and adds the wasm32 target in the
same step — the first version of the branch left the workflow untouched and would have
died at `make check` with `E0463`.

The editor's old claim "no build step beyond vendoring Monaco" is gone: the editor's
core is built. The package already built Rust for the perlmod `.so`; this is the same
cargo, a second target.

## 5. What it cost

Measured at merge; the numbers will drift and the shape of them will not.

**Lines.** 30 files, +3,184 / −1,777, net **+1,407**. Where they went:

| | lines | of which tests |
|---|---|---|
| `crates/pve-meta-core/src/shape.rs` (new) | 568 | ~300 |
| `crates/pve-meta-core/src/edit.rs` (new) | 342 | ~150 |
| `crates/pve-meta-wasm/src/lib.rs` (new) | 694 | ~155 |
| `crates/pve-meta-core/src/registry.rs` | +/− 250 | `governing` and its fixture test out; `from_wire`, `by_specificity`, `rules_reaching`, the name length in |
| `ui-extjs/pve-meta-tree.js` | 5,631 → 5,356 (−275) | the helper region (Utils + Yaml + Lint: 1,080 lines) became Utils + Core + Codec + Access + Shape + EditSet + Markers (837 lines) |
| `ui-extjs/testing/smoke.js` | 1,770 → 1,889 (+119) | rewritten over the new objects; adds the un-loaded and raw-ABI sections; drops the covers/governing tables |
| `ui-extjs/vendor/js-yaml.min.js` | −39,430 bytes | deleted |

Duplicated logic removed from JavaScript, counted by function: the second YAML codec
(js-yaml plus its six settings and the `define`-hiding loader), `covers`, `hasAnyWrite`,
the inlined `can_write`, `governing`, `containsPath`, `bySpecificity`, `depth`, the
selector match in `applicablePrefixes` and `applicablePermissions`, `keyPathError`'s
regex, `setAtPath`, `deleteAtPath`, `applyPending`, `diffDocuments`, `changedPaths`,
`introducedFindings`, `writeView`, `stage`'s subsumption, and `Lint.applicable`,
`findings`, `walk`, `checkValue`, `typeMatches`, `schemaIndex`. Twenty-five functions,
roughly 460 lines with their comments; what replaced them is ~330 lines: two objects
that own their inputs and cache, and two faces that do nothing but name a core call.

**Fixtures.** `testdata/covers-cases.json` (19 cases) and `testdata/governing-cases.json`
(16 cases) are gone — their tables are inline in `scopes.rs` and `shape.rs`, next to the
one implementation. (The first transcription of the governing table dropped four cases,
including the subtlest one — two independently tagged levels, where the more specific
prefix's selector misses and the parent governs; the review caught it.)
`testdata/yaml-cases.json` stays with a different job: it no longer holds two emitters
together, it notices when a `serde_yaml_ng` upgrade moves the bytes, and the editor's
suite runs it through the wasm as an end-to-end check.

**Tests.** 268 in Rust (153 unit + 107 integration in `pve-meta-core`, 8 in
`pve-meta-wasm`, plus one doctest); 416 checks in `smoke.js`, all against the real
`.wasm`, thirteen of them *before* the core is attached.

**Size and load.**

| | raw | gzip −9 | brotli −11 |
|---|---|---|---|
| `pve_meta_wasm.wasm` | 593,003 | 210,067 | 170,120 |
| after `wasm-opt -Os` | 507,683 | 206,148 | — |
| js-yaml 4.1.0 min | 39,430 | 13,059 | — |

Sixteen times js-yaml over the wire, gzipped — ~205 KB once per browser, cached
thereafter, in a UI that lazily loads a 13 MB Monaco tree. Where the bytes are (twiggy on
an unstripped build):

| KB | what |
|---|---|
| 77 | `saphyr_parser` — the YAML safety scan (anchors, aliases, tags, complex keys) |
| 71 | `unsafe_libyaml` — serde_yaml_ng's parser and emitter |
| 31 | `serde_yaml_ng` itself |
| 76 + 36 + 23 | `core`, `alloc`, `std`: formatting, float printing, panicking |
| 68 | `serde_json` (`Value`, de/serialize) |
| 65 | data segments (strings, tables) |
| 25 | `pve_meta_core` — the actual rules |
| 24 | `rustc_demangle` — pulled in by std's panic path; unavoidable on stable |
| 23 | `pve_meta_wasm` — dispatch |

The rules are 4% of the file; half of it is two YAML parsers. The lever, if size ever
matters, is in §8. Compiling and instantiating the module takes 1.0 ms cold and ~0.1 ms
cached in V8; the download is the cost, not the load.

**Per call**, through the real glue with a deliberately fat listing (8 prefixes × 40
declared properties with descriptions, 34 KB of JSON) and a 320-key document with five
`rw` scopes:

| call | µs |
|---|---|
| `covers` | 1.5 |
| `access_can_write`, 5 scopes | 4.9 |
| `shape_prefixes` (first ask on a Shape; re-parses the 34 KB listing) | 314 |
| `shape_governing` on a fresh Shape / on a cached answer | 312 / 0.0 |
| `shape_schema_index` (first ask) | 1,095 |
| `shape_findings`, 320 keys, 320 format checks | 688 |
| `edits_apply`, one edit, 320 keys | 93 |
| `edits_between`, 320 keys | 163 |
| parse / dump YAML, 4 KB | 298 / 177 |

**Per render**, counted from what `buildTree` asks: a warm Shape pays only `findings`
(0.7 ms), a cold one 2.1 ms; the per-row access questions — `covers` once per rule that
reaches the guest and `access_can_write` once — cost ~10 µs a row at five scopes, so 328
rows cost 3.4 ms and are the dominant term. **~4 ms warm, ~5.5 ms cold** for the fat
case; a real listing (two or three prefixes, a few dozen rows) is well under a
millisecond either way. The text editor's per-keystroke `annotateText` is `findings`
plus a parse (~1 ms) on a warm Shape.

## 6. What got harder

- **Async initialisation reaches further.** js-yaml was lazy too, so the editor already
  awaited a codec before reading a document. But now `editableFor`, the two name
  validators and the Access column all need the core, and `syncButtons` runs on render
  before the first load. Three guards (`Core.loaded()`) and an early `Core.load()` kick
  in `initComponent` cover it: `editableFor` fails *closed* (nothing is editable until
  the rule that decides it is here), the validators fail *open* (no early answer; the
  server refuses the same names on its own). `smoke.js` runs its first section before
  attaching the core and checks all three, plus that a direct `Core.call` throws "not
  loaded" rather than trapping. Any *new* call site that reaches the core before
  `loadDocument` has awaited it will throw that error, which is the intended failure:
  loud, not wrong. This editor had shipped two lazy-load ordering bugs before this.
- **A panic is silent.** `panic = "abort"` and a stripped build mean a Rust panic inside
  the module surfaces as `RuntimeError: unreachable` with no message. Every function in
  the dispatch returns `Result` and the tests cover the error paths (bad UTF-8, bad
  JSON, missing and extra arguments, invalid paths, an edit through an array), so a trap
  would be a bug in the core, not in a caller's input. Keeping names would cost 150 KB;
  keeping debug info, 1.5 MB.
- **No stack across the boundary.** A `CoreError` has a message and, for parse errors,
  a line and column. That is all a caller gets, and it is what the editor needs.
- **Two copies per call.** Every argument is stringified, copied in, parsed; every
  result serialized, copied out, parsed. Measured above; fine at this scale. The
  cheaper design (a persistent Shape handle) exists if it stops being fine.
- **Packaging.** One more toolchain target on the build host, and `make install`
  hard-fails without the `.wasm`. `make check` needs cargo *and* node, where the smoke
  suite needed node alone. `instantiateStreaming` requires `Content-Type:
  application/wasm`, which pveproxy's static file table may or may not know, so the
  loader falls back to `fetch` + `WebAssembly.instantiate` when the streaming path
  rejects; whether pveproxy gzips the file decides whether the download is 210 KB or
  593 KB.
- **The Debian build must reach crates.io** for the same reason `dh_auto_test` already
  notes it does for the perlmod crate; nothing new, and the wasm crate adds no
  dependencies (`serde`, `serde_json`, and the core).

## 7. Where the abstraction leaks

- `parse` in the ABI maps a null top level (an empty buffer, `~`) to `{}`. That is an
  editor convention — the model has no nulls and an empty editor is an empty document —
  and it lives in the adapter crate, not the core. The server treats an empty *file*
  differently (unrecoverable, DESIGN §7), on purpose.
- `Shape::findings` keeps the old tolerance that `1`/`0` satisfy `type: boolean`, with
  the old justification (the JSON wire convention). The editor reads YAML now, so a `1`
  is a `1`; the tolerance is inherited, documented, and probably wrong for a document
  written by hand. Behaviour was left unchanged.
- The wasm crate's `truthy` and `Selector::from_wire` exist because Perl renders `true`
  as `1`. The Perl JSON encoding leaks exactly as far as the adapter and no further.
- `FormatCheck` is the core saying "I cannot judge this"; see §3. A type for a gap.
- `EditSet::between` falls back to a whole-document set on *any* order change, including
  inserting a key anywhere but the end. That is the old `diffDocuments` behaviour,
  faithfully moved, and the test says so. A smarter diff that expresses "insert before"
  would need a write operation the API does not have.
- The panel's Shape cache is keyed by document id and validated by the *identity* of
  the Shape's inputs. That holds because every load assigns a fresh array or object;
  a future change that mutated `me.prefixes` in place would serve a stale Shape, and
  nothing but this sentence and a test that copies panels with `Object.assign` says so.
- The un-loaded name validators fail open. That is safe only while the server mirrors
  the rule, which it does (`Meta.pm`'s schema `pattern` and `maxLength`); the day it
  stops, they should fail closed like `editableFor`.

## 8. Still open

- `perl/PVE/API2/Ext/Meta.pm`'s name regex: a `register_format` over
  `PVE::RS::Meta::is_valid_file_name` would make it the same function rather than the
  same shape. Needs the Linux build host.
- `Markers.lineIndex` as a span index from the core (saphyr has the spans; the editor
  scans text with a heuristic). Would make marker placement exact and remove the last
  YAML-shaped code from JavaScript.
- The server does not *use* `EditSet` or `Shape`. Both are the editor's model expressed
  in the server's operations, which is the point, but the API could expose
  `Shape::findings` (schema lint on the server, advisory) and could plan a `PUT` through
  `EditSet`.
- Size, if 200 KB gzipped is ever judged too much: parse with saphyr only, building
  `Value` from its events and rejecting anchors/aliases/tags during the build, and keep
  libyaml for the emitter alone. That would drop ~60–90 KB raw — and it touches the
  *server's* parser: serde_yaml_ng's scalar resolution is what keeps `yes` a string, and
  changing parsers is a change to what the store accepts, so it deserves its own spike.
  `-Zbuild-std` with `panic_immediate_abort` would remove another ~40 KB and is nightly.
- Per-row access (`covers` × rules + `access_can_write` per row, §5) as one batched
  crossing, if a tree ever gets large enough for 10 µs a row to show.
- A handle-based Shape inside the module, if the once-per-load 1.4 ms of a cold Shape
  ever matters. It was rejected because it is a lifetime for JavaScript to manage for a
  cost paid once per load.
- Whether pveproxy serves the `.wasm` as `application/wasm` and gzips it (§6). The
  loader works either way; a `curl -I` on a node answers both.
