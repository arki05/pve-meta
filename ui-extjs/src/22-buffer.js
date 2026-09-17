// ---------------------------------------------------------------------------
// Buffer: what both text editors do to a Monaco buffer, independent of where
// the buffer came from -- the loaded text in the buffer's language, Format,
// the YAML | JSON switch, Diff, and "did anything change".
//
// The whole-document Text card and the subtree window each held a copy of all
// of these, and copies drift. A `buffer` is the small interface both editors hold:
// `{ editor, lang, original }` -- the Monaco editor, the language it is
// showing, and the text it was loaded with, which is always the store's YAML.
// ---------------------------------------------------------------------------

PVE.meta.Buffer = {
    // The loaded text as the buffer's language shows it: what a diff or a "did
    // anything change" is measured against. A stored file that cannot be read as
    // YAML cannot be rendered as JSON; the comparison then falls back to the YAML
    // rather than being lost. Returns `{ lang, text }`.
    baseline: function (buffer) {
        try {
            return { lang: buffer.lang, text: PVE.meta.Codec.originalInLang(buffer.original, buffer.lang) };
        } catch (_err) {
            return { lang: 'yaml', text: buffer.original };
        }
    },

    // True when the buffer holds exactly what was loaded.
    //
    // A predicate, and only that. It used to raise "No changes." itself, which is
    // how a dialog appeared in editors that never asked for one: a function that
    // answers a question and interrupts the user is two functions, and only one of
    // them was in its name. Applying an unchanged buffer resolves to nothing to
    // write, which is the honest outcome and not something to stop for.
    unchanged: function (buffer) {
        return buffer.editor.getValue() === PVE.meta.Buffer.baseline(buffer).text;
    },

    // The buffer against what was loaded, without committing to it.
    diff: function (buffer, title) {
        let base = PVE.meta.Buffer.baseline(buffer);
        PVE.meta.Monaco.confirmDiff({
            title: title,
            original: base.text,
            modified: buffer.editor.getValue(),
            lang: base.lang,
        });
    },

    // Re-dump the buffer canonically in the language showing: two-space indent, no
    // folding, key order preserved -- the same dumper the YAML | JSON toggle uses, so
    // formatting then toggling is a no-op. Refuses a buffer that does not parse
    // rather than mangling it. Returns true when the text changed.
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

    // The first half of the YAML | JSON switch: the buffer as a value, ready to be
    // shown in `lang`. If it does not parse, says so, puts the toggle `btn` back,
    // and returns undefined -- the buffer stays as it was. Between this and
    // `render` the caller records the new language, because rendering fires the
    // editor's change listeners and they read it.
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
            btn.suspendEvents();
            btn.setValue(buffer.lang);
            btn.resumeEvents();
            return undefined;
        }
    },

    // The second half: show `value` in `buffer.lang`. `Codec.render`, not a bare
    // dump: switching back to YAML prefers the server's own text when the document
    // is unchanged, so a presentation toggle never turns a no-op into a whitespace
    // diff.
    render: function (buffer, value) {
        window.monaco.editor.setModelLanguage(buffer.editor.getModel(), buffer.lang);
        buffer.editor.setValue(PVE.meta.Codec.render(value, buffer.lang, buffer.original));
    },
};

