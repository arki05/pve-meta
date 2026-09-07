# pve-metad / pve-meta CLI — implementation specification

Two binaries plus one library crate, all in the `pve-meta` Cargo workspace, built on the
`pve-meta-core` crate (see `crates/pve-meta-core/SPEC.md`) and the upstream Proxmox Rust
crates (git dependencies, pinned revisions, see the `[patch.crates-io]` table in
`ui/Cargo.toml` — copy the same pins into the root workspace).

Reference material (read before coding — every API you use must exist there):
* `/private/tmp/claude-501/-Users-arki-Documents-proxmox/afff609e-fb1e-4d06-8a92-09f2b57a5c90/scratchpad/research-rest-server.md` — how to build a daemon on `proxmox-rest-server`, `proxmox-router`, `proxmox-schema`, `proxmox-daemon`; skeleton main.rs; CLI pattern; gotchas.
* Upstream sources: `/private/tmp/claude-501/-Users-arki-Documents-proxmox/afff609e-fb1e-4d06-8a92-09f2b57a5c90/scratchpad/upstream/proxmox/` (crates), `.../proxmox-datacenter-manager/` (worked example), `.../proxmox-rest-server/examples/minimal-rest-server.rs`.
* API contract: `docs/API.md` (this repo).

## Crates

```
crates/pve-meta-core   (exists) document model / formats / patch engine / file store
crates/pve-meta-api    library: #[api] handlers + Router + auth + shared types
crates/pve-metad       bin: HTTPS daemon (systemd, Type=notify)
crates/pve-meta-cli    bin `pve-meta`: CLI running the same handlers in-process
```

Rust edition 2021 for our crates (upstream crates are edition 2024; the build toolchain is
rustc 1.98 on the build host, 1.93 locally — both fine). Root `Cargo.toml` gets
`[patch.crates-io]` entries for every `proxmox-*`/`pve-api-types`/`pbs-api-types` crate
pulled in, all pinned to rev `7c2efa386352388c8a55902c0bb84741b8aed860` of
`https://git.proxmox.com/git/proxmox.git`. Use `tokio = { version = "1", features = ["full"] }`.

**Important: this workspace is built and tested on Linux (the build container). Locally on
macOS only `pve-meta-core` compiles; `pve-meta-api`/`pve-metad`/`pve-meta-cli` may fail to
compile on macOS because of `proxmox-sys`/`nix` — that is expected. Develop with
`cargo check -p ...` on the Linux build host (`ssh pve-meta-build`, repo synced to
`/root/pve-meta`, `export PATH=$HOME/.cargo/bin:$PATH`), sync with
`rsync -az --exclude target --exclude .git --exclude dist /Users/arki/Documents/proxmox/pve-meta/ pve-meta-build:/root/pve-meta/`.**

## pve-meta-api

### Auth (`auth.rs`)

* `pub struct PveAuth { keyring: Vec<openssl PKey<Public>> + mtimes, www_key_secret: Vec<u8> }` loaded from `/etc/pve/authkey.pub`, `/etc/pve/authkey.pub.old`, `/etc/pve/pve-www.key`. Reload lazily when file mtime changes (check at most once per 10 s).
* **PVE tickets are RSA-SHA1 signatures** (verified against a real ticket), so do not use `proxmox_auth_api::Ticket::verify` (SHA-256). Implement `verify_ticket(&self, ticket: &str) -> Result<Userid>`:
  wire format `PVE:<data>:<HEX8 time>::<base64 sig>` (data = username, `:` uri-escaped as `%3A`); the signed message is `PVE:<data>:<HEX8>` (no aad); verify with `openssl::sign::Verifier::new(MessageDigest::sha1(), key)` against each public key; age = now − hex(time); accept if `-300 < age < 7200` (`ticket_lifetime` 2h, grace 5min — private network: allow a configurable `ticket_lifetime` env override, default 7200). Return the parsed `Userid`. Unit test with the fixture ticket + public key stored in `crates/pve-meta-api/tests/fixtures/` (ticket `PVE:metatest@pve:6A9EF47D::YP61VNCHyWmBaghc/PGb94IVfUZ/d6fQPlYYauZkBlaYUYgF8IajcoTZSDWAXVdHyY7hVDNhWVtG2k7k3kUC/Ni4uCUS63JW6Q2B/hoGv6Ccepv9tNt8spJ5YE2/pX+o/EtuGnrdVX2bU+Hyfctwp2UH3dn+3Q+xexNhmNRYbVIvQolXILrXQXeKvu/IsqhG6xYf8+YvqZEyem1Bqf4CfxqOyE971jX1e7KzK4I+WfvjAyM/4bNT89fpt7BF0yn0fip5qXStu24O/vlkGllCaW9wsP1KpnmWUrNOGU4ngVQCzoAEG3LSworQWz+VlVvr4b+9ZfWXyBfdWLK07euD/A==`, public key in `/private/tmp/claude-501/-Users-arki-Documents-proxmox/afff609e-fb1e-4d06-8a92-09f2b57a5c90/scratchpad/pub.pem` — copy it into the fixtures dir; the test must pass a fixed `now` so the age check succeeds: time 0x6A9EF47D).
