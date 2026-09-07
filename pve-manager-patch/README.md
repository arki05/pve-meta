# pve-meta-patch: "Metadata" tab injection for the PVE web UI

Adds a **Metadata** tab (icon `fa fa-tags`) to every LXC and QEMU guest's
config panel and to the Datacenter panel in the Proxmox VE 9.x web UI. The
tab embeds a same-origin iframe served by pveproxy itself, alongside
pve-manager's own static assets:

- Guest panel: `/pve2/js/pve-meta-ui/index.html?vmid=<vmid>&theme=<light|dark>`
- Datacenter panel: `/pve2/js/pve-meta-ui/index.html?dc=1&theme=<light|dark>`

This is a bare relative URL - no scheme, host, or port of its own - so it
always resolves against whatever host/port the admin is already browsing
pve-manager on, with zero cross-origin/mixed-content concern (see "Serving
the loader" below for how those static files are expected to reach that
path). `<light|dark>` mirrors the PVE UI's own currently active color
theme (see "Theme detection" below).

Tested against **pve-manager 9.2.11** (`f6997e698c7933ea`) on a disposable
lab node (`pvemeta-node1`, 10.10.10.154).

## Files

- `pve-meta-loader.js` — the injected loader. Plain ES2017, no build step,
  no external dependencies. Ends up served by pveproxy at
  `/pve2/js/pve-meta-loader.js`.
- `pve-meta-patch` — bash script, installed as `/usr/sbin/pve-meta-patch`.
  Subcommands: `apply`, `remove`, `verify`, `status`.
- `debian/pve-meta.triggers`, `debian/postinst`, `debian/postrm` — proposed
  packaging glue so a real `pve-meta` .deb keeps the patch applied across
  `pve-manager` upgrades and reverses it cleanly on removal/purge.
- `testing/check-tab.js` — the real-browser (headless Chromium/Puppeteer)
  verification script; `testing/tab-*.png` are screenshots from a passing
  run against the lab node (see "Real-browser verification" below).

## Mechanism

### 1. Why `dpkg-divert` instead of editing `index.html.tpl` in place

`index.html.tpl` belongs to the `pve-manager` package. Editing it directly
would be silently overwritten (or would produce dpkg conffile prompts) on
the next `pve-manager` upgrade, and there would be no clean way to get back
to the exact stock file. Instead:

```
dpkg-divert --package pve-meta --add --rename \
    --divert /usr/share/pve-manager/index.html.tpl.pve-meta-orig \
    /usr/share/pve-manager/index.html.tpl
```

This tells dpkg: "whenever anything (in practice, only `pve-manager`)
tries to install a file at `/usr/share/pve-manager/index.html.tpl`, put it
at `.../index.html.tpl.pve-meta-orig` instead, and leave the name
`index.html.tpl` free for someone else (us) to manage." `--rename`
additionally performs the initial move of the file that's already on disk.
From that point on:

- `/usr/share/pve-manager/index.html.tpl.pve-meta-orig` always holds the
  pristine, upstream `pve-manager` template — including across future
  `pve-manager` upgrades, since dpkg keeps routing its writes there.
- `/usr/share/pve-manager/index.html.tpl` (the name pveproxy actually
  serves) is ours to (re)write as "pristine original + one extra
  `<script>` tag".

This is the standard, fully reversible pattern for this class of local
modification — `dpkg-divert --remove --rename` undoes it exactly.

### 2. The one-line patch

`apply` regenerates `index.html.tpl` from the pristine
`index.html.tpl.pve-meta-orig`, inserting exactly one line immediately
after the existing `pvemanagerlib.js` `<script>` tag:

```html
<script type="text/javascript" src="/pve2/js/pvemanagerlib.js?ver=[% version %]"></script>
<script type="text/javascript" src="/pve2/js/pve-meta-loader.js"></script>
```

That anchor line (matched literally on the substring
`src="/pve2/js/pvemanagerlib.js`) is the only place `index.html.tpl` is
touched. `apply` validates the freshly rendered file before installing it:
the loader `<script>` tag must appear **exactly once**, `</body>` must
still be present, and the `pvemanagerlib.js` tag must still be there — a
cheap but effective "did this render into garbage" check, since fully
exercising PVE's own `Template::process()` server-side isn't practical
from this script.

