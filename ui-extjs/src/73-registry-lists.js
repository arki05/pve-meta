// ---------------------------------------------------------------------------
// The two registry lists.
//
// A grid rather than a tree, because the interesting facts about these files are
// *columns*: which guests a prefix reaches, whether it carries a schema, and
// whether what you are looking at is a package's file or your own on top of one.
// A tree could show none of that, and showed a packaged definition and a cluster
// override as the same thing.
// ---------------------------------------------------------------------------

Ext.define('PVE.meta.RegistryGrid', {
    extend: 'Ext.grid.Panel',
    xtype: 'pveMetaRegistryGrid',

    kind: 'prefixes', // or 'permissions'
    border: false,
    emptyText: gettext('No entries'),

    // The row a list entry becomes. Kept out of initComponent so the offline suite
    // can check the mapping without a DOM.
    statics: {
        rowsFrom: function (kind, list) {
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
                    let name = kind === 'permissions' ? e.name : e.prefix;
                    return {
                        name: name,
                        id: kind + '/' + name,
                        error: e.error,
                        description: e.error,
                        authid: '',
                        summary: '',
                        selector: '',
                        schema: '',
                        enforce: '',
                        hidden: '',
                        origin: e.origin || 'cluster',
                        overrides: false,
                    };
                }
                if (kind === 'permissions') {
                    return {
                        name: e.name,
                        id: 'permissions/' + e.name,
                        authid: e.authid || '',
                        description: e.description || '',
                        // What it actually permits, in one line: prefix, mode and the
                        // selector that decides which guests it reaches.
                        summary: (e.rules || [])
                            .map((g) => g.prefix + ' (' + g.mode + ', ' + U.selectorText(g.selector) + ')')
                            .join(', '),
                        origin: e.origin || 'cluster',
                        overrides: !!e.overrides,
                    };
                }
                return {
                    name: e.prefix,
                    id: 'prefixes/' + e.prefix,
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

        // What the Origin column says. Three states, not two: a cluster file that
        // displaced a package's is the one where Remove does not remove anything --
        // it reverts to what the package ships.
        originText: function (row) {
            if (row.origin === 'packaged') {
                return gettext('packaged');
            }
            return row.overrides ? gettext('cluster (overrides packaged)') : gettext('cluster');
        },
    },

    initComponent: function () {
        let me = this;
        let isPrefix = me.kind === 'prefixes';
        me.store = Ext.create('Ext.data.Store', {
            fields: ['name', 'id', 'authid', 'description', 'selector', 'schema', 'enforce', 'hidden', 'summary', 'origin', 'overrides'],
            data: [],
            sorters: [{ property: 'name' }],
        });
        me.access = { write: 0 };

        let columns = [
            {
                text: isPrefix ? gettext('Prefix') : gettext('Name'),
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
        ];
        if (isPrefix) {
            columns.push(
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
            );
        } else {
            columns.push(
                { text: gettext('Auth ID'), dataIndex: 'authid', flex: 2, renderer: Ext.htmlEncode },
                { text: gettext('Rules'), dataIndex: 'summary', flex: 3, renderer: Ext.htmlEncode },
            );
        }
        columns.push(
            { text: gettext('Description'), dataIndex: 'description', flex: 2, renderer: Ext.htmlEncode },
            {
                text: gettext('Origin'),
                dataIndex: 'origin',
                width: 200,
                renderer: (v, meta, rec) =>
                    Ext.htmlEncode(PVE.meta.RegistryGrid.originText(rec.data)),
            },
        );

        Ext.apply(me, {
            columns: columns,
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
        set('addBtn', !may);
        // A packaged file is editable: the write creates the cluster override rather
        // than touching the package's copy (DESIGN §6). Removing one is not, since
        // there would be nothing of ours to remove.
        set('editBtn', !rec || !may);
        set('removeBtn', !rec || !may || rec.data.origin === 'packaged');
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
            url: '/meta/' + me.kind,
            success: function (response) {
                me.store.setData(PVE.meta.RegistryGrid.rowsFrom(me.kind, response.result.data || []));
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

    // "New" for both lists, and for a permission file both of the things that used
    // to be two buttons: name an existing principal, or make one.
    //
    // When it makes one, the order is chosen so a failure leaves the least behind: a
    // user with no token is inert, a token with no permission file grants nothing at
    // all, and only the last step makes anything true. Each failure says which step
    // it was and what already exists, because "create failed" with three PVE objects
    // half-made is not a message anyone can act on.
    createOne: function () {
        let me = this;
        let win = Ext.create('PVE.meta.NewRegistryWindow', { kind: me.kind });
        win.on('create', function (plan) {
            let made = [];
            let fail = (step) => (response) =>
                Ext.Msg.alert(
                    gettext('Error'),
                    Ext.htmlEncode(step) + ': ' +
                        (response.htmlStatus || Proxmox.Utils.getResponseErrorMessage(response)) +
                        (made.length
                            ? '<br><br>' +
                              Ext.htmlEncode(
                                  Ext.String.format(
                                      gettext('Already created: {0}. Remove it from Datacenter → Permissions, or run this again to reuse it.'),
                                      made.join(', '),
                                  ),
                              )
                            : ''),
                );
            let req = (opts) => Proxmox.Utils.API2Request(Ext.apply({ waitMsgTarget: me }, opts));

            let writeFile = function (secret) {
                req({
                    url: '/meta/' + me.kind + '/' + encodeURIComponent(plan.file),
                    method: 'PUT',
                    // `digest: ''` is "this file must not exist yet", so two
                    // administrators creating the same name is a 409 rather than one
                    // silently overwriting the other.
                    params: { data: Ext.encode(plan.content), mode: 'replace', digest: '' },
                    failure: fail(gettext('writing the file')),
                    success: function () {
                        me.reload();
                        if (secret) {
                            PVE.meta.ServiceToken.showSecret(plan, secret);
                            return;
                        }
                        me.editOne({ data: { id: me.kind + '/' + plan.file } });
                    },
                });
            };
            if (!plan.user) {
                writeFile(null);
                return;
            }
            let addAcl = function (secret) {
                if (!plan.acl) {
                    writeFile(secret);
                    return;
                }
                req({
                    url: '/access/acl',
                    method: 'PUT',
                    params: {
                        path: plan.acl.path,
                        roles: plan.acl.role,
                        propagate: plan.acl.propagate,
                        users: plan.user,
                    },
                    failure: fail(gettext('granting guest access')),
                    success: () => writeFile(secret),
                });
            };
            let addToken = function () {
                req({
                    url: '/access/users/' + encodeURIComponent(plan.user) + '/token/' +
                        encodeURIComponent(plan.tokenid),
                    method: 'POST',
                    // privsep off: this user exists only to carry this token, and with
                    // it on, a role added to the user later would silently not apply.
                    params: { privsep: 0 },
                    failure: fail(gettext('creating the token')),
                    success: function (response) {
                        made.push(plan.authid);
                        addAcl((response.result.data || {}).value);
                    },
                });
            };
            req({
                url: '/access/users',
                method: 'POST',
                // No password: this user cannot log in, only its token can act.
                params: { userid: plan.user, comment: 'pve-meta service principal' },
                failure: function (response) {
                    // An existing user is the normal case for a second token on the
                    // same principal, not an error to stop on.
                    if (String(response.htmlStatus || '').indexOf('already exists') !== -1) {
                        addToken();
                        return;
                    }
                    fail(gettext('creating the user'))(response);
                },
                success: function () {
                    made.push(plan.user);
                    addToken();
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
        // Say which of the two things this is. Removing an override does not remove
        // the prefix -- the packaged file underneath comes back.
        let question = rec.data.overrides
            ? Ext.String.format(
                  gettext('Remove the cluster file for "{0}"? The packaged one takes over again.'),
                  Ext.htmlEncode(rec.data.name),
              )
            : Ext.String.format(gettext('Remove "{0}"?'), Ext.htmlEncode(rec.data.name));
        Ext.Msg.confirm(gettext('Confirm'), question, function (btn) {
            if (btn !== 'yes') {
                return;
            }
            Proxmox.Utils.API2Request({
                url: '/meta/' + rec.data.id,
                method: 'DELETE',
                waitMsgTarget: me,
                success: () => me.reload(),
                failure: (response) =>
                    Ext.Msg.alert(gettext('Error'), response.htmlStatus || gettext('Error')),
            });
        });
    },
});

