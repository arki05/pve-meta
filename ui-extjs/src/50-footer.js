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
// Declare Key, Add Rule), which is a different kind of thing from committing.
// ---------------------------------------------------------------------------

Ext.define('PVE.meta.Footer', {
    singleton: true,

    // cfg: { format: handler?, apply: handler, secondary: handler, secondaryText }
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
        // Diff belongs with Apply and Revert, not with the view switches on the left:
        // it answers the same question they do -- what am I about to do to this
        // document -- and it answers it without committing. It is also not a text-mode
        // button. What is staged is a property of the document, so the tree has a diff
        // to show as much as the buffer does, and checking before Apply is exactly when
        // you want it.
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
            // footer Apply *writes*, and a modal editor's button only hands its result
            // back -- the same OK the row editor and Add Key use. One word for both was
            // how the subtree window came to write directly and see nothing staged.
            // `sync` rewrites this text on every keystroke, so the caller says the
            // word there too (`state.applyText`).
            text: cfg.applyText || gettext('Apply'),
            itemId: 'metaApply',
            iconCls: 'fa fa-check',
            // Stated by the caller, never defaulted. Defaulting it to `disabled` meant
            // a caller that never called `sync` got a button that looked ordinary and
            // did nothing at all -- no click, no request, no message -- which is
            // exactly what happened to the subtree window. A shared builder whose
            // default only one of its callers undoes is a rule with two meanings,
            // which is the thing extracting it was meant to stop.
            disabled: !!cfg.applyDisabled,
            handler: cfg.apply,
        });
        out.push({
            text: cfg.secondaryText || gettext('Revert'),
            itemId: 'metaSecondary',
            iconCls: 'fa fa-undo',
            handler: cfg.secondary,
        });
        return out;
    },

    // `count` on the button it acts on rather than in a label beside it: a label is
    // the first thing clipped when an editor opens in a window, and a counter you
    // cannot read is not one.
    // state: { canApply, count, applyText?, dirty, dirtyText?, cleanText,
    //          secondaryOnlyWhenDirty? }
    sync: function (owner, state) {
        let apply = owner.down('#metaApply');
        if (apply) {
            apply.setDisabled(!state.canApply);
            let word = state.applyText || gettext('Apply');
            apply.setText(
                state.count ? Ext.String.format(gettext('{0} ({1})'), word, state.count) : word,
            );
        }
        let second = owner.down('#metaSecondary');
        if (second) {
            // The icon has to agree with the word: an undo arrow on a button that says
            // Close is a button that looks like it will throw your work away.
            second.setIconCls(state.dirty ? 'fa fa-undo' : 'fa fa-times');
            // A window's Close becomes Discard once there is something to lose, which
            // is the one moment the difference matters.
            second.setText(state.dirty ? state.dirtyText || state.cleanText : state.cleanText);
            second.setDisabled(!!state.secondaryOnlyWhenDirty && !state.dirty);
        }
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

