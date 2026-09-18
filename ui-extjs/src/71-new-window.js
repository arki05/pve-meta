// ---------------------------------------------------------------------------
// "New Prefix" — the two fields a prefix definition cannot be created without.
//
// Everything else about a prefix definition is optional and is filled in by
// editing the document it creates; this only gets far enough that the file parses,
// because a file the loader would skip is refused on the way in (DESIGN §6).
// ---------------------------------------------------------------------------

Ext.define('PVE.meta.NewRegistryWindow', {
    extend: 'Ext.window.Window',
    xtype: 'pveMetaNewRegistryWindow',

    title: gettext('New Prefix'),
    modal: true,
    width: 620,
    layout: 'fit',

    initComponent: function () {
        let me = this;
        Ext.apply(me, {
            items: [
                {
                    xtype: 'form',
                    reference: 'form',
                    bodyPadding: 10,
                    border: false,
                    defaults: { anchor: '100%', labelWidth: 110 },
                    items: [
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
                    ],
                },
            ],
            buttons: [
                { text: gettext('Create'), handler: () => me.submit() },
                { text: gettext('Cancel'), handler: () => me.close() },
            ],
        });
        me.callParent();
        me.on('show', () => me.down('[name=name]').focus(true, 50));
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
        let form = me.down('form').getForm();
        if (!form.isValid()) {
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
