/*
 * pve-ext-loader.js
 *
 * The generic half of the pve-ext UI-page seam (see pve-ext/README.md).
 * Fetches GET /api2/json/ext/pages (served by PVE::API2::Ext from
 * /usr/share/pve-ext/pages/*.json) and, for each manifest, adds one tab -
 * a same-origin iframe, layout: 'fit' - to every matching target config
 * panel (PVE.lxc.Config / PVE.qemu.Config / PVE.node.Config / PVE.dc.Config).
 *
 * This generalizes an early, single-purpose prototype (see
 * docs/LIFECYCLE-PATCHES.md, superseded): same script-tag injection point
 * (right after pvemanagerlib.js in index.html.tpl), same
 * feature-detection posture, and - this is the part that must never be
 * "simplified" back - the *exact* same
 * PVE.panel.Config.prototype.initComponent patch technique: capture the
 * original prototype method in a closure and invoke it with a plain
 * Function.prototype.apply(), no Ext class-system machinery involved at
 * all. An earlier version used the global Ext.override(cls, {...}) shim
 * with this.callParent(arguments) inside the replacement instead, and
 * that combination throws in real ExtJS 7 classic (as shipped by PVE),
 * silently killing the *whole* config panel for every guest. The
 * capture-and-apply fix is proven in a real browser against real
 * pve-manager and must not be changed without re-doing that verification.
 *
 * A manifest's tab content is either a same-origin iframe ("url") or a
 * native ExtJS panel class ("script" + "xtype"): the script is inserted
 * into the document as a <script> tag (once per URL, cached), and once
 * its xtype resolves to a defined class (waitForXtype() below - verified
 * against real ExtJS 7 classic that this needs the "widget." alias
 * lookup, not a bare Ext.ClassManager.isCreated(xtype)), the tab
 * instantiates "{ xtype, ...config }" in place of the iframe, with vmid/
 * type/node/dc passed as config properties (see buildInstanceConfig()).
 * PVE::API2::Ext validates that a manifest declares exactly one of the
 * two forms; this file trusts that already holds.
 *
 * Design goal: NEVER break the PVE UI. Every seam this script depends on
 * (Ext/PVE class shapes, the /ext/pages API, one page manifest's shape) is
 * individually try/catch-guarded; a failure anywhere logs to the console
 * (prefixed "[pve-ext]") and degrades to "that one thing doesn't happen",
 * never to a broken page.
 *
 * Plain ES2017, no build step, no external dependencies.
 */
