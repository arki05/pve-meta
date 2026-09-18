// ---------------------------------------------------------------------------
// FormWindow: the modal window + one form + primary/Cancel buttons every
// dialog here repeats. Subclasses give `formItems()` and `submit()`.
// ---------------------------------------------------------------------------

Ext.define('PVE.meta.FormWindow', {
    extend: 'Ext.window.Window',

    modal: true,
    layout: 'fit',
    primaryText: gettext('OK'),

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
                    items: me.formItems(),
                },
            ],
            buttons: [
                { text: me.primaryText, itemId: 'okBtn', handler: () => me.submit() },
                { text: gettext('Cancel'), handler: () => me.close() },
            ],
        });
        me.callParent();
    },

    // The one form, validated, or null. `submit` reads it with `getValues()` or,
    // for a field whose stored type `getValues()` would flatten, `down('#id')`.
    validForm: function () {
        let form = this.down('form').getForm();
        return form.isValid() ? form : null;
    },
});
