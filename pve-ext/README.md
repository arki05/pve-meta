# pve-ext: a generic extension layer for Proxmox VE

pve-ext is the thing that started as one-off, per-project research into
patching stock pve-manager (see `docs/LIFECYCLE-PATCHES.md`, superseded by
this package) turned into a small, reusable, package-agnostic seam. Any package
that needs to add an API endpoint or a UI tab to stock Proxmox VE, or patch
a stock PVE file some other way, depends on `pve-ext` and drops a
declarative artifact into one of three places, instead of writing (and
re-verifying) its own dpkg-divert/ExtJS-override/ `perl -c`-gate machinery.

This document covers all three seams and how a consumer package uses each
one. `pve-meta` (the sibling project in this repository) is the first real
consumer; see its own `debian/` for a worked example of each.

## 1. API modules — `PVE::API2::Ext`

Ship a plain `PVE::RESTHandler` subclass at
`/usr/share/perl5/PVE/API2/Ext/<Name>.pm` that declares an `ext_path`
class method:

```perl
package PVE::API2::Ext::Meta;

use base qw(PVE::RESTHandler);

sub ext_path { return 'meta'; }

__PACKAGE__->register_method({ ... your own paths, e.g. 'guests', 'guests/{vmid}', ... ... });

1;
```

`PVE::API2::Ext` (installed by pve-ext, `require`d and then explicitly
driven by one `PVE::API2::Ext->register_all();` call, both dpkg-diverted
into stock `PVE/API2.pm` at the very end of that file — i.e. *after* every
one of `PVE::API2`'s own core `register_method` calls) scans
`/usr/share/perl5/PVE/API2/Ext/*.pm` once, at load time (i.e. once per
pvedaemon/pveproxy worker startup), `require`s each file it finds, and — if
the resulting class implements `ext_path` — mounts it directly into the API
root at the path it returns:

```perl
PVE::API2->register_method({ subclass => $class, path => $path });
```

So `PVE::API2::Ext::Meta`'s `ext_path` of `'meta'` ends up reachable at
`/api2/json/meta/...`, exactly like any other native `PVE::API2` subclass —
`pve-ext` itself never appears in the URL (except at its own `/ext`
mount point, below). A module with no `ext_path`, that fails to `require`,
or whose `ext_path` collides with a path something else already
registered — a core `PVE::API2` path, another extension module scanned
earlier, or the reserved name `ext` itself — is skipped with a `warn` to
the log, never a `die`; **one broken or colliding extension module never
takes pvedaemon/pveproxy down with it.** Running `register_all()` only
after every core registration has already happened is what makes the
"core always wins a collision" direction hold: an extension's own
`register_method` call is the one that fails (and is caught) when it
collides with something core already claimed, never the reverse.

`PVE::API2::Ext` also mounts itself at `/api2/json/ext`, with three
endpoints of its own:

| Method | Path | Returns |
|---|---|---|
| GET | `/ext` | `[{subdir: "modules"}, {subdir: "pages"}]` |
| GET | `/ext/modules` | The extension API modules that loaded successfully: `[{module, path}, ...]` |
| GET | `/ext/pages` | The validated page manifests (see below) |

You do not need to install anything to get a package listed at
`/api2/json/`: `PVE::API2`'s own `index` method iterates its
`method_attributes()`, which plain `register_method({ subclass => ...
})` calls already populate — nothing pve-ext-specific is required there.

A new extension module needs a pvedaemon/pveproxy restart to be picked up
(the scan runs once, at process start) — same as adding any other native
`PVE::API2` module.

## 2. UI pages — `js/pve-ext-loader.js`

Drop a manifest at `/usr/share/pve-ext/pages/<id>.json`:

```json
{
  "id": "pve-meta",
  "title": "Metadata",
  "iconCls": "fa fa-tags",
  "targets": ["lxc", "qemu", "dc"],
  "url": "/pve2/js/pve-meta-ui/index.html?{query}",
  "requires": { "vms": ["VM.Audit"], "dc": ["Sys.Audit"] }
}
```

| Field | Required | Meaning |
|---|---|---|
| `id` | yes | Unique id; also used as the tab's `itemId` (`pve-ext-<id>`) and as the JSON filename by convention. |
| `title` | yes | Tab title. |
| `iconCls` | no | An ExtJS/FontAwesome icon class (default: `fa fa-puzzle-piece`). |
| `targets` | yes | Any of `lxc`, `qemu`, `node`, `dc` — which config panel(s) get the tab. |
| `url` | exactly one of `url` or `script`+`xtype` | Iframe `src`, with placeholders substituted (below). Same-origin (relative, no scheme/host/port) is strongly recommended — see "Serving your page's files", below. |
| `script` | exactly one of `url` or `script`+`xtype` | URL of a JS file defining an ExtJS class (placeholders substituted, same as `url`), loaded once and instantiated as the tab's content instead of an iframe. Requires `xtype`. |
| `xtype` | with `script` | The `xtype` (ExtJS alias) the script registers; the tab becomes `{ xtype, vmid, type, node, dc }` (whichever of those apply to the target — see below), not an iframe. |
| `requires` | no | Per-capability-category privilege lists (see "Privilege check", below). Omit to show the tab to every logged-in user. |

`GET /api2/json/ext/pages` (served by `PVE::API2::Ext`, `permissions => {
user => 'all' }`) re-reads this directory **on every call** — no daemon
restart needed to pick up a new/changed manifest — and validates each
file's shape (the fields above; `targets` entries must be one of the four
known values; `requires`, if present, must map to arrays; exactly one of
`url` or `script`+`xtype` must be present). A malformed manifest is
skipped with a `warn`, never breaks the endpoint for every other
manifest. A manifest whose `id` duplicates one already seen in the same
directory listing (first one wins, by sorted filename) is likewise
skipped with a `warn` naming both files — the same warn-and-skip
convention the API-module seam uses for a colliding `ext_path`.

