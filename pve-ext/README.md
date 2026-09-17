# pve-ext: a generic extension layer for Proxmox VE

pve-ext is what pve-meta's research into patching stock Proxmox turned into: a
small, reusable seam. Any package that needs to add an API endpoint or a UI
tab to stock Proxmox VE, or patch a stock PVE file some other way, depends on
`pve-ext` and drops a declarative artifact into one of three places, instead
of writing its own dpkg-divert/ExtJS-override/`perl -c`-gate machinery.
`pve-meta` (the sibling project in this repository) is the one consumer.

## 1. API modules — `PVE::API2::Ext`

Ship a plain `PVE::RESTHandler` subclass at
`/usr/share/perl5/PVE/API2/Ext/<Name>.pm` that declares an `ext_path`
class method:

```perl
package PVE::API2::Ext::Meta;
use base qw(PVE::RESTHandler);
sub ext_path { return 'meta'; }
__PACKAGE__->register_method({ ... });
1;
```

`PVE::API2::Ext` (dpkg-diverted into stock `PVE/API2.pm`, at the very end of
that file, behind one `PVE::API2::Ext->register_all();` call — so every core
`register_method` has already run) scans `/usr/share/perl5/PVE/API2/Ext/*.pm`
once, at load time, `require`s each file it finds, and — if the resulting
class implements `ext_path` — mounts it directly into the API root via
`PVE::API2->register_method({ subclass => $class, path => $path })`, e.g.
`PVE::API2::Ext::Meta`'s `ext_path` of `'meta'` ending up reachable at
`/api2/json/meta/...`. A module with no `ext_path`, that fails to `require`,
or whose `ext_path` collides with a path something else already registered
(a core path, another extension module, or the reserved name `ext` itself)
is skipped with a `warn`, never a `die` — one broken or colliding module
never takes pvedaemon/pveproxy down with it, and core always wins a
collision. A new extension module needs a pvedaemon/pveproxy restart to be
picked up (the scan runs once, at process start).

`PVE::API2::Ext` also mounts itself at `/api2/json/ext`: `GET /ext` (index),
`GET /ext/modules` (the extension modules loaded: `[{module, path}, ...]`)
and `GET /ext/pages` (the validated page manifests, below).

## 2. UI pages — `js/pve-ext-loader.js`

Drop a manifest at `/usr/share/pve-ext/pages/<id>.json`:

```json
{
  "id": "pve-meta",
  "title": "Metadata",
  "iconCls": "fa fa-tags",
  "targets": ["lxc", "qemu", "dc"],
  "script": "/pve2/js/my-app/pve-my-app.js",
  "xtype": "myAppPanel"
}
```

| Field | Required | Meaning |
|---|---|---|
| `id` | yes | Unique id; also the tab's `itemId` (`pve-ext-<id>`). |
| `title` | yes | Tab title. |
| `iconCls` | no | ExtJS/FontAwesome icon class (default: `fa fa-puzzle-piece`). |
| `targets` | yes | Any of `lxc`, `qemu`, `node`, `dc` — which config panel(s) get the tab. |
| `script` | yes | URL of a JS file defining an ExtJS class (placeholders substituted, below), loaded once and instantiated as the tab's content. |
| `xtype` | yes | The `xtype` the script registers; the tab becomes `{ xtype, vmid, type, node, dc }` (whichever apply to the target). |
| `fingerprint` | — | **Server-added.** A content hash of `script`, appended by the loader as `?ver=` (cache busting: pveproxy serves static files with `Last-Modified` and no `ETag`, and dpkg clamps mtimes for reproducible builds, so a rebuilt file of the same version would otherwise revalidate to a stale cached copy). |

`GET /api2/json/ext/pages` (`permissions => { user => 'all' }`) re-reads this
directory on every call, parses each file and checks it has the fields above;
a manifest that doesn't is skipped with one `warn`, never breaking the
endpoint for the others. A duplicate `id` (first one wins, by sorted
filename) is likewise skipped with a `warn`.

`pve-ext-loader.js` (loaded via a `<script>` tag dpkg-diverted into stock
`index.html.tpl`, right after `pvemanagerlib.js`) fetches that endpoint once
and, for every manifest whose `targets` includes the panel being built, adds
a `layout: 'fit'` tab that inserts a `<script>` for `script` (once per URL,
cached), waits for `xtype` to resolve to a defined class, and replaces its
own content with `{ xtype, vmid, type, node, dc }`. Adding a tab works by
patching `PVE.panel.Config.prototype.initComponent` directly (capture the
original, call it, then add tabs) — see the file's header comment for why
this, and not `Ext.override`, is required on ExtJS 7 classic. Every seam
this script touches is individually `try`/`catch`-guarded, degrading to
"that one thing doesn't happen" — it must never be possible for a broken
manifest or script to break the PVE UI itself.

