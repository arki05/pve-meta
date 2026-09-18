// ---------------------------------------------------------------------------
// "New Prefix" — the two fields a prefix definition cannot be created without.
// Everything else is optional and filled in by editing the document it creates;
// this only gets far enough that the file parses (DESIGN §3).
// ---------------------------------------------------------------------------

Ext.define('PVE.meta.NewRegistryWindow', {
    extend: 'PVE.meta.FormWindow',
    xtype: 'pveMetaNewRegistryWindow',

    title: gettext('New Prefix'),
    width: 620,
    primaryText: gettext('Create'),

    formItems: function () {
        let me = this;
        return [
            {
                xtype: 'textfield',
                name: 'name',
                allowBlank: false,
                fieldLabel: gettext('Prefix'),
                // The file name *is* the prefix, so a nested one is dotted and this
                // field is the whole identity of what is being created. The rule is
                // the core's (`registry::is_valid_file_name`); the server refuses the
                // same names on its own, this only says so before the round trip.
                emptyText: gettext('e.g. homelab.docker'),
                validator: (v) => PVE.meta.Utils.fileNameError(v) || true,
            },
            {
                xtype: 'proxmoxKVComboBox',
                name: 'selector',
                fieldLabel: gettext('Applies to'),
                value: 'all',
                comboItems: [
                    ['all', gettext('Every guest')],
                    ['tag', gettext('Guests with a tag')],
                ],
                listeners: {
                    change: (f, v) => me.down('[name=tag]').setHidden(v !== 'tag'),
                },
            },
            {
                xtype: 'textfield',
                name: 'tag',
                fieldLabel: gettext('Tag'),
                hidden: true,
            },
            { xtype: 'textfield', name: 'description', fieldLabel: gettext('Description') },
        ];
    },

    initComponent: function () {
        this.callParent();
        this.on('show', () => this.down('[name=name]').focus(true, 50));
    },

    statics: {
        // Everything the dialog will do, as data: the file to write. Pure, so the
        // offline suite can check the shape without a browser.
        planFrom: function (v) {
            let out = { file: String(v.name || '').trim(), content: {} };
            out.id = PVE.meta.Doc.registryId(out.file);
            if (v.description) {
                out.content.description = v.description;
            }
            out.content.selector = v.selector === 'tag' ? { tag: v.tag } : { all: true };
            return out;
        },
    },

    submit: function () {
        let me = this;
        let form = me.validForm();
        if (!form) {
            return;
        }
        let v = form.getValues();
        if (v.selector === 'tag' && !String(v.tag).trim()) {
            Ext.Msg.alert(gettext('Error'), gettext('A tag selector needs a tag'));
            return;
        }
        me.fireEvent('create', PVE.meta.NewRegistryWindow.planFrom(v));
        me.close();
    },
});
