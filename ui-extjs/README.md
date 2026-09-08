# ui-extjs — the native ExtJS editor

One file, `pve-meta-tree.js`, plain ES2017, no build step, plus `vendor/` (js-yaml) and
`monaco/` (fetched by `make ui`, gitignored). It defines a panel (`xtype: pveMetaTreePanel`)
that pve-ext's page loader instantiates as a native tab inside the PVE guest and
datacenter config panels.

This is **the** editor (DESIGN §8). A second implementation in pwt/Yew was built to the
same specification and compared on the lab; it was removed once the choice was made (git
tag `pwt-ui-removed`).

Session, CSRF, dark theme, i18n and the whole page chrome come from the PVE UI. Nothing
in this file re-derives any of them.

## What it does (DESIGN §5, §7, §8)

The panel is a **card layout over two bodies**, swapped in place by the **Tree | Text**
segmented button at the right end of the toolbar.

### The Tree card

* **One tree of the document the caller can see.** Columns **Key | Value | Description |
  Access**, one line per row. Rows are the union of the keys present in the document and
  the keys the applicable grammars declare. A declared-but-unset key renders faded,
  showing `not set (default: …)`; "setting" it is just editing it.
* **Row icons.** A folder on a map row (`fa fa-folder`, `fa fa-folder-open` while it is
  expanded — swapped on `itemexpand`/`itemcollapse`, since ExtJS has no per-node
  "expanded icon"), a document (`fa fa-file-text-o`) on a value row. No CSS ships with
  this file: ExtJS marks any node carrying an `iconCls` with `x-tree-icon-custom`, which
  is the class PVE's own `ext6-pve.css` sizes and colours for the resource tree (1.25em,
  `#555`; `#e6e6e6` in proxmox-dark), so the icons match the rest of the UI by
  construction.
* **Comment keys are not rows.** `k__` is the **Description** of row `k`; a bare `__`
  documents the map it sits in. They are ordinary data everywhere else. The grammar's
  own `description` is a different thing: it is the row's **tooltip**, never the
  Description column.
* **Arrays are one text leaf**, shown as JSON. On commit, JSON is parsed if the text
  starts with `[`, otherwise a comma-separated list is split.
* **Access** lists *every* registration from `GET /meta/operators` whose scope covers the
  row — `rw` ones by name in normal text, `ro` ones muted with `(ro)`, `rw` first. The
  cell tooltip spells each one out with its selector (`example-traefik (rw, all
  guests)`). Several principals may read a subtree; this column is about who writes and
  who subscribes, not ownership. Scopes apply to guest documents only, so the datacenter
  tree has no entries. `selector: { tag: … }` is resolved against the guest's `tags` from
  `GET /meta/guests`, fetched only when some registration actually uses a tag selector.
* **No per-row action icons.** The toolbar is **Add | Edit | Remove | Edit selection as
  text | Reload**, all targeting the selection: Add goes into the selected map, the
  parent of a selected leaf, or the document root; Edit and Remove need a row; *Edit
  selection as text* needs a selection.
* **The row editor** is a modal window opened by Edit, a double-click, or Enter on the
  selected row. Its field comes from the grammar's type first and the stored value's type
  second: `enum` → combobox, `boolean` → `proxmoxcheckbox`, `integer`/`number` →
  numberfield, everything else → textfield. A declared type wins, deliberately: it is the
  operator's statement of what the key means, and the API's JSON view cannot represent a
  boolean (see *Known API friction* below).
* **Editability is per row**, from `GET /meta/access`: full write, or an `rw` scope
  covering that path. A non-editable row cannot be edited or removed.

### The Text card

* The **whole document** in Monaco as YAML, with the same presentation-only YAML/JSON
  view toggle, **Apply** (a Monaco side-by-side diff as the confirm step, then
  `PUT ?view=&mode=replace&digest=…` at the root) and **Discard** (re-reads the
  document into the buffer, asking first if the buffer was edited).
* Switching back to **Tree** with an edited buffer asks first.
* The card is the *root view*, which a scope-only principal may not read at all
  (DESIGN §3) — so the **Text** segment is disabled when `/meta/access` reports no full
  read, rather than offering a button that can only fail.
* While the Text card is active the tree's toolbar buttons are disabled and the version
  poll is suspended.

### Everywhere

