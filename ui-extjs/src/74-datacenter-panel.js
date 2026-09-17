// ---------------------------------------------------------------------------
// The datacenter tab: the two registry lists.
//
// Two sub-tabs rather than one tree of everything. They are two different kinds
// of thing -- a list of prefix definitions, a list of permissions -- and drawing
// them as branches of a single tree claimed a relationship they do not have,
// while hiding the columns that make a list worth reading.
// ---------------------------------------------------------------------------

Ext.define('PVE.meta.DatacenterPanel', {
    extend: 'Ext.tab.Panel',
    xtype: 'pveMetaDatacenterPanel',

    border: false,
    defaults: { border: false },

    initComponent: function () {
        let me = this;
        Ext.apply(me, {
            items: [
                {
                    title: gettext('Prefixes'),
                    iconCls: 'fa fa-sitemap',
                    xtype: 'pveMetaRegistryGrid',
                    kind: 'prefixes',
                },
                {
                    title: gettext('Permissions'),
                    iconCls: 'fa fa-key',
                    xtype: 'pveMetaRegistryGrid',
                    kind: 'permissions',
                },
            ],
        });
        me.callParent();
    },
});
