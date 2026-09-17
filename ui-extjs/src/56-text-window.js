// ---------------------------------------------------------------------------
// "Edit selection as text" — Monaco on one subtree, with a diff-confirmed Apply.
// ---------------------------------------------------------------------------

Ext.define('PVE.meta.TextWindow', {
    extend: 'Ext.window.Window',
    xtype: 'pveMetaTextWindow',

    modal: true,
    width: 900,
    height: 640,
    layout: 'fit',
    referenceHolder: true, // lookupReference('mount'/'langbtn') needs this
    lang: 'yaml',
    // configs: view (dotted path), text (YAML), tree (the owning panel)

    initComponent: function () {
        let me = this;
        me.original = me.text || '';
        me.title = Ext.String.format(
            gettext('Edit selection as text: {0}'),
            Ext.htmlEncode(me.view || gettext('(whole document)')),
        );

        Ext.apply(me, {
            items: [{ xtype: 'component', reference: 'mount', style: 'height:100%;width:100%' }],
            bbar: [
                PVE.meta.Footer.langToggle({ reference: 'langbtn', onChange: (value) => me.switchLang(value) }),
            ].concat(
                PVE.meta.Footer.actions({
                    // Stated, like the panel states its own: there is no buffer until
                    // Monaco loads, and `syncFooter` turns it on when there is.
                    applyDisabled: true,
                    // No Diff here. This window opens on the *planned* document, so a
                    // diff against what it opened with is empty until you type, which
                    // is what it showed. What a diff is for -- everything staged,
                    // against what is stored -- is one document up, on the panel's own
                    // Apply. A value editor does not have one, and this is a value
                    // editor for a subtree.
                    format: () => me.formatBuffer(),
                    apply: () => me.submit(),
                    applyText: gettext('OK'),
                    secondary: () => me.close(),
                    secondaryText: gettext('Cancel'),
                }),
            ),
        });
        me.callParent();

        me.on('afterrender', function () {
            Proxmox.Utils.setErrorMask(me, true);
            PVE.meta.Monaco.load().then(
                function (monaco) {
                    Proxmox.Utils.setErrorMask(me, false);
                    me.editor = monaco.editor.create(me.lookupReference('mount').getEl().dom, {
                        value: me.original,
                        language: 'yaml',
                        theme: PVE.meta.Monaco.theme(),
                        automaticLayout: true,
                        minimap: { enabled: false },
                        scrollBeyondLastLine: false,
                    });
                    // Without these the footer never learns there is a buffer:
                    // `syncFooter` was written and never called, so the secondary
                    // button kept an undo icon over the word Close whatever you did.
                    me.syncFooter();
                    me.editor.onDidChangeModelContent(() => me.syncFooter());
                },
                (err) => Proxmox.Utils.setErrorMask(me, Ext.htmlEncode(PVE.meta.Utils.errText(err))),
            );
        });

        // Monaco leaks a ResizeObserver and its models otherwise, and a stale buffer
        // is one that can be applied to the wrong path.
        me.on('destroy', function () {
            PVE.meta.Monaco.dispose(me.editor);
            me.editor = null;
        });
    },

    buffer: function () {
        return { editor: this.editor, lang: this.lang, original: this.original };
    },

    // OK is live as soon as there is a buffer, and Cancel stays Cancel. This is a
    // modal that yields a value, like the row editor: closing it abandons what was
    // typed here and nothing else, so there is no Discard to distinguish -- the thing
    // with something to lose is the panel behind it, which keeps its own staged edits
    // either way.
    syncFooter: function () {
        let me = this;
        PVE.meta.Footer.sync(me, {
            canApply: !!me.editor,
            count: 0,
            applyText: gettext('OK'),
            dirty: false,
            cleanText: gettext('Cancel'),
        });
    },

    formatBuffer: function () {
        let me = this;
        if (!me.editor) {
            return;
        }
        PVE.meta.Buffer.format(me.buffer());
    },

    // Presentation only: the same value in the other syntax. If the buffer does not
    // parse we say so and stay put; the server remains the YAML authority.
    switchLang: function (lang) {
        let me = this;
        if (!me.editor || lang === me.lang) {
            return;
        }
        let value = PVE.meta.Buffer.convert(me.buffer(), lang, me.lookupReference('langbtn'));
        if (value === undefined) {
            return;
        }
        me.lang = lang;
        PVE.meta.Buffer.render(me.buffer(), value);
    },

    // OK hands the subtree back and closes. An unchanged buffer is not a special
    // case: staging the value it already had is a no-op the edit set collapses.
    submit: function () {
        let me = this;
        if (!me.editor) {
            return;
        }
        me.apply(me.editor.getValue(), me.lang);
    },

    // Stages what was typed; it does not write. Every other modal in this editor --
    // the row editor, Add Key, Add Rule, Declare Key -- stages, and the panel's footer
    // Apply is the one thing that writes. This window wrote immediately and directly,
    // which is why it could not see staged edits and they could not see it: it was not
    // editing the same document as everything else.
    //
    // One edit at the view's own path, and a set there replaces the whole subtree --
    // a buffer for `traefik` says everything about `traefik.spec`, whatever was
    // staged inside it before.
    apply: function (text, lang) {
        let me = this;
        let value;
        try {
            value = PVE.meta.Codec.parse(text, lang);
        } catch (err) {
            Ext.Msg.alert(gettext('Error'), Ext.htmlEncode(PVE.meta.Utils.errText(err)));
            return;
        }
        // Nothing to say when there is nothing to stage. An edit that changes no
        // value -- because nothing was typed, or because what was typed was layout
        // (decision 007) -- resolves to no staged edit, and closing on that is the
        // honest outcome rather than something to interrupt for. The tree behind
        // this window shows what is staged and offers the per-row undo, so a user
        // who wants to know what happened is already looking at it.
        me.tree.stageFromView(me.view, value);
        me.close();
    },
});

