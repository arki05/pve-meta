/*
 * pve-meta-loader.js
 *
 * Injects a "Metadata" tab into the Proxmox VE web UI's LXC, QEMU and
 * Datacenter config panels. The tab embeds a same-origin iframe served by
 * pveproxy itself: /pve2/js/pve-meta-ui/index.html?vmid=<id>&theme=<theme>
 * (?dc=1&theme=<theme> for the Datacenter panel) - a relative URL, no
 * host/port of its own, so there is no cross-origin/mixed-content
 * concern at all: it resolves against whatever host/port the admin is
 * already browsing pve-manager on (the pve-meta UI's static files are
 * expected to live at /usr/share/pve-manager/js/pve-meta-ui/, the same
 * directory pveproxy already serves under /pve2/js/ - see pve-meta-patch
 * and its README for details; installing those files is outside this
 * loader's scope).
 *
 * This file is served statically by pveproxy (it is dropped into
 * /usr/share/pve-manager/js/, which pveproxy already serves under
 * /pve2/js/) and is loaded via a <script> tag appended to
 * index.html.tpl right after the pvemanagerlib.js tag. At that point:
 *   - Ext JS classic toolkit is fully loaded.
 *   - PVE.panel.Config / PVE.lxc.Config / PVE.qemu.Config / PVE.dc.Config
 *     are already defined (pvemanagerlib.js executes Ext.define calls
 *     synchronously at parse time).
 *   - No panel instances exist yet (Ext.onReady creates PVE.StdWorkspace
 *     afterwards), so we hook by patching
 *     PVE.panel.Config.prototype.initComponent directly (capturing the
 *     original function and always calling it via .apply()), which is
 *     the single base class shared by PVE.lxc.Config, PVE.qemu.Config
 *     and PVE.dc.Config. NOTE: we intentionally do NOT use the global
 *     Ext.override(cls, {...}) shim here, and our replacement does NOT
 *     use this.callParent(...) - see the long comment at the patch site
 *     below for why (it throws in real ExtJS 7, silently killing the
 *     whole config panel).
 *
 * Design goal: NEVER break the PVE UI. Every anchor this script depends on
 * is feature-detected before use; on any mismatch we log to the console
 * and do nothing further.
 *
 * Plain ES2017, no build step, no external dependencies.
 */
