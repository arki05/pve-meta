# The wasm spike: making the browser like Perl

Branch `wasm-unify`, off `main` at `54f2b78`. Three commits made the spike (`968cf82`:
the core grows the concepts the editor was reimplementing; `fdee33e`: the editor asks
the core; `e1a9baa`: load guards and this report), and a fourth answers the adversarial
review's six conditions (§10). Nothing was deployed; everything below was verified
offline — `cargo test`, clippy and rustdoc on `pve-meta-core` and the new
`pve-meta-wasm`, and a node harness that instantiates the built `.wasm` and drives the
editor's helpers through it.

**Reached: all three tiers.** The codec and the pure rules (1), the staging model (2),
and the schema lint (3) are one implementation each, in Rust, and the editor calls
them. js-yaml is deleted. Two of the three shared fixtures are deleted; the third has a
different job now.

**Recommendation: adopt**, with one cost accepted and one thing to verify on the lab
before merging. The cost is ~205 KB gzipped over the wire on first load (against 13 KB
for js-yaml), cached thereafter. The thing to verify is that pveproxy serves a `.wasm`
compressed — the loader works either way, but the download is three times larger if
not. The argument for adopting is not the line count (net +1.4k lines) but what the
lines are: five named concepts where there were two piles of predicates, two of them
real objects on the browser side as well as the server's, and a rule list that is
empty for the first time.

## 1. The problem, restated as a design problem

The brief listed twelve rules with two implementations each. Reading the code, they
were not twelve independent duplicates; they were shards of five concepts, and the
shards had been split not only across languages but across files on each side:

| concept | was, in Rust | was, in JavaScript |
|---|---|---|
| the codec | `format::parse`/`dump`, `view::parse` | js-yaml with six settings, `yamlLoad`/`yamlDump`/`parseBuffer`/`dumpBuffer`/`renderBuffer`/`originalInLang`/`sameDocument` |
| the path rules | `path::is_valid_segment`, `registry::is_valid_file_name` | a regex in `keyPathError`, another regex in `Meta.pm`, and nothing at all on the New dialog's name field despite a comment saying there was |
| who may touch a path | `scopes::Effective` (`can_read`/`can_write`/`has_any_write`) over `covers` | `Utils.covers`, `Utils.hasAnyWrite`, and `can_write` inlined into `editableFor` |
| what describes a document | `registry::governing` (dead: no caller), `load_prefixes`'s sort, `Selector::matches` | `applicablePrefixes` (selector), `bySpecificity` (sort), `governing`, `containsPath`, `Lint.applicable`, and the walks in `Lint.findings`, `Lint.schemaIndex` and `addGrammar`, each pruning by `governing` on its own |
| what staged edits do | `view::replace`/`remove`, `patch::diff` | `setAtPath`, `deleteAtPath`, `applyPending`, `diffDocuments`, `changedPaths`, `writeView`, `stage`'s subsumption, `pendingUnder` |

So the design question was not "how do we call Rust from the browser" — that part is
mechanical — but "what are the things the browser would be calling". Naming them was
most of the work, and it changed the Rust side as much as the JavaScript side.

## 2. The concepts, and what they did to the call sites

### `shape::Shape` — what describes a document

New module. A `Shape` is the prefixes that reach one document, most-specific first.
Built once from the registry's definitions (or the API's listing) and the guest's
tags; asked `governing(path)`, `schema_at(path)`, `schema_index()`, `findings(doc)`.

What it absorbed: the selector filter, the specificity sort (now one comparator,
`registry::by_specificity`, which the API listing uses too, so the order a client sees
is the order the server would resolve), the dead `registry::governing`, and the three
JavaScript walks that each re-derived "stop where a more specific prefix governs". The
schema lint — Tier 3 — turned out not to be a separate step: once `Shape` owned the
walk, `findings` was sixty lines.

The registry meta-schema (a prefix or permission file described as a document) used to
be a special case in three places — `grammarSplit` refused to mix it with prefixes,
`Lint.findings` walked it "with no owner", `documentEntries` branched on `!ns.prefix`.
It is `Shape::rooted(schema)` now: a `Declared` at the empty path, which is a prefix of
everything and the least specific of all, so it governs the whole document with no
branch anywhere below. The special case was an artefact of not having the type.

