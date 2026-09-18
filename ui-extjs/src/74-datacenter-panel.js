// ---------------------------------------------------------------------------
// The datacenter tab: the prefix registry list.
// ---------------------------------------------------------------------------

Ext.define('PVE.meta.DatacenterPanel', {
    extend: 'Ext.panel.Panel',
    xtype: 'pveMetaDatacenterPanel',

    border: false,
    layout: 'fit',

    initComponent: function () {
        let me = this;
        Ext.apply(me, {
            items: [{ xtype: 'pveMetaRegistryGrid' }],
        });
        me.callParent();
    },
});