(function () {
    'use strict';

    // Idempotency / re-inclusion guard.
    if (window.PveMetaLoaded) {
        return;
    }

    var LOG_PREFIX = '[pve-meta]';
    var PVE_META_UI_PATH = '/pve2/js/pve-meta-ui/index.html';
    var ITEM_ID = 'pvemeta';
    var TAB_TITLE = 'Metadata';
    var ICON_CLS = 'fa fa-tags';

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

    // --- Feature detection -------------------------------------------------
    //
    // Bail out immediately (and loudly, but harmlessly) if any of the
    // anchors this script relies on are not present. This is what keeps
    // the patch "reversible" in spirit even without removing it: a future
    // pve-manager release that restructures these classes simply causes
    // the loader to no-op instead of throwing inside the PVE UI.

    if (typeof Ext === 'undefined') {
        warn('Ext JS not found - skipping Metadata tab injection.');
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

    // --- Helpers -------------------------------------------------------

    // Resolve the fully-qualified Ext class name of an instance across the
    // handful of ways Ext JS exposes it, without assuming any one of them
    // is present (defensive against toolkit/version differences).
    function getClassName(cmp) {
        try {
            if (cmp && typeof cmp.$className === 'string' && cmp.$className) {
                return cmp.$className;
            }
            if (cmp && cmp.self && typeof cmp.self.getName === 'function') {
                return cmp.self.getName();
            }
            if (typeof Ext.getClassName === 'function') {
                return Ext.getClassName(cmp) || '';
            }
        } catch (e) {
            warn('failed to determine component class name', e);
        }
        return '';
    }

    // Read the PVEThemeCookie the same way PVE's own UI does
    // (Proxmox.window.ThemeEditWindow) and translate it into a plain
    // light/dark value for the embedded iframe. Values:
    //   'crisp'          -> light theme, explicitly chosen
    //   'proxmox-dark'   -> dark theme, explicitly chosen
    //   unset / '__default__' / anything else -> follow the OS/browser
    //   preference via prefers-color-scheme, same as PVE's own charts and
    //   gauges do (see checkThemeColors() in proxmoxlib.js).
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

    function buildMetaSrc(query) {
        // Deliberately a same-origin, host/port-relative URL: it resolves
        // against whatever origin the admin is already viewing pve-manager
        // on (pveproxy itself), exactly like every other /pve2/... asset
        // pve-manager loads. No window.location.hostname, no separate
        // port, no cross-origin/mixed-content concern.
        return PVE_META_UI_PATH + '?' + query;
    }

    function buildMetadataTabItem(src) {
        return {
            xtype: 'panel',
            itemId: ITEM_ID,
            title: TAB_TITLE,
            iconCls: ICON_CLS,
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

    // Build the Metadata tab config for a given PVE.panel.Config instance,
    // or return null if this instance is not one we want to touch, or if
    // required data (vmid, node) is missing.
    function metadataTabFor(me) {
        var className = getClassName(me);
        var theme = getPveTheme();

        if (className === 'PVE.dc.Config') {
            return buildMetadataTabItem(buildMetaSrc('dc=1&theme=' + theme));
        }

        if (className === 'PVE.lxc.Config' || className === 'PVE.qemu.Config') {
            var selData = me.pveSelNode && me.pveSelNode.data;
            var vmid = selData && selData.vmid;
            if (!vmid) {
                warn('could not determine vmid for ' + className + ' - skipping Metadata tab for this panel.');
                return null;
            }
            return buildMetadataTabItem(buildMetaSrc('vmid=' + encodeURIComponent(vmid) + '&theme=' + theme));
        }

        // Not a panel we care about (node/pool/storage/sdn config, etc).
        return null;
    }

    // --- The actual patch ------------------------------------------------
    //
    // PVE.lxc.Config, PVE.qemu.Config and PVE.dc.Config each build their
    // own `me.items` array (of {itemId, title, iconCls, ...} tab
    // descriptors, NOT live Ext components) inside their own
    // initComponent, then call `me.callParent()` as the LAST step, which
    // resolves to PVE.panel.Config's initComponent. That base
    // initComponent is what actually turns `me.items` into the treelist
    // navigation + card layout (see insertNodes()/activateCard() in
    // pvemanagerlib.js) and then deletes `me.items`.
    //
    // So the correct interception point is PVE.panel.Config.initComponent
    // itself: by the time it runs (via callParent from the subclass), the
    // subclass's real tabs are already in me.items and ripe for one more
    // push before the base class consumes them.

    // NOTE: we deliberately do NOT use the global Ext.override(cls, {...})
    // form here, and we deliberately do NOT rely on this.callParent(...)
    // inside our replacement. In a real browser (ExtJS 7 classic, as
    // shipped by PVE) that combination throws:
    //   TypeError: Cannot read properties of null (reading 'apply')
    //     at constructor.callParent (ext-all.js)
    //     at constructor.initComponent (pve-meta-loader.js)
    // i.e. the legacy global Ext.override() shim does not always wire up
    // the $previous link that callParent() needs, so calling
    // this.callParent(arguments) from inside the override can throw -
    // which would abort PVE.qemu.Config/PVE.lxc.Config's own
    // initComponent entirely and render an EMPTY config panel. That is
    // exactly the kind of breakage this loader must never cause.
    //
    // Instead we patch the prototype method directly and capture the
    // original function in a closure, then invoke it with a plain
    // Function.prototype.apply() call - no Ext class-system machinery
    // involved, so there is nothing for it to fail to wire up. Our own
    // logic is fully wrapped in try/catch and the original
    // initComponent is ALWAYS invoked (even if our logic throws),
    // exactly once, with the original `this` and `arguments`.
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
                var tabItem = metadataTabFor(me);
                if (tabItem) {
                    me.items = me.items || [];
                    if (!me.items.some(function (it) { return it && it.itemId === ITEM_ID; })) {
                        me.items.push(tabItem);
                    }
                }
            } catch (e) {
                warn('failed to inject Metadata tab for this panel, continuing without it', e);
            }
            // Always call the real, original initComponent - our logic
            // above must never be able to prevent the actual PVE config
            // panel from being built.
            return origInitComponent.apply(me, arguments);
        };
    } catch (e) {
        warn('failed to patch PVE.panel.Config.prototype.initComponent - Metadata tab will not be available.', e);
        return;
    }

    window.PveMetaLoaded = true;
    info('Metadata tab injection active.');
})();
