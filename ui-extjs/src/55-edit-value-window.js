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
        let value;
        if (d.present) {
            value = d.kind === 'boolean' || d.kind === 'number' ? d.rawValue : d.valueText;
        } else if (d.defaultValue !== undefined) {
            value = d.defaultValue;
        } else {
            value = d.kind === 'boolean' ? false : '';
        }
        // An enum's combobox holds its members as strings, and matches the value it
        // is given strictly: a stored `443` against `'443'` opened the field empty,
        // and OK on it then wrote whatever an empty field parses to.
        if (d.enumValues && value !== '') {
            value = String(value);
        }

        let items = [
            {
                xtype: 'displayfield',
                fieldLabel: gettext('Key'),
                value: Ext.htmlEncode(me.label()),
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

    // What is being edited: the path, and for a list member which one -- a member
    // shares its list's path, so the path alone named the whole list.
    label: function () {
        let d = this.rec.data;
        let member = d.arrayIndex !== undefined && d.arrayIndex !== null;
        return member ? d.path + '[' + d.arrayIndex + ']' : d.path;
    },

    initComponent: function () {
        let me = this;
        let d = me.rec.data;
        me.title = Ext.String.format(gettext('Edit: {0}'), Ext.htmlEncode(me.label()));
        if (PVE.meta.Utils.editorKind(d) === 'multiline') {
            me.width = 640; // room for the textarea
        }
        me.callParent();
        me.on('show', () => me.down('#valueField').focus(true, 50));
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