### 3. Serving the loader — no `pveproxy.pm` changes needed

`PVE::Service::pveproxy` (`add_dirs()` calls in `run()`) already maps:

```
/pve2/js/  ->  /usr/share/pve-manager/js/
```

(alongside `/pve2/css/`, `/pve2/images/`, etc. — see
`/usr/share/perl5/PVE/Service/pveproxy.pm`). So `apply` simply copies
`pve-meta-loader.js` into `/usr/share/pve-manager/js/pve-meta-loader.js`,
and it is immediately reachable, unauthenticated, at
`/pve2/js/pve-meta-loader.js` — exactly like `pvemanagerlib.js` itself.
**No changes to pveproxy.pm or its systemd unit are required or made.**

The Metadata tab's iframe (`/pve2/js/pve-meta-ui/index.html?...`) relies
on this exact same mapping: the pve-meta frontend's static files are
expected to live at `/usr/share/pve-manager/js/pve-meta-ui/`, a
subdirectory of the same tree pveproxy already serves as `/pve2/js/` -
again, no pveproxy.pm changes needed. Installing those files is a
separate deliverable from `pve-meta-patch` (which only ever installs
`pve-meta-loader.js`); until they exist, the iframe correctly points at
the right same-origin path but gets a "no such file" error from pveproxy
- see "Real-browser verification" below.

### 4. The ExtJS override

This is the part most likely to break on a future PVE release, so it's
documented in full.

`pvemanagerlib.js` defines (in this order, all synchronously at parse
time, since it's one big classic-toolkit file with no async
`Ext.require`):

```js
Ext.define('PVE.panel.Config', {
    extend: 'Ext.panel.Panel',
    ...
    initComponent: function () {
        var me = this;
        me.items = me.items || [];
        if (me.showSearch) { me.items.unshift({ ...search tab... }); }
        me.savedItems = {};
        me.store = Ext.create('Ext.data.TreeStore', ...);
        me.insertNodes(me.items);   // turns me.items into the left-hand
                                     // treelist + lazily-built card layout
        delete me.items;
        me.defaults = ...;
        me.callParent();            // Ext.panel.Panel.initComponent
        ...
    },
});

Ext.define('PVE.lxc.Config',  { extend: 'PVE.panel.Config', initComponent: function () {
    var me = this;
    me.items = [ {itemId: 'summary', ...} ];
    me.items.push({itemId: 'resources', ...});
    ... // ~10 more me.items.push(...) calls
    me.callParent();   // <- calls PVE.panel.Config.initComponent, LAST
}});

Ext.define('PVE.qemu.Config', { extend: 'PVE.panel.Config', initComponent: function () { ... same shape ... }});
Ext.define('PVE.dc.Config',   { extend: 'PVE.panel.Config', initComponent: function () { ... same shape ... }});
```

Two things fall out of this:

1. **`PVE.panel.Config` is not a tab panel.** It's a `card`-layout panel
   with a custom left-hand `treelist` for navigation. `me.items` is a
   plain array of `{itemId, title, iconCls, xtype, ...}` *descriptors*
   (not live components) that `PVE.panel.Config.initComponent` consumes:
   it builds a tree node per item, stashes the raw config in
   `me.savedItems[itemId]`, and only instantiates (`me.add(...)`) the
   config for whichever item is currently selected. So "adding a tab"
   here really means "pushing one more descriptor onto `me.items` before
   it gets consumed."
2. **All three classes (`PVE.lxc.Config`, `PVE.qemu.Config`,
   `PVE.dc.Config`) build the entirety of their own `me.items` first, and
   call `me.callParent()` as the very last statement of their own
   `initComponent`.** `me.callParent()` resolves dynamically to whatever
   `PVE.panel.Config.prototype.initComponent` currently is — which is
   exactly the hook point `Ext.override` gives us.

So the loader needs exactly one interception point on the shared base
class — but **not** via the global `Ext.override(cls, {...})` shim, and
**not** using `this.callParent(...)` inside the replacement. An earlier
version of this loader did exactly that:

