# pve-meta-ui — implementation specification

A standalone single-page app (Rust → wasm, Yew + Proxmox's `pwt` widget toolkit +
`proxmox-yew-comp`) served by `pve-metad` at `https://<node>:8007/ui/`. It is the v1
editor for guest metadata documents; later it is embedded in the PVE web UI in an
iframe, so it must work with only a `?vmid=105` (or `?dc=1`) and `?theme=dark|light`
query string and the PVE auth cookie.

Skeleton already in place: `ui/` (Cargo workspace of its own, `Trunk.toml`, `index.html`,
vendored `pwt-assets`, `src/main.rs` with theme-from-query, cookie auth, header). It
builds on the Linux build host: `ssh pve-meta-build`, repo at `/root/pve-meta`,
`export PATH=$HOME/.cargo/bin:$PATH; cd /root/pve-meta/ui && trunk build` (≈90 s
incremental). Sync from the Mac with
`rsync -az --exclude target --exclude .git --exclude dist /Users/arki/Documents/proxmox/pve-meta/ui/ pve-meta-build:/root/pve-meta/ui/`.
wasm cannot be built on the Mac. Only the build host has `trunk`, `grass`, `wasm-opt`.

Reference (read first, every API you use must exist there):
* `/private/tmp/claude-501/-Users-arki-Documents-proxmox/afff609e-fb1e-4d06-8a92-09f2b57a5c90/scratchpad/research-pwt-ui.md` — widget/form/http-client/theme guide with file:line citations.
* Upstream sources: `.../scratchpad/upstream/proxmox-yew-widget-toolkit/src` (pwt), `.../proxmox-yew-comp/src`, `.../proxmox-datacenter-manager/ui/src` (worked example app).
* API contract: `docs/API.md`. The daemon serves both `/api2/json` and `/api2/extjs`; `proxmox_yew_comp::http_get/http_put/http_post/http_delete` use the extjs prefix, which is fine.

## Auth and CSRF

`pve-metad` injects `window.Proxmox = { Setup: { auth_cookie_name: 'PVEAuthCookie' }, UserName: "...", CSRFPreventionToken: "..." }` into `index.html` when the request carries a valid ticket cookie. `authentication_from_cookie(&ExistingProduct::PVE)` + `http_set_auth` (already in the skeleton) then work unchanged. If `UserName` is empty / no cookie: render a full-page notice "Not logged in to Proxmox VE — log in to the PVE web UI on this host first, then reload" with a link to `https://<host>:8006/`. No login form of our own in v1.

## Layout

```
┌──────────────────────────────────────────────────────────────────────┐
│ pve-meta   [guest selector ▾]  105 wiki (lxc, node1)      user  ◐    │  header (Row)
├───────────────┬──────────────────────────────────────────────────────┤
│ Guests        │  [Form] [Source]                     ● changed on    │
│ ─────────     │                                        server ↻      │
│ datacenter    │  ▸ traefik      traefik operator · rw   [remove]     │
│ 105 wiki      │    spec                                              │
│ 106 media     │      host  [wiki.arkenberg.eu   ]  public name…      │
│ 200 test-ct   │      port  [ 8080 ]                                  │
│ …             │  ▸ netbird      netbird-sync · ro                    │
│               │  [+ add namespace]                                   │
│               │──────────────────────────────────────────────────────│
│               │  Pending: traefik.spec.host (set)  [Discard] [Apply] │
└───────────────┴──────────────────────────────────────────────────────┘
```

* **Header**: app title, current document label (`105 wiki (lxc, pvemeta-node1)` or `Datacenter`), user name, `ThemeModeSelector`.
* **Left column** (`pwt::widget::data_table::DataTable` over a `Store`, or a simple `List`): "Datacenter" entry + every guest from `GET /meta/inventory` (vmid, name, type, node, a dot marker when `has_meta`). Selecting loads the document. Hidden when the page was opened with `?vmid=` / `?dc=1` (embedded mode) — then only the header + editor for that document are shown.
* **Editor area**: `pwt::widget::TabPanel` (or two `Button`s toggling) with two views sharing one loaded `Document { id, format, digest, data (comments kept: fetch with comments=1), raw }`.

### Form view

* One `pwt::widget::Panel` per top-level namespace (non-comment key), title = namespace, right-aligned tools = owner text from the registry (`GET /meta/registry`: first operator whose claim prefix matches, show `name · rw|ro`; else "unclaimed") and a `remove namespace` button. Panels are collapsible (own state: clicking the title toggles the body).
* Inside a panel, the subtree is rendered recursively:
  * object → nested section with a small label header (indented `Column`), collapsible;
  * string/number/bool leaf → one row: label = key, then `Field` (`InputType::Text`), `Number::<f64>` (keep integers when the original was an integer: format without `.0` and parse back to integer if the text has no `.`/`e`), or `Checkbox`; right of it the comment-key text (`key__`) as muted help text, editable via a tiny "note" button that turns it into a `Field`;
  * array of scalars → a `Field` with comma-separated values (v1); array of objects → shown read-only as JSON text with an "edit in Source" hint;
  * a `delete` icon-button per row; an `+ add key` button at the end of each object section (asks key name + type via a small `Dialog`).
