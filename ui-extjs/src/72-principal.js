// ---------------------------------------------------------------------------
// Creating a service principal, for the "New" dialog's second half.
//
// Two things about PVE's model shape this:
//
// * **A `pve`-realm user with no password cannot log in at all** (`/access/ticket`
//   answers "authentication failure"), while its token keeps working. That is the
//   closest thing PVE has to a service principal: there is no userless API key, so
//   every token hangs off a user, and this makes that user a dead end.
// * **Privilege separation is an intersection.** With privsep on, a token's rights
//   are its own ACLs *and* its user's, so granting the user a role later would
//   silently do nothing. This user exists only to carry this token, so privsep off
//   is what makes "add a role later" behave the way anyone would expect.
//
// The optional role goes on `/vms`, not per-guest and not `/`: per-guest silently
// misses guests created later, and `PVEAuditor` on `/` would also hand over
// `Sys.Audit`, far more than a metadata reader needs.
// ---------------------------------------------------------------------------

Ext.define('PVE.meta.ServiceToken', {
    singleton: true,

    // A token id with no meaning, for when you do not want to invent one. Not a
    // secret -- PVE generates that itself and shows it once -- just a name.
    randomTokenId: function () {
        let out = '';
        for (let i = 0; i < 6; i++) {
            out += 'abcdefghijklmnopqrstuvwxyz0123456789'.charAt(Math.floor(Math.random() * 36));
        }
        return 't-' + out;
    },

    // The secret, once. PVE never shows it again and it cannot be recovered, so
    // this is a copyable field rather than a message: the one moment it exists.
    showSecret: function (plan, secret) {
        Ext.create('Ext.window.Window', {
            title: gettext('Service Token Created'),
            modal: true,
            width: 620,
            bodyPadding: 10,
            items: [
                {
                    xtype: 'form',
                    border: false,
                    defaults: { anchor: '100%', labelWidth: 120 },
                    items: [
                        {
                            xtype: 'displayfield',
                            fieldLabel: gettext('Token ID'),
                            value: Ext.htmlEncode(plan.authid),
                        },
                        {
                            xtype: 'textfield',
                            fieldLabel: gettext('Secret'),
                            value: secret || '',
                            editable: false,
                            selectOnFocus: true,
                        },
                        {
                            xtype: 'displayfield',
                            userCls: 'faded',
                            value: Ext.htmlEncode(
                                gettext(
                                    'Copy it now — PVE does not show it again. Use it as the header ' +
                                        'Authorization: PVEAPIToken=<id>=<secret>. It can touch nothing ' +
                                        'until you add rules to its permission file.',
                                ),
                            ),
                        },
                    ],
                },
            ],
            buttons: [{ text: gettext('Close'), handler: function () { this.up('window').close(); } }],
        }).show();
    },
});

