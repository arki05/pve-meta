// ---------------------------------------------------------------------------
// "Add Rule" — one entry of a permission file, as a form.
//
// The same shape as Declare Key, one document over: a permission file's `rules`
// is the whole point of the file, and leaving it to the text editor made the
// interesting part of it the one part with no affordance.
//
// It appends rather than edits, and it stages like everything else. Appending
// replaces the whole `rules` array, because a view addresses through maps only —
// there is no path to `rules[1]` (DESIGN §2). Changing or removing a rule is
// still the text editor; adding one is what you do a hundred times more often.
// ---------------------------------------------------------------------------

Ext.define('PVE.meta.AddRuleWindow', {
    extend: 'Ext.window.Window',
    xtype: 'pveMetaAddRuleWindow',

    title: gettext('Add Rule'),
    modal: true,
    width: 520,
    layout: 'fit',
    prefixes: null, // the declared prefixes, for the combobox
    existing: null, // the rules already in the file (appending)
    rule: null, // the rule being edited, if this is an edit

    initComponent: function () {
        let me = this;
        // One entry per prefix: the unscoped listing names a prefix once per file, and
        // a cluster file and a node file of one name are one prefix to a rule.
        let seen = Object.create(null);
        let declared = (me.prefixes || [])
            .filter((p) => !seen[p.prefix] && (seen[p.prefix] = true))
            .map((p) => [p.prefix, p.prefix]);
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
                            // Editable on purpose: a rule may name a prefix nobody has
                            // declared yet. The list is a convenience, not a
                            // constraint -- permissions and prefix definitions are
                            // independent files and neither waits for the other.
                            xtype: 'combobox',
                            name: 'prefix',
                            fieldLabel: gettext('Prefix'),
                            allowBlank: false,
                            store: declared,
                            queryMode: 'local',
                            editable: true,
                            forceSelection: false,
                            emptyText: gettext('a declared prefix, or any key path'),
                        },
                        {
                            xtype: 'proxmoxKVComboBox',
                            name: 'mode',
                            fieldLabel: gettext('Mode'),
                            value: 'ro',
                            comboItems: [
                                ['ro', gettext('Read only')],
                                ['rw', gettext('Read and write')],
                            ],
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
                        { xtype: 'textfield', name: 'tag', fieldLabel: gettext('Tag'), hidden: true },
                    ],
                },
            ],
            buttons: [
                { text: me.rule ? gettext('OK') : gettext('Add'), handler: () => me.submit() },
                { text: gettext('Cancel'), handler: () => me.close() },
            ],
        });
        me.callParent();
        me.on('show', function () {
            if (me.rule) {
                let sel = me.rule.selector || {};
                me.down('form').getForm().setValues({
                    prefix: me.rule.prefix,
                    mode: me.rule.mode,
                    selector: sel.tag ? 'tag' : 'all',
                    tag: sel.tag || '',
                });
            }
            me.down('[name=prefix]').focus(true, 50);
        });
    },

    statics: {
        // The `rules` list this form would produce. Pure, so the offline suite can
        // check the arithmetic without a DOM.
        rulesWith: function (existing, v) {
            let rule = {
                prefix: String(v.prefix).trim(),
                mode: v.mode === 'rw' ? 'rw' : 'ro',
                selector: v.selector === 'tag' ? { tag: String(v.tag).trim() } : { all: true },
            };
            return (Array.isArray(existing) ? existing : []).concat([rule]);
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
        me.fireEvent('addrule', PVE.meta.AddRuleWindow.rulesWith(me.existing, v));
        me.close();
    },
});

