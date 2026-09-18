# ui-extjs — the native ExtJS editor

One shipped script, `pve-meta-tree.js`, which `make js` generates from `src/*.js` --
plain ES2017, one file per section, concatenated in name order with no bundler and no
transform -- plus the core (`pve-meta-core.wasm`, built by `make wasm` from
`crates/pve-meta-wasm`) and `monaco/` (fetched by `make ui`). The generated script and
`monaco/` are gitignored; the sources are what is edited. It defines two panels that pve-ext's page loader
instantiates as native tabs: `pveMetaTreePanel`, one document's editor, on every guest's
config panel; and `pveMetaDatacenterPanel`, the Prefixes registry grid, on the Datacenter
panel, each row of which opens in that same editor.

The editor implements no rules. The YAML codec, the key-name charset, which prefix
governs a path and what a schema makes of a value are all `pve-meta-core` -- the
server's own crate -- compiled for the browser. The panel holds one object over it --
a `PVE.meta.Shape` per document, which owns the prefix listing and caches what the
core derives from it -- and calls a stateless face, `PVE.meta.Codec`, plus the
key-name checks on `Utils`. See `docs/WASM-CORE.md` for how and why.

This is **the** editor (DESIGN §8). A second implementation in pwt/Yew was built to the
same specification and compared on the lab; it was removed once the choice was made (git
tag `pwt-ui-removed`).

Session, CSRF, dark theme, i18n and the whole page chrome come from the PVE UI. Nothing
in this file re-derives any of them.

## What it does

The behaviour is specified in `docs/DESIGN.md` §8 and not repeated here: one tree of
the document the caller can see; lists as containers with one row per member; **an
edit is one write with the digest, and a 409 reloads**; Tree and Text as two views of
the same document; the schema markers; the datacenter tab's registry grid and the
form behind it. What follows is what is specific to *this* file.

### The Tree card

* Columns **Key | Value | Description**, one line per row. A declared-but-unset key
  renders faded, showing `not set (default: …)`.
* **Row icons.** A folder on a map row (`fa fa-folder`, `fa fa-folder-open` while it is
  expanded — swapped on `itemexpand`/`itemcollapse`, since ExtJS has no per-node
  "expanded icon"), a document (`fa fa-file-text-o`) on a value row. No CSS ships with
  this file: ExtJS marks any node carrying an `iconCls` with `x-tree-icon-custom`, which
  is the class PVE's own `ext6-pve.css` sizes and colours for the resource tree (1.25em,
  `#555`; `#e6e6e6` in proxmox-dark), so the icons match the rest of the UI by
  construction.
* **Comment keys are not rows.** `k__` is the **Description** of row `k`; a bare `__`
  documents the map it sits in. They are ordinary data everywhere else. The schema's
  own `description` is a different thing: it is the row's **tooltip**, never the
  Description column.
* **The toolbar** acts on the document's *contents*: Add, Edit, Set to Default, Declare
  Key (prefix files only), Remove, Edit selection as text, Reload, and at the right the
  muted *Read-only* label that appears only when the document is not writable. Each of
  those is **one write** — `PUT ?view=<the row's path>&mode=replace&digest=…`, or
  `DELETE ?view=…` for Remove — followed by a reload; a list member has no path of its
  own, so acting on one writes the whole list at the list's path. The footer
  (`PVE.meta.Footer`) carries the Tree | Text toggle and, in Text, that card's own
  Format, Diff, Apply and Revert.
* **A new key** is written at its own dotted path: `view::replace` creates the maps
  above it, so `added.by.ui` needs no write of its own to make room.
* **`Doc.writeFor(docId, edit, digest, force)`** is that whole write path as a pure
  function — every editor hands it an edit and sends what comes back, and the smoke
  suite drives it as a table.
* **The row editor** is a modal window opened by Edit, a double-click, or Enter on the
  selected row. Its field comes from the schema's type first and the stored value's type
  second: `enum` → combobox, `boolean` → `proxmoxcheckbox`, `integer`/`number` →
  numberfield, a `multiline` string or one that already has newlines → textarea,
  everything else → textfield. A map, or a list of maps, skips the row editor and opens
  Monaco on that subtree. A declared type wins, deliberately: it is the operator's
  statement of what the key means.
