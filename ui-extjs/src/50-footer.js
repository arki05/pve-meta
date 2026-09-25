// ---------------------------------------------------------------------------
// The editor footer, shared by the tree, the text card and the text window: which
// view you are looking at goes bottom-left, what you can do about it bottom-right.
// ---------------------------------------------------------------------------

Ext.define('PVE.meta.Footer', {
    singleton: true,

    // cfg: { format: handler?, diff: handler?, apply: handler, applyText?,
    //        applyDisabled, applyHidden, secondary: handler, secondaryText }
    actions: function (cfg) {
        let out = [];
        if (cfg.format) {
            out.push({
                text: gettext('Format'),
                itemId: 'metaFormat',
                iconCls: 'fa fa-indent',
                tooltip: gettext('Re-indent the buffer canonically'),
                handler: cfg.format,
            });
        }
        out.push('->');
        // Diff belongs with Apply, not the view switches: same question, no commit.
        if (cfg.diff) {
            out.push({
                text: gettext('Diff'),
                itemId: 'metaDiff',
                iconCls: 'fa fa-exchange',
                tooltip: gettext('Show what Apply would write, against the stored document'),
                handler: cfg.diff,
            });
        }
        out.push({
            // Named by the caller: writes the buffer in the panel's footer, only
            // hands the result back in a modal editor (the row editor's OK).
            text: cfg.applyText || gettext('Apply'),
            itemId: 'metaApply',
            iconCls: 'fa fa-check',
            disabled: !!cfg.applyDisabled,
            // Not offered at all where the caller may not write: a button that
            // exists only to be refused is worse than no button (DESIGN §4).
            hidden: !!cfg.applyHidden,
            handler: cfg.apply,
        });
        out.push({
            text: cfg.secondaryText || gettext('Cancel'),
            itemId: 'metaSecondary',
            iconCls: 'fa fa-times', // the panel swaps in the undo arrow when it says Revert
            handler: cfg.secondary,
        });
        return out;
    },

    // The YAML | JSON view switch every text editor and the diff window carry.
    // `ui` per item, not the container's `defaultUI`, which would only reach a
    // child with no `ui` of its own -- the theme's default is too loud here.
    // cfg: { onChange(lang), value?, itemId?, reference?, hidden? }
    langToggle: function (cfg) {
        let out = {
            xtype: 'segmentedbutton',
            value: cfg.value || 'yaml',
            hidden: !!cfg.hidden,
            items: [
                { text: 'YAML', value: 'yaml', ui: 'default-toolbar' },
                { text: 'JSON', value: 'json', ui: 'default-toolbar' },
            ],
            listeners: { change: (btn, value) => cfg.onChange(value) },
        };
        ['itemId', 'reference'].forEach(function (k) {
            if (cfg[k]) {
                out[k] = cfg[k];
            }
        });
        return out;
    },
});
