// ---------------------------------------------------------------------------
// Buffer: what both text editors (the Text card, the subtree window) do to a
// Monaco buffer -- `{ editor, lang, original }` -- independent of where it came
// from: Format, the YAML | JSON switch, Diff, and "did anything change".
// ---------------------------------------------------------------------------

PVE.meta.Buffer = {
    // What the YAML side held when the JSON side was entered. A switch to JSON and
    // back is a presentation toggle and has to give back exactly the text it took:
    // key order and `#` comments are not values, so nothing that went through JSON
    // can carry them back. Keyed by the Monaco editor, since a `buffer` is built
    // fresh for every call; a WeakMap, so a disposed editor needs no bookkeeping.
    stashed: new WeakMap(),

    // The loaded text as the buffer's language shows it, for a diff or a "did
    // anything change"; falls back to YAML if it cannot be rendered as JSON.
    // Returns `{ lang, text }`.
    baseline: function (buffer) {
        try {
            return { lang: buffer.lang, text: PVE.meta.Codec.originalInLang(buffer.original, buffer.lang) };
        } catch (_err) {
            return { lang: 'yaml', text: buffer.original };
        }
    },

    // True when the buffer holds exactly what was loaded. A predicate, and only
    // that: applying an unchanged buffer resolves to nothing to write. On an
    // untouched JSON side the answer is the YAML behind it, which switching back
    // gives back: an edit JSON cannot show (a `#` comment, a re-indent) is still
    // an edit, and one Revert or leaving Text must ask about.
    unchanged: function (buffer) {
        let text = buffer.editor.getValue();
        let held = PVE.meta.Buffer.stashed.get(buffer.editor);
        if (held && buffer.lang === 'json' && text === held.json) {
            return held.yaml === buffer.original;
        }
        return text === PVE.meta.Buffer.baseline(buffer).text;
    },

    // Put the stored document into the buffer (a load, a Revert, a re-read after
    // Apply). What the YAML side held before is not that document's, so it goes:
    // left in place, switching back from JSON gave the reverted edit back.
    load: function (editor, text) {
        PVE.meta.Buffer.stashed.delete(editor);
        editor.setValue(text);
    },

    // The buffer against what was loaded, without committing to it.
    diff: function (buffer, title) {
        let base = PVE.meta.Buffer.baseline(buffer);
        PVE.meta.Monaco.showDiff({
            title: title,
            original: base.text,
            modified: buffer.editor.getValue(),
            lang: base.lang,
        });
    },

    // Re-dump the buffer canonically in the language showing, the same dumper the
    // YAML | JSON toggle uses. Refuses a buffer that does not parse. Returns true
    // when the text changed.
    format: function (buffer) {
        let text = buffer.editor.getValue();
        try {
            let formatted = PVE.meta.Codec.dump(PVE.meta.Codec.parse(text, buffer.lang), buffer.lang);
            if (formatted === text) {
                return false;
            }
            buffer.editor.setValue(formatted);
            return true;
        } catch (err) {
            Ext.Msg.alert(gettext('Cannot format'), Ext.htmlEncode(PVE.meta.Utils.errText(err)));
            return false;
        }
    },

    // The first half of the YAML | JSON switch: the buffer as a value. If it does
    // not parse, says so, puts `btn` back once its change handler has run, and
    // returns undefined. The caller
    // records the new language before calling `render`, whose change listeners read it.
    convert: function (buffer, lang, btn) {
        try {
            return PVE.meta.Codec.parse(buffer.editor.getValue(), buffer.lang);
        } catch (err) {
            Ext.Msg.alert(
                gettext('Error'),
                Ext.String.format(
                    gettext('Cannot convert to {0}: {1}'),
                    lang.toUpperCase(),
                    Ext.htmlEncode(PVE.meta.Utils.errText(err)),
                ),
            );
            // After the handler, not in it: this runs inside the button's own
            // `change`, and a value set there is undone as the button finishes
            // its toggle -- which left it with no value at all, and the next
            // click handed `null` on as the language.
            let lang0 = buffer.lang;
            setTimeout(function () {
                if (btn.isDestroyed) {
                    return;
                }
                btn.suspendEvents();
                btn.setValue(lang0);
                btn.resumeEvents();
            }, 0);
            return undefined;
        }
    },

    // The second half: show `value` in `buffer.lang`. The text being left goes with
    // it, since leaving YAML is the only chance to remember how it looked.
    render: function (buffer, value) {
        let leaving = buffer.editor.getValue();
        window.monaco.editor.setModelLanguage(buffer.editor.getModel(), buffer.lang);
        buffer.editor.setValue(PVE.meta.Buffer.rendered(buffer, value, leaving));
    },

    // What that shows. Into JSON: the dump, with the YAML it replaced stashed.
    // Back into YAML: that exact text, if the JSON side was never touched -- once
    // it was, the value is all that carries over, and the layout it used to have is
    // not that value's. Then it is `Codec.render`, which still prefers the text the
    // document was loaded as when the value is the one it was loaded with.
    rendered: function (buffer, value, leaving) {
        let me = PVE.meta.Buffer;
        if (buffer.lang === 'json') {
            let json = PVE.meta.Codec.dump(value, 'json');
            me.stashed.set(buffer.editor, { yaml: leaving, json: json });
            return json;
        }
        let held = me.stashed.get(buffer.editor);
        me.stashed.delete(buffer.editor);
        if (held && held.json === leaving) {
            return held.yaml;
        }
        return PVE.meta.Codec.render(value, 'yaml', buffer.original);
    },
};

