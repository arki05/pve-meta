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
form behind it.

Two things worth knowing that are specific to *this* implementation, not the design:
row icons ride on ExtJS's own `x-tree-icon-custom` class, so no CSS ships with this
file; and `Doc.writeFor(docId, edit, digest, force)` is the whole write path as one
pure function, so every editor hands it an edit and sends back what it builds, rather
than each assembling its own request.

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
  gets tested: the raw ABI (a megabyte through the buffer, a memory growth, non-ASCII,
  error locations, a trap-vs-error distinction), one marshaling spot-check per wasm
  export, and the editor's own logic on top of it (the row-editor field choice, the
  document+shape row merge, the request each kind of edit produces as a table, the
  byte-exact YAML the store writes). The rules themselves are tested where they live,
  in Rust; this suite does not re-prove them.
* `testing/lab/headless-check.js` is not a test suite: it needs a live PVE host and
  nothing in `make check` or CI runs it. It drives headless Chromium through the real
  pve-manager SPA over one path -- open the tab, add a key, edit it, remove it, apply
  from the Text card, provoke a 409, then the datacenter tab's registry grid:
  `node headless-check.js <host> <vmid> [light|dark] [--ro]`. `--ro` stubs
  `GET /meta/access` read-only, so the "Read-only" toolbar label can be seen without a
  second principal's credentials. Needs `puppeteer-core` and a chromium binary; writes
  screenshots to `/root/headless/shots`; run from the lab build host, not a workstation.

## Screenshots

`docs/screenshots/` holds only what the top-level README shows, light and dark: the
guest tree, Text mode with the diff, the Prefixes grid, and the declaration form. They were taken on `pvemeta-node1`
(pve-manager 9.2.11, ExtJS 7.0.0, proxmox-widget-toolkit 5.2.8) inside the real UI.
Anything else -- the row editor, the JSON view, a read-only caller's tab -- is a run of
`testing/lab/headless-check.js` away, which writes a full set into the directory you
give it; that set is not committed.