`Shape` deliberately does not use `covers`. The brief's rule 6 — schemas use plain
containment and do *not* alias `p__` — is the doc comment on the module, and the test
table has the case: with prefixes `a` and `a__` both declared, `a__.note` is governed by
`a__`, and with only `a` declared, `a__` is governed by nothing. Two opposite nesting
rules are two types (`Shape` and `Effective`), and each one's doc says why it is not
the other.

At the call sites: `grammarFor` + `grammarSplit` + `applicablePrefixes` + `Lint.applicable`
became `shapeFor(docId)`, which returns a Shape. `addGrammar` (an 85-line recursive walk
with a governing check per child) became `addShape`, a flat loop over the core's
already-pruned index. `findingsFor`, `applyFindingsFor`, `textFindings` and `annotateText`
each dropped their `{all, withSchema}` pair for one `shape`.

### `edit::EditSet` — what staged edits do

New module. An `EditSet` is the list the editor's `me.pending` always was, given the
operations the editor performed on it by hand: `stage` (an edit at `p` drops everything
staged under `p`; the root drops all), `under`/`discard_under`, `apply` (the planned
document), `between` (recover the edits from a typed document, with the self-check
that falls back to a whole-document set when key order changed), `write_view` (the
narrowest view covering every staged path, moved up one level when a delete sits
exactly there), and `changed_paths`.

The important property is what `apply` is made of: a `set` is `view::replace` and a
`delete` is `view::remove` — the two operations a `PUT ?view=` and a `DELETE ?view=`
perform on the server. The editor's prediction of a write and the store's execution of
it are now the same function rather than two that agreed. The old `setAtPath` silently
turned a scalar intermediate into a map; `view::replace` refuses to address through a
scalar. The refusal is unreachable from a row edit or a text diff (neither produces such
a path), and if it ever becomes reachable it will be an error rather than a silent
rewrite.

The self-check in `between` needed something `Value` lacked: `serde_json::Value`'s `==`
compares maps as sets (which is right for `patch::diff` — a reordering touches no path,
rule 4), so `model::same_ordered` was added for "is this the document that was typed".
`JSON.stringify` equality was doing that job in JavaScript without anyone having said so.

At the call sites: `me.pending` *is* a `PVE.meta.EditSet` now — an object that owns
its list and whose methods (`stage`, `under`, `discardUnder`, `apply`, `writeView`) are
the core's — so `stage`, `pendingUnder`, `discardRow`, `plannedData`, `leaveTextMode`,
`confirmAndApply` and `applyFindingsFor` each became one method call on it.
`discardRow` had been filtering by object identity against the result of
`pendingUnder`; that could not survive a boundary crossing and became `discardUnder`.
The places that still read the raw list (`documentEntries`' ghost rows, `buildTree`'s
staged-path map, the lone-`DELETE` check) read `me.pending.edits` and say so.

### What the browser holds, honestly

The first version of this branch had `PVE.meta.Shape.make` return a bag of closures
over two captured arguments and `PVE.meta.Edits` as free functions over a plain
array, and called that five concepts. The review was right that it was two on the
Rust side and RPC stubs on the browser side — and that the lack of an object is why
`declared()` and `hasSchema()` each re-asked the core for the same prefix list on
every render. Both are objects now:

- **`PVE.meta.Shape`** owns its listing and tags and caches what the core derives from
  them alone: the applicable prefix names, the schema index, and each `governing`
  answer. Only `findings(doc)` crosses every time, because only it takes a document.
  The panel keeps one Shape per document (`shapeFor`) and validates the cache against
  its inputs *by identity* — every load replaces `prefixes`, `tags` or `schemas` with a
  new object — rather than clearing it at the right moment, because a cache that must
  be told is a cache that is stale the first time someone forgets. (The first draft
  had a `forgetShapes()` hook in `reload`; the test suite's stubs, which copy a panel
  with `Object.assign`, found the staleness within a minute.)
- **`PVE.meta.EditSet`** owns the staged list. `stage` and `discardUnder` change it in
  place, `between` and `empty` make one, `apply`/`writeView`/`under` ask the core.

`Codec` and `Access` stay stateless faces: a codec has no state, and an `Effective`
is one `GET /meta/access` answer the panel already holds. So the honest count is two
objects and two faces on the browser side, over five concepts in the core.

### `scopes::Effective` — who may touch a path