* **Editability is per document**, from `GET /meta/access`: every row is editable when
  the document is writable, none when it is not -- nothing is decided per path.

### The Text card

* The **whole document** in Monaco as YAML, with the presentation-only YAML/JSON view
  toggle, loaded with the file's own text. Apply sends the buffer as `text` at the root
  view (`PUT ?view=&mode=replace&digest=…`) — the one write that is text rather than a
  subtree, and therefore the only way a `#` comment or a hand-written key order reaches
  the file. Revert re-reads it; Diff shows the buffer against what is stored.
* The buffer is **the only unwritten state in the editor**, so leaving Text with a
  dirty one asks once whether to discard it. A document that does not parse opens here
  and can only be repaired here, by a whole-document write.
* The **Text** segment is disabled when `/meta/access` reports no read, rather than
  offering a button that can only fail.

### Everywhere

* **No background poll.** The toolbar's Reload is manual; a reload preserves which
  rows were expanded.
* **Conflicts.** Every write carries the digest. On 409 the panel reloads (or re-reads
  the text buffer) and shows the API's message verbatim under a *Conflict* title. Every
  other error is the API's message verbatim too. This is what catches a concurrent
  change without a poll: the next write finds out.
* **Enforced schemas.** A prefix with `enforce: true` makes the server refuse a write
  that breaks it: 422, naming the paths. That message is the dialog, and *Save anyway*
  sends the same write again with `force=1`. Every other schema finding is advisory and
  only marks the row amber (and squiggles the line in Text).
* **Monaco**, three jobs: *Edit selection as text* (the selected subtree, in a window,
  whose OK is one `replace` of that subtree), the Text card (the whole document, in the
  panel body), and the diff behind the Diff button. Its AMD loader is fetched lazily on first use from
  `/pve2/js/pve-meta-extjs/vs/loader.js` — Monaco is vendored into the package by the
  top-level `make ui` (npm), never fetched from a CDN — and every editor and model is
  disposed when its owner goes away. `PVE.meta.Monaco.load()` waits for the core first,
  since every Monaco caller also needs the codec.

## The core

`pve-meta-core.wasm` is `crates/pve-meta-wasm`: `pve-meta-core` behind a JSON-string
ABI of five exports (`pm_alloc`/`pm_free` for the request, `pm_call` to run it,
`pm_output` for the response, `pm_abi` for the version the glue checks on attach), built
with a plain `cargo build --target wasm32-unknown-unknown --profile wasm` and nothing
else -- no wasm-bindgen, no generated glue. It installs next to the panel as
`/usr/share/pve-manager/js/pve-meta-extjs/pve-meta-core.wasm` and is loaded lazily by
`PVE.meta.Core.load()` — `WebAssembly.instantiateStreaming` where pveproxy serves the
file as `application/wasm`, falling back to `fetch` + `WebAssembly.instantiate` where
it does not — the same way Monaco is.

`PVE.meta.Core.call(name, ...args)` is the whole glue: encode `{fn, args}` as UTF-8 into
a buffer the module hands out, call, decode the response, and turn an `err` into a
`PVE.meta.CoreError` carrying the parser's `line`/`column` when it has one. Nothing
holds state inside the module between calls; a Shape is rebuilt from the prefix
listing on every question, which costs microseconds and no lifetime to manage.

The YAML the editor shows is therefore the YAML the store writes, by construction: one
emitter, not two kept in step by settings. The server stays the authority all the same --
Apply in the YAML view sends the buffer to the API untouched as `text`, a row edit sends
one subtree as `data`, and the server runs the same code again on the real write.

The editor **reads every document as YAML** (`format=yaml`), never as JSON: perlmod
renders a document as a native Perl hash on the way out, and a Perl hash has no key
order, so `format=json` cannot carry the order the store holds (DESIGN §5, decision 007).
That is what makes a root write put the file back in the order it was in.

## How it is wired

pve-ext's page loader (`pve-ext/js/pve-ext-loader.js`) reads page manifests from
`/usr/share/pve-ext/pages/*.json` through `GET /api2/json/ext/pages`. A manifest declares
either `url` (a same-origin iframe) or `script` + `xtype` (a native panel class). This
editor ships **two** manifests in the second form over one script, because a manifest
carries a single `xtype` and the guest tab and the datacenter tab are different panels
(DESIGN §8):

`pages/pve-meta.json`:

```json
{
    "id": "pve-meta",
    "title": "Metadata",
    "iconCls": "fa fa-tags",
    "targets": ["lxc", "qemu"],
    "script": "/pve2/js/pve-meta-extjs/pve-meta-tree.js",
    "xtype": "pveMetaTreePanel",
    "requires": { "vms": ["VM.Audit"] }
}
```

`pages/pve-meta-dc.json` is the same with `"id": "pve-meta-dc"`, `"targets": ["dc"]`,
`"xtype": "pveMetaDatacenterPanel"` and `"requires": { "dc": ["Sys.Audit"] }`. The
loader fetches a `script` once per URL, so the second manifest costs no second download.

The file installs to `/usr/share/pve-manager/js/pve-meta-extjs/pve-meta-tree.js` with
`pve-meta-core.wasm` next to it, which pveproxy serves at the `script` path above
(top-level `Makefile`, `install` target).

**The contract this panel expects from the loader.** The loader loads the script once
per URL, waits until the `xtype` resolves to a defined class, then instantiates
`{ xtype, ...instanceConfig }` into the tab's wrapper panel. `instanceConfig` carries
`type` plus `vmid` and `node` for `lxc`/`qemu`, or `dc: 1` for the datacenter. The panel
needs only `vmid` (guest) or its absence (datacenter); it also falls back to
`pveSelNode.data.vmid`, so it still works if it is ever added to a `PVE.panel.Config`
directly, which injects `pveSelNode` through that panel's `defaults`.

Note for whoever maintains the loader: `Ext.ClassManager.isCreated()` takes a class
*name*, not an xtype — the xtype resolves through `getNameByAlias('widget.' + xtype)`
first, which is what its `waitForXtype()` does.

## Verifying it

* `make js` — generates `pve-meta-tree.js` and runs `node --check` on it.
* `node testing/smoke.js` — offline, no DOM, after `make wasm js` (`make check` runs it
  when `node` is present). It loads the real file into `node:vm` behind a small
  `Ext`/`Proxmox` shim, instantiates the *built* `pve-meta-core.wasm` synchronously and
  hands it to `PVE.meta.Core.attach`, so the shipped bytes and the shipped glue are what
  gets tested. It starts *before* attaching the core, so the pre-load branches (the two
  name validators fail open, `Core.call` says "not loaded" rather than trapping) are
  exercised; then the raw ABI (a megabyte through the buffer, a memory growth, non-ASCII,
  error locations); then the editor's own logic: the row-editor field choice, the
  document+shape row merge, per-row editability, the request each kind of edit produces
  (`Doc.writeFor`, as a table), and the codec — including a
  **round-trip property test**: a fixed corpus of hostile documents plus 500 generated
  ones (keys and values with colons, quotes, hashes, unicode, numeric-looking strings,
  booleans, nested maps and arrays), asserting `parse(dump(x))` deep-equals `x` with key
  order intact. The rules themselves are tested where they live, in Rust.
* `testing/lab/` is not a test suite: the two scripts there need a live PVE host and
  nothing in `make check` or CI runs them. `headless-tab-check.js` and
  `headless-flows-check.js` (open the tab, add a key, edit it, remove it, apply from the
  Text card, provoke a 409) run headless Chromium against the real pve-manager SPA:
  `node headless-tab-check.js <host> <vmid> <light|dark> [--stub-registry] [--readonly]
  [--ro]`. `--stub-registry` stubs a `GET /meta/prefixes` payload; `--ro` stubs
  `GET /meta/access` with a read-only answer so the "Read-only" toolbar label and the
  per-row editability can be seen without a second principal's credentials. They need
  `puppeteer-core` and a chromium binary and write their screenshots to
  `/root/headless/shots`; they were run from the lab build host, not from a workstation.

## Screenshots

`docs/screenshots/` holds only what the top-level README shows, light and dark: the
guest tree, Text mode with the diff, the Prefixes grid, and the declaration form. They were taken on `pvemeta-node1`
(pve-manager 9.2.11, ExtJS 7.0.0, proxmox-widget-toolkit 5.2.8) inside the real UI.
Anything else -- the row editor, the JSON view, a read-only caller's tab -- is a run of
`testing/lab/headless-tab-check.js` away, which writes a full set into the directory
you give it; that set is not committed.

