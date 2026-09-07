# Native PVE API module — `PVE::API2::Meta`

Decision (Sep 2026, revision 3): the pve-meta HTTP API is served by **pveproxy/pvedaemon
on port 8006** as a native PVE API module, `/api2/json/meta/...`, instead of a separate
daemon on 8007. Rationale: same certificate as the PVE UI (including custom certs in
`/etc/pve/local/pveproxy-ssl.pem`), PVE's own ticket/token/CSRF handling, per-guest ACL
checks, no extra port, and it is exactly the shape an upstream feature would have. The
Rust daemon (`pve-metad`) stays in the tree as an optional component but is not installed
by default. The Rust CLI is unchanged (it talks to the store directly).

Mechanism: `PVE::API2::Meta` is a thin Perl module whose methods call into the Rust core
through the existing perlmod bindings (`PVE::RS::Meta`, crate `crates/pve-meta-perl`).
It is registered in the API root with a one-line insertion in
`/usr/share/perl5/PVE/API2.pm` (package pve-manager), applied with the same
dpkg-divert/patch/verify tool as the lifecycle hooks:

```perl
__PACKAGE__->register_method({
    subclass => "PVE::API2::Meta",
    path => 'meta',
});
```

placed next to the existing `PVE::API2::Pool` registration.

## Process model

* pveproxy runs as `www-data`: `/etc/pve/meta/*` files are `root:www-data 0640`, so all
  **read** methods run in pveproxy directly (`protected => 0`).
* All **write** methods are `protected => 1` (executed by pvedaemon as root).
* No long-poll: `GET /meta/version` is a cheap call (content-hash token from the core);
  clients poll every few seconds. The `wait`/`since` parameters are accepted and ignored
  (documented), so existing clients keep working.

## Rust side (extend `crates/pve-meta-perl`, package `PVE::RS::Meta`)

Add API-shaped exports that return plain JSON-able Perl structures (hashes/arrays) and
die with a message carrying an HTTP status prefix the Perl layer maps, e.g. the Rust
error `Display` is prefixed like `409: digest mismatch ...`, `404: ...`, `400: ...`:

* `api_version()` → `{ token, changed }`
* `api_health()` → `{ store: {root, files, bytes}, version, hooks: {} }`
* `api_inventory()` → list of `{ vmid, node, type, name, has_meta, format }` (name read
  from `/etc/pve/nodes/<node>/{qemu-server,lxc}/<vmid>.conf`: `name:` / `hostname:`)
* `api_list_guests($has)` → list per API.md (`has` optional dotted prefix)
* `api_get($id, $comments, $raw)` → document hash (`id` is a vmid or `"datacenter"`)
* `api_subtree($id, $path)` → `{ data, digest }`
* `api_patch($id, $patch_json, $digest, $dry_run)` → document + `touched`
  (`$patch_json` is a JSON string to avoid Perl number/string ambiguity)
* `api_put_raw($id, $content, $format, $digest, $dry_run)` → document + `touched`
* `api_convert($id, $format, $digest)` → document
* `api_delete($id)` → 1
* `api_snapshots($id)`, `api_snapshot($id,$name)`, `api_rollback($id,$name)`,
  `api_delete_snapshot($id,$name)`, `api_clone($id,$newid)` — thin over the existing
  functions
* `api_registry()` → list from `datacenter.operators`
* `api_schemas($id)` → `{ "<prefix>": schema }` map applicable to a guest

Document values cross the boundary as JSON **strings** (`data_json`) and are decoded in
Perl with `JSON::PP`/`PVE::JSONSchema` to preserve numbers/booleans faithfully; the Perl
layer returns them as data so pveproxy encodes them once.

## Perl side (`perl/PVE/API2/Meta.pm`, installed to `/usr/share/perl5/PVE/API2/Meta.pm`)

`PVE::RESTHandler` subclass with the standard `register_method` blocks. Endpoint map =
`docs/API.md` (paths relative to `/meta`):

| method | path | protected | permissions |
|---|---|---|---|
| GET | `/` | no | Anybody (lists subdirs) |
| GET | `version`, `health`, `inventory`, `registry` | no | `['perm', '/', ['Sys.Audit']]` for inventory/registry; version/health: Anybody (authenticated) |
| GET | `guests` | no | Anybody, but the list is filtered to vmids where the user has `VM.Audit` (use `$rpcenv->check($authuser, "/vms/$vmid", ['VM.Audit'], 1)`) |
| GET | `guests/{vmid}`, `guests/{vmid}/subtree`, `guests/{vmid}/snapshots` | no | `['perm', '/vms/{vmid}', ['VM.Audit']]` |
| PUT | `guests/{vmid}` (patch), `guests/{vmid}/raw` | yes | `['perm', '/vms/{vmid}', ['VM.Config.Options']]` |
| POST | `guests/{vmid}/convert`, `guests/{vmid}/snapshot`, `guests/{vmid}/rollback`, `guests/{vmid}/clone` | yes | `VM.Config.Options` (clone additionally `VM.Clone`) |
| DELETE | `guests/{vmid}`, `guests/{vmid}/snapshots/{name}` | yes | `VM.Config.Options` |
| GET | `datacenter`, `datacenter/subtree` | no | `['perm', '/', ['Sys.Audit']]` |
| PUT/POST/DELETE | `datacenter`, `datacenter/raw`, `datacenter/convert` | yes | `['perm', '/', ['Sys.Modify']]` |
| GET | `schemas/{vmid}` | no | `VM.Audit` |

Parameters use `PVE::JSONSchema` (`vmid` with `pve-vmid` format, `digest` string,
`patch`/`content` strings — the patch is passed as a JSON string parameter `patch`;
document this in API.md: bodies are ordinary PVE form/JSON parameters, `patch` is a
JSON-encoded string), returns `type => 'object'` with `additionalProperties => 1`.
Errors from Rust (`NNN: message`) are re-raised with `PVE::Exception::raise($msg,
code => NNN)` so clients get the right HTTP status.

`use PVE::RS::Meta;` at the top; `use PVE::RESTHandler; use PVE::JSONSchema qw(get_standard_option); use PVE::RPCEnvironment; use PVE::Exception qw(raise raise_param_exc); use JSON;`.

## UI serving

The wasm UI is installed to `/usr/share/pve-manager/js/pve-meta-ui/` and is therefore
served by pveproxy at `https://<node>:8006/pve2/js/pve-meta-ui/index.html` with no
proxy change. Trunk `public_url = "/pve2/js/pve-meta-ui/"`. Same origin as the PVE UI:
the injected tab's iframe uses that URL; the cookie is shared; the CSRF token is taken
from `window.parent.PVE.CSRFPreventionToken` when embedded, otherwise the app renews
its ticket via `POST /api2/json/access/ticket` (username + ticket as password) to get a
fresh token. `Proxmox.Setup.auth_cookie_name` comes from the same place.

## Patch tooling

`pve-manager-patches/lifecycle/pve-manager_API2.pm.diff` + one more row in the
lifecycle tool's file table (path `/usr/share/perl5/PVE/API2.pm`, package pve-manager)
and one more `interest-noawait` trigger line. After apply: `systemctl restart pvedaemon
pveproxy` (the tool prints the hint; postinst does it).

## Packaging

`perl/PVE/API2/Meta.pm` → `/usr/share/perl5/PVE/API2/Meta.pm` (package pve-meta),
UI dist → `/usr/share/pve-manager/js/pve-meta-ui/` (package pve-meta), `pve-metad`
binary + unit stay in a separate, optional package `pve-metad` (not installed by
default) or are dropped from the .deb entirely for now (leave the crate in the tree).
