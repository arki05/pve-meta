// ---------------------------------------------------------------------------
// "Declare Key" — the small form behind a prefix's `schema.properties.<key>`.
//
// A prefix definition's schema is a document like any other now, so this window writes one
// property of it with an ordinary view PUT; there is no second write path. It covers
// the seven things the editor actually consumes (type, description, optional,
// default, enum, minimum/maximum, format) plus `multiline`. Anything beyond that --
// nested `properties`, a shape this form has no field for -- is what "Edit as text"
// is for, and the schema row opens Monaco on itself.
//
// The key is not validated here on purpose: `schema.properties.<key>` is a document
// path like any other, so the server's one lint decides what a key may be and says
// so (DESIGN §7). A *dotted* key is refused, because that would silently declare a
// nested property rather than the one the form is asking about.
//
// There is deliberately **no Optional field**. Every key of a guest document is
// optional: nothing in pve-meta ever requires one, so `optional: 0` would be a claim
// no code reads and no write enforces. A missing value is a legitimate state -- an
// operator fills it in, or there is a reason it is not there -- and a `default` is an
// offer the row makes ("Set to default"), never something written behind your back.
// (`optional` survives in the meta-schema, DESIGN §6, because *those* files really do
// have required fields: a prefix file without a `selector` is refused on the way in.)
// ---------------------------------------------------------------------------

