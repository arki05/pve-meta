# ui-extjs — the native ExtJS editor

One file, `pve-meta-tree.js`, plain ES2017, no build step. It defines an
`Ext.tree.Panel` (`xtype: pveMetaTreePanel`) that pve-ext's page loader instantiates as
a native tab inside the PVE guest and datacenter config panels. This is one of the two
implementations DESIGN §8 asks to be built and compared on the lab; the other is
`../ui/` (pwt/Yew in an iframe).

Session, CSRF, dark theme, i18n and the whole page chrome come from the PVE UI. Nothing
in this file re-derives any of them.

## What it does (DESIGN §5, §7, §8)

* **One tree of the document the caller can see.** Columns **Key | Value | Owner**.
  Rows are the union of the keys present in the document and the keys the applicable
  grammars declare. A declared-but-unset key renders faded, showing `not set (default:
  …)` and the grammar's description; "setting" it is just editing its Value cell.
* **Comment keys are not rows.** `k__` becomes the description (and hover tooltip) of
  row `k`; a bare `__` documents the map it sits in. They are ordinary data everywhere
  else.
* **Arrays are one text leaf**, shown as JSON. On commit, JSON is parsed if the text
  starts with `[`, otherwise a comma-separated list is split.
* **Owner** is the registration from `GET /meta/operators` whose scope covers the row —
  longest matching prefix wins — annotated with the selector (`(tag: traefik)`) and with
  `[ro]` for a read-only scope. Scopes apply to guest documents only, so the datacenter
  tree has no owners. `selector: { tag: … }` is resolved against the guest's `tags` from
  `GET /meta/guests`, fetched only when some registration actually uses a tag selector.
* **Editors** come from the grammar's type first and the stored value's type second:
  `enum` → combobox, `boolean` → checkbox, `integer`/`number` → numberfield, everything
  else → textfield. A declared type wins, deliberately: it is the operator's statement
  of what the key means, and the API's JSON view cannot represent a boolean (see
  *Known API friction* below).
* **Editability is per row**, from `GET /meta/access`: full write, or an `rw` scope
  covering that path. A non-editable row will not open an editor and shows no row
  actions.
* **Writes are minimal.** A cell commit and the Add window both issue
  `PUT /meta/guests/{vmid}?view=<dotted.path>&mode=replace&data=<json scalar>&digest=…`;
  Remove issues `DELETE …?view=<path>&digest=…`. On 409 the panel reloads and shows the
  API's message verbatim under a *Conflict* title. Every other error is the API's
  message verbatim too, through `Ext.Msg.alert` / `Proxmox.Utils.setErrorMask`.
* **The version poll** runs every 5 s (`Ext.TaskManager`), compares the token from
  `GET /meta/version`, and reloads the tree when it changed — never while a cell editor
  or the text window is open. A reload preserves which nodes were expanded.
* **Monaco, two jobs only.** *Edit as Text* opens the selected subtree (or the whole
  document) in an `Ext.window.Window` with Monaco in YAML, with a YAML/JSON toggle that
  re-renders the same value; Apply first shows a Monaco side-by-side diff (original vs
  edited) as the confirm step, then `PUT ?view=…&mode=replace&digest=…` with `text=` in
  YAML mode or `data=` in JSON mode. Monaco's AMD loader is fetched lazily on first use
  from `/pve2/js/pve-meta-ui/vs/loader.js` (shipped by the pve-meta UI package) and
  every editor and model is disposed when its window closes.

The YAML/JSON toggle is presentation only. The file carries a small YAML dumper and a
parser for the shape the store itself emits (block maps, block sequences at the key's own
indentation, plain and quoted scalars). Anything richer — anchors, aliases, block
scalars, non-empty flow collections, tabs, nulls — throws and the toggle refuses with the
reason rather than mangling the buffer. The server stays the authority on YAML: Apply in
YAML mode sends the text untouched.

## How it is wired

pve-ext's page loader (`pve-ext/js/pve-ext-loader.js`) reads page manifests from
`/usr/share/pve-ext/pages/*.json` through `GET /api2/json/ext/pages`. A manifest declares
either `url` (a same-origin iframe) or `script` + `xtype` (a native panel class). This one
uses the second form:

`pages/pve-meta-extjs.json`:

```json
{
    "id": "pve-meta-extjs",
    "title": "Metadata (ExtJS)",
    "iconCls": "fa fa-tags",
    "targets": ["lxc", "qemu", "dc"],
    "script": "/pve2/js/pve-meta-extjs/pve-meta-tree.js",
    "xtype": "pveMetaTreePanel",
    "requires": { "vms": ["VM.Audit"], "dc": ["Sys.Audit"] }
}
```

The file installs to `/usr/share/pve-manager/js/pve-meta-extjs/pve-meta-tree.js`, which
pveproxy serves at the `script` path above. The manifest and the packaging are another
agent's; this directory owns only the panel.

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
  `eqeqeq`, `no-var`, …) with `Ext`, `PVE`, `Proxmox` and `gettext` as globals — clean.
* `node testing/smoke.js` — offline, no DOM. It loads the real file into `node:vm`
  behind a small `Ext`/`Proxmox` shim and drives the pure parts: the path and scope
  helpers, the YAML codec round trip against the store's own canonical dump and its
  refusals, the document+grammar row merge, owner resolution and per-row editability.
* `testing/headless-tab-check.js` and `testing/headless-flows-check.js` run headless
  Chromium against the real pve-manager SPA (`node headless-tab-check.js <host> <vmid>
  <light|dark> [--readonly] [--operators]`). They need `puppeteer-core` and a chromium
  binary and write their screenshots to `/root/headless/shots`; they were run from the
  lab build host, not from a workstation.

## Screenshots

Taken on `pvemeta-node1` (pve-manager 9.2.11, ExtJS 7.0.0, proxmox-widget-toolkit 5.2.8)
inside the real UI, against the live revision-5 API.

| File | What it shows |
|---|---|
| `extjs-tree-light.png`, `extjs-tree-dark.png` | the tree in both themes: present rows, faded declared-but-unset rows with their defaults, the Owner column |
| `extjs-celledit-light.png`, `extjs-celledit-dark.png` | an inline editor open on the array leaf, with a comment key rendered as the row's tooltip |
| `extjs-enum-light.png` | the combobox editor on a grammar-declared `enum` |
| `extjs-monaco-light.png`, `extjs-monaco-dark.png` | *Edit as Text* on a subtree, YAML |
| `extjs-monaco-json-light.png`, `extjs-monaco-json-dark.png` | the same value after the JSON toggle |
| `extjs-diff-light.png`, `extjs-diff-dark.png` | the Monaco diff shown to confirm Apply |
| `extjs-conflict-light.png`, `extjs-conflict-dark.png` | a 409 reported with the API's message verbatim |
| `extjs-addkey.png` | the Add Key window |
| `extjs-scoped-light.png` | a principal with `VM.Audit` and one `rw` scope: row actions only inside `traefik`, "Scoped write access" in the toolbar |
| `extjs-datacenter.png` | the same panel over `/meta/datacenter` |

Some shots include one extra grammar-declared row (`traefik.spec.scheme`) that came from
a temporary demo registration used to exercise the enum and boolean editors; it was
removed from the lab afterwards.

## Known API friction

`GET …?format=json` returns YAML booleans as `1`/`0` — the file still says `flag: true`,
but the JSON view cannot tell a boolean from the integer 1. Any client is affected, not
just this one. This panel copes by letting a grammar's declared `type: boolean` decide
the editor and by rendering booleans through `Proxmox.Utils.format_boolean`; without a
grammar such a key looks like a number and gets a numberfield.
