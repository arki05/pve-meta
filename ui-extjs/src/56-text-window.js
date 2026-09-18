// ---------------------------------------------------------------------------
// "Edit selection as text" — Monaco on one subtree; OK is one write of it.
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
                    // Monaco loads, and the `afterrender` handler turns it on then.
                    applyDisabled: true,
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
                    // Without this the footer never learns there is a buffer, and OK
                    // stays the disabled button it was built as.
                    me.down('#metaApply').setDisabled(false);
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

    // OK writes the subtree and closes.
    submit: function () {
        let me = this;
        if (!me.editor) {
            return;
        }
        me.apply(me.editor.getValue(), me.lang);
    },

    // One `replace` at the view's own path, which says everything about what is
    // inside that view -- a buffer for `traefik` replaces `traefik.spec` too.
    apply: function (text, lang) {
        let me = this;
        let value;
        try {
            value = PVE.meta.Codec.parse(text, lang);
        } catch (err) {
            Ext.Msg.alert(gettext('Error'), Ext.htmlEncode(PVE.meta.Utils.errText(err)));
            return;
        }
        me.tree.writeSubtree(me.view, value);
        me.close();
    },
});

