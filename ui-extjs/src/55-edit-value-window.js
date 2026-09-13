// ---------------------------------------------------------------------------
// The row editor — opened by Edit, double-click or Enter (DESIGN §12).
// ---------------------------------------------------------------------------

Ext.define('PVE.meta.EditValueWindow', {
    extend: 'Ext.window.Window',
    xtype: 'pveMetaEditValueWindow',

    modal: true,
    width: 480,
    layout: 'fit',
    defaultButton: 'okBtn',
    // configs: rec (the tree record being edited)

    initComponent: function () {
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

        Ext.apply(me, {
            items: [
                {
                    xtype: 'form',
                    reference: 'form',
                    bodyPadding: 10,
                    border: false,
                    defaults: { anchor: '100%', labelWidth: 110 },
                    items: items,
                },
            ],
            buttons: [
                { text: gettext('OK'), itemId: 'okBtn', handler: () => me.submit() },
                { text: gettext('Cancel'), handler: () => me.close() },
            ],
        });
        me.callParent();
        me.on('show', () => me.down('#valueField').focus(true, 50));
    },

    submit: function () {
        let me = this;
        let form = me.down('form').getForm();
        if (!form.isValid()) {
            return;
        }
        let d = me.rec.data;
        let value;
        try {
            value = PVE.meta.Utils.parseValue(me.down('#valueField').getValue(), d.kind);
        } catch (err) {
            Ext.Msg.alert(gettext('Error'), Ext.htmlEncode(PVE.meta.Utils.errText(err)));
            return;
        }
        if (d.present && Ext.encode(value) === Ext.encode(d.rawValue)) {
            me.close(); // nothing actually changed
            return;
        }
        me.fireEvent('setvalue', value);
        me.close();
    },
});

