// ---------------------------------------------------------------------------
// The row editor — opened by Edit, double-click or Enter (DESIGN §8).
// ---------------------------------------------------------------------------

Ext.define('PVE.meta.EditValueWindow', {
    extend: 'PVE.meta.FormWindow',
    xtype: 'pveMetaEditValueWindow',

    width: 480,
    defaultButton: 'okBtn',
    // configs: rec (the tree record being edited)

    formItems: function () {
        let me = this;
        let U = PVE.meta.Utils;
        let d = me.rec.data;
        if (U.editorKind(d) === 'multiline') {
            me.width = 640;
        }
        me.title = Ext.String.format(gettext('Edit: {0}'), Ext.htmlEncode(d.path));

        let value;
        if (d.present) {
            value = d.kind === 'boolean' || d.kind === 'number' ? d.rawValue : d.valueText;
        } else if (d.defaultValue !== undefined) {
            value = d.defaultValue;
        } else {
            value = d.kind === 'boolean' ? false : '';
        }

        let items = [
            {
                xtype: 'displayfield',
                fieldLabel: gettext('Key'),
                value: Ext.htmlEncode(d.path),
            },
        ];
        let note = d.description || d.grammarDescription;
        if (note) {
            items.push({
                xtype: 'displayfield',
                fieldLabel: gettext('Description'),
                value: Ext.htmlEncode(note),
            });
        }
        items.push(
            Ext.apply(
                {
                    name: 'value',
                    itemId: 'valueField',
                    fieldLabel: gettext('Value'),
                    value: value,
                },
                U.editorFor(d),
            ),
        );
        if (!d.present && d.defaultValue !== undefined) {
            items.push({
                xtype: 'displayfield',
                fieldLabel: gettext('Default'),
                value: Ext.htmlEncode(U.displayValue(d.defaultValue, U.kindOf(d.defaultValue))),
            });
        }
        return items;
    },

    initComponent: function () {
        this.callParent();
        this.on('show', () => this.down('#valueField').focus(true, 50));
    },

    submit: function () {
        let me = this;
        let form = me.validForm();
        if (!form) {
            return;
        }
        let d = me.rec.data;
        let value;
        try {
            value = PVE.meta.Utils.parseValue(me.down('#valueField').getValue(), d.kind);
        } catch (err) {
            PVE.meta.Utils.alertError(err);
            return;
        }
        // The core's own comparison, the one `setToDefault` asks: `Ext.encode` of
        // each made key order part of the answer, which it is not (DESIGN §2).
        if (d.present && PVE.meta.Utils.sameValue(value, d.rawValue)) {
            me.close(); // nothing actually changed
            return;
        }
        // The write is one round trip away and may fail -- a lint error, a 403, a
        // dropped connection -- so the window closes in the callback, not here.
        PVE.meta.writeFromWindow(me, (done) => me.fireEvent('setvalue', value, done));
    },
});
