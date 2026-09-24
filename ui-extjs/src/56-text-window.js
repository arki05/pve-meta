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
    // configs: view (dotted path), text (YAML), tree (the owning panel). OK writes
    // through `tree`, so the window names a view of *its* document and never a
    // document of its own -- an id here would only be one the write could ignore.

    // Whether the document behind this view can be written at all (DESIGN §4). A
    // read-only caller gets the same window as a look at the YAML, with no OK to
    // press and nothing to type into: the 403 was the only thing telling them so.
    mayWrite: function () {
        let me = this;
        return !!(me.tree && me.tree.access && me.tree.access.write);
    },

    // The body, not the window: Cancel has to stay clickable under a Monaco that
    // did not load, exactly as 60-doc.js `setMask` says of the panel's own footer.
    setMask: function (msg) {
        Proxmox.Utils.setErrorMask({ el: this.body || this.el }, msg);
    },

    initComponent: function () {
        let me = this;
        let write = me.mayWrite();
        me.original = me.text || '';
        me.title = Ext.String.format(
            write ? gettext('Edit selection as text: {0}') : gettext('View selection as text: {0}'),
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
                    applyHidden: !write,
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
            me.setMask(true);
            PVE.meta.Monaco.load().then(
                function () {
                    me.setMask(false);
                    me.editor = PVE.meta.Monaco.create(
                        me.lookupReference('mount').getEl().dom,
                        me.original,
                        { readOnly: !write },
                    );
                    if (write) {
                        // Without this the footer never learns there is a buffer, and
                        // OK stays the disabled button it was built as.
                        me.down('#metaApply').setDisabled(false);
                    }
                },
                (err) => me.setMask(Ext.htmlEncode(PVE.meta.Utils.errText(err))),
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

    // A 409 under this window, with the view changed underneath it as well:
    // `subtree` is the view as stored *now*. The buffer is unwritten work and the
    // only copy of it, so it is left alone; what it is compared against is the
    // new subtree from here on.
    conflict: function (subtree) {
        this.original = PVE.meta.Codec.dump(subtree === undefined ? {} : subtree, 'yaml');
    },

    // The buffer against that -- the question a conflict actually raises. Opened by
    // the panel once its Conflict alert is dismissed.
    showDiff: function () {
        let me = this;
        if (me.editor) {
            PVE.meta.Buffer.diff(me.buffer(), me.view || gettext('(whole document)'));
        }
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
        if (!me.editor || !lang || lang === me.lang) {
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
        if (!me.editor || !me.mayWrite()) {
            return;
        }
        me.apply(me.editor.getValue(), me.lang);
    },

    // One `replace` at the view's own path, which says everything about what is
    // inside that view -- a buffer for `traefik` replaces `traefik.spec` too. The
    // window closes only once the server has it: a page of YAML refused by a lint
    // rule or a 403 is a page of YAML this window is still the only copy of.
    apply: function (text, lang) {
        let me = this;
        let value;
        try {
            value = PVE.meta.Codec.parse(text, lang);
        } catch (err) {
            PVE.meta.Utils.alertError(err);
            return;
        }
        PVE.meta.writeFromWindow(me, (done) => me.tree.writeSubtree(me.view, value, done));
    },
});

