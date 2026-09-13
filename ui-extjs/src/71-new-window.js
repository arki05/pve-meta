// ---------------------------------------------------------------------------
// "New" — the two fields a registry file cannot be created without.
//
// Everything else about a prefix definition is optional and is filled in by
// editing the document it creates; this only gets far enough that the file parses,
// because a file the loader would skip is refused on the way in (DESIGN §6).
// ---------------------------------------------------------------------------

Ext.define('PVE.meta.NewRegistryWindow', {
    extend: 'Ext.window.Window',
    xtype: 'pveMetaNewRegistryWindow',

    modal: true,
    width: 620,
    layout: 'fit',
    kind: 'prefixes',

    initComponent: function () {
        let me = this;
        let isPrefix = me.kind === 'prefixes';
        me.title = isPrefix ? gettext('New Prefix') : gettext('New Permission');
        let items = [
            {
                xtype: 'textfield',
                name: 'name',
                allowBlank: false,
                fieldLabel: isPrefix ? gettext('Prefix') : gettext('Name'),
                // The file name *is* the prefix, so a nested one is dotted and this
                // field is the whole identity of what is being created. The rule is
                // the core's (`registry::is_valid_file_name`); the server refuses the
                // same names on its own, this only says so before the round trip.
                emptyText: isPrefix ? gettext('e.g. homelab.docker') : gettext('file name'),
                validator: (v) => PVE.meta.Utils.fileNameError(v) || true,
            },
        ];
        if (isPrefix) {
            items.push(
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
            );
        } else {
            // One dialog, not two. "Add" and "Create Service Token" were the same act
            // -- write a permission file for a principal -- differing only in whether
            // the principal exists yet, and a second button for that is a question the
            // dialog can just ask.
            let toggle = function () {
                let fresh = me.down('[name=principal]').getValue() === 'new';
                ['authid'].forEach((n) => me.down('[name=' + n + ']').setHidden(fresh));
                ['user', 'tokenid', 'role'].forEach((n) =>
                    me.down('[name=' + n + ']').setHidden(!fresh),
                );
                me.down('#tokenNote').setHidden(!fresh);
                me.down('[name=authid]').allowBlank = fresh;
                me.down('[name=user]').allowBlank = !fresh;
                me.down('[name=tokenid]').allowBlank = !fresh;
            };
            items.push(
                {
                    xtype: 'proxmoxKVComboBox',
                    name: 'principal',
                    fieldLabel: gettext('For'),
                    value: 'new',
                    comboItems: [
                        ['new', gettext('A new service token')],
                        ['existing', gettext('An existing user or token')],
                    ],
                    listeners: { change: toggle },
                },
                {
                    // Editable: a permission file may name a principal that does not
                    // exist yet, and the parser only checks the *shape* of an authid.
                    xtype: 'combobox',
                    name: 'authid',
                    fieldLabel: gettext('Auth ID'),
                    hidden: true,
                    allowBlank: true,
                    store: [],
                    queryMode: 'local',
                    editable: true,
                    forceSelection: false,
                    emptyText: gettext('user@realm, or user@realm!tokenid'),
                },
                {
                    xtype: 'textfield',
                    name: 'user',
                    fieldLabel: gettext('User'),
                    value: '@pve',
                    emptyText: 'traefik@pve',
                    regex: /^[^\s@]+@[A-Za-z0-9-]+$/,
                    regexText: gettext('A user id is user@realm'),
                    listeners: {
                        change: function (f, v) {
                            let name = me.down('[name=name]');
                            if (!name.isDirty()) {
                                name.setValue(String(v).split('@')[0]);
                            }
                        },
                    },
                },
                {
                    xtype: 'fieldcontainer',
                    fieldLabel: gettext('Token ID'),
                    layout: 'hbox',
                    items: [
                        {
                            xtype: 'textfield',
                            name: 'tokenid',
                            flex: 1,
                            value: 'meta',
                            regex: /^[A-Za-z0-9_-]+$/,
                            regexText: gettext('Letters, digits, - and _'),
                        },
                        {
                            xtype: 'button',
                            text: gettext('Generate'),
                            margin: '0 0 0 5',
                            handler: () =>
                                me.down('[name=tokenid]').setValue(PVE.meta.ServiceToken.randomTokenId()),
                        },
                    ],
                },
                {
                    xtype: 'proxmoxKVComboBox',
                    name: 'role',
                    fieldLabel: gettext('Guest access'),
                    // `none`, not `''`: a KVComboBox whose key is the empty string
                    // hands back the store record's internal id (`KeyValue-1`).
                    value: 'none',
                    comboItems: [
                        ['none', gettext('None — metadata only')],
                        ['PVEAuditor', gettext('Read guest configs (PVEAuditor on /vms)')],
                        ['PVEVMAdmin', gettext('Manage guests (PVEVMAdmin on /vms)')],
                    ],
                },
                {
                    xtype: 'displayfield',
                    itemId: 'tokenNote',
                    userCls: 'faded',
                    value: Ext.htmlEncode(
                        gettext(
                            'A new service token is a pve-realm user that cannot log in, with one token on ' +
                                'it. Guest access is a PVE role on /vms, covering guests created later too — ' +
                                'and it also lets this principal read ALL metadata on those guests, since ' +
                                'VM.Audit is full read. Writes stay inside the rules you give it.',
                        ),
                    ),
                },
                { xtype: 'textfield', name: 'description', fieldLabel: gettext('Description') },
            );
        }
        Ext.apply(me, {
            items: [
                {
                    xtype: 'form',
                    reference: 'form',
                    bodyPadding: 10,
                    border: false,
                    defaults: { anchor: '100%', labelWidth: 110 },
                    items: items,
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
            let box = me.down('[name=authid]');
            if (!box) {
                return;
            }
            // Users and their tokens in one call, so the picker costs one request.
            Proxmox.Utils.API2Request({
                url: '/access/users',
                params: { full: 1 },
                method: 'GET',
                failure: Ext.emptyFn, // a picker that did not load is still typable
                success: function (response) {
                    let out = [];
                    (response.result.data || []).forEach(function (u) {
                        out.push(u.userid);
                        (u.tokens || []).forEach((t) => out.push(u.userid + '!' + t.tokenid));
                    });
                    box.setStore(out);
                },
            });
        });
    },

    statics: {
        // Everything the dialog will do, as data: the file to write, and -- when the
        // principal does not exist yet -- the PVE objects to make first. Pure, so the
        // offline suite can check the order and the shape without a browser.
        //
        // The permission file must name the **token**, not the user. Naming the user
        // produces a file that parses, loads, and grants the token nothing.
        planFrom: function (kind, v) {
            let out = { file: String(v.name || '').trim(), content: {} };
            if (kind !== 'permissions') {
                if (v.description) {
                    out.content.description = v.description;
                }
                out.content.selector = v.selector === 'tag' ? { tag: v.tag } : { all: true };
                return out;
            }
            if (v.principal === 'new') {
                out.user = String(v.user || '').trim();
                out.tokenid = String(v.tokenid || '').trim();
                out.authid = out.user + '!' + out.tokenid;
                if (v.role && v.role !== 'none') {
                    out.acl = { path: '/vms', role: v.role, propagate: 1 };
                }
            } else {
                out.authid = String(v.authid || '').trim();
            }
            out.content.authid = out.authid;
            if (v.description) {
                out.content.description = v.description;
            }
            // No rules on purpose: it permits nothing until an administrator says what.
            out.content.rules = [];
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
        if (me.kind === 'prefixes' && v.selector === 'tag' && !String(v.tag).trim()) {
            Ext.Msg.alert(gettext('Error'), gettext('A tag selector needs a tag'));
            return;
        }
        let plan = PVE.meta.NewRegistryWindow.planFrom(me.kind, v);
        if (me.kind === 'permissions' && !plan.authid) {
            Ext.Msg.alert(gettext('Error'), gettext('A permission needs an auth id'));
            return;
        }
        me.fireEvent('create', plan);
        me.close();
    },
});