(function () {
    'use strict';

    if (window.PveExtLoaded) {
        return;
    }

    var LOG_PREFIX = '[pve-ext]';
    var PAGES_URL = '/api2/json/ext/pages';

    // PVE.<target>.Config class name -> the loader's own target keyword,
    // and the Ext.state.Manager.get('GuiCap') top-level key that carries
    // the privileges relevant to that target (see checkRequires() below).
    var TARGETS = {
        'PVE.lxc.Config': { target: 'lxc', capKey: 'vms' },
        'PVE.qemu.Config': { target: 'qemu', capKey: 'vms' },
        'PVE.node.Config': { target: 'node', capKey: 'nodes' },
        'PVE.dc.Config': { target: 'dc', capKey: 'dc' },
    };

    function warn(msg, err) {
        try {
            if (err) {
                // eslint-disable-next-line no-console
                console.warn(LOG_PREFIX + ' ' + msg, err);
            } else {
                // eslint-disable-next-line no-console
                console.warn(LOG_PREFIX + ' ' + msg);
            }
        } catch (e) {
            /* console unavailable, nothing we can do */
        }
    }

    function info(msg) {
        try {
            // eslint-disable-next-line no-console
            console.info(LOG_PREFIX + ' ' + msg);
        } catch (e) {
            /* ignore */
        }
    }

    // --- Feature detection ---------------------------------------------

    if (typeof Ext === 'undefined') {
        warn('Ext JS not found - skipping extension tab injection.');
        return;
    }

    if (
        typeof PVE === 'undefined' ||
        typeof PVE.panel === 'undefined' ||
        typeof PVE.panel.Config === 'undefined' ||
        typeof PVE.panel.Config.prototype === 'undefined' ||
        typeof PVE.panel.Config.prototype.initComponent !== 'function'
    ) {
        warn('PVE.panel.Config (or its initComponent) not found - pvemanagerlib.js may have changed. Skipping.');
        return;
    }

    // --- Theme detection (same logic as pve-meta-loader.js) -------------
    //
    // Mirrors PVE's own color-theme picker (Proxmox.window.ThemeEditWindow
    // in proxmoxlib.js), which stores the choice in the PVEThemeCookie
    // cookie: 'crisp' -> light, 'proxmox-dark' -> dark, anything else
    // (unset/'__default__'/a future theme) -> follow the OS/browser
    // preference, same as PVE's own charts/gauges (checkThemeColors()).
    function getPveTheme() {
        var cookieVal = '';
        try {
            if (typeof Ext !== 'undefined' && Ext.util && Ext.util.Cookies && typeof Ext.util.Cookies.get === 'function') {
                cookieVal = Ext.util.Cookies.get('PVEThemeCookie') || '';
            } else {
                var match = document.cookie.match(/(?:^|;\s*)PVEThemeCookie=([^;]*)/);
                cookieVal = match ? decodeURIComponent(match[1]) : '';
            }
        } catch (e) {
            warn('failed to read PVEThemeCookie, falling back to OS preference', e);
        }

        if (cookieVal === 'proxmox-dark') {
            return 'dark';
        }
        if (cookieVal === 'crisp') {
            return 'light';
        }

        try {
            if (window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches) {
                return 'dark';
            }
        } catch (e) {
            /* matchMedia unsupported - default to light below */
        }
        return 'light';
    }

    // --- Capability check (client-side filter only; each page's own API
    //     is the actual server-side enforcement - see README.md) --------
    //
    // manifest.requires is e.g. { vms: ["VM.Audit"], dc: ["Sys.Audit"] }:
    // per-capability-category lists of privileges, all of which must be
    // present for the *category relevant to this target* (see TARGETS
    // above) for the tab to be added. A category the manifest doesn't
    // mention at all imposes no restriction for that target.
    function hasRequiredCaps(manifest, capKey) {
        var requires = manifest && manifest.requires;
        if (!requires || typeof requires !== 'object') {
            return true;
        }
        var need = requires[capKey];
        if (!need) {
            return true;
        }
        if (!Ext.isArray(need)) {
            warn('page "' + manifest.id + '": requires.' + capKey + ' is not an array, ignoring it (failing open)');
            return true;
        }

        var caps;
        try {
            caps = Ext.state.Manager.get('GuiCap');
        } catch (e) {
            warn('could not read GuiCap capabilities - skipping page "' + (manifest && manifest.id) + '"', e);
            return false;
        }
        var bucket = caps && caps[capKey];
        if (!bucket) {
            return need.length === 0;
        }
        for (var i = 0; i < need.length; i++) {
            if (!bucket[need[i]]) {
                return false;
            }
        }
        return true;
    }

    // --- GET /ext/pages, fetched once and cached ------------------------
    //
    // Deliberately lazy (fetched on first use, not at script-load time)
    // and deliberately synchronous: at script-load time (this file is
    // loaded on every index.html.tpl render, including the pre-login
    // screen) there is no authenticated session yet and the request would
    // just 401. By the time any PVE.lxc.Config / PVE.qemu.Config /
    // PVE.node.Config / PVE.dc.Config panel is actually *constructed*,
    // the user must already be logged in (the resource tree these panels
    // come from only exists post-login) - so deferring the fetch to that
    // point means it succeeds, and doing it synchronously means the tab
    // list is available in time for that very first panel's
    // initComponent, with no async/race complexity. This is a same-origin
    // call to pveproxy itself (typically sub-10ms); the one-time
    // synchronous-XHR cost is judged worth avoiding an entire
    // fetch-then-retroactively-patch-the-already-rendered-treelist design.
    // Cached after the first attempt (success or failure) - never retried
    // for the lifetime of the page, matching the "computed once" posture
    // pve-meta-loader.js already documents for its iframe src.
    var pagesCache = null;

    function fetchPagesOnce() {
        if (pagesCache !== null) {
            return pagesCache;
        }
        pagesCache = [];
        try {
            var xhr = new XMLHttpRequest();
            xhr.open('GET', PAGES_URL, false);
            xhr.setRequestHeader('Accept', 'application/json');
            xhr.send(null);
            if (xhr.status >= 200 && xhr.status < 300) {
                var body = JSON.parse(xhr.responseText);
                if (body && Ext.isArray(body.data)) {
                    pagesCache = body.data;
                } else {
                    warn('unexpected ' + PAGES_URL + ' response shape, ignoring it');
                }
            } else {
                warn(PAGES_URL + ' returned HTTP ' + xhr.status + ' - no extension tabs will be added this session');
            }
        } catch (e) {
            warn('failed to fetch ' + PAGES_URL + ' - no extension tabs will be added this session', e);
        }
        return pagesCache;
    }

    // --- Placeholder substitution ----------------------------------------

    // {query} is deliberately NOT handled here (see expandQueryPlaceholder,
    // below): it substitutes to an already-encoded query string, not a
    // single value, and must never be run through encodeURIComponent().
    function expandUrl(template, vars) {
        return String(template).replace(/\{(vmid|node|type|theme)\}/g, function (whole, name) {
            var v = vars[name];
            return v === undefined || v === null ? '' : encodeURIComponent(v);
        });
    }

    // {query} is itself an already-encoded query string, not a single
    // value - substitute it separately (raw, not re-encoded) after the
    // single-value placeholders are done.
    function expandQueryPlaceholder(template, query) {
        return String(template).replace(/\{query\}/g, query);
    }

    function buildQuery(target, vars) {
        var theme = encodeURIComponent(vars.theme);
        if (target === 'dc') {
            return 'dc=1&theme=' + theme;
        }
        if (target === 'node') {
            return 'node=' + encodeURIComponent(vars.node) + '&theme=' + theme;
        }
        // lxc / qemu
        return (
            'vmid=' +
            encodeURIComponent(vars.vmid) +
            '&type=' +
            encodeURIComponent(vars.type) +
            '&node=' +
            encodeURIComponent(vars.node) +
            '&theme=' +
            theme
        );
    }

    // ExtJS renders a panel's `title` as markup (Ext.panel.Title does not
    // HTML-encode it), so a manifest's title must be escaped before it
    // reaches buildTabItem below - manifests are root-owned today (see
    // pve-ext/README.md), but a page manifest is still attacker-adjacent
    // enough (any package that depends on pve-ext can drop one) to be
    // worth not trusting blindly.
    function escapeHtml(s) {
        return String(s).replace(/[&<>"']/g, function (c) {
            return (
                { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]
            );
        });
    }

    // iconCls ends up as a CSS class list, not text content, so HTML
    // escaping isn't the relevant defense - restrict it to characters
    // that are actually legal in a class-list attribute value instead,
    // falling back to the default icon for anything else.
    function sanitizeIconCls(cls) {
        var s = String(cls || '');
        return /^[A-Za-z0-9 _-]+$/.test(s) ? s : '';
    }

    function buildTabItem(manifest, src) {
        return {
            xtype: 'panel',
            itemId: 'pve-ext-' + manifest.id,
            title: escapeHtml(manifest.title),
            iconCls: sanitizeIconCls(manifest.iconCls) || 'fa fa-puzzle-piece',
            layout: 'fit',
            border: 0,
            items: [
                {
                    xtype: 'component',
                    autoEl: {
                        tag: 'iframe',
                        src: src,
                        style: 'border:0;width:100%;height:100%',
                    },
                },
            ],
        };
    }

    // --- "script" + "xtype" pages: a native ExtJS panel instead of an
    //     iframe -----------------------------------------------------
    //
    // Loads a page's script exactly once (per URL), regardless of how
    // many tabs/targets reference it, and caches success/failure so a
    // second reference never re-fetches or re-`<script>`-injects it.
    // Queues every caller's callback while a load is in flight.
    var scriptLoadState = {}; // src -> 'loaded' | 'error' | [pending callbacks]

    function loadScriptOnce(src, cb) {
        var state = scriptLoadState[src];
        if (state === 'loaded') {
            cb();
            return;
        }
        if (state === 'error') {
            cb(new Error('a previous attempt to load this script already failed'));
            return;
        }
        if (Ext.isArray(state)) {
            state.push(cb);
            return;
        }
        scriptLoadState[src] = [cb];
        try {
            var el = document.createElement('script');
            el.src = src;
            el.async = true;
            el.onload = function () {
                var callbacks = scriptLoadState[src] || [];
                scriptLoadState[src] = 'loaded';
                for (var i = 0; i < callbacks.length; i++) {
                    callbacks[i]();
                }
            };
            el.onerror = function () {
                var callbacks = scriptLoadState[src] || [];
                scriptLoadState[src] = 'error';
                for (var i = 0; i < callbacks.length; i++) {
                    callbacks[i](new Error('script failed to load: ' + src));
                }
            };
            document.head.appendChild(el);
        } catch (e) {
            var callbacks = scriptLoadState[src] || [];
            scriptLoadState[src] = 'error';
            callbacks.forEach(function (fn) {
                fn(e);
            });
        }
    }

    // Polls (simple setTimeout loop - no Ext.util.TaskManager dependency
    // needed for this) until the manifest's xtype resolves to a defined
    // class, or timeoutMs elapses.
    function waitForXtype(xtype, timeoutMs, cb) {
        var deadline = Date.now() + timeoutMs;
        function poll() {
            var created = false;
            try {
                // Ext.ClassManager.isCreated() takes a class *name*
                // ("Foo.bar.Panel"), not an xtype/alias - an xtype only
                // resolves to a name through the "widget." alias
                // namespace ExtJS registers as part of Ext.define(), via
                // getNameByAlias(). Verified against real ExtJS 7 classic:
                // isCreated(xtype) and isCreated('widget.' + xtype) both
                // always report false, even once the class is fully
                // defined and instantiable.
                var name = Ext.ClassManager && Ext.ClassManager.getNameByAlias('widget.' + xtype);
                created = !!(name && Ext.ClassManager.isCreated(name));
            } catch (e) {
                created = false;
            }
            if (created) {
                cb(true);
                return;
            }
            if (Date.now() >= deadline) {
                cb(false);
                return;
            }
            setTimeout(poll, 100);
        }
        poll();
    }

    // vmid/type/node/dc as config properties on the instantiated xtype,
    // matching whichever of them apply to this target - the same shape
    // buildQuery() above encodes into a query string for the iframe form.
    function buildInstanceConfig(target, vars) {
        var cfg = { type: vars.type };
        if (target === 'lxc' || target === 'qemu') {
            cfg.vmid = vars.vmid;
            cfg.node = vars.node;
        } else if (target === 'node') {
            cfg.node = vars.node;
        } else {
            cfg.dc = 1;
        }
        return cfg;
    }

    var SCRIPT_XTYPE_TIMEOUT_MS = 15000;

    function buildScriptTabItem(manifest, scriptSrc, instanceConfig) {
        return {
            xtype: 'panel',
            itemId: 'pve-ext-' + manifest.id,
            title: escapeHtml(manifest.title),
            iconCls: sanitizeIconCls(manifest.iconCls) || 'fa fa-puzzle-piece',
            layout: 'fit',
            border: 0,
            items: [{ xtype: 'component', html: '' }],
            listeners: {
                afterrender: function () {
                    var panel = this;
                    loadScriptOnce(scriptSrc, function (loadErr) {
                        if (loadErr) {
                            warn('page "' + manifest.id + '": failed to load script "' + scriptSrc + '"', loadErr);
                            return;
                        }
                        waitForXtype(manifest.xtype, SCRIPT_XTYPE_TIMEOUT_MS, function (ok) {
                            if (!ok) {
                                warn(
                                    'page "' +
                                        manifest.id +
                                        '": xtype "' +
                                        manifest.xtype +
                                        '" was never registered after loading "' +
                                        scriptSrc +
                                        '"',
                                );
                                return;
                            }
                            try {
                                if (panel.destroying || panel.destroyed) {
                                    return;
                                }
                                panel.removeAll(true);
                                panel.add(Ext.apply({ xtype: manifest.xtype }, instanceConfig));
                            } catch (e) {
                                warn('page "' + manifest.id + '": failed to instantiate xtype "' + manifest.xtype + '"', e);
                            }
                        });
                    });
                },
            },
        };
    }

    // Builds the list of tab item configs to add to a given PVE.panel.Config
    // instance, or [] if this instance's class isn't one of our targets, or
    // no manifest applies to it.
    function tabsFor(me) {
        var className = '';
        try {
            if (me && typeof me.$className === 'string' && me.$className) {
                className = me.$className;
            } else if (me && me.self && typeof me.self.getName === 'function') {
                className = me.self.getName();
            } else if (typeof Ext.getClassName === 'function') {
                className = Ext.getClassName(me) || '';
            }
        } catch (e) {
            warn('failed to determine component class name', e);
            return [];
        }

        var targetInfo = TARGETS[className];
        if (!targetInfo) {
            return []; // not a panel we care about (pool/storage/sdn/... config)
        }
        var target = targetInfo.target;

        var vars = { theme: getPveTheme() };
        if (target === 'lxc' || target === 'qemu') {
            var selData = me.pveSelNode && me.pveSelNode.data;
            vars.vmid = selData && selData.vmid;
            vars.node = selData && selData.node;
            vars.type = target;
            if (!vars.vmid || !vars.node) {
                warn('could not determine vmid/node for ' + className + ' - skipping all extension tabs for this panel.');
                return [];
            }
        } else if (target === 'node') {
            vars.node = me.pveSelNode && me.pveSelNode.data && me.pveSelNode.data.node;
            vars.type = 'node';
            if (!vars.node) {
                warn('could not determine node name for ' + className + ' - skipping all extension tabs for this panel.');
                return [];
            }
        } else {
            // dc
            vars.type = 'dc';
        }

        var query = buildQuery(target, vars);

        var pages = fetchPagesOnce();
        var items = [];
        for (var i = 0; i < pages.length; i++) {
            var manifest = pages[i];
            try {
                if (!manifest || !manifest.id || !manifest.title || !Ext.isArray(manifest.targets)) {
                    warn('ignoring malformed page manifest (missing id/title/targets): ' + JSON.stringify(manifest));
                    continue;
                }
                var isScriptForm = !!(manifest.script && manifest.xtype);
                if (!manifest.url && !isScriptForm) {
                    warn('ignoring page manifest "' + manifest.id + '": neither "url" nor "script"+"xtype" present');
                    continue;
                }
                if (manifest.targets.indexOf(target) === -1) {
                    continue;
                }
                if (!hasRequiredCaps(manifest, targetInfo.capKey)) {
                    continue; // user lacks a listed privilege - never add the tab
                }
                if (isScriptForm) {
                    var scriptSrc = expandUrl(manifest.script, vars);
                    scriptSrc = expandQueryPlaceholder(scriptSrc, query);
                    var instanceConfig = buildInstanceConfig(target, vars);
                    items.push(buildScriptTabItem(manifest, scriptSrc, instanceConfig));
                } else {
                    var src = expandUrl(manifest.url, vars);
                    src = expandQueryPlaceholder(src, query);
                    items.push(buildTabItem(manifest, src));
                }
            } catch (e) {
                warn('failed to build tab for page manifest "' + (manifest && manifest.id) + '", skipping it', e);
            }
        }
        return items;
    }

    // --- The actual patch -------------------------------------------------
    //
    // See the file header comment: patch PVE.panel.Config.prototype.
    // initComponent directly (capture the original, invoke it via a plain
    // .apply(), no Ext.override()/callParent() involved) - this is the one
    // part of pve-meta-loader.js this file must keep byte-for-byte
    // equivalent in spirit, proven against real ExtJS 7 classic.
    try {
        var configProto = PVE.panel.Config.prototype;
        var origInitComponent = configProto && configProto.initComponent;

        if (typeof origInitComponent !== 'function') {
            warn('PVE.panel.Config.prototype.initComponent is not a function - skipping.');
            return;
        }

        configProto.initComponent = function () {
            var me = this;
            try {
                var tabs = tabsFor(me);
                if (tabs.length) {
                    me.items = me.items || [];
                    for (var i = 0; i < tabs.length; i++) {
                        var tabItem = tabs[i];
                        if (!me.items.some(function (it) { return it && it.itemId === tabItem.itemId; })) {
                            me.items.push(tabItem);
                        }
                    }
                }
            } catch (e) {
                warn('failed to inject extension tab(s) for this panel, continuing without them', e);
            }
            // Always call the real, original initComponent - our logic
            // above must never be able to prevent the actual PVE config
            // panel from being built.
            return origInitComponent.apply(me, arguments);
        };
    } catch (e) {
        warn('failed to patch PVE.panel.Config.prototype.initComponent - extension tabs will not be available.', e);
        return;
    }

    window.PveExtLoaded = true;
    info('extension tab injection active.');
})();
