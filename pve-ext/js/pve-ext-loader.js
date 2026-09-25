/*
 * pve-ext-loader.js
 *
 * Fetches GET /api2/json/ext/pages (served by PVE::API2::Ext from
 * /usr/share/pve-ext/pages/*.json) and, for each manifest, adds one tab -
 * a native ExtJS panel built from its "script"+"xtype" - to every matching
 * target config panel (PVE.lxc.Config / PVE.qemu.Config / PVE.node.Config /
 * PVE.dc.Config). See pve-ext/README.md for the manifest format.
 *
 * The script tag is injected right after pvemanagerlib.js in
 * index.html.tpl. This is the part that must never be "simplified" back:
 * the *exact* PVE.panel.Config.prototype.initComponent patch technique of
 * capturing the original prototype method in a closure and invoking it
 * with a plain Function.prototype.apply(), no Ext class-system machinery
 * involved at all. The obvious alternative - Ext.override(cls, {...})
 * with this.callParent(arguments) inside the replacement - cannot work
 * from this file: ExtJS resolves what callParent() is to call through
 * Function.prototype.caller, and reading .caller from inside a
 * strict-mode function throws a TypeError. Everything below is
 * 'use strict', so the override would throw and silently kill the
 * *whole* config panel for every guest. (Dropping 'use strict' is not
 * the fix.) The capture-and-apply technique is proven in a real browser
 * against real pve-manager and must not be changed without re-doing that
 * verification.
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

    // PVE.<target>.Config class name -> the loader's own target keyword.
    var TARGETS = {
        'PVE.lxc.Config': 'lxc',
        'PVE.qemu.Config': 'qemu',
        'PVE.node.Config': 'node',
        'PVE.dc.Config': 'dc',
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

    // --- Theme detection -------------------------------------------------
    // Mirrors PVE's PVEThemeCookie ('crisp' -> light, 'proxmox-dark' ->
    // dark, anything else -> follow the OS/browser preference).
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

    // --- GET /ext/pages, prefetched once, asynchronously -----------------
    //
    // Started right here at script-load time: this file is injected after
    // pvemanagerlib.js and before the app starts, so the request is
    // normally answered long before the first config panel is built, and
    // that panel gets its tabs from initComponent like any other. A panel
    // built while the prefetch is still in flight registers a waiter and
    // is given its tabs when the answer arrives (addTabsWhenPagesArrive,
    // below, which has to take a different seam). Never synchronous:
    // an XHR with async=false here froze the UI thread for exactly as
    // long as pveproxy took to answer.
    //
    // index.html.tpl is also rendered pre-login, so the prefetch can
    // legitimately 401. A failed request is therefore not cached as an
    // empty list for the session: the next config panel - which can only
    // be built post-login - asks again.
    var pagesState = 'idle'; // 'idle' | 'loading' | 'ready' | 'failed'
    var pagesList = [];
    var pagesWaiters = [];

    function notifyPagesWaiters(pages) {
        var waiters = pagesWaiters;
        pagesWaiters = [];
        for (var i = 0; i < waiters.length; i++) {
            try {
                waiters[i](pages);
            } catch (e) {
                warn('a panel waiting for ' + PAGES_URL + ' failed to handle the answer', e);
            }
        }
    }

    function pagesFailed(reason, err) {
        pagesState = 'failed';
        warn(reason + ' - no extension tabs until a config panel asks again', err);
        notifyPagesWaiters([]);
    }

    function requestPages() {
        try {
            var xhr = new XMLHttpRequest();
            xhr.open('GET', PAGES_URL, true);
            xhr.setRequestHeader('Accept', 'application/json');
            xhr.onload = function () {
                var pages = null;
                var reason = PAGES_URL + ' returned HTTP ' + xhr.status;
                try {
                    if (xhr.status >= 200 && xhr.status < 300) {
                        var body = JSON.parse(xhr.responseText);
                        if (body && Ext.isArray(body.data)) {
                            pages = body.data;
                        } else {
                            reason = 'unexpected ' + PAGES_URL + ' response shape';
                        }
                    }
                } catch (e) {
                    reason = PAGES_URL + ' did not answer with parsable JSON';
                }
                if (pages) {
                    pagesState = 'ready';
                    pagesList = pages;
                    notifyPagesWaiters(pagesList);
                } else {
                    pagesFailed(reason);
                }
            };
            xhr.onerror = function () {
                pagesFailed(PAGES_URL + ' could not be reached');
            };
            xhr.send(null);
        } catch (e) {
            pagesFailed('failed to request ' + PAGES_URL, e);
        }
    }

    function startPagesPrefetch() {
        if (pagesState === 'idle' || pagesState === 'failed') {
            pagesState = 'loading';
            requestPages();
        }
    }

    // Calls cb(pages) - synchronously if the prefetch is already done,
    // otherwise once it is (with [] if it gave up).
    function whenPagesReady(cb) {
        if (pagesState === 'ready') {
            cb(pagesList);
            return;
        }
        pagesWaiters.push(cb);
        startPagesPrefetch();
    }

    // --- Placeholder substitution ----------------------------------------

    // {query} is handled separately (expandQueryPlaceholder, below): it
    // substitutes an already-encoded query string, never re-encoded.
    function expandUrl(template, vars) {
        return String(template).replace(/\{(vmid|node|type|theme)\}/g, function (whole, name) {
            var v = vars[name];
            return v === undefined || v === null ? '' : encodeURIComponent(v);
        });
    }

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

    // ExtJS renders a panel's `title` as markup, so it must be escaped
    // before use - manifests are root-owned, but a page manifest is still
    // attacker-adjacent enough (any package depending on pve-ext can drop
    // one) to be worth not trusting blindly.
    function escapeHtml(s) {
        return String(s).replace(/[&<>"']/g, function (c) {
            return (
                { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]
            );
        });
    }

    // iconCls ends up as a CSS class list, not text content - restrict it
    // to characters legal in a class-list attribute value instead.
    function sanitizeIconCls(cls) {
        var s = String(cls || '');
        return /^[A-Za-z0-9 _-]+$/.test(s) ? s : '';
    }

    // Loads a page's script exactly once (per URL), regardless of how many
    // tabs/targets reference it, and caches success/failure so a second
    // reference never re-fetches or re-<script>-injects it. Queues every
    // caller's callback while a load is in flight.
    var scriptLoadState = {}; // src -> 'loaded' | 'error' | [pending callbacks]

    // Appends the manifest's `fingerprint` as `?ver=`, the way PVE versions
    // its own bundle - without it, a rebuilt file of the same package
    // version revalidates to the browser's stale cached copy indefinitely
    // (pveproxy sends Last-Modified with no Cache-Control/ETag, and dpkg
    // clamps mtimes for reproducible builds).
    function withVersion(url, fingerprint) {
        if (!url || !fingerprint) {
            return url;
        }
        return url + (url.indexOf('?') === -1 ? '?' : '&') + 'ver=' + encodeURIComponent(fingerprint);
    }

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

    // Polls until the manifest's xtype resolves to a defined class, or
    // timeoutMs elapses. Ext.ClassManager.isCreated() takes a class name,
    // not an xtype/alias - it only resolves through the "widget." alias
    // namespace (getNameByAlias()); verified against real ExtJS 7 classic
    // that isCreated(xtype) alone never works.
    function waitForXtype(xtype, timeoutMs, cb) {
        var deadline = Date.now() + timeoutMs;
        function poll() {
            var created = false;
            try {
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
    // matching whichever of them apply to this target.
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

    // Everything about a given PVE.panel.Config instance that the tab
    // configs are built from, or null if this instance's class isn't one
    // of our targets (or its vmid/node cannot be determined). Resolved
    // during initComponent, so a panel that has to wait for the prefetch
    // still describes the guest/node it was built for.
    function targetContextFor(me) {
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
            return null;
        }

        var target = TARGETS[className];
        if (!target) {
            return null; // not a panel we care about (pool/storage/sdn/... config)
        }

        var vars = { theme: getPveTheme() };
        if (target === 'lxc' || target === 'qemu') {
            var selData = me.pveSelNode && me.pveSelNode.data;
            vars.vmid = selData && selData.vmid;
            vars.node = selData && selData.node;
            vars.type = target;
            if (!vars.vmid || !vars.node) {
                warn('could not determine vmid/node for ' + className + ' - skipping all extension tabs for this panel.');
                return null;
            }
        } else if (target === 'node') {
            vars.node = me.pveSelNode && me.pveSelNode.data && me.pveSelNode.data.node;
            vars.type = 'node';
            if (!vars.node) {
                warn('could not determine node name for ' + className + ' - skipping all extension tabs for this panel.');
                return null;
            }
        } else {
            // dc
            vars.type = 'dc';
        }

        return { target: target, vars: vars, query: buildQuery(target, vars) };
    }

    // The tab item configs the given pages contribute to that context, in
    // manifest order - the one order tabs are ever added in, whether they
    // go into `items` before initComponent or through add() afterwards.
    function buildTabItems(context, pages) {
        var target = context.target;
        var vars = context.vars;
        var query = context.query;

        var items = [];
        for (var i = 0; i < pages.length; i++) {
            var manifest = pages[i];
            try {
                // PVE::API2::Ext only lists a manifest that has every field.
                if (manifest.targets.indexOf(target) === -1) {
                    continue;
                }
                var scriptSrc = expandUrl(manifest.script, vars);
                scriptSrc = expandQueryPlaceholder(scriptSrc, query);
                scriptSrc = withVersion(scriptSrc, manifest.fingerprint);
                var instanceConfig = buildInstanceConfig(target, vars);
                items.push(buildScriptTabItem(manifest, scriptSrc, instanceConfig));
            } catch (e) {
                warn('failed to build tab for page manifest "' + (manifest && manifest.id) + '", skipping it', e);
            }
        }
        return items;
    }

    // Tabs for a panel that is still being built: they go into `items`,
    // before the original initComponent gets to see them.
    function addTabConfigs(me, tabs) {
        if (!tabs.length) {
            return;
        }
        me.items = me.items || [];
        for (var i = 0; i < tabs.length; i++) {
            var tabItem = tabs[i];
            if (!me.items.some(function (it) { return it && it.itemId === tabItem.itemId; })) {
                me.items.push(tabItem);
            }
        }
    }

    // Tabs for a panel that was already built when the answer arrived.
    // PVE.panel.Config is a card panel with a treelist nav, not a tab
    // panel: initComponent hands `items` to its own insertNodes(), which
    // files each one in savedItems and appends a node to the nav store,
    // and then deletes `items`. A plain add() would therefore install a
    // card nothing can ever navigate to - the late path has to go through
    // insertNodes() too. The nav treelist is bound to that store and
    // picks up the appended node by itself. Appended in manifest order,
    // so the order is the same as the pre-initComponent path's.
    function addTabsWhenPagesArrive(me, context) {
        whenPagesReady(function (pages) {
            try {
                if (!pages.length || me.destroying || me.destroyed) {
                    return;
                }
                if (typeof me.insertNodes !== 'function' || !me.savedItems || !me.store) {
                    warn('this PVE.panel.Config has no insertNodes()/savedItems - pvemanagerlib.js may have changed. Skipping its extension tab(s).');
                    return;
                }
                var tabs = buildTabItems(context, pages);
                for (var i = 0; i < tabs.length; i++) {
                    // insertNodes() throws on an itemId it already holds.
                    if (!me.savedItems[tabs[i].itemId]) {
                        me.insertNodes([tabs[i]]);
                    }
                }
            } catch (e) {
                warn('failed to add extension tab(s) to an already-built panel, continuing without them', e);
            }
        });
    }

    // --- The actual patch -------------------------------------------------
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
                var context = targetContextFor(me);
                if (context && pagesState === 'ready') {
                    addTabConfigs(me, buildTabItems(context, pagesList));
                } else if (context) {
                    addTabsWhenPagesArrive(me, context);
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
    startPagesPrefetch();
    info('extension tab injection active.');
})();