`pve-ext-loader.js` (installed by pve-ext, loaded via one `<script>` line
dpkg-diverted into stock `index.html.tpl`, right after `pvemanagerlib.js`)
fetches that endpoint once (lazily, on first use — see the file's own
header comment for why) and, for every manifest whose `targets` includes
the panel currently being built, adds one tab. For a `url` manifest that
tab is a `layout: 'fit'` panel containing a same-origin `<iframe>`, sized
to fill it. For a `script`+`xtype` manifest the tab is instead a
`layout: 'fit'` panel that, once rendered, inserts a `<script>` tag for
`script` (once per URL — cached, so several tabs referencing the same
script only load it once), waits for `xtype` to resolve to a defined
class (`Ext.ClassManager.getNameByAlias('widget.' + xtype)` then
`isCreated()` on the resolved name — a bare `isCreated(xtype)` does
**not** work, verified against real ExtJS 7 classic), and then replaces
its own content with
`{ xtype, vmid, type, node, dc }` — the same placeholder values `url`
would have received, passed as config properties instead of substituted
into a URL, so the panel's own initComponent can read `this.vmid`/
`this.node`/etc. directly. Either way, adding a tab works by patching
`PVE.panel.Config.prototype.initComponent` directly (replacing the
function, calling the original it captured first, then adding the extra
tabs) — proved out in a real browser against ExtJS 7 classic. It must
**never** be done via the global `Ext.override(cls, {...})` shim with
`this.callParent(...)` inside the replacement: that form throws in real
ExtJS 7 classic and silently kills the whole config panel. Every seam
this script touches is individually `try`/`catch`-guarded; a failure
anywhere logs to the console (prefixed `[pve-ext]`) and degrades to "that
one thing doesn't happen" — **it must never be possible for a broken
manifest, a broken `/ext/pages` response, or a script that never defines
its `xtype`, to break the PVE UI itself.**

### Placeholders

| Placeholder | Guest (`lxc`/`qemu`) | `node` | `dc` |
|---|---|---|---|
| `{vmid}` | the guest's vmid | — | — |
| `{node}` | the guest's node | the node's name | — |
| `{type}` | `lxc` or `qemu` | `node` | `dc` |
| `{theme}` | `light` or `dark`, mirroring the admin's current PVE color theme | same | same |
| `{query}` | `vmid=<id>&type=<type>&node=<node>&theme=<theme>` | `node=<node>&theme=<theme>` | `dc=1&theme=<theme>` |

