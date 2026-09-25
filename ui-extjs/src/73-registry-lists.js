// ---------------------------------------------------------------------------
// The registry list: prefix definitions, as a grid -- the interesting facts about
// these files (reach, schema, packaged vs. cluster override) are columns.
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
                // A file in the directory that did not load: its row says what is
                // wrong, in the column that would otherwise say what it does, and
                // Edit still opens it, which is where it gets repaired.
                if (e.error) {
                    return {
                        name: e.prefix,
                        id: PVE.meta.Doc.registryId(e.prefix),
                        error: e.error,
                        description: e.error,
                        selector: '',
                        schema: '',
                        enforce: '',
                        hidden: '',
                        nodes: '',
                        origin: e.origin || 'cluster',
                        overrides: false,
                    };
                }
                return {
                    name: e.prefix,
                    id: PVE.meta.Doc.registryId(e.prefix),
                    description: e.description || '',
                    selector: U.selectorText(e.selector),
                    schema: e.schema ? gettext('yes') : '',
                    enforce: e.enforce && e.enforce !== '0' ? gettext('yes') : '',
                    hidden: e.hidden && e.hidden !== '0' ? gettext('yes') : '',
                    // Per-node overrides live inside the file (DESIGN §3); the grid
                    // has no per-node row, so their names ride along on this one.
                    nodes: e.nodes ? Object.keys(e.nodes).sort().join(', ') : '',
                    origin: e.origin || 'cluster',
                    overrides: !!e.overrides,
                };
            });
        },

        // A text cell: encoded, with the whole of it as the cell's tooltip, since a
        // flexed column cuts off exactly the long ones. The tooltip is HTML inside
        // an attribute, so the encoded text is encoded once more for the attribute.
        textCell: function (value, meta) {
            let html = Ext.htmlEncode(value === undefined || value === null ? '' : String(value));
            if (html && meta) {
                meta.tdAttr = 'data-qtip="' + Ext.htmlEncode(html) + '"';
            }
            return html;
        },

        // What the Origin column says. A cluster file that displaced a package's is
        // the one where Remove does not remove anything -- it reverts to what the
        // package ships.
        originText: function (row) {
            if (row.origin === 'packaged') {
                return gettext('packaged');
            }
            return row.overrides ? gettext('cluster (overrides packaged)') : gettext('cluster');
        },
    },

    initComponent: function () {
        let me = this;
        me.store = Ext.create('Ext.data.Store', {
            fields: ['name', 'id', 'description', 'selector', 'schema', 'enforce', 'hidden', 'nodes', 'origin', 'overrides'],
            data: [],
            sorters: [{ property: 'name' }],
        });
        me.access = { write: 0 };
        let cell = (v, meta) => PVE.meta.RegistryGrid.textCell(v, meta);

        Ext.apply(me, {
            // Text flexes, flags do not: the prefix, what it is for and what it
            // reaches are what gets cut off, and a yes/empty column needs no more
            // than its header. Nodes shows only when some file has overrides.
            columns: [
                {
                    text: gettext('Prefix'),
                    dataIndex: 'name',
                    flex: 2,
                    minWidth: 120,
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
                    text: gettext('Description'),
                    dataIndex: 'description',
                    flex: 3,
                    minWidth: 160,
                    renderer: cell,
                },
                {
                    text: gettext('Applies to'),
                    dataIndex: 'selector',
                    flex: 2,
                    minWidth: 110,
                    renderer: cell,
                },
                { text: gettext('Schema'), dataIndex: 'schema', width: 75, renderer: Ext.htmlEncode },
                {
                    text: gettext('Enforced'),
                    dataIndex: 'enforce',
                    width: 85,
                    renderer: Ext.htmlEncode,
                    tooltip: gettext('A write that would not match the schema is refused unless saved anyway'),
                },
                {
                    text: gettext('Hidden'),
                    dataIndex: 'hidden',
                    width: 75,
                    renderer: Ext.htmlEncode,
                    tooltip: gettext('Declared keys are not offered as rows until something is stored there'),
                },
                {
                    text: gettext('Nodes'),
                    itemId: 'nodesCol',
                    dataIndex: 'nodes',
                    width: 100,
                    hidden: true,
                    renderer: cell,
                    tooltip: gettext('Nodes this file overrides schema, enforcement or visibility for'),
                },
                {
                    text: gettext('Origin'),
                    dataIndex: 'origin',
                    width: 190,
                    renderer: (v, meta, rec) =>
                        PVE.meta.RegistryGrid.textCell(PVE.meta.RegistryGrid.originText(rec.data), meta),
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
        // Any file opens (read is open to everyone); a packaged file is editable --
        // the write creates the cluster override (DESIGN §3) -- but not removable.
        set('editBtn', !rec);
        set('removeBtn', !rec || rec.data.origin === 'packaged' || !may);
    },

    request: function (opts) {
        PVE.meta.request(this, opts);
    },

    reload: function () {
        let me = this;
        me.request({
            url: '/meta/access',
            // No id: the registry's own answer, `write` is Sys.Modify on `/` (DESIGN §3).
            params: {},
            success: function (response) {
                me.access = response.result.data || { write: 0 };
                me.syncButtons();
            },
            failure: Ext.emptyFn,
        });
        me.request({
            url: '/meta/prefixes',
            // Every file, as it is, rather than a guest's resolved set.
            params: {},
            success: function (response) {
                // The failure below masks the grid with its message; nothing else
                // ever took it off, so one failed Reload hid every later one.
                Proxmox.Utils.setErrorMask(me, false);
                let rows = PVE.meta.RegistryGrid.rowsFrom(response.result.data || []);
                me.store.setData(rows);
                let nodes = me.down('#nodesCol');
                if (nodes) {
                    nodes.setHidden(!rows.some((r) => r.nodes));
                }
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
        win.on('create', function (plan, done) {
            Proxmox.Utils.API2Request({
                url: PVE.meta.Doc.urlFor(plan.id),
                method: 'PUT',
                waitMsgTarget: me,
                params: PVE.meta.Doc.docParams({
                    data: Ext.encode(plan.content),
                    mode: 'replace',
                    digest: '',
                }),
                failure: function (response) {
                    done(false);
                    // The empty digest matched nothing, so the name is taken: said
                    // in those words, since "digest mismatch" names a document the
                    // form never read. The list shows whose it is.
                    if (String((response.result || {}).status) === '409') {
                        me.reload();
                        Ext.Msg.alert(
                            gettext('Conflict'),
                            Ext.String.format(
                                gettext('A prefix named "{0}" already exists; pick another name or edit that one.'),
                                Ext.htmlEncode(plan.file),
                            ),
                        );
                        return;
                    }
                    Ext.Msg.alert(
                        gettext('Error'),
                        response.htmlStatus || Proxmox.Utils.getResponseErrorMessage(response),
                    );
                },
                success: function () {
                    done(true);
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
        // prefix -- the packaged file underneath comes back.
        let name = Ext.htmlEncode(rec.data.name);
        let question = rec.data.overrides
            ? Ext.String.format(
                  gettext('Remove the cluster file for "{0}"? The packaged one takes over again.'),
                  name,
              )
            : Ext.String.format(gettext('Remove "{0}"?'), name);
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