* CSRF: PVE's token is `<HEX8 time>:<base64 hmac_sha256(secret, "<HEX8>:<username>")>` where `secret = base64(hmac_sha256(key=<contents of /etc/pve/pve-www.key>, data=""))`... **careful**: Perl `Digest::SHA::hmac_sha256_base64($input)` with ONE argument means data=`$input`, key=`""` (empty). So `secret = base64_nopad(hmac_sha256(key = "", data = www_key_file_contents))`, then `token = "<HEX8>:" + base64_nopad(hmac_sha256(key = secret, data = "<HEX8>:<username>"))`. Perl's `_base64` variants omit padding. Implement `assemble_csrf(&self, username) -> String` and `verify_csrf(&self, username, token) -> bool` (age window `-300 < age < 7200`). Non-GET requests authenticated by cookie **must** carry a valid `CSRFPreventionToken` header; token-authenticated requests need none.
* API tokens: `Authorization: PVEAPIToken=<user@realm!tokenid>=<secret>` (split on the **last** `=`), parsed into `Authid`; **not verified** in v1 (trusted lab) but the token id is the auth id. Also accept `Authorization: PVEAuthCookie=<ticket>` like pveproxy does.
* Cookie names: `__Host-PVEAuthCookie` first, then `PVEAuthCookie`. Iterate all `Cookie` headers.
* `check_auth` closure for `ApiConfig::auth_handler_func` returns `(auth_id_string, Box<AllowAll>)` where `AllowAll: UserInformation` grants everything (no per-path authz in v1). `Permission::Anybody` on every endpoint except `/meta/health` and `/meta/version` which are `Permission::World`? No — keep everything authenticated except `health` (World) so monitoring works without a ticket.

### Store access

A global `OnceLock<MetaStore>` initialised from `PVE_META_ROOT` env (default `/etc/pve/meta`) and the vmlist path `PVE_META_VMLIST` (default `/etc/pve/.vmlist`). All handlers are synchronous `fn` (core is sync; files are tiny) except the long-poll `version` handler which is `async fn` and sleeps in 500 ms steps re-checking `store.version()` until the token differs or the deadline passes.

### Endpoints (`api/*.rs`)

Implement exactly `docs/API.md`. Router tree under `/api2/json/meta/...` (register with `default_api2_handler`, and remember the `json`/`extjs` format segment is consumed by rest-server). Rules:
* **No PATCH verb exists in proxmox-router** — the document patch endpoint is `PUT /meta/guests/{vmid}` with body `{ patch, digest? }` (as in API.md). Use `#[api]` with `input: { properties: { vmid: { type: Integer, minimum: 100, maximum: 999999999 }, patch: { type: Object, additional_properties: true, description }, digest: { type: String, optional: true } } }`. For arbitrary JSON objects use `serde_json::Value` params with an `ObjectSchema` allowing additional properties — check how `proxmox-schema` expresses `additional_properties` (look at upstream usage; `Schema::AnyObject`/`ObjectSchema::additional_properties(true)`).
* Path parameters: `Router::match_all("vmid", ...)` and for the subtree endpoint `match_all("path", ...)` — a path with slashes: rest-server splits components, so `/meta/guests/105/traefik/spec` has two extra components. Simplest: give the item router a `subdirs` for the fixed children (`raw`, `convert`, `snapshots`, `snapshot`, `rollback`, `clone`) and a final `match_all("path", ...)` for the first subtree segment, and accept the remaining components via... proxmox-router cannot match variable depth. **Decision: the subtree endpoint takes the path as a query parameter instead**: `GET /meta/guests/{vmid}/get?path=traefik.spec` (dotted). Update `docs/API.md` accordingly (also for datacenter). Keep the Python client consistent later (note it in the report).
* `GET /meta/guests` returns node/type from the vmlist; guests that have a document but are not in the vmlist are still listed with `node: null, type: null` and `orphan: true`.
* `raw=1` adds the raw text; `comments=1` keeps comment keys; both default off.
* Responses use `rpcenv["digest"]`-style attribs? No — put `digest` inside the returned object (simpler for clients).
* Errors: map `pve_meta_core::Error` → HTTP: `DigestMismatch` 409, `NotFound` 404, `Lint`/`Parse`/`InvalidPath`/`InvalidName`/`TooLarge` 400, `Conflict` 409, others 500. Use `proxmox_router::http_err!`/`HttpError` so rest-server emits the status.
* Every write logs `auth_id`, doc id, and the touched paths at info level (`tracing`/`log`).
* `/meta/health`: store root, file count, total bytes, daemon version (from `CARGO_PKG_VERSION`), `hooks: {}` placeholder (filled by the perl-hook milestone), `auth: { keyring_keys: n }`.
* `/meta/registry` and `/meta/schemas/{vmid}` read `operators.*` from the datacenter document.
* Lifecycle endpoints per API.md (`snapshot`, `rollback`, `snapshots` list/delete, `clone`) call the store methods.

