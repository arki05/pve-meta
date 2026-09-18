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
                            // Where the file lives: the cluster's directory, reaching guests on
                            // every node, or one node's, reaching the guests on that node and
                            // overriding a cluster file of the same name there (DESIGN §3). The
                            // nodes are filled in on show.
                            xtype: 'proxmoxKVComboBox',
                            name: 'location',
                            fieldLabel: gettext('Location'),
                            value: PVE.meta.NewRegistryWindow.CLUSTER_LOCATION,
                            comboItems: [[PVE.meta.NewRegistryWindow.CLUSTER_LOCATION, gettext('Cluster (every node)')]],
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
        me.on('show', function () {
            me.down('[name=name]').focus(true, 50);
            Proxmox.Utils.API2Request({
                url: '/nodes',
                method: 'GET',
                failure: Ext.emptyFn, // the cluster is still a location
                success: function (response) {
                    me.down('[name=location]').setComboItems(
                        PVE.meta.NewRegistryWindow.locationItems(response.result.data || []),
                    );
                },
            });
        });
    },

    statics: {
        // The Location key for the cluster directory. Not a node name: `_` is not a
        // node-name character, and '' is not a usable KVComboBox key.
        CLUSTER_LOCATION: '__cluster__',

        // The Location choices: the cluster first, then every node by name, from
        // `GET /nodes`.
        locationItems: function (nodes) {
            let names = nodes.map((n) => n.node).filter((n) => n);
            names.sort();
            return [[this.CLUSTER_LOCATION, gettext('Cluster (every node)')]].concat(
                names.map((n) => [n, Ext.String.format(gettext('Node {0}'), n)]),
            );
        },

        // Everything the dialog will do, as data: the file to write. Pure, so the
        // offline suite can check the shape without a browser.
        planFrom: function (v) {
            let out = { file: String(v.name || '').trim(), content: {} };
            if (v.location && v.location !== this.CLUSTER_LOCATION) {
                out.node = v.location;
            }
            out.id = PVE.meta.Doc.registryId('prefixes', out.file, out.node);
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
