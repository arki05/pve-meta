// ---------------------------------------------------------------------------
// "Add Key" — its own window because the key is a path and the type picks the editor.
// ---------------------------------------------------------------------------

Ext.define('PVE.meta.AddKeyWindow', {
    extend: 'PVE.meta.FormWindow',
    xtype: 'pveMetaAddKeyWindow',

    title: gettext('Add Key'),
    width: 480,
    primaryText: gettext('Add'),
    parentPath: '', // dotted path of the map the key goes into ('' = the document root)
    list: false, // appending to a list instead: a member has no name to give it

    formItems: function () {
        let me = this;
        return [
            {
                xtype: 'displayfield',
                fieldLabel: me.list ? gettext('Append to') : gettext('Under'),
                value: Ext.htmlEncode(me.parentPath || gettext('(document root)')),
            },
            {
                xtype: 'textfield',
                name: 'key',
                allowBlank: me.list,
                hidden: me.list,
                fieldLabel: gettext('Key'),
                emptyText: gettext('key, or a dotted path'),
                // Leading and trailing dots are stripped on submit, so
                // do not fail the field for them while it is being typed.
                validator: (v) =>
                    me.list && !v
                        ? true
                        : PVE.meta.Utils.keyPathError(String(v || '').replace(/^\.+|\.+$/g, '')) || true,
            },
            {
                xtype: 'proxmoxKVComboBox',
                name: 'kind',
                fieldLabel: gettext('Type'),
                value: 'string',
                comboItems: [
                    ['string', gettext('String')],
                    ['number', gettext('Number')],
                    ['boolean', gettext('Boolean')],
                    ['array', gettext('Array')],
                    ['map', gettext('Map')],
                ],
                listeners: {
                    change: (f, v) => me.down('[name=value]').setDisabled(v === 'map'),
                },
            },
            { xtype: 'textfield', name: 'value', fieldLabel: gettext('Value') },
        ];
    },

    submit: function () {
        let me = this;
        let form = me.validForm();
        if (!form) {
            return;
        }
        let v = form.getValues();
        let key = String(v.key || '').replace(/^\.+|\.+$/g, '');
        try {
            if (!key && !me.list) {
                throw new Error(gettext('Key must not be empty'));
            }
            let value = v.kind === 'map' ? {} : PVE.meta.Utils.parseValue(v.value || '', v.kind);
            let path = PVE.meta.Utils.joinPath(me.parentPath, key);
            // Closed by the write, not by the click: a key refused by the server is
            // one the window still holds, ready to be corrected.
            PVE.meta.writeFromWindow(me, (done) => me.fireEvent('addkey', path, value, done));
        } catch (err) {
            PVE.meta.Utils.alertError(err);
        }
    },
});