```js
// DO NOT DO THIS - see below for why.
Ext.override(PVE.panel.Config, {
    initComponent: function () {
        var me = this;
        var tabItem = metadataTabFor(me);
        if (tabItem) { me.items = me.items || []; me.items.push(tabItem); }
        return this.callParent(arguments);
    },
});
```

This looked correct, matched the pattern PVE's own `proxmoxlib.js` uses
elsewhere (`Ext.override(Ext.data.Store, {...})`,
`Ext.override(Ext.Msg, {...})`), and passed a headless Node.js stub of
the class hierarchy — but **broke in a real browser**. Selecting any VM
or CT (or the Datacenter) rendered a completely empty content area, with:

```
TypeError: Cannot read properties of null (reading 'apply')
    at constructor.callParent (ext-all.js:22:97949)
    at constructor.initComponent (pve-meta-loader.js)
    at constructor.callParent (ext-all.js:22:97949)
    at constructor.initComponent (pvemanagerlib.js:...)   <- PVE.qemu.Config's own initComponent calling me.callParent()
```

The legacy global `Ext.override(cls, obj)` compatibility shim in this
build of ExtJS 7 classic does not reliably wire up the `$previous` link
that `callParent()` needs when replacing a method this way - calling
`this.callParent(arguments)` from inside such an override can throw,
which aborts the *caller's* `initComponent` entirely (here,
`PVE.qemu.Config`'s/`PVE.lxc.Config`'s own `initComponent`, since it's
literally the last line of that function) - turning "add a tab" into "the
whole config panel never renders." A headless stub that models
`Ext.override`/`callParent` as *working* correctly cannot catch this; it
was only found by testing in an actual browser (see "Real-browser
verification" below).

**The fix**: patch `PVE.panel.Config.prototype.initComponent` directly,
capturing the original function in a closure and invoking it with a
plain `Function.prototype.apply()` - no Ext class-system machinery
involved at all, so there is nothing for it to fail to wire up:

```js
var configProto = PVE.panel.Config.prototype;
var origInitComponent = configProto.initComponent;

configProto.initComponent = function () {
    var me = this;
    try {
        var tabItem = metadataTabFor(me);   // null unless me is an
                                             // lxc/qemu/dc config panel
        if (tabItem) {
            me.items = me.items || [];
            me.items.push(tabItem);
        }
    } catch (e) {
        console.warn('[pve-meta] failed to inject Metadata tab for this panel, continuing without it', e);
    }
    // ALWAYS call the real, original initComponent - our logic above
    // must never be able to prevent the actual PVE config panel from
    // being built.
    return origInitComponent.apply(me, arguments);
};
```

Call order for e.g. opening an LXC's config panel is otherwise unchanged
from the original design:

1. `Ext.create('PVE.lxc.Config', {...})` → `PVE.lxc.Config`'s own
   (untouched) `initComponent` runs, builds the real ~10 tabs into
   `me.items`, and finally calls `me.callParent()`.
2. `me.callParent()` resolves dynamically to whatever
   `PVE.panel.Config.prototype.initComponent` currently is - our patched
   version. `me.items` already contains every real tab; we push one more
   (`itemId: 'pvemeta'`) onto the end, wrapped in `try/catch` so any
   failure here degrades to a console warning, never an exception.
3. We call `origInitComponent.apply(me, arguments)` - the *original*
   `PVE.panel.Config.initComponent`, captured in closure before we
   overwrote the prototype slot - which is what actually turns `me.items`
   (now including ours) into treelist nodes + `savedItems`, then deletes
   `me.items`. This call happens unconditionally, outside the `try`, so
   nothing our own logic does can skip it.

Crucially, step 2's `me.callParent()` is *not* our code - it's real,
built-in Ext.js machinery inside `PVE.lxc.Config`'s own unmodified
`initComponent`, dispatching (via the normal prototype chain, not the
global override shim) to whatever function currently sits at
`PVE.panel.Config.prototype.initComponent`. That part of the class
hierarchy was never the problem; only *our own* replacement's internals
(global `Ext.override` + `this.callParent(...)`) were.

Panel type is distinguished by the instance's fully-qualified Ext class
name (`PVE.lxc.Config` / `PVE.qemu.Config` / `PVE.dc.Config`), read via
`me.$className` / `me.self.getName()` / `Ext.getClassName(me)` (whichever
is available — all three are tried). Any other `PVE.panel.Config`
subclass (node/pool/storage/sdn/... config panels) is left untouched.

For LXC/QEMU, the vmid comes from `me.pveSelNode.data.vmid`, exactly the
same field `PVE.lxc.Config`/`PVE.qemu.Config`'s own `initComponent` reads
(`var vm = me.pveSelNode.data; ... var vmid = vm.vmid;`). If it's missing
for some reason, the loader logs a warning and skips adding the tab for
that one panel instance — it never throws.

### 5. Theme detection

PVE's own color-theme picker (`Proxmox.window.ThemeEditWindow` in
`proxmoxlib.js`) stores the choice in a cookie:

