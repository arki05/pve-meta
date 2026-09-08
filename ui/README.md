# pve-meta-ui

The metadata page: **one tree of one document** (`docs/DESIGN.md` §8). Rust → wasm (Yew +
[`pwt`](https://git.proxmox.com/git/ui/proxmox-yew-widget-toolkit.git) +
[`proxmox-yew-comp`](https://git.proxmox.com/git/ui/proxmox-yew-comp.git)), installed as a
static bundle at `/usr/share/pve-manager/js/pve-meta-ui/` and served by pveproxy at
`https://<node>:8006/pve2/js/pve-meta-ui/index.html`. Same origin, same port, same session
cookie as the PVE web interface, which is what lets it run as a tab inside it.

Query parameters, as substituted by `pve-ext-loader.js`:
`?vmid=<id>&type=lxc|qemu&node=<node>&theme=light|dark` or `?dc=1&theme=…`.

## The page

A `DataTable` over a `TreeStore` — the pattern `proxmox_yew_comp::PermissionPanel` uses,
which is the stack's own `DataTable`-over-`TreeStore` page. **Four one-line columns**, at
the density of the ExtJS grid one tab away (24 px rows, measured against it — see
*Looking like PVE* below):

* **Key** — the key, with a folder icon for a map, a list icon for an array and a leaf icon
  for a value, next to the expander.
* **Value** — the scalar, an array as one JSON leaf, nothing for a map. A row that only a
  grammar declares is greyed across every column and reads `not set (default: 80)`.
* **Description** — the note the *document* carries: the sibling comment key (`host__`
  documents `host`, a bare `__` documents the map it is in). Nothing else; a grammar's
  `description` is the column's **tooltip**, never its text, because it is documentation
  about the key rather than data in this document. Comment keys are never rows of their own.
* **Access** — every registration whose scope covers the row, `rw` ones by name and `ro`
  ones muted with `(ro)`; the tooltip is one line per registration with its authid, prefix,
  mode and the selector that made it apply. Several principals may read the same subtree,
  so this is *not* an owner column: it answers who writes this row and who is watching it.

Rows are the union of the keys present and the keys the applicable grammars declare
(`GET /meta/operators`, matched by prefix and by selector against this guest's tags).
Siblings sort **alphabetically** — `data` is unordered on the wire (§4) — unless an object
schema declares an `order` array, whose keys come first in that order.

Above the grid the page adds at most one muted line: the guest's tags and the document's
own `__` note. The PVE tab already names the guest, so the page does not repeat it.

### The toolbar

**Add**, **Edit**, **Remove**, **Edit selection as text**, **Reload**, and at the right end
a `Tree | Text` toggle. Everything acts on the selection and nothing lives in a row:

| Control | Acts on |
|---|---|
| Add | the selected map, else the selected leaf's parent, else the document root |
| Edit | the selected row — also a double click, or Enter |
| Remove | the selected row, confirmed |
| Edit selection as text | the selected row's subtree; disabled without a selection |
| `Tree \| Text` | swaps the panel body in place |

A muted **Scoped write access** / **Read-only** label sits next to the toggle, and only
when the caller is actually restricted.

Editing is per row and goes straight to the server; there is no draft, no Apply button and
nothing to lose on a reload:

| Action | Request |
|---|---|
| Edit / Set a row | `PUT ?view=<path>&mode=replace` with the scalar as `data`, plus the digest |
| its description | a second `PUT` at `<path>__` (or `<path>.__`), against the digest the first returned |
| Add | the same `PUT` at the new path |
| Remove | `DELETE ?view=<path>&digest=…` (query string — pveproxy refuses a body on DELETE) |

A 409 reloads the tree and leaves a standing notice; the server's own message is shown
verbatim underneath. `GET /meta/version` is polled every 5 s and any change reloads the
tree, the grants, the registrations and the guest's tags — **except** while a dialog is
open or the text buffer is dirty, when the page holds still and says the document moved
instead.

Monaco has three jobs (§8), all of them diff-confirmed:

* **Edit selection as text** — a dialog over the selected row's subtree, with a YAML/JSON
  toggle that is presentation only. Applies as `PUT ?view=<path>&text=…`.
* the **Text** half of the `Tree | Text` toggle — the whole document in the panel body,
  with **Apply** (a root `PUT` with the digest) and **Discard**. Leaving it while it holds
  unapplied changes asks first.
* the **diff** that confirms either apply.

The two buffers never coexist: the dialog is only reachable from the tree body. The
version poll holds still while a dialog is open or the text buffer is dirty, and says the
document moved instead; a clean Text body simply re-reads, because there is nothing there
to lose.

### Looking like PVE

The page is a same-origin iframe inside a PVE tab, so it has to read as the same product as
the ExtJS grid next to it (`docs/design/comparison/ct200-extjs-*.jpg`). Everything that
matters is a pwt token or class; `css/pve-meta.scss` carries the seven rules no token can
express, each with its reason in the file — the font retarget, Monaco's host box, the
iframe edge, PVE's selection and hover colours, the 24 px row, tooltips inside a grid cell,
and the monospace Value cell's line box. Measured on the lab node, the ExtJS grid puts
13px/15px text in a 23-24 px row with a 29 px header and a 36 px toolbar; this page is
13px/17px in a 24 px row, with the same 29 px header and 36 px toolbar.

### Why an `EditWindow` per row and not an editor in the cell

Because that is what the Proxmox stack does for every key/value grid it has:
`proxmox_yew_comp::ObjectGrid` — the widget behind the node and datacenter option pages —
selects a row and opens an `EditWindow` on the Edit button, a double click or Space
(`COMP/src/object_grid.rs:314-341,447-470`). pwt's `DataTable` has no editable-cell
support at all: no cell editor, no commit/rollback, no per-cell validation state. An
inline field would be a bespoke widget living outside the design system in the one place
where the system already has an answer. The dialog also has somewhere to put the row's
description, which is a *second key* on the wire and has no room in a cell.

## Layout

| File | Contents |
|---|---|
| `src/model.rs` | `DocId`, grants (`Access::may_write`), comment-key rules. Pure, unit-tested natively. |
| `src/grammar.rs` | `GET /meta/operators`: registrations, selectors, the `PVE::JSONSchema` accessors. Pure. |
| `src/tree.rs` | The row model: present ∪ declared, notes, the Access entries, per-row editability, ordering. Pure. |
| `src/edit.rs` | One row edit → one request: value parsing and the `PUT`/`DELETE` bodies. Pure. |
| `src/request.rs` | Request identity: what an async answer was asked for. Pure. |
| `src/api.rs` | `/api2/json/meta/...` wrappers (`docs/DESIGN.md` §5). |
| `src/auth.rs` | Ticket/CSRF bootstrap: parent-frame token when embedded, ticket renewal else. |
| `src/theme.rs` | `PVEThemeCookie`/`?theme=` → pwt's `ThemeName`/`ThemeMode`. |
| `src/app.rs` | Session gate, routing, the `pwt-content-spacer` page frame. |
| `src/editor.rs` | The page: a `LoadableComponent` with the table, the toolbar and the dialogs. |
| `src/monaco.rs` | `#[wasm_bindgen]` externs for the glue. |
| `js/pve-meta-monaco.js` | Monaco glue: mount, diff, language, theme from `--pwt-*`. |
| `css/pve-meta.scss` | Seven rules, all of them about looking like PVE; each one explains itself. |

## Build

wasm is built on the Linux build host, which has `trunk`, `grass` and `wasm-opt`:

```sh
rsync -az --exclude target --exclude .git --exclude dist --exclude node_modules \
    ui/ pve-meta-build:/root/pve-meta/ui/
ssh pve-meta-build 'export PATH=$HOME/.cargo/bin:$PATH; cd /root/pve-meta/ui && trunk build'
```

Monaco is shipped in the package, never loaded from a CDN. It comes from npm and trunk
copies `node_modules/monaco-editor/min/vs` to `dist/vs`, so the build host needs it once:

```sh
cd /root/pve-meta/ui && npm install
```

`Trunk.toml` sets `release = true` and `public_url = "/pve2/js/pve-meta-ui/"`, so a plain
`trunk build` produces an installable `dist/` (about 18 MB, most of it Monaco).

Checks:

```sh
cargo clippy --target wasm32-unknown-unknown --all-targets -- -D warnings
cargo fmt --check
cargo test --lib          # native: model, grammar, tree, edit, request
```

The five pure modules are the only ones without `#[cfg(target_arch = "wasm32")]`, and
everything browser-shaped is declared under
`[target.'cfg(target_arch = "wasm32")'.dependencies]`, so a native `cargo test` never
fetches or builds pwt, yew or web-sys.

## Install

```sh
rsync -a dist/ root@<node>:/usr/share/pve-manager/js/pve-meta-ui/
ssh root@<node> systemctl restart pveproxy
```

The restart is required, not cosmetic: `PVE::APIServer::AnyEvent::add_dirs` walks the
static tree once at startup, so directories added after it started (`js/`, `vs/`) are
answered with a 500 until pveproxy is restarted.

## Notes

* The theme is PVE's while embedded. `PVEThemeCookie` (`crisp` → light, `proxmox-dark` →
  dark, anything else → follow the OS) is mapped into pwt's `localStorage` keys before the
  first render; the page has no theme switcher of its own. A theme change reaches Monaco
  too (`pwt-theme-changed` → `monaco.editor.setTheme`, re-derived from the `--pwt-*`
  variables once the new stylesheet has applied).
* The pwt theme is **Crisp**, the one written to look like the Proxmox products.
* **Every async answer carries the identity it was asked for** (`src/request.rs`).
  `LoadableComponentMaster` respawns a load per `Msg::Load` and cancels nothing, so a load,
  a write and the text dialog's fetch are each dropped when the page has moved on since
  they went out; a Monaco instance is disposed the moment its dialog stops showing it.
* **Concurrency is the digest.** Every write sends the one the tree was built from, and a
  chained edit (a value plus its note) sends the digest the previous write returned. The
  version token is read *with* the document in `load()`, so a change between the load and
  the first poll tick is a change the poll acts on rather than adopts as its baseline.
* Editability is per row, from `GET /meta/access?vmid=<id>` / `?dc=1`, whose `write` half
  is separate from `read`: an auditor gets a readable tree with Add/Edit/Remove disabled
  and a "Read-only" label, not a 403 after the fact. If the call fails, the page stays
  read-only and says why rather than not loading at all. A root replace — the Text body's
  Apply — needs the full `write` grant; no scope, however wide, grants it (§3).
* Scopes apply to guest documents only (§3), so the datacenter document has no declared
  rows and no Access entries.
* A row's *note* and its *description* are deliberately different things: the note is what
  the document carries, the description is the note or — failing that — the grammar's own
  prose. The edit dialog pre-fills from the note, so saving a row never copies a grammar's
  documentation into the document.
* If the stored file does not parse, the page shows the parse error and offers "Edit as
  text" on the root, which is the only repair the API allows (§4).
* `GET /meta/operators` is revision 5. On an older node the page logs it, shows a muted
  note under the tree, and renders the document without declared rows or Access entries.

## Screenshots

`docs/screenshots/`, all from the lab node, each in light and dark:

| File | What it shows |
|---|---|
| `tree-{light,dark}.png` | the four columns, a selected row, and a declared-but-unset row greyed with its default |
| `text-{light,dark}.png` | the Text half of the `Tree \| Text` toggle |
| `selection-dialog-{light,dark}.png` | "Edit selection as text" on one subtree |
| `access-tooltip-{light,dark}.png` | the Access column's tooltip, with the selector |
| `embedded-{light,dark}.png` | the page inside the real PVE Metadata tab, next to the ExtJS one |

Regenerate with `testing/meta-ui-shots.js`.