* **A muted access label** — *Scoped write access* or *Read-only* — sits next to the
  Tree | Text toggle, and **only when the caller is restricted**. A caller with full
  write sees nothing there.
* **Writes are minimal.** A row edit and the Add window issue
  `PUT /meta/guests/{vmid}?view=<dotted.path>&mode=replace&data=<json scalar>&digest=…`;
  Remove issues `DELETE …?view=<path>&digest=…`. On 409 the panel reloads (or re-reads
  the text buffer) and shows the API's message verbatim under a *Conflict* title. Every
  other error is the API's message verbatim too.
* **The version poll** runs every 5 s (`Ext.TaskManager`), compares the token from
  `GET /meta/version`, and reloads the tree when it changed — never while a row editor,
  the selection text window or the Text card is open. A reload preserves which nodes were
  expanded.
* **Monaco**, three jobs: *Edit selection as text* (the selected subtree, in a window),
  the Text card (the whole document, in the panel body), and the diff that confirms
  either one's Apply. Its AMD loader is fetched lazily on first use from
  `/pve2/js/pve-meta-extjs/vs/loader.js` — Monaco is vendored into the package by the
  top-level `make ui` (npm), never fetched from a CDN — and every editor and model is
  disposed when its owner goes away.

## YAML

`vendor/js-yaml.min.js` is js-yaml **4.1.0**'s `dist/js-yaml.min.js`, downloaded verbatim
from the upstream release tag, with its `LICENSE` beside it (MIT; recorded in
`debian/copyright`). It installs next to the panel as
`/usr/share/pve-manager/js/pve-meta-extjs/vendor/` and is loaded lazily, the same way
Monaco is.

`jsyaml.load` runs on js-yaml's default schema, which is the safe one; `jsyaml.dump` is
pinned to `{ indent: 2, lineWidth: -1, noRefs: true, sortKeys: false }` — never fold a
long line (a folded line is a changed line in the diff), never emit an anchor, never
reorder an ordered map.

It is used for **presentation only**: the YAML/JSON view toggle and the "original" side
of a diff. The server stays the authority on YAML — an Apply in YAML view sends the
buffer to the API untouched as `text`, and only an Apply made in JSON view sends `data`.

One loading subtlety: js-yaml ships a UMD bundle that prefers an AMD `define` if one is
present, and Monaco's loader installs exactly such a `define`. The loader here hides
`window.define` for the duration of that one script load and puts it back afterwards, and
`PVE.meta.Monaco.load()` waits for `PVE.meta.Yaml.load()` first so the two never overlap.

## How it is wired

pve-ext's page loader (`pve-ext/js/pve-ext-loader.js`) reads page manifests from
`/usr/share/pve-ext/pages/*.json` through `GET /api2/json/ext/pages`. A manifest declares
either `url` (a same-origin iframe) or `script` + `xtype` (a native panel class). This one
uses the second form:

`pages/pve-meta.json`:

```json
{
    "id": "pve-meta",
    "title": "Metadata",
    "iconCls": "fa fa-tags",
    "targets": ["lxc", "qemu", "dc"],
    "script": "/pve2/js/pve-meta-extjs/pve-meta-tree.js",
    "xtype": "pveMetaTreePanel",
    "requires": { "vms": ["VM.Audit"], "dc": ["Sys.Audit"] }
}
```

The file installs to `/usr/share/pve-manager/js/pve-meta-extjs/pve-meta-tree.js` and
`vendor/` next to it, which pveproxy serves at the `script` path above (top-level
`Makefile`, `install` target).

**The contract this panel expects from the loader.** The loader loads the script once
per URL, waits until the `xtype` resolves to a defined class, then instantiates
`{ xtype, ...instanceConfig }` into the tab's wrapper panel. `instanceConfig` carries
`type` plus `vmid` and `node` for `lxc`/`qemu`, or `dc: 1` for the datacenter. The panel
needs only `vmid` (guest) or its absence (datacenter); it also falls back to
`pveSelNode.data.vmid`, so it still works if it is ever added to a `PVE.panel.Config`
directly, which injects `pveSelNode` through that panel's `defaults`.

Note for whoever maintains the loader: `Ext.ClassManager.isCreated()` takes a class
*name*, not an xtype — the xtype resolves through `getNameByAlias('widget.' + xtype)`
first. The installed loader already does this correctly; only the stale comment in the
repository copy's header still says otherwise.