```js
cookieName: 'PVEThemeCookie',   // values come from Proxmox.Utils.theme_map:
                                //   'crisp'         -> light theme
                                //   'proxmox-dark'  -> dark theme
                                //   '__default__' (or unset)  -> "auto"
```

The loader reads this cookie the same way PVE itself does
(`Ext.util.Cookies.get('PVEThemeCookie')`, with a manual `document.cookie`
regex fallback if `Ext.util.Cookies` isn't available) and maps it to the
iframe's `theme` query param:

- `proxmox-dark` → `theme=dark`
- `crisp` → `theme=light`
- anything else (unset, `__default__`, a future theme name) → follow
  `window.matchMedia('(prefers-color-scheme: dark)')`, exactly like PVE's
  own chart/gauge widgets do in `checkThemeColors()`.

### 6. Feature detection — the UI must never break

Before doing anything, the loader checks, in order:

1. `window.PveMetaLoaded` truthy → already applied, no-op (double-include
   guard).
2. `typeof Ext === 'undefined'` → `console.warn`, stop.
3. `PVE.panel.Config.prototype.initComponent` not a function (covers
   `PVE`/`PVE.panel`/`PVE.panel.Config` all being missing too) →
   `console.warn`, stop.

Only after both pass does it patch
`PVE.panel.Config.prototype.initComponent`, and that patching itself is
wrapped in `try/catch`. Inside the replacement, per panel instance,
class-name resolution and vmid lookup are both wrapped in `try/catch` and
fail closed (skip that one tab, `console.warn`) - and, as described
above, the original `initComponent` is invoked unconditionally afterward
regardless of what happened in our own logic, so a bug in `metadataTabFor`
can at worst cost that one panel its Metadata tab, never the panel
itself. `window.PveMetaLoaded = true` is only set after the prototype
patch is installed successfully.

This was exercised two ways:

1. A small headless Node.js harness (a faithful stub of
   `PVE.panel.Config` / `PVE.lxc.Config` / `PVE.qemu.Config` /
   `PVE.dc.Config`'s real `initComponent`/`callParent` shape - critically,
   *without* implementing `Ext.override` at all, to prove the loader no
   longer depends on it - plus cookies and `matchMedia`) that `eval()`s
   the real `pve-meta-loader.js` and asserts:
   - the `pvemeta` item is injected for LXC, QEMU and DC panels, in the
     correct position (after all real tabs), with `Ext.override`
     undefined the whole time;
   - the iframe `src` is exactly
     `/pve2/js/pve-meta-ui/index.html?vmid=<id>&theme=<light|dark>` (guests)
     or `?dc=1&theme=<light|dark>` (datacenter) - a bare relative path,
     with no host/port of its own;
   - theme resolves to `dark` for `PVEThemeCookie=proxmox-dark` and to
     `light` otherwise (with `matchMedia` stubbed to non-matching);
   - an LXC panel with no `vmid` on `pveSelNode.data` gets **no** metadata
     tab (and logs a warning) instead of throwing;
   - if `PVE.panel.Config` doesn't exist at all (simulating a future
     pve-manager release that restructured the class), the loader logs a
     warning, never touches any prototype, and never sets
     `window.PveMetaLoaded`.
2. A real headless-Chromium (Puppeteer) session against the actual
   patched lab node, driving the actual PVE UI - see "Real-browser
   verification" below. This is what caught the `Ext.override`/
   `callParent` bug above in the first place; the headless-Node stub,
   because it modeled `callParent` as working, did not.

## `pve-meta-patch` subcommands