Already the right type. `covers` became `pub`; nothing else changed in the module. The
editor's `Access.canWrite(access, path)` is `Effective::can_write` over the
`GET /meta/access` answer. `editableFor` is one line. The Access column's "which rules
reach this guest" question got its own function, `registry::rules_reaching`, and
`scopes_for` (the server's per-principal version) is now that narrowed by authid — one
place decides reach.

### The codec — `format`

Unchanged except that `Error::Parse` now carries a `Location`. The editor's marker on
the offending line was the one thing it took from js-yaml that the server's message did
not carry; now the parse error itself says where (serde_yaml_ng's own location for the
YAML parse, saphyr's marker for the safety scan, serde_json's for JSON). This found a
convention mismatch on the way: saphyr counts lines from 1 and columns from 0, and the
first draft added one to both.

### The path rules — `path`

`is_valid_segment` is public and defined by `invalid_char`, so the editor can name the
character it refuses ("a space is not allowed in a key") without restating the charset.
`registry::is_valid_file_name` reaches the New dialog's name field for the first time —
and now carries the whole rule: the Perl schema for `{name}` had a `maxLength => 128`
beside its pattern, and only the pattern had a Rust twin, so the first version of this
branch let the dialog accept a name the server 400s. `MAX_FILE_NAME_LEN` is in Rust
with the charset (the name becomes `<name>.yaml` on disk), the Perl schema keeps both
as the friendly-400 mirror, and since the loader filters directory entries through the
same function, a file with a longer name would no longer load — which changes nothing
in practice, because no such file could ever have been written through the API.

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
  document paths to line numbers in the buffer for Monaco. It could be a span index
  from saphyr in the core (the parser already has spans); that would replace a
  heuristic with a real answer and is the next thing I would move. `hoverText` is
  gettext.
- **The `format:` check.** A schema's `format` is a `PVE::JSONSchema` format name, and
  the editor validates it with proxmoxlib's own vtype for that name (DESIGN §8). The
  core does not know what `ipv4` means and should not learn — that would be a third
  implementation of PVE's formats. So `Shape::findings` returns one list of `Report`s
  — findings, and `FormatCheck { path, format, value }` entries it could not judge —
  sorted by path once, in Rust, and `PVE.meta.Shape.findings` resolves the format
  entries *in place* through `Utils.checkFormat`. (The first version returned two lists
  and re-sorted the merge in JavaScript by string order, which disagrees with `Path`'s
  segment order on `a.b` versus `a-c`; there is one sort now.) This is a deliberate
  leak with a type on it.
- **`valueAt`, `rollUp`, `sameValue`, `itemSummary`, `editorKind`, `editorFor` …**:
  render-time lookups and UI choices with no server twin.
- **The Perl regex** in `perl/PVE/API2/Ext/Meta.pm:624` for registry names. It is a
  `PVE::JSONSchema` `pattern`, evaluated before Rust is called. The fix is a
  `register_format` whose checker calls `PVE::RS::Meta::is_valid_file_name`, but
  `pve-meta-perl` does not compile on this machine and I would not change what I cannot
  build. **This is the one duplicate left.**

## 4. The build story

**Chosen: (b), a hand-written JSON-string ABI over a plain `cargo build`.** Four exports:

```
pm_alloc(len) -> ptr        pm_free(ptr, len)
pm_call(ptr, len) -> len    pm_output() -> ptr
```

A request is `{"fn": name, "args": [...]}`; a response is `{"ok": value}` or
`{"err": {message, line?, column?}}`. The JavaScript glue (`PVE.meta.Core.call`) is
twenty lines: encode, copy in, call, re-read `memory.buffer` (a call may grow it), copy
out, decode. The output buffer belongs to the module and is reused; the caller frees
only its request.

Why not (a), wasm-bindgen: it was not close. Every function here takes and returns
documents — a `Value`, a path string, an edit list, a listing — so JSON is the natural
type of the interface, and wasm-bindgen's ergonomics (typed exports, a generated
`.js`) would have bought a generated file to ship and a `wasm-bindgen-cli` whose version
must equal the crate's, in a Debian build that has neither. The cost of (b) is that
argument checking is by hand (`Args` does it, and reports "argument 2 must be a string"
rather than a type error) and that there is one dispatch `match` to keep in step with
the JavaScript faces. Both are tested.

