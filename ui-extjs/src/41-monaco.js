// ---------------------------------------------------------------------------
// Monaco, loaded lazily on first use from the tree the pve-meta UI package ships.
// ---------------------------------------------------------------------------

PVE.meta.Monaco = {
    // Under Monaco's version (`make js` writes it in), so an upgrade is a new tree
    // and never half-cached.
    VS: '/pve2/js/pve-meta-extjs/@MONACO_DIR@/vs',
    // A script that neither loads nor errors -- a proxy that swallows it, a
    // truncated response -- would otherwise leave the caller's mask up for the
    // life of the page, with nothing to say.
    TIMEOUT: 30000,
    promise: null,

    // ExtJS marks every function with an *enumerable* `Function.prototype.$isFunction`,
    // and Monaco's ESM-to-AMD interop copies the own descriptor of every `for...in` key
    // it finds, which an inherited one does not have. Ext only reads the marker as a
    // truth value, so hiding it costs nothing.
    hideExtFunctionMarker: function () {
        let d = Object.getOwnPropertyDescriptor(Function.prototype, '$isFunction');
        if (d && d.enumerable && d.configurable) {
            d.enumerable = false;
            Object.defineProperty(Function.prototype, '$isFunction', d);
        }
    },

    load: function () {
        let me = PVE.meta.Monaco;
        if (!me.promise) {
            // Every caller of Monaco also needs the codec, so load it first.
            me.promise = PVE.meta.Core.load()
                .then(() => me.loadEditor())
                // As the core: a failed load is this attempt's answer, not the
                // session's. Text mode is one Monaco away, and one dropped request
                // must not be the end of it until the page is reloaded.
                .catch(function (err) {
                    me.promise = null;
                    throw err;
                });
        }
        return me.promise;
    },

    // The AMD tree itself, once the core is there. Settles exactly once, whichever
    // of the four ways it ends: loaded, a chunk that threw, a script that 404ed,
    // or nothing at all inside `TIMEOUT`.
    loadEditor: function () {
        let me = PVE.meta.Monaco;
        return new Promise(function (resolve, reject) {
            if (window.monaco && window.monaco.editor) {
                resolve(window.monaco);
                return;
            }
            me.hideExtFunctionMarker();
            // Absolute: a language worker resolves its own scripts against
            // this and has no page URL to make a relative path absolute with.
            let vs = window.location.origin + me.VS;
            // A Monaco chunk that throws does so in its own `<script>`, out
            // of reach of the loader's errback: without this the promise
            // never settles and the caller's mask never comes off.
            let onError = function (event) {
                if (String(event.filename || '').indexOf(vs) === 0) {
                    done(reject, event.error || new Error(event.message));
                }
            };
            let timer = setTimeout(function () {
                done(reject, new Error(gettext('Timed out loading the text editor')));
            }, me.TIMEOUT);
            let done = function (settle, value) {
                clearTimeout(timer);
                window.removeEventListener('error', onError);
                settle(value);
            };
            window.addEventListener('error', onError);
            window.MonacoEnvironment = { baseUrl: vs };
            let script = document.createElement('script');
            script.src = vs + '/loader.js';
            script.onload = function () {
                try {
                    window.require.config({ paths: { vs: vs } });
                    window.require(
                        ['vs/editor/editor.main'],
                        () => done(resolve, window.monaco),
                        (err) => done(reject, err),
                    );
                } catch (err) {
                    done(reject, err);
                }
            };
            script.onerror = () => done(reject, new Error('failed to load ' + script.src));
            document.head.appendChild(script);
        });
    },

    // Monaco does not inherit the page's CSS, so pick its built-in theme from the
    // cookie PVE's own theme picker writes (proxmoxlib's ThemeEditWindow); anything
    // else follows the browser preference, exactly like PVE's charts do.
    theme: function () {
        let cookie = '';
        try {
            cookie = Ext.util.Cookies.get('PVEThemeCookie') || '';
        } catch (_e) {
            cookie = '';
        }
        if (cookie === 'proxmox-dark') {
            return 'vs-dark';
        } else if (cookie === 'crisp') {
            return 'vs';
        }
        return window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches
            ? 'vs-dark'
            : 'vs';
    },

    // A standalone editor with the options every text buffer in this editor shares.
    create: function (mount, value) {
        return window.monaco.editor.create(mount, {
            value: value,
            language: 'yaml',
            theme: PVE.meta.Monaco.theme(),
            automaticLayout: true,
            minimap: { enabled: false },
            scrollBeyondLastLine: false,
        });
    },

    dispose: function (editor) {
        if (!editor) {
            return;
        }
        let model = editor.getModel();
        if (model && model.original) {
            // A diff editor does not own its two models; they must be detached
            // before disposal, or Monaco complains the model got reset live.
            editor.setModel(null);
            model.original.dispose();
            model.modified.dispose();
        }
        editor.dispose();
    },

    // The buffer against the stored file, side by side: a look, not a decision.
    // cfg: { title, original, modified, lang }. Both sides are shown as given text,
    // never re-dumped, so a diff never canonicalises away the reason to write text
    // at all. Loads Monaco itself, since it cannot draw without it.
    showDiff: function (cfg) {
        PVE.meta.Monaco.load().then(
            () => PVE.meta.Monaco.showDiffWindow(cfg),
            PVE.meta.Utils.alertError,
        );
    },

    showDiffWindow: function (cfg) {
        let state = {};
        let win = Ext.create('Ext.window.Window', {
            title: gettext('Changes') + ': ' + Ext.htmlEncode(cfg.title),
            itemId: 'pveMetaDiffWindow',
            modal: true,
            // Fitted to the viewport, not fixed, so Close never falls off a short screen.
            width: Math.min(1000, Ext.Element.getViewportWidth() - 40),
            height: Math.min(620, Ext.Element.getViewportHeight() - 40),
            maxHeight: Ext.Element.getViewportHeight() - 40,
            constrain: true,
            layout: 'fit',
            referenceHolder: true,
            items: [
                {
                    xtype: 'component',
                    reference: 'diff',
                    style: 'height:100%;width:100%',
                },
            ],
            buttons: [{ text: gettext('Close'), handler: () => win.close() }],
        });
        win.on('afterrender', function () {
            let monaco = window.monaco;
            let lang = cfg.lang || 'yaml';
            state.editor = monaco.editor.createDiffEditor(win.lookupReference('diff').getEl().dom, {
                theme: PVE.meta.Monaco.theme(),
                automaticLayout: true,
                readOnly: true,
                renderSideBySide: true,
                minimap: { enabled: false },
                // Monaco defaults this to true, which would hide the indentation-only
                // changes a Format or a YAML<->JSON round trip produces.
                ignoreTrimWhitespace: false,
            });
            state.editor.setModel({
                original: monaco.editor.createModel(cfg.original, lang),
                modified: monaco.editor.createModel(cfg.modified, lang),
            });
        });
        win.on('destroy', function () {
            PVE.meta.Monaco.dispose(state.editor);
            state.editor = null;
        });
        win.show();
        return win;
    },
};