### Index / UI serving

`ApiConfig::new("/usr/share/pve-meta/ui", PUBLIC)` with env override `PVE_META_UI_DIR`. The index handler for `/` and `/ui/` serves `index.html` from that dir, **injecting** `<script>Proxmox = { Setup: { auth_cookie_name: 'PVEAuthCookie' }, UserName: "<user>", CSRFPreventionToken: "<token>" };</script>` into `<head>` when the request carries a valid ticket cookie (compute the token with `assemble_csrf`); without a valid ticket serve the page with `UserName: ""` (the UI shows "not logged in"). Static files under `/ui/...` come from the same dir via `.alias("ui", dir)`. Set `Cache-Control: no-cache` on index.

### Daemon (`pve-metad`)

Follow the research skeleton: `proxmox_log` init (journald when not a tty, else stderr), `ApiConfig` + `RestServer`, TLS from `/etc/pve/local/pve-ssl.key` + `.pem` (env override `PVE_META_TLS_KEY/CERT`), listen `[::]:8007` (env `PVE_META_LISTEN`), `proxmox_daemon::server::create_daemon`, `SystemdNotify::Ready`, pidfile `/run/pve-metad.pid`, graceful shutdown. Ship `debian/pve-metad.service` (Type=notify, After=pve-cluster.service, Wants=pve-cluster.service, Restart=on-failure). Must run as root (reads `/etc/pve/priv`-adjacent files; writes `/etc/pve/meta`).

### CLI (`pve-meta`)

`proxmox_router::cli` with `CliCommandMap`, commands per API.md, wrapping the same `API_METHOD_*` constants; positional args via `arg_param`. Output: `--output-format text|json|json-pretty` (json-pretty default for `get`; text tables for `list`). `set` takes `k=v` pairs: values parsed as JSON if they parse (numbers, bools, arrays, objects), else string. `edit` opens `$EDITOR` (fallback `vi`) on the raw text in a temp file and `put_raw`s it back with the original digest (abort if unchanged). `raw` prints the file. `datacenter` is a nested command map. The CLI uses the store directly (no HTTP) and sets auth id `root@pam`.

## Packaging (`debian/`)

Single source package `pve-meta` producing binaries `pve-meta` (daemon + CLI + UI files in `/usr/share/pve-meta/ui`) — the UI dist is produced separately (`ui/dist`, built with trunk) and copied by the Makefile. Provide a top-level `Makefile` with targets `build` (cargo build --release for the three crates), `ui` (runs `trunk build` in `ui/`), `deb` (`dpkg-buildpackage -b -us -uc` using `debian/rules` with `dh` + `override_dh_auto_build` calling `make build ui`), `install` (DESTDIR-aware: `/usr/sbin/pve-metad`, `/usr/bin/pve-meta`, `/usr/share/pve-meta/ui/*`, `/lib/systemd/system/pve-metad.service`). `debian/control`: `Depends: pve-manager (>= 9.0), ${shlibs:Depends}, ${misc:Depends}`; postinst enables + starts the service (`dh_installsystemd` does it). No dependency ceilings. Build-Depends: `debhelper-compat (= 13)`, `cargo`, `rustc` are NOT used from Debian — the build host uses rustup; so `debian/rules` must call the rustup cargo (`$(HOME)/.cargo/bin/cargo` if present else `cargo`) and we build with `dpkg-buildpackage -b -us -uc -d` (skip build-dep check). Document this in `docs/BUILD.md`.

## Tests

* Unit tests for auth (ticket fixture, csrf round trip, token parsing).
* Integration test for the API using the store on a tempdir: spin up the router **without HTTP** by calling handlers via `ApiMethod.handler` with a `CliEnvironment`-like RpcEnvironment (see how the research report's CLI section invokes `ApiHandler::Sync`), covering: create by patch, get, subtree get, raw put with digest mismatch, convert, list with `has`, delete, snapshots, datacenter doc + registry.
* A smoke script `scripts/smoke.sh` that, given `PVE_META_URL` and a token or ticket, exercises the live daemon with curl (`version`, `health`, create/patch/get/raw/convert/delete on vmid 999999).