```
pve-meta-patch apply    # divert (if needed) + write patched template + install loader
pve-meta-patch remove   # restore pristine template + remove loader + remove diversion
pve-meta-patch verify [pvemanagerlib.js] [index.html.tpl]
                         # grep-based anchor check; run before/without applying
pve-meta-patch status   # report whether the patch is currently applied
```

`verify`'s anchors:

- `Ext.define('PVE.panel.Config'` — the override target.
- `Ext.define('PVE.lxc.Config'`
- `Ext.define('PVE.qemu.Config'`
- `Ext.define('PVE.dc.Config'`
- literal `src="/pve2/js/pvemanagerlib.js` in `index.html.tpl` — the
  insertion anchor.

`verify` defaults to the installed `pvemanagerlib.js` and (if a diversion
is already active) the pristine backup, else the live template; it can
also be pointed at arbitrary files, e.g. to sanity-check a `pve-manager`
package's contents before upgrading.

`apply` refuses to touch anything if the `pvemanagerlib.js` anchors are
missing, and refuses to touch `index.html.tpl` if it's diverted by a
*different* package. `remove` likewise refuses if the diversion isn't
owned by `pve-meta`.

### A `dpkg-divert` gotcha worth calling out

`dpkg-divert --remove --rename` aborts with *"rename involves overwriting
... not allowed"* if the "real" filename currently holds anything other
than nothing (per `dpkg-divert --help`: *"dpkg-divert will abort operation
in case the destination file already exists"*) — which it always does
here, since that's where our patched template lives. `remove` therefore
does `rm -f index.html.tpl` immediately before calling `dpkg-divert
--remove --rename`, so the rename-back onto a clear destination succeeds;
if `dpkg-divert` still fails for some other reason, it falls back to
copying the pristine backup into place manually so the host is never left
without an `index.html.tpl`.

## Packaging proposal (`debian/`)

- `pve-meta.triggers`:
  ```
  interest-noawait /usr/share/pve-manager/index.html.tpl
  interest-noawait /usr/share/pve-manager/js/pvemanagerlib.js
  ```
  Both paths are declared by their *nominal* (dpkg-recorded) name; dpkg
  activates path-based triggers on that nominal name even though
  `index.html.tpl`'s actual bytes get redirected to the diverted path by
  `pve-manager`'s own unpacking — this is the standard way
  divert-and-patch packages stay in sync with the package they're
  patching.
- `postinst`: on `configure` and on `triggered`, re-runs
  `pve-meta-patch apply` (idempotent: reuses the existing diversion,
  re-renders from the current pristine original). Never fails the
  package install if the patch application itself fails — it warns
  instead, consistent with the loader's own fail-safe design.
- `postrm`: on `remove`/`purge`, runs `pve-meta-patch remove` so the host
  reverts to a fully stock `pve-manager` on package removal.

These are proposals/snippets (`#DEBHELPER#` marker included) to fold into
the real `pve-meta` package's `debian/` directory — this repo does not
build an actual `.deb`.

## What was tested on the lab node (10.10.10.154, disposable)

- `pveversion`: `pve-manager/9.2.11/f6997e698c7933ea`.
- Fetched and read the real `/usr/share/pve-manager/js/pvemanagerlib.js`,
  `/usr/share/javascript/proxmox-widget-toolkit/proxmoxlib.js`,
  `/usr/share/pve-manager/index.html.tpl`, and
  `/usr/share/perl5/PVE/Service/pveproxy.pm` to derive every anchor and
  mechanism described above (not guessed/assumed).
- Copied `pve-meta-loader.js` + `pve-meta-patch` to the node and ran, as
  root:
  1. `pve-meta-patch verify` — all anchors present.
  2. `pve-meta-patch status` — correctly reports "not applied".
  3. `pve-meta-patch apply` — diversion created, patched template
     written, loader installed; `diff` against the pristine backup shows
     **exactly** the one expected inserted line.
  4. `systemctl restart pveproxy`, then from outside the node:
     `curl -sk https://10.10.10.154:8006/` → `HTTP 200`, response body
     contains the loader `<script>` tag exactly once, immediately after
     the `pvemanagerlib.js` tag; `curl -sk
     https://10.10.10.154:8006/pve2/js/pve-meta-loader.js` → `HTTP 200`,
     byte-identical to the local `pve-meta-loader.js`.
  5. `pve-meta-patch remove` — `md5sum index.html.tpl` before `apply` and
     after `remove` are **identical**
     (`c6ed9775cab14e33e3d1b6c4ece1d706`), diversion gone, loader file
     gone.
  6. `pve-meta-patch apply` again — reapplied cleanly, `pveproxy`
     restarted, left in the **applied** state (confirmed via `curl` and
     `pve-meta-patch status`) for interactive browser verification.
- Ran the loader's actual logic (not a rewrite of it) headlessly under
  Node.js against a hand-built stub of Ext JS's class hierarchy and PVE's
  real `PVE.panel.Config` / `PVE.lxc.Config` / `PVE.qemu.Config` /
  `PVE.dc.Config` `initComponent` shapes (see "Feature detection" above
  for what was asserted) - **and** in a real headless-Chromium browser
  against the actually-patched lab node (see next section), which is what
  caught a real bug the stub could not.
- Never touched `arkantos` (10.10.10.10) — all commands ran against
  10.10.10.154 only, reached via `arkantos` purely as an SSH jump host /
  relay for file transfer.

## Real-browser verification (headless Chromium / Puppeteer)

A build host with headless Chromium + `puppeteer-core` was used to drive
the actual PVE UI end-to-end against 10.10.10.154: log in via the
`/api2/json/access/ticket` API, set the resulting `PVEAuthCookie`, load
`https://10.10.10.154:8006/`, then interact with the page exactly like a
person would - expand the resource tree via a real Puppeteer
`ElementHandle.click()` on the guest's row, read the resulting config
panel's own left-hand nav, click the "Metadata" entry, and inspect the
resulting `<iframe>`. The test script lives at
[`testing/check-tab.js`](testing/check-tab.js); representative
screenshots are in the same directory.

### The bug this caught

The first version of the loader used `Ext.override(PVE.panel.Config,
{...})` with `this.callParent(arguments)` inside the replacement (see
"The ExtJS override" above for the full story). In the real browser,
selecting **any** guest or the Datacenter rendered a completely empty
content area, with a console error showing `this.callParent` throwing
`TypeError: Cannot read properties of null (reading 'apply')` from
*inside* `PVE.qemu.Config`'s/`PVE.lxc.Config`'s own `initComponent` -
i.e. our override broke the base config panel entirely, for every guest
type, the opposite of "never break the UI." The headless Node.js stub
(which modeled `Ext.override`/`callParent` as working) never caught this
- only testing against real ExtJS did.

**Fix**: patch `PVE.panel.Config.prototype.initComponent` directly
(capture the original, call it via `.apply()`, no `Ext.override`/
`callParent` involved at all - see above for the code). Re-tested in the
same real browser after the fix, and again after switching the iframe
from a cross-origin `https://<host>:8007/ui/...` URL to the final,
same-origin, pveproxy-served `/pve2/js/pve-meta-ui/index.html?...`
(relative URL, no scheme/host/port of its own):

| Target | Result | Normal tabs rendered | Metadata tab | iframe `src` (attribute, as written) |
|---|---|---|---|---|
| QEMU VM 300 (`test-vm-300`) | **PASS** | 16 (Summary, Console, Hardware, Cloud-Init, Options, Task History, Monitor, Backup, Replication, Snapshots, Firewall, Options, Alias, IPSet, Log, Permissions) | present, at the end of the nav | `/pve2/js/pve-meta-ui/index.html?vmid=300&theme=light` (949×614) |
| LXC CT 200 (`test-ct-200`) | **PASS** | 16 (Summary, Console, Resources, Network, DNS, Options, Task History, Backup, Replication, Snapshots, Firewall, Options, Alias, IPSet, Log, Permissions) | present, at the end of the nav | `/pve2/js/pve-meta-ui/index.html?vmid=200&theme=light` (949×614) |
| Datacenter | **PASS** | 41 (Search, Summary, Notes, Cluster, Ceph, ... Notifications, Support) | present, at the end of the nav | `/pve2/js/pve-meta-ui/index.html?dc=1&theme=light` (898×614) |

All three: `window.PveMetaLoaded === true`, the iframe's `src` attribute
verified to be a bare relative path (no `scheme://` prefix) that resolves
(per the browser's own URL resolution) to `https://10.10.10.154:8006/...`
- the same origin pve-manager itself is served from, not a separate
host/port - and zero page/console **errors** during the actual target
interaction, screenshots saved (`testing/tab-qemu-300.png`,
`testing/tab-lxc-200.png`, `testing/tab-dc.png`). Since the pve-meta UI's
static files are not installed as part of this patch, that iframe
currently gets a `500 (no such file '/pve2/js/pve-meta-ui/index.html')`
from pveproxy - visible as a benign `console.error` "Failed to load
resource" line (recorded in `consoleLogs`, deliberately **not** counted
as a test failure - see the comment at the top of `check-tab.js`) and, in
the screenshots, as pveproxy's own plain-text error message rendered
inside the tab's iframe. The test asserts the `src` and the tab/nav
rendering only, exactly as expected until those files are deployed
alongside the loader. Dark theme was also confirmed end-to-end by setting
`PVEThemeCookie=proxmox-dark` before load: `src` came back with
`theme=dark` for the same VM. Re-run 3 more times back-to-back for VM 300
with identical (passing, zero-error) results.

### A second, pre-existing bug found along the way (not caused by this patch)

While debugging the above, headless clicking intermittently (~1-in-5 in
this lab) reproduced a *second*, unrelated error on the **initial**,
automatic Datacenter view PVE shows on page load:

```
TypeError: Cannot read properties of undefined (reading 'Mapping.Audit')
    at initComponent (pvemanagerlib.js:33014)   <- PVE.dc.Config's own
                                                    "Resource Mappings"
                                                    tab-building code
```

This is a race between the async `GuiCap` capability fetch and PVE's own
automatic initial tree selection: if the Datacenter panel auto-constructs
before `GuiCap.mapping` is populated, `PVE.dc.Config`'s own code (not
ours - the check is `caps.mapping['Mapping.Audit'] || ...` guarding its
"Resource Mappings" tab) throws. **This was confirmed to reproduce
identically with `pve-meta-patch remove` run and pveproxy restarted -
i.e. on a completely stock, unpatched pve-manager 9.2.11** - so it is
pre-existing and out of scope for this patch, not something
`pve-meta-loader.js` introduced. `testing/check-tab.js` works around it
by waiting for the initial view's nav to render with zero errors before
touching the actual target under test, retrying with a fresh page load
(bounded at 5 attempts) if it doesn't; this is purely a test-harness
concern; it does not change how the loader itself behaves.