Ext.define('PVE.meta.DeclareKeyWindow', {
    extend: 'Ext.window.Window',
    xtype: 'pveMetaDeclareKeyWindow',

    modal: true,
    width: 520,
    layout: 'fit',
    prefix: '', // the prefix this declaration is for, shown in the header

    statics: {
        // The keys the form owns. Everything else in an existing declaration -- a
        // nested `properties`, a keyword this dialect has and the form does not
        // show, such as `optional` -- is kept as it was when the declaration is
        // edited: the form rewrites what it asks about and nothing more. Without
        // this, editing the description of a map declaration dropped every key
        // declared inside it.
        FORM_KEYS: ['type', 'description', 'default', 'enum', 'minimum', 'maximum', 'format', 'multiline', 'hidden', 'enforce'],

        // An edited declaration: the form's fields laid over what the declaration
        // already held outside them. The form's keys come first, in the order a
        // reader wants, and the kept ones after -- `properties` naturally last.
        merged: function (existing, fresh) {
            let out = Object.assign({}, fresh);
            let owned = PVE.meta.DeclareKeyWindow.FORM_KEYS;
            Object.keys(existing || {}).forEach(function (k) {
                if (owned.indexOf(k) === -1) {
                    out[k] = existing[k];
                }
            });
            return out;
        },
    },

    // Editing an existing declaration rather than adding one: the key is fixed
    // (renaming it is moving a key, not editing what it says) and every field
    // starts from what the declaration holds.
    declaration: null,
    keyName: '',

    initComponent: function () {
        let me = this;
        let editing = !!me.declaration;
        // One inherited flag as a radio group: Inherit (writes nothing), then the
        // word for `false` and the word for `true`.
        let tristate = (name, label, whenFalse, whenTrue) => ({
            xtype: 'radiogroup',
            fieldLabel: label,
            items: [
                { boxLabel: gettext('Inherit'), name: name, inputValue: 'inherit', checked: true },
                { boxLabel: whenFalse, name: name, inputValue: 'false' },
                { boxLabel: whenTrue, name: name, inputValue: 'true' },
            ],
        });
        me.title = editing
            ? Ext.String.format(gettext('Edit declaration: {0}'), Ext.htmlEncode(me.keyName))
            : gettext('Declare Key');
        let numeric = () => ['integer', 'number'].indexOf(me.down('[name=type]').getValue()) !== -1;
        let isString = () => me.down('[name=type]').getValue() === 'string';
        let sync = function () {
            let type = me.down('[name=type]').getValue();
            ['minimum', 'maximum'].forEach((n) => me.down('[name=' + n + ']').setDisabled(!numeric()));
            me.down('[name=format]').setDisabled(!isString());
            me.down('[name=multiline]').setDisabled(!isString());
            // A boolean picks its default from a list; a map has no default the
            // editor would ever read (`addShape` stops at an object and walks into
            // it), so offering one would be a field that does nothing.
            me.down('[name=defaultBool]').setHidden(type !== 'boolean');
            let plain = me.down('[name=default]');
            plain.setHidden(type === 'boolean');
            plain.setDisabled(type === 'object');
        };
        Ext.apply(me, {
            items: [
                {
                    xtype: 'form',
                    reference: 'form',
                    bodyPadding: 10,
                    border: false,
                    defaults: { anchor: '100%', labelWidth: 120 },
                    items: [
                        {
                            xtype: 'displayfield',
                            fieldLabel: gettext('Prefix'),
                            value: Ext.htmlEncode(me.prefix),
                        },
                        {
                            xtype: 'textfield',
                            name: 'key',
                            allowBlank: false,
                            fieldLabel: gettext('Key'),
                            emptyText: gettext('the key this declares'),
                            // A declared key becomes a real key in a real document,
                            // so it lives under the same charset -- and a schema that
                            // declares an unwritable key is worse than a rejected
                            // form, because nothing rejects it until someone tries.
                            validator: (v) => PVE.meta.Utils.keyPathError(v) || true,
                        },
                        {
                            xtype: 'proxmoxKVComboBox',
                            name: 'type',
                            fieldLabel: gettext('Type'),
                            value: 'string',
                            comboItems: [
                                ['string', gettext('String')],
                                ['integer', gettext('Integer')],
                                ['number', gettext('Number')],
                                ['boolean', gettext('Boolean')],
                                ['object', gettext('Map')],
                                ['array', gettext('Array')],
                            ],
                            listeners: { change: sync },
                        },
                        { xtype: 'textfield', name: 'description', fieldLabel: gettext('Description') },
                        // Two fields, one label: a boolean default typed into a text
                        // box is a trap. `parseValue` reads truth the way the row
                        // editor's checkbox produces it -- `true`/`1` -- so "True" or
                        // "yes" would have been accepted and stored as `false`, the
                        // opposite of what was typed, into a cluster-wide file.
                        { xtype: 'textfield', name: 'default', fieldLabel: gettext('Default') },
                        {
                            xtype: 'proxmoxKVComboBox',
                            name: 'defaultBool',
                            fieldLabel: gettext('Default'),
                            hidden: true,
                            value: '',
                            comboItems: [
                                ['', gettext('none')],
                                ['true', gettext('Yes')],
                                ['false', gettext('No')],
                            ],
                        },
                        {
                            xtype: 'textfield',
                            name: 'enum',
                            fieldLabel: gettext('Values'),
                            emptyText: gettext('comma-separated; leave empty for any'),
                        },
                        { xtype: 'numberfield', name: 'minimum', fieldLabel: gettext('Minimum'), hideTrigger: true },
                        { xtype: 'numberfield', name: 'maximum', fieldLabel: gettext('Maximum'), hideTrigger: true },
                        {
                            xtype: 'proxmoxKVComboBox',
                            name: 'format',
                            fieldLabel: gettext('Format'),
                            // `none`, not `''`: a KVComboBox whose key is the empty
                            // string hands back the store record's internal id
                            // (`KeyValue-1`) instead of the key, and it renders blank
                            // rather than showing its own default. Caught in a browser
                            // -- this wrote `format: KeyValue-1` into a schema, and the
                            // same shape broke Create Service Token outright.
                            value: 'none',
                            comboItems: [['none', gettext('none')]].concat(
                                Object.keys(PVE.meta.Utils.FORMAT_VTYPES).map((f) => [f, f]),
                            ),
                        },
                        {
                            xtype: 'proxmoxcheckbox',
                            name: 'multiline',
                            fieldLabel: gettext('Multi-line'),
                            boxLabel: gettext('edit this string in a text box'),
                        },
                        // Three states, not a checkbox, because the rule is inherit
                        // unless this node says otherwise: a key inside a hidden
                        // subtree that wants to be shown has to be able to say so,
                        // and a checkbox can only say "hidden" or "not stated".
                        //
                        // Radios rather than a dropdown, so all three states are
                        // readable at once and the label does not have to explain
                        // what the values mean. PVE's own UI uses them.
                        tristate('hidden', gettext('Visibility'), gettext('Show'), gettext('Hide')),
                        tristate('enforce', gettext('Validation'), gettext('Advisory'), gettext('Enforced')),
                    ],
                },
            ],
            buttons: [
                { text: editing ? gettext('OK') : gettext('Declare'), handler: () => me.submit() },
                { text: gettext('Cancel'), handler: () => me.close() },
            ],
        });
        me.callParent();
        me.on('show', function () {
            if (editing) {
                me.down('form').getForm().setValues(me.valuesFrom(me.declaration, me.keyName));
                me.down('[name=key]').setReadOnly(true);
            }
            sync();
            me.down(editing ? '[name=description]' : '[name=key]').focus(true, 50);
        });
    },

    // `schemaFrom` backwards: a declaration as the form's own fields, so editing
    // one starts from what it says. The two have to agree about every field, which
    // is why they sit next to each other -- a field one of them forgot is a field
    // that silently resets the moment somebody edits the key.
    valuesFrom: function (d, key) {
        let out = {
            key: key,
            type: d.type || 'string',
            description: d.description || '',
            enum: (d.enum || []).join(', '),
            minimum: d.minimum,
            maximum: d.maximum,
            format: d.format || 'none',
            multiline: !!d.multiline,
            hidden: d.hidden === undefined ? 'inherit' : String(!!d.hidden),
            enforce: d.enforce === undefined ? 'inherit' : String(!!d.enforce),
        };
        if (d.default !== undefined) {
            if (d.type === 'boolean') {
                out.defaultBool = String(!!d.default);
            } else {
                out.default = String(d.default);
            }
        }
        return out;
    },

    // The declaration, in the order a reader wants it, with every empty field left
    // out entirely: a schema full of nulls describes nothing and would not pass the
    // document lint anyway.
    schemaFrom: function (v) {
        let U = PVE.meta.Utils;
        let out = { type: v.type };
        if (v.description) {
            out.description = v.description;
        }
        let dflt = v.type === 'boolean' ? v.defaultBool : v.type === 'object' ? '' : v.default;
        if (dflt !== undefined && dflt !== '') {
            out.default = U.parseValue(dflt, U.schemaValueKind(v.type));
        }
        if (v.enum) {
            // Members typed like the key: an integer key's `enum: [80, 443]` must not
            // come back as strings after a trip through the form.
            let kind = U.schemaValueKind(v.type);
            out.enum = String(v.enum)
                .split(',')
                .map((x) => x.trim())
                .filter((x) => x !== '')
                .map((x) => (['integer', 'number'].indexOf(v.type) !== -1 ? U.parseValue(x, kind) : x));
        }
        ['minimum', 'maximum'].forEach(function (n) {
            if (['integer', 'number'].indexOf(v.type) !== -1 && v[n] !== undefined && v[n] !== '') {
                out[n] = Number(v[n]);
            }
        });
        if (v.type === 'string' && v.format && v.format !== 'none') {
            out.format = v.format;
        }
        if (v.type === 'string' && v.multiline) {
            out.multiline = 1;
        }
        // Omitted when inherited, which is what "inherit" means: the value comes
        // from the node above, or from the prefix. Writing `false` here would say
        // something different -- stop inheriting, and be visible/advisory -- so the
        // two are not interchangeable and neither is a default.
        ['hidden', 'enforce'].forEach(function (flag) {
            if (v[flag] === 'true') {
                out[flag] = true;
            } else if (v[flag] === 'false') {
                out[flag] = false;
            }
        });
        return out;
    },

    submit: function () {
        let me = this;
        let form = me.down('form').getForm();
        if (!form.isValid()) {
            return;
        }
        let v = form.getValues();
        let key = String(v.key).trim();
        try {
            if (key.indexOf('.') !== -1) {
                throw new Error(
                    gettext('A key is one name; edit the schema as text to nest one inside another'),
                );
            }
            me.fireEvent('declarekey', key, me.schemaFrom(v));
            me.close();
        } catch (err) {
            Ext.Msg.alert(gettext('Error'), Ext.htmlEncode(PVE.meta.Utils.errText(err)));
        }
    },
});

