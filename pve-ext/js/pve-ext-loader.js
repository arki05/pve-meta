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
 * with this.callParent(arguments) inside the replacement - throws in real
 * ExtJS 7 classic (as shipped by PVE), silently killing the *whole*
 * config panel for every guest. The capture-and-apply fix is proven in a
 * real browser against real pve-manager and must not be changed without
 * re-doing that verification.
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

    // --- Theme detection (same logic as pve-meta-loader.js) -------------
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

    // --- GET /ext/pages, fetched once and cached ------------------------
    //
    // Deliberately lazy and synchronous: at script-load time (every
    // index.html.tpl render, including pre-login) there is no session yet
    // and the request would just 401; by the time a config panel is
    // constructed the user is logged in, so a synchronous fetch here has
    // the tab list ready for that panel's own initComponent with no
    // async/race complexity. Same-origin call to pveproxy itself
    // (typically sub-10ms). Cached after the first attempt, never retried.
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

        var target = TARGETS[className];
        if (!target) {
            return []; // not a panel we care about (pool/storage/sdn/... config)
        }

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
                if (!manifest || !manifest.id || !manifest.title || !manifest.script || !manifest.xtype || !Ext.isArray(manifest.targets)) {
                    warn('ignoring malformed page manifest (missing id/title/script/xtype/targets): ' + JSON.stringify(manifest));
                    continue;
                }
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
