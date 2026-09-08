# pve-meta-ui

The metadata editor page: one document (`docs/DESIGN.md` §6), a "View as" prefix
selector, and a Monaco YAML editor. Rust → wasm (Yew +
[`pwt`](https://git.proxmox.com/git/ui/proxmox-yew-widget-toolkit.git) +
[`proxmox-yew-comp`](https://git.proxmox.com/git/ui/proxmox-yew-comp.git)), installed as
a static bundle at `/usr/share/pve-manager/js/pve-meta-ui/` and served by pveproxy at
`https://<node>:8006/pve2/js/pve-meta-ui/index.html`. Same origin, same port, same
session cookie as the PVE web interface, which is what lets it run as a tab inside it.

Query parameters, as substituted by `pve-ext-loader.js`:
`?vmid=<id>&type=lxc|qemu&node=<node>&theme=light|dark` or `?dc=1&theme=…`.

## Layout

| File | Contents |
|---|---|
| `src/model.rs` | `DocId`, grants, views (key-path prefixes). Pure, unit-tested natively. |
| `src/request.rs` | Request identity: what an async answer was asked for. Pure, unit-tested natively. |
| `src/api.rs` | `/api2/json/meta/...` wrappers (`docs/DESIGN.md` §3). |
| `src/auth.rs` | Ticket/CSRF bootstrap: parent-frame token when embedded, ticket renewal else. |
| `src/theme.rs` | `PVEThemeCookie`/`?theme=` → pwt's `ThemeName`/`ThemeMode`. |
| `src/app.rs` | Session gate, routing, the `pwt-content-spacer` page frame. |
| `src/editor.rs` | The page: a `LoadableComponent` modelled on `proxmox_yew_comp::NotesView`. |
| `src/monaco.rs` | `#[wasm_bindgen]` externs for the glue. |
| `js/pve-meta-monaco.js` | Monaco glue: mount, diff, theme from `--pwt-*`. |
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
`trunk build` produces an installable `dist/` (about 19 MB, most of it Monaco).

Checks:

```sh
cargo clippy --target wasm32-unknown-unknown -- -D warnings
cargo fmt --check
cargo test --lib          # native: src/model.rs, src/request.rs
```

`model.rs` and `request.rs` are the only modules without `#[cfg(target_arch = "wasm32")]`,
and everything browser-shaped is declared under
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
  dark, anything else → follow the OS) is mapped into pwt's `localStorage` keys before
  the first render; the page has no theme switcher of its own. A theme change reaches
  Monaco too (`pwt-theme-changed` → `monaco.editor.setTheme`, re-derived from the
  `--pwt-*` variables once the new stylesheet has applied).
* The pwt theme is **Crisp**, the one written to look like the Proxmox products.
* Concurrency is the digest. `GET /meta/version` is polled every 5 s; when its token
  moves, the document's digest is re-read. A clean editor reloads silently, a dirty one
  raises the "changed on the server" notice with a Reload button.
* **Unapplied edits are never thrown away without asking.** Reload (toolbar and banner)
  and a "View as" switch raise the same confirmation the Discard button uses; declining
  leaves the draft — and the selector — exactly where they were.
* **Every async answer carries the identity it was asked for** — `(document, view,
  generation)`, `src/request.rs`. `LoadableComponentMaster` respawns a load per
  `Msg::Load` and cancels nothing, so a load, a write, the diff and the poll's digest
  read are all dropped when the page has moved on since they went out; a Monaco instance
  is disposed the moment the page stops showing what it was mounted for.
* Read-only views disable both the editor and Apply. The grant comes from
  `GET /meta/access?vmid=<id>` / `?dc=1`, whose `write` half is separate from `read`
  (`docs/DESIGN.md` §8): an auditor gets a readable document, not an editable buffer and
  a 403. If that call fails, the page stays read-only and says so, rather than not
  loading at all.
* The "View as" list comes from the document read's ordered `keys` array (the YAML text
  is the fallback). `format=json`'s `data` object is unordered on the wire, so the page
  never asks for it.

## Screenshots

`docs/screenshots/`: `embedded-{light,dark}.png` (inside the PVE tab),
`standalone-{light,dark}.png`, `view-traefik.png`, `read-only.png`,
`diff-dialog.png`, `after-apply.png`, `conflict-banner.png`, `stale-banner.png`,
`after-conflict-reload.png`, and the two confirmations that guard a draft,
`reload-confirm.png` and `switch-confirm.png`.

- If the selected view stops being one of your options (key deleted, scope revoked), the editor falls back to the whole document: silently when the buffer is clean, otherwise after the same confirmation Discard/Reload use (`model::view_outcome` is the source of truth). Access grants are re-read on Reload and on every store version change.
