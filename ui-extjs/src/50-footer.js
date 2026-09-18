// ---------------------------------------------------------------------------
// The editor footer — one bar, three editors.
//
// There are three places you edit a document here: the tree, the text card behind
// the Tree | Text toggle, and the text window over one subtree. They had grown
// three different chromes — the subtree window put its view switch on *top* and had
// no Format button at all, the tree put Apply and Revert on top, and a document
// window's Close sat at the bottom while the Apply for the same document sat at the
// top of the panel inside it. Nothing about the three is different enough to
// justify that.
//
// So: **which view you are looking at goes bottom-left, what you can do about it
// goes bottom-right**, and every editor builds both halves from here. The top
// toolbar is left for acting on the document's *contents* (Add, Edit, Remove,
// Declare Key), which is a different kind of thing from committing.
// ---------------------------------------------------------------------------

Ext.define('PVE.meta.Footer', {
    singleton: true,

    // cfg: { format: handler?, diff: handler?, apply: handler, applyText?,
    //        applyDisabled, secondary: handler, secondaryText }
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
        // Diff belongs with Apply, not with the view switches on the left: it answers
        // the same question Apply does -- what am I about to do to this document --
        // and it answers it without committing.
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
            // Named by the caller, because the two mean different things: the panel's
            // footer Apply *writes* the buffer, and a modal editor's button only hands
            // its result back -- the same OK the row editor and Add Key use.
            text: cfg.applyText || gettext('Apply'),
            itemId: 'metaApply',
            iconCls: 'fa fa-check',
            // Stated by the caller, never defaulted. Defaulting it to `disabled` meant
            // a caller that never enabled it got a button that looked ordinary and
            // did nothing at all -- no click, no request, no message -- which is
            // exactly what happened to the subtree window.
            disabled: !!cfg.applyDisabled,
            handler: cfg.apply,
        });
        out.push({
            text: cfg.secondaryText || gettext('Cancel'),
            itemId: 'metaSecondary',
            // The icon has to agree with the word, and only the panel's Revert throws
            // anything away: the panel swaps in the undo arrow when it says Revert.
            iconCls: 'fa fa-times',
            handler: cfg.secondary,
        });
        return out;
    },

    // The YAML | JSON view switch every text editor and the diff window carry.
    // `ui` per item, not the container's `defaultUI`: the latter only reaches a
    // child that has no `ui` of its own, and the theme's plain `default` is PVE's
    // blue primary button -- far too loud for a view switch in a bar of grey ones.
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