## Verifying it

* `node --check pve-meta-tree.js`
* eslint 9 (`no-unused-vars` with `caughtErrorsIgnorePattern: '^_'`, `no-undef`,
  `eqeqeq`, `no-var`, …) with `Ext`, `PVE`, `Proxmox` and `gettext` as globals — clean,
  for the panel and for `testing/*.js`.
* `node testing/smoke.js` — offline, no DOM. It loads the real file into `node:vm` behind
  a small `Ext`/`Proxmox` shim (with the *vendored* js-yaml `require`d in as
  `window.jsyaml`, so the shipped build is what gets tested) and drives the pure parts:
  the path and scope helpers, the row-editor field choice, the document+grammar row
  merge, Access resolution and per-row editability, and the YAML codec — including a
  **round-trip property test**: a fixed corpus of hostile documents plus 500 generated
  ones (keys and values with colons, quotes, hashes, unicode, numeric-looking strings,
  booleans, nested maps and arrays), asserting `load(dump(x))` deep-equals `x` with key
  order intact.
* `testing/headless-tab-check.js` and `testing/headless-flows-check.js` run headless
  Chromium against the real pve-manager SPA:
  `node headless-tab-check.js <host> <vmid> <light|dark> [--operators] [--readonly]
  [--scoped] [--ro]`. `--operators` stubs a `GET /meta/operators` payload; `--scoped` and
  `--ro` stub `GET /meta/access` so the restricted toolbar labels and the per-row
  editability can be seen without a second principal's credentials. They need
  `puppeteer-core` and a chromium binary and write their screenshots to
  `/root/headless/shots`; they were run from the lab build host, not from a workstation.

## Screenshots

Taken on `pvemeta-node1` (pve-manager 9.2.11, ExtJS 7.0.0, proxmox-widget-toolkit 5.2.8)
inside the real UI, against the live revision-5 API, with two live registrations
(`example-traefik`, `rw` on `traefik` for all guests, with a grammar; `scoped`, `rw` on
`traefik` for tag `traefik` and `ro` on `netbird` for all guests).

| File | What it shows |
|---|---|
| `extjs-tree-light.png`, `extjs-tree-dark.png` | the Tree card: row icons, the Description column from a comment key, the Access column, a faded declared-but-unset row |
| `extjs-access-tip-light.png`, `extjs-access-tip-dark.png` | the Access tooltip, one registration per line with its mode and selector |
| `extjs-rowedit-light.png`, `extjs-rowedit-dark.png` | the row editor (opened by a double-click), with the comment key as its Description field |
| `extjs-text-light.png`, `extjs-text-dark.png` | the Text card: the whole document in Monaco, YAML |
| `extjs-text-json-light.png`, `extjs-text-json-dark.png` | the same after the JSON view toggle |
| `extjs-text-dirty-light.png`, `extjs-text-dirty-dark.png` | leaving Text with an edited buffer asks first |
| `extjs-selection-text-light.png`, `extjs-selection-text-dark.png` | *Edit selection as text* on the `traefik` subtree |
| `extjs-selection-text-json-light.png`, `extjs-selection-text-json-dark.png` | the same value after the JSON toggle |
| `extjs-diff-light.png`, `extjs-diff-dark.png` | the Monaco diff shown to confirm Apply |
| `extjs-conflict-light.png`, `extjs-conflict-dark.png` | a 409 reported with the API's message verbatim |
| `extjs-scoped-light.png` | a caller with one `rw` scope: "Scoped write access", rows outside `traefik` not editable |
| `extjs-readonly-dark.png` | a caller with read but no write: "Read-only", every editing button disabled |
| `extjs-addkey.png` | the Add Key window |
| `extjs-datacenter.png` | the same panel over `/meta/datacenter` |

## Known API friction

`GET …?format=json` returns YAML booleans as `1`/`0` — the file still says `flag: true`,
but the JSON view cannot tell a boolean from the integer 1. Any client is affected, not
just this one. This panel copes by letting a grammar's declared `type: boolean` decide
the editor (a `proxmoxcheckbox`, which accepts the integer 1 as true) and by rendering
booleans through `Proxmox.Utils.format_boolean`; without a grammar such a key looks like
a number and gets a numberfield.
