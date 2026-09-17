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

    // Monaco's other job: original vs edited, side by side, as the confirm step
    // before anything is written. Shared by the selection window, the Text card and
    // the tree's Apply.
    //
    // cfg: { title, original, modified, lang, warnings, apply }, or the same with
    // `originalValue`/`modifiedValue` -- documents, rendered here as YAML.
    //
    // **It loads Monaco itself.** It cannot draw without it, every caller had to
    // remember to, and the one that forgot threw "YAML support is not loaded" before
    // it could reach the server -- with edits staged and no way to apply them. A
    // function that needs a thing should get the thing; `Monaco.load()` is a cached
    // promise, so callers that already awaited it pay nothing.
    confirmDiff: function (cfg) {
        PVE.meta.Monaco.load().then(
            () => PVE.meta.Monaco.showDiffWindow(cfg),
            (err) => Ext.Msg.alert(gettext('Error'), Ext.htmlEncode(PVE.meta.Utils.errText(err))),
        );
    },

    showDiffWindow: function (cfg) {
        let state = {};
        cfg = Ext.apply({}, cfg);

        // Two kinds of caller. The tree passes *documents* (`originalValue`,
        // `modifiedValue`): they are rendered here, and the window can re-render
        // them in the other syntax, so it gets the YAML|JSON switch. Text mode
        // passes *buffers*, and those are shown exactly as given -- never parsed and
        // re-dumped -- because the buffer is what Apply from Text writes: a `#`
        // comment or a reordering is in the file after the write, and a diff that
        // canonicalised it away would show the user something other than what
        // they are about to store. No switch for a buffer, then; re-rendering is
        // exactly what it must not do.
        let values = null;
        let lang = cfg.lang || 'yaml';
        if (cfg.originalValue !== undefined || cfg.modifiedValue !== undefined) {
            values = { original: cfg.originalValue, modified: cfg.modifiedValue };
            cfg.original = PVE.meta.Codec.dump(values.original, lang);
            cfg.modified = PVE.meta.Codec.dump(values.modified, lang);
        }
        // `cfg.warnings` (grammar findings) turns this into the warned form: a banner
        // above the diff and an Apply gated on an explicit tick.
        let warnings = cfg.warnings || [];
        let win = Ext.create('Ext.window.Window', {
            title: gettext('Confirm') + ': ' + Ext.htmlEncode(cfg.title),
            itemId: 'pveMetaDiffWindow',
            modal: true,
            // Fitted to the viewport, not fixed: at 620px tall on a shorter window it
            // pushed its own Apply and Back buttons off the bottom of the screen with
            // nothing to scroll, so the only way out was Escape -- which nothing says.
            width: Math.min(1000, Ext.Element.getViewportWidth() - 40),
            height: Math.min(620, Ext.Element.getViewportHeight() - 40),
            maxHeight: Ext.Element.getViewportHeight() - 40,
            constrain: true,
            layout: 'border',
            referenceHolder: true,
            items: [
                // The schema warning lives *in* the confirm step rather than in a
                // dialog before it: one decision, with the diff that decision is
                // about visible underneath it, instead of an alert to dismiss and
                // then a second window to read.
                {
                    xtype: 'panel',
                    region: 'north',
                    hidden: !warnings.length,
                    bodyPadding: 8,
                    border: false,
                    cls: 'pve-meta-diff-warning',
                    style: 'border-bottom:1px solid var(--pwt-color-outline,#c0c0c0)',
                    html:
                        '<div style="display:flex;gap:8px;align-items:flex-start">' +
                        '<i class="fa fa-exclamation-triangle" style="color:#e6a23c;margin-top:2px"></i>' +
                        '<div><b>' +
                        Ext.htmlEncode(gettext('This does not match the schema the operators declare')) +
                        '</b><ul style="margin:4px 0 0 0;padding-left:18px">' +
                        warnings.slice(0, 8).map((w) => '<li>' + Ext.htmlEncode(w) + '</li>').join('') +
                        '</ul>' +
                        (warnings.length > 8
                            ? '<div>' +
                              Ext.htmlEncode(
                                  Ext.String.format(gettext('... and {0} more.'), warnings.length - 8),
                              ) +
                              '</div>'
                            : '') +
                        '</div></div>',
                },
                {
                    xtype: 'component',
                    region: 'center',
                    reference: 'diff',
                    style: 'height:100%;width:100%',
                },
            ],
            buttons: [
                // The same switch the editors have, here rather than before here:
                // wanting to read a diff in the other syntax is not a reason to close
                // it, change the editor's language and open it again. Only for
                // documents; a buffer is shown as it is (above).
                PVE.meta.Footer.langToggle({
                    itemId: 'diffLangBtn',
                    hidden: !values,
                    value: lang,
                    onChange: (value) => state.render(value),
                }),
                {
                    xtype: 'proxmoxcheckbox',
                    itemId: 'diffAckBox',
                    hidden: !warnings.length,
                    boxLabel: gettext('Save anyway'),
                    // Advisory, not a gate: the server's lint decides what is storable
                    // (DESIGN section 7). The tick is here so a mismatch is a deliberate
                    // act rather than a dialog reflex -- never to make it impossible.
                    listeners: {
                        change: (box, value) => win.down('#diffApplyBtn').setDisabled(!value),
                    },
                },
                '->',
                // Without an `apply` this window is just a look at the difference --
                // the same view, without the decision. Being able to see the diff
                // without committing to it is the point of offering it outside Apply.
                {
                    text: gettext('Apply'),
                    itemId: 'diffApplyBtn',
                    hidden: !cfg.apply,
                    disabled: !!warnings.length,
                    handler: function () {
                        win.close();
                        cfg.apply();
                    },
                },
                { text: cfg.apply ? gettext('Back') : gettext('Close'), handler: () => win.close() },
            ],
        });
        win.on('afterrender', function () {
            let monaco = window.monaco;
            state.editor = monaco.editor.createDiffEditor(win.lookupReference('diff').getEl().dom, {
                theme: PVE.meta.Monaco.theme(),
                automaticLayout: true,
                readOnly: true,
                renderSideBySide: true,
                minimap: { enabled: false },
                // Monaco defaults this to true, which hides indentation-only changes --
                // exactly what a YAML -> JSON -> YAML round trip produces. A confirm
                // dialog that shows nothing while Apply is enabled is worse than no
                // dialog, so show them.
                ignoreTrimWhitespace: false,
            });
            // One place that builds the models, so the toggle and the first draw
            // cannot disagree. The previous pair is disposed *after* the new one is
            // in, never while the widget still holds it.
            state.render = function (to) {
                let old = state.editor.getModel();
                state.editor.setModel({
                    original: monaco.editor.createModel(
                        values ? PVE.meta.Codec.dump(values.original, to) : cfg.original,
                        to,
                    ),
                    modified: monaco.editor.createModel(
                        values ? PVE.meta.Codec.dump(values.modified, to) : cfg.modified,
                        to,
                    ),
                });
                if (old) {
                    old.original.dispose();
                    old.modified.dispose();
                }
            };
            state.render(lang);
        });
        win.on('destroy', function () {
            PVE.meta.Monaco.dispose(state.editor);
            state.editor = null;
        });
        win.show();
        return win;
    },
};