What the build needs: `rustup target add wasm32-unknown-unknown` on the build host
(which uses rustup, per the Makefile's own comments); with a distro `rustc`, Debian's
`libstd-rust-dev-wasm32` — I could not reach the build host to confirm it is in trixie.
CI (`.github/workflows/build.yml`) installs rustup with `--profile minimal`, which is
the host target only, so it now adds the wasm32 target (and clippy, which `make check`
has always needed) in the same step; the first version of this branch left the
workflow untouched and would have died at `make check` with `E0463`.
No wasm-opt (measured: it saves 15% raw and 2% gzipped; not worth a build dependency),
no npm, no bindgen. `make wasm` is one cargo invocation with a `[profile.wasm]` (size
optimisation, LTO, one codegen unit, `panic = "abort"`, stripped). `make build` and
`make check` depend on it; `make install` ships the file next to the editor and fails
if it is missing, because an editor without its core cannot read a document.

The claim "no build step beyond vendoring Monaco" is gone from the README, honestly:
the editor's core is built. The package already built Rust for the perlmod `.so`; this
is the same cargo, a second target.

## 5. Counts

Branch against `main`, this report excluded: 30 files, **+3,184 / −1,777**, net
**+1,407** lines. (The review round added ~230 of those: the four restored cases, the
length rule and its tests, the `Report` enum, the two browser objects, and the
un-loaded tests.)

Where the lines went:

| | lines | of which tests |
|---|---|---|
| `crates/pve-meta-core/src/shape.rs` (new) | 568 | ~300 |
| `crates/pve-meta-core/src/edit.rs` (new) | 342 | ~150 |
| `crates/pve-meta-wasm/src/lib.rs` (new) | 694 | ~155 |
| `crates/pve-meta-core/src/registry.rs` | +/− 250 | `governing` and its fixture test out; `from_wire`, `by_specificity`, `rules_reaching`, the name length in |
| `ui-extjs/pve-meta-tree.js` | 5,631 → 5,356 (−275) | the helper region (Utils + Yaml + Lint: 1,080 lines on `main`) became Utils + Core + Codec + Access + Shape + EditSet + Markers (837 lines); `addGrammar` 87 → `addShape` 96 |
| `ui-extjs/testing/smoke.js` | 1,770 → 1,889 (+119) | rewritten over the new objects; adds the un-loaded and raw-ABI sections; drops the covers/governing tables |
| `ui-extjs/vendor/js-yaml.min.js` | −39,430 bytes | 2 "lines" in git |

Duplicated logic removed from JavaScript, counted by function: the second YAML codec
(js-yaml plus its six settings and the `define`-hiding loader), `covers`, `hasAnyWrite`,
the inlined `can_write`, `governing`, `containsPath`, `bySpecificity`, `depth`, the
selector match in `applicablePrefixes` and `applicablePermissions`, `keyPathError`'s
regex, `setAtPath`, `deleteAtPath`, `applyPending`, `diffDocuments`, `changedPaths`,
`introducedFindings`, `writeView`, `stage`'s subsumption, and `Lint.applicable`,
`findings`, `walk`, `checkValue`, `typeMatches`, `schemaIndex`. Twenty-five functions,
roughly 460 lines with their comments; what replaced them is ~330 lines: two objects
(`Shape`, `EditSet`) that own their inputs and cache, and two faces (`Codec`, `Access`)
that do nothing but name a core call.

Fixtures: `testdata/covers-cases.json` (19 cases) and `testdata/governing-cases.json`
(16 cases) deleted — their tables are inline in `scopes.rs` (19 of 19) and `shape.rs`
(all 16, plus four of its own), next to the one implementation. The first transcription
of the governing table dropped four cases, including the subtlest one (two
independently tagged levels, where the more specific prefix's selector misses and the
parent governs); the review caught it and they are back, verbatim in intent.
`testdata/yaml-cases.json` kept: its reason changed from "two emitters agree" to "a
`serde_yaml_ng` upgrade did not move the bytes", and the harness runs it through the
wasm as an end-to-end check. Honest score: two of three made unnecessary, one
re-purposed.

Tests: **268** in Rust (153 unit + 107 integration in `pve-meta-core`, 8 in
`pve-meta-wasm`, plus one doctest), clippy and rustdoc clean with `-D warnings`.
JavaScript: **416** checks in `smoke.js`, every section of the old suite carried over,
all against the real `.wasm` — and thirteen of them run *before* the core is attached,
so the three pre-load branches (`editableFor`, `keyPathError`, `fileNameError`) are
exercised in the un-loaded state: the first fails closed, the two validators fail open,
and none of them throws.

## 6. Size and load time

| | raw | gzip −9 | brotli −11 |
|---|---|---|---|
| `pve_meta_wasm.wasm` | 593,003 | 210,067 | 170,120 |
| after `wasm-opt -Os` | 507,683 | 206,148 | — |
| js-yaml 4.1.0 min | 39,430 | 13,059 | — |

Sixteen times js-yaml over the wire, gzipped. Where the bytes are (twiggy on an
unstripped build, code and data only):

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

The rules this spike is about are 4% of the file. Half of it is two YAML parsers. The
lever, if size matters: parse with saphyr only, building `Value` from its events and
rejecting anchors/aliases/tags during the build, and keep libyaml for the emitter alone.
That would drop ~60–90 KB raw and touch the *server's* parser, which I did not want to
do inside a spike — serde_yaml_ng's scalar resolution is what DESIGN §8 relies on for
`yes` staying a string, and changing parsers is a change to what the store accepts.
`-Zbuild-std` with `panic_immediate_abort` would remove another ~40 KB and is nightly.

Load: compiling and instantiating the 593 KB module takes **1.0 ms** cold and ~0.1 ms
with the module cached, in node's V8. Browsers' streaming compilers are in the same
range for this size. Not the cost; the download is.

Per-call cost, measured through the real glue with a deliberately fat listing (8
prefixes × 40 declared properties with descriptions, 34 KB of JSON) and a 320-key
document with five `rw` scopes:

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

What a render costs with that listing, counted from what `buildTree` actually asks
(the first version of this report said ~2.5 ms; the review measured 6–7 ms and was
right, for two reasons fixed since):

- **Shape questions**: `declared` + `hasSchema` + `schemaIndex` + `findings`. The
  first three are answered once per Shape now and the panel keeps one Shape per
  document, so a render with a warm Shape pays only `findings` (0.7 ms); a cold one
  pays 2.1 ms. Before memoisation `declared` and `hasSchema` each re-asked
  `shape_prefixes` and a render asked two or three times over: ~0.8 ms of exact
  duplicate work, which the review found and an object was the fix for.
- **Per-row access**: the old per-row `governing` calls are gone, but *not* the
  per-row calls — `accessFor` asks `covers` once per rule that reaches the guest and
  `editableFor` asks `access_can_write` once, so a row costs ~10 µs at five scopes,
  and 328 rows cost **3.4 ms**. That is now the dominant render cost, and it is the
  next thing to batch (one `access_rows(access, rules, paths[])` call would make it
  one crossing); it is left as is because 3 ms on a 328-row tree with five scopes is
  not something anyone will see, and because a `Shape`-style object for access has no
  cache to offer — every row is a different path.

So: **~4 ms warm, ~5.5 ms cold** for the fat case; a real listing (two or three
prefixes, a few dozen rows) is well under a millisecond either way. The text editor's
per-keystroke `annotateText` is `findings` plus a parse (~1 ms) on a warm Shape. A
handle to a Shape kept inside the module would remove the 0.3 ms `shape_prefixes` and
1.1 ms `schema_index` a cold Shape pays, and was rejected because it is a lifetime for
JavaScript to manage for a cost paid once per load.

## 7. What got harder

- **Async initialisation reaches further.** js-yaml was lazy too, so the editor already
  awaited a codec before reading a document. But now `editableFor`, the two name
  validators and the Access column all need the core, and `syncButtons` runs on render
  before the first load. Three guards (`Core.loaded()`) and an early `Core.load()` kick
  in `initComponent` cover it: `editableFor` fails *closed* (nothing is editable until
  the rule that decides it is here), the validators fail *open* (no early answer; the
  server refuses the same names on its own, as it did before this branch). The review
  pointed out that none of the three branches had a test, and this editor has shipped
  two lazy-load ordering bugs already; `smoke.js` now runs its first section before
  attaching the core and checks all three, plus that a direct `Core.call` throws
  "not loaded" rather than trapping. Any *new* call site that reaches the core before
  `loadDocument` has awaited it will still throw that error, which is the intended
  failure: loud, not wrong.
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
- **Packaging.** One more toolchain target on the build host, and `make install` now
  hard-fails without the `.wasm`. CI's `make check` needs cargo *and* node, where the
  smoke suite needed node alone. The `Content-Type` pveproxy gives a `.wasm` is
  unverified (the lab was unreachable from here): `instantiateStreaming` requires
  `application/wasm`, so the loader falls back to `fetch` + `WebAssembly.instantiate`
  when the streaming path rejects. Whether pveproxy gzips `application/wasm` decides
  whether the download is 210 KB or 593 KB.
- **The Debian build must reach crates.io** for the same reason `dh_auto_test` already
  notes it does for the perlmod crate; nothing new, but the wasm crate adds no
  dependencies either (`serde`, `serde_json`, and the core).

## 8. What I could not finish, and what I would do next

- `perl/PVE/API2/Ext/Meta.pm`'s name regex: a `register_format` over
  `PVE::RS::Meta::is_valid_file_name`. Needs the Linux build host.
- `Markers.lineIndex` as a span index from the core (saphyr has the spans; the editor
  scans text with a heuristic). Would make marker placement exact and remove the last
  YAML-shaped code from JavaScript.
- The server does not yet *use* `EditSet` or `Shape`. Both are the editor's model
  expressed in the server's operations, which is the point, but the API could expose
  `Shape::findings` (schema lint on the server, advisory) and could plan a `PUT` through
  `EditSet` — the brief's Tier 3 "so the server could expose it too" is possible now and
  not done.
- Size: the one-parser codec described in §6, if 200 KB gzipped is judged too much.
- Per-row access (`covers` × rules + `access_can_write` per row, §6) as one batched
  crossing, if a tree ever gets large enough for 10 µs a row to show.
- A handle-based Shape inside the module, if the once-per-load 1.4 ms of a cold Shape
  ever matters. It does not today.
- The lab: nothing here ran in a browser. The headless scripts (`testing/headless-*.js`)
  need the lab and were updated only where they probed for js-yaml.

## 9. Where the abstraction leaks, honestly

- `parse` in the ABI maps a null top level (an empty buffer, `~`) to `{}`. That is an
  editor convention — the model has no nulls and an empty editor is an empty document —
  and it lives in the adapter crate, not the core. The server treats an empty *file*
  differently (unrecoverable, §4), on purpose.
- `Shape::findings` keeps the old tolerance that `1`/`0` satisfy `type: boolean`, with
  the old justification (the JSON wire convention). The editor reads YAML now, so a `1`
  is a `1`; the tolerance is inherited, documented, and probably wrong for a document
  written by hand. I left behaviour unchanged.
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

## 10. What the review changed

Six conditions, all addressed in the fourth commit; the substantive ones are folded
into the sections above where they belong, so this is the index:

1. **CI** installed no wasm32 target and would have failed at `make check` (§4).
2. **Four `governing` cases** were dropped in the fixture-to-Rust transcription; all
   sixteen are in `shape.rs` now (§5).
3. **The three pre-load guards had no test**; `smoke.js` runs before attaching the
   core and exercises them (§7).
4. **The file-name length** was a client/server divergence in the one rule the branch
   claimed to unify; `MAX_FILE_NAME_LEN` is in Rust beside the charset (§2, §3).
5. **Duplicate `shape_prefixes` calls** per render, and **two sorts** of the findings
   by two rules: memoised on a real Shape object, and one sort in Rust over one list
   (§2, §6). The render number in this report was wrong and is corrected (§6).
6. **The browser side had no objects**: `Shape` and `EditSet` are objects now, and the
   framing of what the browser gained is corrected to what it is (§2).

Being as critical of the fixes as the review was of the original: (2) and (4) were
carelessness, not judgement — a transcription I did not diff against its source, and a
Perl schema I read for its pattern and not its second line. (5)'s duplicate call is the
kind of thing an object catches and a bag of closures does not, which is the review's
point about (6) made concrete. (3) I had written the guards *because* of the two prior
ordering bugs and still shipped them untested. What I would still push on: the identity
check in `shapeFor` is a convention with one sentence guarding it; the per-row access
cost is real and unbatched; and the un-loaded validators failing open is a choice that
should be re-examined the day the server stops mirroring the rule.

## 11. The recommendation, and why

Adopt. The measure the owner asked for was the quality of the abstractions, and by that
measure the result is better on both sides of the boundary, not only in the browser: the
core gained two types it should have had (`Shape` replaces a function with no caller;
`EditSet` names the editor's plan in the store's own operations), the two nesting rules
are two documented types instead of two sets of predicates, and every rule in the
brief's table now has one implementation with one test table beside it. The build story
is one cargo command with a standard target and no tool to pin, which is what the
packaging concern was really about. The only real cost is the download, and it is a
one-time 200 KB in a UI that lazily loads a 13 MB Monaco tree.

I would not adopt without two things: a lab run of the editor (the headless scripts
exist for this), and a check of what pveproxy does with `application/wasm`. And I would
schedule the one-parser codec if the size is judged to matter — it is the only lever
that moves the number meaningfully, and it is a change to the server's parser, which
deserves its own spike.