* If `GET /meta/schemas/{vmid}` returns a JSON schema for the namespace prefix, use it to: order/label properties (`title`, `description` → help text), pick widgets (`enum` → `Combobox`, `type: boolean` → Checkbox, `integer`/`number` → Number with `minimum`/`maximum`), mark `required`, and offer missing properties in `+ add key`. Schema support is a JSON-schema *subset*: `type`, `properties`, `required`, `enum`, `description`, `title`, `minimum`, `maximum`, `items` (for scalars), `default`. Unknown constructs are ignored.
* Edits do not touch the server. They accumulate in a local `Value` (a working copy) and the pending list is `pve_meta`-style merge patch computed client-side: implement `make_patch(old, new)` (same semantics as the core crate: removed keys → `null`, changed leaves → value, arrays atomic) in `ui/src/patch.rs` with unit tests (`cargo test` runs natively on the Mac for this module — keep it free of web-sys).
* **Apply** → `PUT /meta/guests/{vmid}` `{ patch, digest }` (or `/meta/datacenter`). On success replace the loaded document with the response. On HTTP 409 show a `Dialog`: "The document changed on the server. Reload and re-apply your changes?" with buttons Reload (re-fetch, re-apply the pending patch on top locally, keep pending) / Cancel. On 400 show the error text.
* **Discard** resets the working copy.

### Source view

* `TextArea` (pwt `form::TextArea`, class `pve-meta-source`, monospace, full height) with the raw file text; a format `Combobox` (yaml/toml/json) with a **Convert** button → `POST .../convert {format, digest}` after a confirm dialog ("comments in the file are lost, comment keys survive"); a **Verify** button → `PUT .../raw {content, digest, dry_run: true}` and shows either the touched-path list or the error; **Apply** → dry run first, then a `Dialog` with a unified diff (compute with the `similar` crate, render lines with `+`/`-` classes `pwt-color-success`/`pwt-color-error` in a `<pre>`) and the touched paths; confirm → real `PUT .../raw {content, digest}`.
* Switching to the Source view while form changes are pending shows the form's pending patch applied as text? No — v1 rule: the two views are exclusive; switching views with pending changes asks to apply or discard first.

### Change detection

Poll `GET /meta/version?wait=25&since=<token>` in a loop (long-poll); on a new token re-fetch the current document's digest (`GET /meta/guests/{vmid}` without raw) and, if it differs from the loaded one, show a banner "changed on server — Reload" (never auto-reload while there are pending edits; auto-reload when there are none). Also refresh the guest list when the token changes.

### Embedded mode / theme

`?vmid=105` or `?dc=1` selects the document and hides the left column and the guest selector. `?theme=dark|light` is already handled. Keep the header in embedded mode but drop the user name (the parent page shows it).

## Code structure

```
ui/src/main.rs        bootstrap (exists), App component: routing of ?vmid/?dc, layout
ui/src/api.rs         typed wrappers over proxmox_yew_comp::http_*: inventory(), registry(), schemas(id), get(id, comments, raw), patch(id, patch, digest, dry_run), put_raw(...), convert(...), version(wait, since); DocId enum { Guest(u32), Datacenter } with url helpers
ui/src/model.rs       Document struct, DocId, Touched, helpers to strip/collect comment keys
ui/src/patch.rs       make_patch / apply_patch (pure, unit-tested)
ui/src/schema.rs      JSON-schema subset parsing → FieldSpec
ui/src/form/…         namespace panels, value rows, add-key dialog
ui/src/source.rs      source view, diff dialog
ui/src/guests.rs      left list
```

Keep components small; prefer `pwt` builders over raw `html!` where a widget exists. Strings via plain `&str` (no i18n catalogs). No `unwrap()` on JS results — log with `log::error!`.

## Definition of done

* `trunk build` succeeds on the build host without warnings from our crate (`cargo clippy --target wasm32-unknown-unknown` clean is a bonus).
* `cargo test` for the pure modules (`patch.rs`, `schema.rs`, `model.rs`) passes on the Mac (`cd ui && cargo test --lib` — make the crate a lib + bin, or gate wasm-only modules with `#[cfg(target_arch = "wasm32")]` so native tests compile).
* A `ui/README.md` describing build, dev loop (`trunk serve --proxy-backend=https://<node>:8007/api2/ --proxy-insecure`), and the query parameters.
* Report: what is implemented, what is stubbed, screenshots are not required (the main session tests in a browser).
