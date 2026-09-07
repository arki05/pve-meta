# pve-meta-ui

The v1 editor for guest/datacenter metadata documents, served by `pve-metad` at
`https://<node>:8007/ui/`. A standalone Rust → wasm single-page app (Yew +
[`pwt`](https://git.proxmox.com/git/ui/proxmox-yew-widget-toolkit.git) +
[`proxmox-yew-comp`](https://git.proxmox.com/git/ui/proxmox-yew-comp.git)), also
embeddable in the PVE web UI in an iframe (`?vmid=105`/`?dc=1`, `?theme=dark|light`).

See `../docs/UI-SPEC.md` for the full design and `../docs/API.md` for the HTTP contract
this UI consumes.

## Build

wasm cannot be built on macOS with this toolchain; build on the Linux host that has
`trunk`/`grass`/`wasm-opt` installed:

```sh
rsync -az --exclude target --exclude .git --exclude dist \
    /path/to/pve-meta/ui/ pve-meta-build:/root/pve-meta/ui/
ssh pve-meta-build 'export PATH=$HOME/.cargo/bin:$PATH; cd /root/pve-meta/ui && trunk build'
```

For a release build (what `pve-metad` would actually serve): `trunk build --release`
(the `Trunk.toml` in this repo already sets `release = true`, so a plain `trunk build`
already produces optimized output). Output lands in `dist/`.

Faster iteration on the build host (skips the wasm-bindgen/wasm-opt/grass steps, just
type-checks):

```sh
cargo check --target wasm32-unknown-unknown
cargo clippy --target wasm32-unknown-unknown -- -D warnings
```

### Native tests

`model.rs`, `patch.rs` and `schema.rs` are pure (no `web-sys`/`wasm-bindgen`) and are
unit-tested on the Mac directly — every other module is behind
`#[cfg(target_arch = "wasm32")]` in `src/lib.rs`, and the crates they depend on
(`pwt`, `proxmox-yew-comp`, `yew`, `web-sys`, ...) are declared as
`[target.'cfg(target_arch = "wasm32")'.dependencies]` in `Cargo.toml`, so a native
`cargo test` never needs to fetch or build them:

```sh
cd ui
cargo test --lib
```

## Dev loop against a real node

```sh
trunk serve --proxy-backend=https://<node>:8007/api2/ --proxy-insecure
```

This proxies `/api2/...` requests to a real `pve-metad`. You still need a valid
`PVEAuthCookie` for that node in your browser (log into the node's PVE web UI on
`:8006` first — trunk's dev server can't set that cookie for you, and this UI has no
login form of its own, see "Auth" below) for anything past a read of publicly-cacheable
data to work; without one you'll see the "Not logged in" notice.

## Query parameters

| Param | Effect |
|---|---|
| `vmid=<n>` | Select guest `<n>`'s metadata document; hides the left guest list and the header's guest selector (embedded mode). |
| `dc=1` | Select the datacenter document; same embedded-mode effect as `vmid`. `dc=1` wins if both are given. |
| `theme=dark` / `theme=light` | Force the color scheme (persisted to `localStorage`, applied before first paint). Omit to follow the OS/browser preference. |

Without `vmid`/`dc`, the app shows the left guest list (from `GET /meta/inventory`) and
a "select a guest or the datacenter document" placeholder until something is clicked.

## Auth

Reuses whatever `PVEAuthCookie` the browser already holds (via
`proxmox_yew_comp::authentication_from_cookie` + `http_set_auth`) — no login form. If
there's no usable cookie, the whole app is replaced with a full-page notice linking to
`https://<host>:8006/`. There is no handling of a ticket going stale mid-session beyond
`proxmox-yew-comp`'s built-in background refresh loop; if that fails outright (e.g. the
PVE session was actually logged out server-side) API calls will start erroring rather
than the app reverting to the "not logged in" screen — a reload picks that up correctly.

## Code structure

```
src/lib.rs        crate root: pure modules unconditionally, everything else behind
                  #[cfg(target_arch = "wasm32")]
src/main.rs       wasm bootstrap (theme-from-query, http_setup, render App)
src/model.rs      Document/DocId/Touched, comment-key helpers            (pure, tested)
src/patch.rs      make_patch/apply_patch (RFC 7386 merge patch) + path helpers
                  + describe_patch (for the "Pending" bar)               (pure, tested)
src/schema.rs     JSON-schema subset (docs/UI-SPEC.md's list) -> FieldSpec (pure, tested)
src/api.rs        typed wrappers over proxmox_yew_comp::http_* for docs/API.md
src/app.rs        App: login check, ?vmid/?dc routing, guest list, version long-poll
src/guests.rs     left column (guest list)
src/editor.rs     Editor: per-document state machine (load, working copy, pending
                  patch, Apply/Discard, 409-conflict dialog, Form/Source toggle,
                  version-poll banner)
src/form/         Form view: mod.rs (namespaces + add-namespace), section.rs (recursive
                  object/array/leaf rendering, the comment "note" toggle),
                  add_key.rs (the "+ add key" dialog)
src/source.rs     Source view (raw text + format converter + verify + diff-confirm apply)
```

## What's implemented

Everything in `docs/UI-SPEC.md`: header with theme/user, left guest list (a plain
clickable list rather than `DataTable`, per the spec's "or a simple List" option),
Form/Source view toggle (two buttons rather than `TabPanel`, also spec-sanctioned, so
that switching views could be intercepted to ask about pending changes), recursive
namespace panels with schema-aware widgets (`enum` → combobox, `boolean` → checkbox,
`integer`/`number` → number field with min/max, required marking, missing-property
picker in "+ add key"), comment-key ("note") editing, scalar-array comma fields,
read-only display for arrays of objects, per-row delete and per-namespace remove,
client-side merge-patch computation with a "Pending" bar, Apply/Discard, the 409
conflict-reload dialog, the Source view's Convert (with confirm)/Verify (dry-run)/Apply
(dry-run → unified diff confirm → real write) flow, the version long-poll banner, and
`?vmid=`/`?dc=`/`?theme=` handling including embedded mode.

## Simplified vs. the letter of the spec

- **Header guest-selector dropdown**: skipped. The left column already covers
  selection when not embedded; a second, duplicate picker in the header seemed like
  pure redundancy for a v1 tool, not an extra capability.
- **Switching views with pending changes**: the confirm dialog only offers
  Discard-and-switch or Cancel, not an inline "Apply, then switch" shortcut (which
  would mean threading the Source view's multi-step dry-run/diff-confirm flow through
  a second dialog). Use the view's own Apply button first, then switch.
- **Integer vs. float round-tripping**: `pwt::widget::form::Number<f64>`'s `on_change`
  only hands back the parsed `f64`, not the raw text the user typed, so "no `.`/`e` in
  the text" isn't observable after the fact. Approximated instead: a value with no
  fractional part is stored as an integer, one with a fractional part as a float. A
  deliberately-typed `8080.0` round-trips as the integer `8080`.
- **Not-logged-in detection**: uses the same cookie/ticket check that already gates
  `http_set_auth` (`authentication_from_cookie`) rather than separately reading
  `window.Proxmox.UserName`. Both reflect the same underlying session state; this
  avoids a second, redundant bit of raw JS interop.
- **Apply doesn't trust the write response's shape for round-tripping comments**: after
  any successful write (form Apply, Source Apply, Convert) the editor re-fetches the
  document with `comments=1&raw=1` instead of relying on whatever the write endpoint's
  response happens to include, since `docs/API.md` doesn't pin that down precisely. One
  extra request per write; simpler and more robust than guessing.
- **Missing document (no metadata yet)**: not explicitly specified. Treated a `404` on
  the initial `GET` as "start from an empty document" (so a guest with `has_meta:
  false` is still fully editable — add a namespace, fill it in, Apply creates the file
  per `docs/API.md`) rather than a dead-end error panel. Worth confirming against the
  real daemon's actual behavior for a missing document.
- No i18n catalog (per the spec: "Strings via plain `&str`").
- `GET .../subtree` is not called anywhere — no UI flow in the spec needs it (the
  whole document's `data` is already available), so it's not wrapped in `api.rs`.

## Known gaps / not exercised

The daemon didn't exist yet while this was written, so nothing here has been tested
against a live `pve-metad` — only compiled (`cargo check`/`clippy --target
wasm32-unknown-unknown`, both clean) and unit-tested where pure. Things most likely to
need a follow-up fix once there's a real backend to click against: the exact shape of
error bodies (`error_text`/`is_conflict`/`is_bad_request`/`is_not_found` all assume the
daemon's JSON error envelope carries a `status` field the way PVE's own API does, per
`proxmox_client::RawApiResponse`), and the 404-on-missing-document assumption above.
