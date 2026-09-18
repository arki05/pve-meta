// ---------------------------------------------------------------------------
// Buffer: what both text editors (the Text card, the subtree window) do to a
// Monaco buffer -- `{ editor, lang, original }` -- independent of where it came
// from: Format, the YAML | JSON switch, Diff, and "did anything change".
// ---------------------------------------------------------------------------

PVE.meta.Buffer = {
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
    // that: applying an unchanged buffer resolves to nothing to write.
    unchanged: function (buffer) {
        return buffer.editor.getValue() === PVE.meta.Buffer.baseline(buffer).text;
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
    // not parse, says so, puts `btn` back, and returns undefined. The caller
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
            btn.suspendEvents();
            btn.setValue(buffer.lang);
            btn.resumeEvents();
            return undefined;
        }
    },

    // The second half: show `value` in `buffer.lang`, via `Codec.render` so a
    // no-op toggle back to YAML never becomes a whitespace diff.
    render: function (buffer, value) {
        window.monaco.editor.setModelLanguage(buffer.editor.getModel(), buffer.lang);
        buffer.editor.setValue(PVE.meta.Codec.render(value, buffer.lang, buffer.original));
    },
};