Placeholders substituted into `script` (and into `{query}`, a ready-made
query string): `{vmid}`/`{node}`/`{type}` (guest vmid/node/`lxc`|`qemu`, or
just `node`/`dc` for those targets), and `{theme}` (`light`/`dark`,
mirroring the admin's PVE color theme).

pveproxy already maps `/pve2/js/` to `/usr/share/pve-manager/js/` — ship
your page's static files under `/usr/share/pve-manager/js/<your-app>/`, and
point `script` at `/pve2/js/<your-app>/...` (same-origin, exactly like the
main PVE UI's own assets).

## 3. Managed patches — `pve-ext-patch`

For anything that isn't "add an API module" or "add a UI tab" — most
commonly, hooking a few lines into stock PVE Perl at points with no plugin
seam — ship a TOML manifest at `/usr/share/pve-ext/patches/<name>.toml`:

```toml
[[file]]
path = "/usr/share/perl5/PVE/API2.pm"
package = "pve-manager"
diff = "pve-manager_API2.pm.diff"     # relative to the manifest's own directory
marker = "PVE::API2::Ext->register_all();"
check = "perl"                         # perl -c gate; or "template" for HTML
```

| Field | Required | Meaning |
|---|---|---|
| `path` | yes | Absolute path of the file to patch. |
| `package` | no | The upstream package that ships `path` — informational, shown in `status`. |
| `diff` | yes | A unified diff (`a/`/`b/` headers using `path` minus its leading `/`), applied with `patch -p1`, relative to the manifest's own directory. |
| `marker` | yes | A literal string `status`/`verify` grep for, and, for `check = "template"`, the tag expected exactly once in the output. |
| `check` | no (default `perl`) | `perl` gates on `perl -c` reporting `syntax OK` last; `template` gates on `marker` appearing exactly once and `</body>` still present. |

`pve-ext-patch apply|remove|verify|status [--root DIR] [manifest...]`
(`/usr/sbin/pve-ext-patch`) works as follows, per file:

- `dpkg-divert --package pve-ext --add --rename --divert <path>.pve-ext-orig
  <path>`, the first time it's touched. The diversion is always owned by
  `pve-ext` (never a manifest's `package`), so `<path>.pve-ext-orig` always
  holds the pristine upstream content (across future upgrades of the file's
  real package too) and `<path>` is pve-ext's to (re)write. If `<path>` is
  already diverted by a *different* package, `apply`/`remove` refuse to
  touch it — a real accident, never a case pve-ext caused itself.
- The patched file is regenerated from that pristine backup + the diff, in
  a scratch copy, never in place — so re-running `apply` is idempotent.
- `patch -p1 --fuzz=0 --dry-run` gates the real `patch -p1 --fuzz=0`
  (`--fuzz=0`: default fuzz can slide a hunk onto the wrong one of several
  near-identical anchors and report success); the result is only
  `install`ed after it also passes its `check`.
- On any failure after a diversion that already existed (a re-apply after
  an upstream upgrade replaced the pristine), `apply` restores the
  *current* pristine rather than leaving stale patched content, and
  records the failure to syslog and `<ROOT_DIR>/run/pve-ext-patch/failed`
  (never only stderr, which a postinst commonly swallows).
- Best-effort per file, across every file in every selected manifest: one
  file's anchors moving must never block patching the others.
- `remove` restores every entry's pristine file and removes its diversion.

With no manifest named, `apply`/`remove`/`verify`/`status` act on every
`*.toml` under `/usr/share/pve-ext/patches/` (or, in a checkout, `../patches`
next to the script).

Using it from your own package: ship your diffs and manifest under
`/usr/share/pve-ext/patches/` (`Depends: pve-ext`); your `postinst`, on
`configure` and `triggered`, runs `pve-ext-patch apply <manifest-name>`
(best-effort, never fails your install); your `prerm`, on `remove`, runs
`pve-ext-patch remove <manifest-name>` **before** dpkg deletes your
package's files (ordering falls out of `Depends: pve-ext`); your
`debian/triggers` declares `interest-noawait` on every path you patch, so
it survives upgrades of whatever package ships those files. `pve-meta`'s
own `patches/lifecycle.toml` (installed as
`/usr/share/pve-ext/patches/pve-meta-lifecycle.toml`) is a worked example;
see `docs/LIFECYCLE.md`.

## Summary: what pve-ext ships

```
perl/PVE/API2/Ext.pm             -> /usr/share/perl5/PVE/API2/Ext.pm
(empty dir, for consumers)       -> /usr/share/perl5/PVE/API2/Ext/
js/pve-ext-loader.js             -> /usr/share/pve-manager/js/pve-ext-loader.js
bin/pve-ext-patch                -> /usr/sbin/pve-ext-patch
man/pve-ext-patch.8              -> /usr/share/man/man8/pve-ext-patch.8
patches/pve-manager.toml + diffs -> /usr/share/pve-ext/patches/
(empty dir, for consumers)       -> /usr/share/pve-ext/pages/
```

pve-ext's own `debian/postinst`/`debian/prerm` apply/remove exactly its own
`pve-manager.toml` manifest; every other manifest, page and API module comes
from whatever package depends on `pve-ext` and drops it in. `make deb` runs
`lintian` against the built `.deb`: fatal when `$CI` is set, advisory
otherwise.