## Known limitations / open doubts

- **iframe `src` is computed once, at tab-creation time.** If the admin
  changes the color theme without a full page reload, or if the tab is
  created before the value of `PVEThemeCookie` is what you'd expect, the
  embedded page won't re-theme live. Given PVE's own theme switcher does
  a full `window.location.reload()` after setting the cookie
  (`applyTheme()` in `proxmoxlib.js`), this should be a non-issue in
  practice.
- **The pve-meta UI's static files are not installed by this patch.**
  `pve-meta-patch` only ever installs `pve-meta-loader.js`; the actual
  `pve-meta` frontend (whatever lives at
  `/pve2/js/pve-meta-ui/index.html` and its assets) is a separate
  deliverable that needs to be dropped into
  `/usr/share/pve-manager/js/pve-meta-ui/` (again, no pveproxy changes
  needed - it's already inside the directory pveproxy serves as
  `/pve2/js/`). Until that happens, the Metadata tab renders correctly
  but its iframe shows pveproxy's own "no such file" error - confirmed in
  real-browser testing (see above) and expected, not a bug in this patch.
  Because it's now a same-origin relative URL rather than a separate
  host/port, there is no mixed-content or cross-origin-reachability
  concern to worry about once those files exist.
- **Detection by literal class name string** (`'PVE.lxc.Config'` etc.) is
  robust to code motion inside those files but not to a hypothetical
  future PVE renaming these classes; the `verify` subcommand exists
  precisely to catch that *before* `apply` runs, and the loader itself
  degrades to a warning-and-no-op rather than breaking anything if it
  ever happens silently in the field.