`{query}` is the one most manifests want (it's a ready-to-use query
string); the individual placeholders exist for a URL that needs one value
somewhere other than the query string.

### Privilege check (client-side; not the enforcement)

`requires` is checked against `Ext.state.Manager.get('GuiCap')` — the same
capability map the rest of the PVE UI already uses to show/hide its own
buttons and tabs (`caps.vms['VM.Audit']`, `caps.dc['Sys.Audit']`, etc.). For
a guest target the `vms` bucket is checked, for `node` the `nodes` bucket,
for `dc` the `dc` bucket; every privilege listed for the relevant bucket
must be present, or the tab is not added at all. **This is a UX
convenience only** — it means a user simply never sees a tab they have no
access to — **it is not the access control.** Your page's own backend API
must enforce the real permission check server-side regardless of whether
the tab was shown; never rely on `requires` for security.

### Serving your page's files

pveproxy already maps `/pve2/js/` to `/usr/share/pve-manager/js/` (see
`add_dirs()` in `PVE::Service::pveproxy`) — that's how
`pve-ext-loader.js` itself gets served. Ship your page's own static files
(e.g. `pve-meta`'s Monaco-based iframe editor, or its ExtJS `script`+`xtype`
page) under `/usr/share/pve-manager/js/<your-app>/`, and point `url` (an
HTML entry point) or `script` (a JS file defining your `xtype`) at
`/pve2/js/<your-app>/...?{query}` — a same-origin, host/port-relative path
either way, so there is no cross-origin/mixed-content concern, exactly like
the main PVE UI's own assets.

## 3. Managed patches — `pve-ext-patch`

For anything that isn't "add an API module" or "add a UI tab" — most
commonly, hooking a few lines into stock PVE Perl at points where there is
no plugin seam at all (pve-ext's own `index.html.tpl`/`PVE/API2.pm` hooks
are exactly this) — ship a TOML manifest at
`/usr/share/pve-ext/patches/<name>.toml`:

```toml
id = "pve-manager"                        # this manifest's stable claim identity

[[file]]
path = "/usr/share/perl5/PVE/API2.pm"
package = "pve-manager"
diff = "pve-manager_API2.pm.diff"        # relative to the manifest's own directory
marker = "PVE::API2::Ext->register_all();"
check = "perl"                            # perl -c gate; or "template" for an HTML template
```

| Field | Required | Meaning |
|---|---|---|
| `id` (top-level, before any `[[file]]`) | strongly recommended | This manifest's stable claim identity (see "Claim identity" below); falls back to the manifest's own filename basename, with a warning, when absent. |
| `path` | yes | Absolute path of the file to patch. |
| `package` | no | The upstream package that ships `path` — informational, shown in `status` output. |
| `diff` | yes | A unified diff (`a/`/`b/` headers using `path` with its leading `/` stripped), applied with `patch -p1`, relative to the manifest's own directory. |
| `marker` | yes | A literal string used both by `status`/`verify` (a quick "is this patched" grep) and, for `check = "template"`, as the exact tag that must appear **exactly once** in the patched output. |
| `check` | no (default `perl`) | `perl` gates on `perl -c` reporting `syntax OK` as the *last* line of its output; `template` gates on `marker` appearing exactly once and `</body>` still being present. |

`pve-ext-patch apply|remove|verify|status [--root DIR] [manifest...]`
(installed as `/usr/sbin/pve-ext-patch`) keeps every mechanic the two
tools this generalizes proved out; a fifth subcommand, `pve-ext-patch
manifest-id <manifest-file>`, just prints a manifest's declared `id` (see
"Claim identity" below) for build/install tooling that needs it:

