# The wasm spike: making the browser like Perl

Branch `wasm-unify`, off `main` at `54f2b78`. Two commits: `968cf82` (the core grows
the concepts the editor was reimplementing) and the one that follows it (the editor
asks the core). Nothing was deployed; everything below was verified offline — `cargo
test`, clippy and rustdoc on `pve-meta-core` and the new `pve-meta-wasm`, and a node
harness that instantiates the built `.wasm` and drives the editor's helpers through it.

**Reached: all three tiers.** The codec and the pure rules (1), the staging model (2),
and the schema lint (3) are one implementation each, in Rust, and the editor calls
them. js-yaml is deleted. Two of the three shared fixtures are deleted; the third has a
different job now.

**Recommendation: adopt**, with one cost accepted and one thing to verify on the lab
before merging. The cost is ~205 KB gzipped over the wire on first load (against 13 KB
for js-yaml), cached thereafter. The thing to verify is that pveproxy serves a `.wasm`
compressed — the loader works either way, but the download is three times larger if
not. The argument for adopting is not the line count (net +1.2k lines) but what the
lines are: five named concepts where there were two piles of predicates, and a rule
list that is empty for the first time.

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

At the call sites: `stage`, `pendingUnder`, `discardRow`, `plannedData`, `leaveTextMode`,
`confirmAndApply` and `applyFindingsFor` each became one call on `PVE.meta.Edits`.
`discardRow` had been filtering by object identity against the result of
`pendingUnder`; that could not survive a boundary crossing and became `discardUnder`.

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
`registry::is_valid_file_name` reaches the New dialog's name field for the first time.

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
  implementation of PVE's formats. So `Shape::findings` returns findings *plus* a list
  of `FormatCheck { path, format, value }` it could not judge, and `PVE.meta.Shape.findings`
  runs those through `Utils.checkFormat` and merges. This is a deliberate leak with a
  type on it.
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
No wasm-opt (measured: it saves 15% raw and 2% gzipped; not worth a build dependency),
no npm, no bindgen. `make wasm` is one cargo invocation with a `[profile.wasm]` (size
optimisation, LTO, one codegen unit, `panic = "abort"`, stripped). `make build` and
`make check` depend on it; `make install` ships the file next to the editor and fails
if it is missing, because an editor without its core cannot read a document.

The claim "no build step beyond vendoring Monaco" is gone from the README, honestly:
the editor's core is built. The package already built Rust for the perlmod `.so`; this
is the same cargo, a second target.

## 5. Counts

Branch against `main`, this report excluded: 28 files, **+2,919 / −1,738**, net
**+1,181** lines.

Where the lines went:

| | lines | of which tests |
|---|---|---|
| `crates/pve-meta-core/src/shape.rs` (new) | 523 | ~270 |
| `crates/pve-meta-core/src/edit.rs` (new) | 342 | ~150 |
| `crates/pve-meta-wasm/src/lib.rs` (new) | 684 | ~150 |
| `crates/pve-meta-core/src/registry.rs` | +/− 233 | `governing` and its fixture test out; `from_wire`, `by_specificity`, `rules_reaching` in |
| `ui-extjs/pve-meta-tree.js` | 5,631 → 5,230 (−401) | the helper region (Utils + Yaml + Lint: 1,075 lines) became Utils + Core + Codec + Access + Shape + Edits + Markers (752 lines); `addGrammar` 87 → `addShape` 96 |
| `ui-extjs/testing/smoke.js` | 1,770 → 1,848 (+78) | rewritten over the new faces; adds the raw-ABI section; drops the covers/governing tables |
| `ui-extjs/vendor/js-yaml.min.js` | −39,430 bytes | 2 "lines" in git |

Duplicated logic removed from JavaScript, counted by function: the second YAML codec
(js-yaml plus its six settings and the `define`-hiding loader), `covers`, `hasAnyWrite`,
the inlined `can_write`, `governing`, `containsPath`, `bySpecificity`, `depth`, the
selector match in `applicablePrefixes` and `applicablePermissions`, `keyPathError`'s
regex, `setAtPath`, `deleteAtPath`, `applyPending`, `diffDocuments`, `changedPaths`,
`introducedFindings`, `writeView`, `stage`'s subsumption, and `Lint.applicable`,
`findings`, `walk`, `checkValue`, `typeMatches`, `schemaIndex`. Twenty-five functions,
roughly 460 lines with their comments; what replaced them is ~250 lines of faces that
do nothing but name a core call.

Fixtures: `testdata/covers-cases.json` (24 lines) and `testdata/governing-cases.json`
(118 lines) deleted — their tables are inline in `scopes.rs` and `shape.rs`, next to the
one implementation. `testdata/yaml-cases.json` kept: its reason changed from "two
emitters agree" to "a `serde_yaml_ng` upgrade did not move the bytes", and the harness
runs it through the wasm as an end-to-end check. Honest score: two of three made
unnecessary, one re-purposed.

Rust tests: 153 unit + 107 integration in core, 8 in the wasm crate; clippy and
rustdoc clean with `-D warnings`. JavaScript: 403 checks in `smoke.js`, every section
of the old suite carried over, all against the real `.wasm`.

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
prefixes × 40 declared properties with descriptions, 34 KB of JSON):

| call | µs |
|---|---|
| `covers` | 1.4 |
| `shape_governing` (re-parses the 34 KB listing) | 322 |
| `shape_schema_index` | 1,131 |
| `shape_findings` on a 320-key document | 699 |
| `edits_apply`, one edit, 320 keys | 96 |
| `edits_between`, 320 keys | 170 |
| parse / dump YAML, 4 KB | 312 / 186 |

A render makes about four Shape calls (`declared`, `hasSchema`, `schemaIndex`,
`findings`), so ~2.5 ms with that listing and well under a millisecond with a real one.
The text editor's per-keystroke `annotateText` is the same three calls plus a parse.
The old per-row `governing` calls are gone (the walks moved into the core), which is
why a stateless Shape that re-parses its listing on every question was acceptable; a
handle to a Shape kept inside the module would be the answer if listings ever grew to
where this shows, and it was rejected here because it is a lifetime for JavaScript to
manage for a gain nobody can measure today.

## 7. What got harder

- **Async initialisation reaches further.** js-yaml was lazy too, so the editor already
  awaited a codec before reading a document. But now `editableFor`, the two name
  validators and the Access column all need the core, and `syncButtons` runs on render
  before the first load. Three guards (`Core.loaded()`) and an early `Core.load()` kick
  in `initComponent` cover it; a validator with no core answers "fine" and leaves the
  refusal to the server, which is what it did before this branch anyway. This is the
  kind of edge an adversarial review should push on: any new call site that reaches the
  core before `loadDocument` has awaited it will throw "The pve-meta core is not loaded".
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
- The `PVE.meta.Shape` face rebuilds the Rust `Shape` per question. A handle-based
  variant is the obvious optimisation if a profile ever asks for it.
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

## 10. The recommendation, and why

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
