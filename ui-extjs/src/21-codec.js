// ---------------------------------------------------------------------------
// Codec: a buffer as a document and back (`format::parse` / `format::dump`).
//
// The store writes documents with serde_yaml_ng; so does this, because it *is*
// serde_yaml_ng. There is no second emitter to keep in step, and no setting on
// this side that could drift -- every line the editor shows is a line the file
// would hold.
// ---------------------------------------------------------------------------

PVE.meta.Codec = {
    // A buffer, in `lang` ('yaml' or 'json'), as a value. An empty buffer is the
    // empty document. Throws a CoreError carrying `line`/`column`.
    parse: function (text, lang) {
        return PVE.meta.Core.call('parse', lang === 'json' ? 'json' : 'yaml', String(text));
    },

    // The inverse: `value` as canonical text in `lang`. Always re-dumps -- what
    // Format wants, since a re-indent is the point of clicking it.
    dump: function (value, lang) {
        return PVE.meta.Core.call('dump', lang === 'json' ? 'json' : 'yaml', value);
    },

    // As `dump`, but for a switch *into* YAML prefers `originalYaml` when it renders
    // the same text. The store rewrites a file only when asked to write one, so what
    // the editor is handed can be a file as somebody *wrote* it -- valid YAML in a
    // layout no emitter would choose -- and re-dumping that on a presentation toggle
    // invents changes to a document nobody edited.
    render: function (value, lang, originalYaml) {
        if (lang !== 'json' && originalYaml !== undefined && PVE.meta.Codec.same(value, originalYaml)) {
            return originalYaml;
        }
        return PVE.meta.Codec.dump(value, lang);
    },

    // True if `value` equals what `yamlText` parses to; order is not a value.
    same: function (value, yamlText) {
        try {
            return PVE.meta.Core.call('same', value, PVE.meta.Codec.parse(yamlText, 'yaml'));
        } catch (_err) {
            return false;
        }
    },

    // The text an editor loaded (its `original`), rendered in `lang`. Throws if it
    // cannot be read as YAML -- callers fall back to 'yaml' rather than lose the
    // comparison.
    originalInLang: function (originalYaml, lang) {
        return lang === 'json'
            ? PVE.meta.Codec.dump(PVE.meta.Codec.parse(originalYaml, 'yaml'), 'json')
            : originalYaml;
    },
};