- `dpkg-divert --package pve-ext --add --rename --divert <path>.pve-ext-orig <path>`
  per file, the first time it's touched — the diversion is always owned by
  `pve-ext` itself (never by whatever a manifest's `package` field names),
  so `<path>.pve-ext-orig` always holds the pristine upstream content
  (including across that package's future upgrades — dpkg keeps routing
  its writes there) and `<path>` is pve-ext's to (re)write.
- The patched file is always regenerated from that pristine backup + the
  diff, in a scratch copy, **never in place** and never touching the
  pristine backup itself except to read it — so re-running `apply` is
  idempotent and never double-applies.
- `patch -p1 --fuzz=0 --dry-run` gates the real `patch -p1 --fuzz=0`; the
  resulting file is only `install`ed over the real path after it *also*
  passes its `check`. `--fuzz=0` is deliberate, not an oversight: GNU
  patch's default fuzz (2) will slide a hunk onto the wrong one of
  several near-identical anchors in the same file and report success —
  pve-meta's own lifecycle-patch target has exactly that shape (see
  `docs/LIFECYCLE-PATCHES.md`) — and a hunk that silently lands
  in the wrong place is worse than one that fails loudly. On any failure
  after a diversion that already existed (i.e. a re-apply after an
  upstream package upgrade replaced the diverted pristine with a newer
  one), `apply` restores the *current* pristine over the real path rather
  than leaving it holding content patched against the old one, and
  records the failure to syslog and to `<ROOT_DIR>/run/pve-ext-patch/failed`
  (never only stderr, which a postinst commonly swallows) — under `--root`
  that marker directory, and the syslog write, both resolve inside the
  given root: `--root` is a hard isolation boundary, so nothing this tool
  does — including its failure trail, not just `dpkg-divert`/the diversion
  itself/the claim marker — ever touches the real `/run` or the real
  syslog while it's given.
- Best-effort per file, across every file in every selected manifest: one
  file's anchors moving on some future PVE point release must never block
  patching the others.
- `remove` restores every entry's pristine file and removes its diversion
  (mirroring `apply`'s rollback logic exactly, including the
  divert-then-clear-before-rename-back dance `dpkg-divert --remove
  --rename` needs).

With no manifest named, `apply`/`remove`/`verify`/`status` act on every
`*.toml` found under `/usr/share/pve-ext/patches/` (or, in a checkout,
`../patches` next to the script) — so pve-ext's own manifest and every
consumer's manifest are all covered by e.g. a bare `pve-ext-patch status`
with no arguments.

### Claim identity

A manifest's claim identity — what the `.claimed-by` marker (below) records,
and what a re-`apply` compares against to decide "is this still my file" —
comes from its own top-level `id` field, **not** from the path or basename
it was invoked with. This matters because the same manifest content is
often reachable under two different names: a checkout path like
`patches/lifecycle.toml` and an installed name like
`pve-meta-lifecycle.toml` (the root Makefile installs pve-meta's manifest
under its declared `id`, precisely so the two stay in sync — see that
Makefile's `install` target). Without a stable `id`, those two names would
claim the same files under two different identities and refuse each
other — the exact trap a filename-derived identity falls into. A manifest
with no `id` field still works, falling back to its filename basename with
a `WARNING:` on every invocation; add the field to silence it and to make
the manifest's identity stable across renames.

Anything outside this tool that needs a manifest's declared `id` — the
root Makefile's `install` target is the only current example, installing
`patches/lifecycle.toml` under its declared identity rather than its
checkout filename — should resolve it via `pve-ext-patch manifest-id
<manifest-file>` rather than re-parsing the manifest's TOML a second time.
Unlike `apply`/`remove`, `manifest-id` never falls back to the filename
basename: a caller asking for the identity explicitly wants the real one,
and errors out if the manifest has no `id` field.

### Limitation: no two manifests may patch the same file

A diversion is owned by the `pve-ext` dpkg-divert package name regardless
of which manifest created it, so `dpkg-divert` alone cannot tell two
manifests apart. `pve-ext-patch` tracks, in a small marker file next to
each diversion (`<path>.pve-ext-orig.claimed-by`, never touched by dpkg),
which manifest currently claims that path, and:

- `apply` refuses — with a clear error, not a silent merge or overwrite —
  to patch a path a *different* manifest has already diverted. Two
  manifests can never stack their diffs onto the same file today; if your
  package needs to change a file another manifest already patches, either
  fold your change into that manifest's own diff (coordinate with its
  owner) or patch a different file.
