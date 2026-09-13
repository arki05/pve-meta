// ---------------------------------------------------------------------------
// One document in a window — what a registry grid opens.
//
// It is the ordinary editor panel, unchanged: tree, row editors, markers, the
// Tree | Text toggle, the diff. A prefix definition or a permission file is a document
// (DESIGN §6), so "edit one" was never a thing that needed its own editor.
// ---------------------------------------------------------------------------

Ext.define('PVE.meta.DocumentWindow', {
    extend: 'Ext.window.Window',
    xtype: 'pveMetaDocumentWindow',

    modal: true,
    width: 860,
    height: 560,
    layout: 'fit',
    // configs: docId ('prefixes/<name>' or 'permissions/<name>')

    initComponent: function () {
        let me = this;
        me.title = Ext.String.format(gettext('Edit: {0}'), Ext.htmlEncode(me.docId));
        Ext.apply(me, {
            items: [
                {
                    xtype: 'pveMetaTreePanel',
                    // Which ACL answers apply, and that this is not a guest, are both
                    // decided by `docId`, which `loadAccess` sends as-is.
                    docId: me.docId,
                    border: false,
                    // The panel's footer is the only bar: a window with Close at the
                    // bottom and the Apply for the same document at the top of the
                    // panel inside it was the worst of the three chromes.
                    onClose: () => me.close(),
                },
            ],
        });
        me.callParent();
    },
});

