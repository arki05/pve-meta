// ---------------------------------------------------------------------------
// The registry list: prefix definitions.
//
// A grid rather than a tree, because the interesting facts about these files are
// *columns*: which guests a prefix reaches, whether it carries a schema, and
// whether what you are looking at is a package's file, the cluster's on top of one,
// or one node's on top of either.
// A tree could show none of that, and showed a packaged definition and a cluster
// override as the same thing.
// ---------------------------------------------------------------------------

Ext.define('PVE.meta.RegistryGrid', {
    extend: 'Ext.grid.Panel',
    xtype: 'pveMetaRegistryGrid',

    border: false,
    emptyText: gettext('No entries'),

    // The row a list entry becomes. Kept out of initComponent so the offline suite
    // can check the mapping without a DOM.
    statics: {
        rowsFrom: function (list) {
            let U = PVE.meta.Utils;
            return (list || []).map(function (e) {
                // A file in the directory that did not load. It is named -- the
                // loader only ever tries files whose name is already valid -- and
                // that is the whole point: before this it was simply absent, so a
                // prefix that stopped parsing ceased to exist with nothing anywhere
                // saying so. Its row says what is wrong, in the column that would
                // otherwise say what it does, and Edit still opens it, which is
                // where it gets repaired.
                if (e.error) {
                    return {
                        name: e.prefix,
                        id: PVE.meta.Doc.registryId('prefixes', e.prefix, e.node),
                        node: e.node || '',
                        error: e.error,
                        description: e.error,
                        selector: '',
                        schema: '',
                        enforce: '',
                        hidden: '',
                        origin: e.origin || 'cluster',
                        overrides: false,
                    };
                }
                return {
                    name: e.prefix,
                    id: PVE.meta.Doc.registryId('prefixes', e.prefix, e.node),
                    node: e.node || '',
                    description: e.description || '',
                    selector: U.selectorText(e.selector),
                    schema: e.schema ? gettext('yes') : '',
                    enforce: e.enforce && e.enforce !== '0' ? gettext('yes') : '',
                    hidden: e.hidden && e.hidden !== '0' ? gettext('yes') : '',
                    origin: e.origin || 'cluster',
                    overrides: !!e.overrides,
                };
            });
        },

        // What the Origin column says. A cluster file that displaced a package's is
        // the one where Remove does not remove anything -- it reverts to what the
        // package ships -- and a node file that displaced a cluster-wide one reverts
        // that node's guests to the cluster or packaged file.
        originText: function (row) {
            if (row.origin === 'packaged') {
                return gettext('packaged');
            }
            if (row.origin === 'node') {
                return row.overrides ? gettext('node (overrides cluster or packaged)') : gettext('node');
            }
            return row.overrides ? gettext('cluster (overrides packaged)') : gettext('cluster');
        },
    },

    initComponent: function () {
        let me = this;
        me.store = Ext.create('Ext.data.Store', {
            fields: ['name', 'id', 'node', 'description', 'selector', 'schema', 'enforce', 'hidden', 'origin', 'overrides'],
            data: [],
            sorters: [{ property: 'name' }, { property: 'node' }],
        });
        me.access = { write: 0 };

        Ext.apply(me, {
            columns: [
                {
                    text: gettext('Prefix'),
                    dataIndex: 'name',
                    flex: 2,
                    // The same marker the tree puts on a row whose value does not match
                    // its schema, one step further out: this whole file does not match
                    // the format it is in. The Description column carries the parser's
                    // own words, so the icon does not have to say anything.
                    renderer: (v, meta, rec) =>
                        rec.data.error
                            ? Ext.htmlEncode(v) + ' <i class="fa fa-exclamation-triangle warning"></i>'
                            : Ext.htmlEncode(v),
                },
                {
                    text: gettext('Node'),
                    dataIndex: 'node',
                    width: 110,
                    renderer: Ext.htmlEncode,
                    tooltip: gettext('A node file reaches only the guests on that node'),
                },
                { text: gettext('Applies to'), dataIndex: 'selector', flex: 1, renderer: Ext.htmlEncode },
                { text: gettext('Schema'), dataIndex: 'schema', width: 90, renderer: Ext.htmlEncode },
                {
                    text: gettext('Enforced'),
                    dataIndex: 'enforce',
                    width: 90,
                    renderer: Ext.htmlEncode,
                    tooltip: gettext('A write that would not match the schema is refused unless saved anyway'),
                },
                {
                    text: gettext('Hidden'),
                    dataIndex: 'hidden',
                    width: 80,
                    renderer: Ext.htmlEncode,
                    tooltip: gettext('Declared keys are not offered as rows until something is stored there'),
                },
                { text: gettext('Description'), dataIndex: 'description', flex: 2, renderer: Ext.htmlEncode },
                {
                    text: gettext('Origin'),
                    dataIndex: 'origin',
                    width: 200,
                    renderer: (v, meta, rec) =>
                        Ext.htmlEncode(PVE.meta.RegistryGrid.originText(rec.data)),
                },
            ],
            tbar: [
                {
                    text: gettext('Add'),
                    itemId: 'addBtn',
                    iconCls: 'fa fa-plus',
                    disabled: true,
                    handler: () => me.createOne(),
                },
                {
                    text: gettext('Edit'),
                    itemId: 'editBtn',
                    iconCls: 'fa fa-pencil',
                    disabled: true,
                    handler: () => me.editOne(me.getSelection()[0]),
                },
                {
                    text: gettext('Remove'),
                    itemId: 'removeBtn',
                    iconCls: 'fa fa-trash-o',
                    disabled: true,
                    handler: () => me.removeOne(me.getSelection()[0]),
                },
                '->',
                { text: gettext('Reload'), iconCls: 'fa fa-refresh', handler: () => me.reload() },
            ],
            listeners: {
                itemdblclick: (view, rec) => me.editOne(rec),
                selectionchange: () => me.syncButtons(),
            },
        });
        me.callParent();
        me.on('afterrender', () => me.reload());
    },

    syncButtons: function () {
        let me = this;
        let rec = me.getSelection()[0];
        let may = !!me.access.write;
        let set = function (id, disabled) {
            let btn = me.down('#' + id);
            if (btn) {
                btn.setDisabled(disabled);
            }
        };
        // Add writes to the cluster directory by default, which is Sys.Modify on `/`.
        set('addBtn', !may);
        // Any file opens: read is open to everyone, and the document editor asks
        // about write for that one id. A packaged file is editable -- the write
        // creates the cluster override rather than touching the package's copy
        // (DESIGN §6) -- and removing one is not, since there would be nothing of
        // ours to remove. A node file's write is Sys.Modify on that node, which the
        // registry answer does not say, so its Remove is left to the server's 403.
        set('editBtn', !rec);
        set(
            'removeBtn',
            !rec || rec.data.origin === 'packaged' || (rec.data.origin !== 'node' && !may),
        );
    },

    request: function (opts) {
        PVE.meta.request(this, opts);
    },

    reload: function () {
        let me = this;
        me.request({
            url: '/meta/access',
            // No id: the registry's own answer, whose `write` is Sys.Modify on `/`
            // (DESIGN §6). A list needs no file to ask about.
            params: {},
            success: function (response) {
                me.access = response.result.data || { write: 0 };
                me.syncButtons();
            },
            failure: Ext.emptyFn,
        });
        me.request({
            url: '/meta/prefixes',
            // Every file, a node's beside the cluster's, rather than a resolved set.
            params: { all: 1 },
            success: function (response) {
                me.store.setData(PVE.meta.RegistryGrid.rowsFrom(response.result.data || []));
                me.syncButtons();
            },
            failure: (response) =>
                Proxmox.Utils.setErrorMask(me, response.htmlStatus || gettext('Error')),
        });
    },

    editOne: function (rec) {
        let me = this;
        if (!rec) {
            return;
        }
        let win = Ext.create('PVE.meta.DocumentWindow', { docId: rec.data.id });
        win.on('destroy', () => me.reload());
        win.show();
    },

    // "New Prefix": writes the file with `digest: ''`, so two administrators
    // creating the same name is a 409 rather than one silently overwriting the
    // other.
    createOne: function () {
        let me = this;
        let win = Ext.create('PVE.meta.NewRegistryWindow', {});
        win.on('create', function (plan) {
            Proxmox.Utils.API2Request({
                url: PVE.meta.Doc.urlFor(plan.id),
                method: 'PUT',
                waitMsgTarget: me,
                params: PVE.meta.Doc.docParams({
                    data: Ext.encode(plan.content),
                    mode: 'replace',
                    digest: '',
                }),
                failure: (response) =>
                    Ext.Msg.alert(
                        gettext('Error'),
                        response.htmlStatus || Proxmox.Utils.getResponseErrorMessage(response),
                    ),
                success: function () {
                    me.reload();
                    me.editOne({ data: { id: plan.id } });
                },
            });
        });
        win.show();
    },

    removeOne: function (rec) {
        let me = this;
        if (!rec || rec.data.origin === 'packaged') {
            return;
        }
        // Say which of the things this is. Removing an override does not remove the
        // prefix -- the file underneath comes back, for every guest or for the guests
        // on that node.
        let name = Ext.htmlEncode(rec.data.name);
        let node = Ext.htmlEncode(rec.data.node || '');
        let question;
        if (rec.data.origin === 'node') {
            question = rec.data.overrides
                ? Ext.String.format(
                      gettext('Remove the file for "{0}" on node {1}? The cluster or packaged file takes over again there.'),
                      name,
                      node,
                  )
                : Ext.String.format(gettext('Remove "{0}" from node {1}?'), name, node);
        } else {
            question = rec.data.overrides
                ? Ext.String.format(
                      gettext('Remove the cluster file for "{0}"? The packaged one takes over again.'),
                      name,
                  )
                : Ext.String.format(gettext('Remove "{0}"?'), name);
        }
        Ext.Msg.confirm(gettext('Confirm'), question, function (btn) {
            if (btn !== 'yes') {
                return;
            }
            Proxmox.Utils.API2Request({
                url: PVE.meta.Doc.urlFor(rec.data.id),
                method: 'DELETE',
                waitMsgTarget: me,
                success: () => me.reload(),
                failure: (response) =>
                    Ext.Msg.alert(gettext('Error'), response.htmlStatus || gettext('Error')),
            });
        });
    },
});
