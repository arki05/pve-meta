# pve-meta-ui

The v1 editor for guest/datacenter metadata documents. Served natively by pveproxy as a
static bundle installed at `/usr/share/pve-manager/js/pve-meta-ui/`, i.e. reachable at
`https://<node>:8006/pve2/js/pve-meta-ui/index.html` — same origin, same port, and the
same session cookie as the PVE web UI itself. A Rust → wasm single-page app (Yew +
[`pwt`](https://git.proxmox.com/git/ui/proxmox-yew-widget-toolkit.git) +
[`proxmox-yew-comp`](https://git.proxmox.com/git/ui/proxmox-yew-comp.git)), also
embeddable in the PVE web UI in an iframe at that same URL (`?vmid=105`/`?dc=1`,
`?theme=dark|light`).

See `../docs/UI-SPEC.md` for the full design, `../docs/API.md` for the HTTP contract
this UI consumes, and `../docs/NATIVE-API-SPEC.md` for how that API is served (the
`PVE::API2::Meta` Perl module, `/api2/{json,extjs}/meta/...` on port 8006 — there is no
separate `pve-metad` daemon/port in this deployment model; the crate for one is kept in
the tree as an optional component but isn't what this UI talks to by default).

## Build

wasm cannot be built on macOS with this toolchain; build on the Linux host that has
`trunk`/`grass`/`wasm-opt` installed:

```sh
rsync -az --exclude target --exclude .git --exclude dist \
    /path/to/pve-meta/ui/ pve-meta-build:/root/pve-meta/ui/
ssh pve-meta-build 'export PATH=$HOME/.cargo/bin:$PATH; cd /root/pve-meta/ui && trunk build'
```

For a release build (what actually gets installed to
`/usr/share/pve-manager/js/pve-meta-ui/`): `trunk build --release` (the `Trunk.toml` in
this repo already sets `release = true`, so a plain `trunk build` already produces
optimized output). `Trunk.toml`'s `public_url = "/pve2/js/pve-meta-ui/"` matches that
install path, so every asset link (CSS, fonts, the wasm bundle itself) in `dist/`
resolves correctly once installed. Output lands in `dist/`.

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
(`pwt`, `proxmox-yew-comp`, `yew`, `web-sys`, `gloo-net`, ...) are declared as
`[target.'cfg(target_arch = "wasm32")'.dependencies]` in `Cargo.toml`, so a native
`cargo test` never needs to fetch or build them:

```sh
cd ui
cargo test --lib
```

## Dev loop against a real node

```sh
trunk serve --public-url /pve2/js/pve-meta-ui/ \
    --proxy-backend=https://<node>:8006/api2/ --proxy-insecure
```

This serves the UI locally while proxying `/api2/...` requests to a real node's
pveproxy. Matching `--public-url` to the real install path matters here: `crate::auth`'s
same-origin-parent-frame check and the app's own relative `/api2/json/...` calls both
assume the UI is reachable at that path. You still need a valid `PVEAuthCookie` for that
node in your browser (log into the node's PVE web UI first — trunk's dev server can't
set that cookie for you, and this UI has no login form of its own, see "Auth" below);
without one you'll see the "Not logged in" notice.

## Query parameters

| Param | Effect |
|---|---|
| `vmid=<n>` | Select guest `<n>`'s metadata document; hides the left guest list and the header's guest selector (embedded mode). |
| `dc=1` | Select the datacenter document; same embedded-mode effect as `vmid`. `dc=1` wins if both are given. |
| `theme=dark` / `theme=light` | Force the color scheme (persisted to `localStorage`, applied before first paint). Omit to follow the OS/browser preference. |

Without `vmid`/`dc`, the app shows the left guest list (from `GET /meta/inventory`) and
a "select a guest or the datacenter document" placeholder until something is clicked.

## Auth

Reuses whatever `PVEAuthCookie` the browser already holds for the node — no login form.
Since this page is a plain static file (no per-request server-side templating), nothing
hands it a fresh `CSRFPreventionToken` the way a templated PVE page would; `src/auth.rs`
resolves one itself at startup:

1. If embedded in a same-origin parent frame (the PVE web UI's own tab iframe), copy
   `window.parent.PVE.CSRFPreventionToken`.
2. Otherwise, if a `PVEAuthCookie` is present, renew it (`POST /api2/json/access/ticket`
   with the ticket's userid and the ticket itself as the password —
   `proxmox_login::Login::renew_ticket`) to mint a fresh one.

Either way the token is stored both on `window.Proxmox.CSRFPreventionToken` and via
`proxmox_yew_comp::store_csrf_token` (sessionStorage). If there's no usable cookie at
all, the whole app is replaced with a full-page notice linking to
`https://<host>:8006/`. While auth is being resolved (the renewal round trip in case 2)
the app shows a plain "Loading…" screen rather than flashing "not logged in". There is
no handling of a ticket going stale mid-session beyond `proxmox-yew-comp`'s built-in
background refresh loop; if that fails outright (e.g. the PVE session was actually
logged out server-side) API calls will start erroring rather than the app reverting to
the "not logged in" screen — a reload picks that up correctly.

## Talking to the API

`src/api.rs` calls `/api2/json/meta/...` directly via `gloo-net` rather than going
through `proxmox_yew_comp::http_get/http_put/http_post`, for two reasons specific to
the native `PVE::API2::Meta` module (see its doc comment for the full rationale):

- PVE request parameters are form/JSON parameters where **object-valued parameters are
  JSON-encoded strings** — the patch endpoint's `patch` parameter is a JSON string, not
  a nested JSON object (`serde_json::to_string`d before sending); booleans are sent as
  `1`, not `true`.
- Error classification (409/400/404) is based on the actual HTTP status code
  (`Response::status()`), not a body-embedded one — pveproxy's error envelope is
  `{"data": null, "message": "...", "errors": {...}}` with the real status on the
  transport, which `proxmox_client`'s generic envelope parsing doesn't surface the same
  way.

`GET /meta/version` has no long-poll on the native module (it answers immediately); the
app polls it every 5 seconds instead of long-polling.

## Code structure

```
src/lib.rs        crate root: pure modules unconditionally, everything else behind
                  #[cfg(target_arch = "wasm32")]
src/main.rs       wasm bootstrap (theme-from-query, http_setup, render App)
src/model.rs      Document/DocId/Touched, comment-key helpers            (pure, tested)
src/patch.rs      make_patch/apply_patch (RFC 7386 merge patch) + path helpers
                  + describe_patch (for the "Pending" bar)               (pure, tested)
src/schema.rs     JSON-schema subset (docs/UI-SPEC.md's list) -> FieldSpec (pure, tested)
src/auth.rs       CSRF/session bootstrap (see "Auth" above)
src/api.rs        typed wrappers over /api2/json/meta/... (see "Talking to the API")
src/app.rs        App: auth resolution, ?vmid/?dc routing, guest list, version poll
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
(dry-run → unified diff confirm → real write) flow, the version poll banner, same-origin
CSRF-token bootstrap, and `?vmid=`/`?dc=`/`?theme=` handling including embedded mode.

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
- **Apply doesn't trust the write response's shape for round-tripping comments**: after
  any successful write (form Apply, Source Apply, Convert) the editor re-fetches the
  document with `comments=1&raw=1` instead of relying on whatever the write endpoint's
  response happens to include, since `docs/API.md` doesn't pin that down precisely. One
  extra request per write; simpler and more robust than guessing.
- **Missing document (no metadata yet)**: not explicitly specified. Treated a `404` on
  the initial `GET` as "start from an empty document" (so a guest with `has_meta:
  false` is still fully editable — add a namespace, fill it in, Apply creates the file
  per `docs/API.md`) rather than a dead-end error panel. Worth confirming against the
  real API module's actual behavior for a missing document.
- No i18n catalog (per the spec: "Strings via plain `&str`").
- `GET .../subtree` is not called anywhere — no UI flow in the spec needs it (the
  whole document's `data` is already available), so it's not wrapped in `api.rs`.

## Known gaps / not exercised

The `PVE::API2::Meta` Perl module didn't exist yet while this was written (another
agent is implementing it in parallel), so nothing here has been tested against a live
endpoint — only compiled (`cargo check`/`clippy --target wasm32-unknown-unknown`, both
clean) and unit-tested where pure. Things most likely to need a follow-up fix once
there's a real backend to click against: the exact shape of error bodies (`error_text`
et al. assume `{"data": ..., "message": "..."}`, with the real status read from the
transport — see "Talking to the API" — but the precise field names for `message` and
whether `errors` needs surfacing too are worth double-checking against the actual Perl
module), the ticket-renewal flow in `src/auth.rs` (never exercised against a real
`/api2/json/access/ticket` from this code path), and the 404-on-missing-document
assumption above.