- `remove` refuses to undivert a path a different manifest still claims,
  so removing one package's manifest can never yank the pristine file out
  from under another package's patch.

This is a known, intentional limitation of the current design, not a bug:
composing two independent diffs on one file safely (in an order-
independent way, surviving either manifest's removal) is real work this
tool does not attempt. Today's only two manifests (pve-ext's own
`pve-manager.toml` and pve-meta's `pve-meta-lifecycle.toml`) are disjoint
by construction — they simply never name the same file.

### Using it from your own package

1. Ship your diffs and manifest under `/usr/share/pve-ext/patches/`
   (`Depends: pve-ext`).
2. Your `postinst`, on `configure` and `triggered`, runs
   `pve-ext-patch apply <your-manifest-name>` — best-effort, never fails
   your package's install.
3. Your `prerm`, on `remove`, runs
   `pve-ext-patch remove <your-manifest-name>` — **before** dpkg deletes
   your package's files, mirroring pve-ext's own `prerm` (see there for
   why `prerm`, not `postrm`). This runs correctly before pve-ext is
   itself removed as long as your package declares `Depends: pve-ext` (dpkg
   always removes dependents before their dependencies).
4. Your `debian/triggers` declares `interest-noawait` on every path your
   manifest patches, so the patch survives upgrades of whatever package
   ships those files.

`pve-meta`'s own lifecycle patch (`patches/lifecycle.toml` in that
project, installed as `/usr/share/pve-ext/patches/pve-meta-lifecycle.toml`)
is a worked example of all four steps, for the one guest-lifecycle file it
patches (lifecycle is snapshot-only; see `docs/LIFECYCLE-PATCHES.md`).

## Summary: what pve-ext ships

```
perl/PVE/API2/Ext.pm             -> /usr/share/perl5/PVE/API2/Ext.pm
(empty dir, for consumers)       -> /usr/share/perl5/PVE/API2/Ext/
js/pve-ext-loader.js             -> /usr/share/pve-manager/js/pve-ext-loader.js  (served as /pve2/js/pve-ext-loader.js)
bin/pve-ext-patch                -> /usr/sbin/pve-ext-patch
man/pve-ext-patch.8              -> /usr/share/man/man8/pve-ext-patch.8
patches/pve-manager.toml + diffs -> /usr/share/pve-ext/patches/
(empty dir, for consumers)       -> /usr/share/pve-ext/pages/
```

pve-ext's own `debian/postinst`/`debian/prerm` apply/remove exactly its own
`pve-manager.toml` manifest (the `index.html.tpl`/`PVE/API2.pm` hooks
everything else in this document depends on); every other manifest, page
and API module comes from whatever package depends on `pve-ext` and drops
it in.

## Building and packaging

`make deb` (from this directory, or via the root `Makefile`'s `deb` target,
which builds this package first) runs `lintian` against the freshly built
`.deb`(s) as part of the target itself, not as a separate manual step (see
`docs/design/PROXMOX-CONVENTIONS.md` §7.6 and §8, and `docs/DESIGN.md` §9):
fatal (a non-zero `make deb` exit) when `$CI` is set in the environment, and
`|| true` (advisory only, output still printed) for a local/dev build where
`$CI` is unset — CI is expected to catch anything a local build let through.
The root Makefile's `deb` target lintians `pve-meta`, `libpve-meta-rs-perl`
and `pve-ext` together after both builds finish; running `make -C pve-ext
deb` on its own (as the ceiling watcher and CI both also do to build
`pve-ext` independently) lintians `pve-ext` a second time on its own.
Package-specific false positives are silenced with a `debian/*.lintian-
overrides` file and a comment explaining why, never by skipping the check —
see `debian/libpve-meta-rs-perl.lintian-overrides` for a worked example
(the `unsafe-libyaml` crate trips lintian's `embedded-library` heuristic on
panic-message/registry-path text, not on any actual C object it duplicates).
