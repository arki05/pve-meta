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
which is the stack's own `DataTable`-over-`TreeStore` page. Three columns:

* **Key** — the key, with its *description* underneath. A description is the sibling
  comment key (`host__` documents `host`, a bare `__` documents the map it is in), or, when
  the document carries no note, the `description` a grammar declares. Comment keys are
  never rows of their own.
* **Value** — the scalar, an array as one JSON leaf, nothing for a map. A row that only a
  grammar declares is greyed and shows the declared default plus a **Set** action.
* **Owner** — the registration whose scope covers the row, with the selector that made it
  apply (`traefik (tag: traefik)`); the tooltip adds the authid and the scope's mode.

Rows are the union of the keys present and the keys the applicable grammars declare
(`GET /meta/operators`, matched by prefix and by selector against this guest's tags).
Siblings sort **alphabetically** — `data` is unordered on the wire (§4) — unless an object
schema declares an `order` array, whose keys come first in that order.

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
open, when the page holds still and says the document moved instead.

Monaco keeps exactly two jobs (§8): **"Edit as text"** for the selected subtree (the whole
document when nothing is selected) with a YAML/JSON toggle that is presentation only, and
the **diff** that confirms applying it as `PUT ?view=<path>&text=…`.

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
| `src/tree.rs` | The row model: present ∪ declared, descriptions, owners, per-row editability, ordering. Pure. |
| `src/edit.rs` | One row edit → one request: value parsing and the `PUT`/`DELETE` bodies. Pure. |
| `src/request.rs` | Request identity: what an async answer was asked for. Pure. |
| `src/api.rs` | `/api2/json/meta/...` wrappers (`docs/DESIGN.md` §5). |
| `src/auth.rs` | Ticket/CSRF bootstrap: parent-frame token when embedded, ticket renewal else. |
| `src/theme.rs` | `PVEThemeCookie`/`?theme=` → pwt's `ThemeName`/`ThemeMode`. |
| `src/app.rs` | Session gate, routing, the `pwt-content-spacer` page frame. |
| `src/editor.rs` | The page: a `LoadableComponent` with the table, the toolbar and the dialogs. |
| `src/monaco.rs` | `#[wasm_bindgen]` externs for the glue. |
| `js/pve-meta-monaco.js` | Monaco glue: mount, diff, language, theme from `--pwt-*`. |
| `css/pve-meta.scss` | Two rules: the font retarget and Monaco's host box. |

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
  read-only and says why rather than not loading at all.
* Scopes apply to guest documents only (§3), so the datacenter document has no declared
  rows and no owners.
* A row's *note* and its *description* are deliberately different things: the note is what
  the document carries, the description is the note or — failing that — the grammar's own
  prose. The edit dialog pre-fills from the note, so saving a row never copies a grammar's
  documentation into the document.
* If the stored file does not parse, the page shows the parse error and offers "Edit as
  text" on the root, which is the only repair the API allows (§4).
* `GET /meta/operators` is revision 5. On an older node the page logs it, shows a muted
  note under the tree, and renders the document without declared rows or owners.

## Screenshots

`docs/screenshots/`, all from the lab node: `embedded-{light,dark}.png` (inside the PVE
tab), `standalone-{light,dark}.png`, `declared-rows.png` (a grammar-declared row, greyed,
with its default and the Set action), `row-edit.png`, `add-row.png`, `edit-as-text.png`,
`edit-as-text-json.png`, `diff-dialog.png`, `conflict-notice.png` (a 409: the banner, the
reloaded tree and the server's message), `read-only.png` (an auditor) and
`datacenter.png`. Regenerate with `testing/meta-ui-shots.js`.
