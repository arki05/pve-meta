// ---------------------------------------------------------------------------
// Monaco, loaded lazily on first use from the tree the pve-meta UI package ships.
// ---------------------------------------------------------------------------

PVE.meta.Monaco = {
    VS: '/pve2/js/pve-meta-extjs/vs',
    promise: null,

    load: function () {
        let me = PVE.meta.Monaco;
        me.promise =
            me.promise ||
            // The core first: every caller of Monaco here also needs the codec, and
            // a buffer rendered before it is loaded is a buffer rendered from nothing.
            PVE.meta.Core.load().then(
                () =>
                    new Promise(function (resolve, reject) {
                        if (window.monaco && window.monaco.editor) {
                            resolve(window.monaco);
                            return;
                        }
                        // Absolute, because a language worker resolves its own scripts
                        // against this and has no page URL to make a root-relative path
                        // absolute with.
                        let vs = window.location.origin + me.VS;
                        window.MonacoEnvironment = { baseUrl: vs };
                        let script = document.createElement('script');
                        script.src = vs + '/loader.js';
                        script.onload = function () {
                            try {
                                window.require.config({ paths: { vs: vs } });
                                window.require(
                                    ['vs/editor/editor.main'],
                                    () => resolve(window.monaco),
                                    reject,
                                );
                            } catch (err) {
                                reject(err);
                            }
                        };
                        script.onerror = () => reject(new Error('failed to load ' + script.src));
                        document.head.appendChild(script);
                    }),
            );
        return me.promise;
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

    dispose: function (editor) {
        if (!editor) {
            return;
        }
        let model = editor.getModel();
        if (model && model.original) {
            // A diff editor does not own its two models, so they have to be detached
            // before they are disposed - disposing them under a live DiffEditorWidget
            // is what "TextModel got disposed before ... model got reset" complains
            // about. A standalone editor created with a `value` owns its model and
            // disposes it itself.
            editor.setModel(null);
            model.original.dispose();
            model.modified.dispose();
        }
        editor.dispose();
    },

    // Monaco's other job: the buffer against the stored file, side by side. A look,
    // not a decision -- the write is the Apply beside the button that opened this.
    //
    // cfg: { title, original, modified, lang }. Both sides are text, shown exactly as
    // given: the buffer is what Apply writes, and a diff that parsed and re-dumped it
    // would canonicalise away the `#` comment or the reordering that is the reason to
    // write text at all.
    //
    // **It loads Monaco itself.** It cannot draw without it, every caller had to
    // remember to, and the one that forgot threw "YAML support is not loaded" before
    // it could reach the server. A function that needs a thing should get the thing;
    // `Monaco.load()` is a cached promise, so callers that already awaited it pay
    // nothing.
    showDiff: function (cfg) {
        PVE.meta.Monaco.load().then(
            () => PVE.meta.Monaco.showDiffWindow(cfg),
            (err) => Ext.Msg.alert(gettext('Error'), Ext.htmlEncode(PVE.meta.Utils.errText(err))),
        );
    },

    showDiffWindow: function (cfg) {
        let state = {};
        let win = Ext.create('Ext.window.Window', {
            title: gettext('Changes') + ': ' + Ext.htmlEncode(cfg.title),
            itemId: 'pveMetaDiffWindow',
            modal: true,
            // Fitted to the viewport, not fixed: at 620px tall on a shorter window it
            // pushed its own Close button off the bottom of the screen with nothing to
            // scroll, so the only way out was Escape -- which nothing says.
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
                // Monaco defaults this to true, which hides indentation-only changes --
                // exactly what a YAML -> JSON -> YAML round trip produces, and exactly
                // what a Format is for. A diff that shows nothing while the buffer is
                // dirty is worse than no diff, so show them.
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

