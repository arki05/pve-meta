// ---------------------------------------------------------------------------
// The whole-document Monaco card and the Tree | Text mode switch: the document as
// one text buffer instead of rows. The one editor that writes **text**, so a `#`
// comment or key order (outside the document model, DESIGN §2) survives.
// ---------------------------------------------------------------------------

PVE.meta.TextCard = {
    buildTextCard: function () {
        return {
            xtype: 'panel',
            itemId: 'metaText',
            layout: 'fit',
            border: false,
            items: [{ xtype: 'component', itemId: 'metaTextMount', style: 'height:100%;width:100%' }],
        };
    },

    textBuffer: function () {
        return { editor: this.textEditor, lang: this.textLang, original: this.textOriginal };
    },

    // The buffer against the file, without committing to anything. Wanting to see
    // what you changed is not the same as wanting to write it.
    showDiff: function () {
        let me = this;
        if (!me.textEditor) {
            return;
        }
        PVE.meta.Buffer.diff(me.textBuffer(), me.docId);
    },

    setModeButton: function (value) {
        let btn = this.down('#modeBtn');
        if (!btn || btn.getValue() === value) {
            return;
        }
        btn.suspendEvents();
        btn.setValue(value);
        btn.resumeEvents();
    },

    // --- the Text card ------------------------------------------------------

    onModeChange: function (value) {
        let me = this;
        if (value === me.mode) {
            return;
        }
        if (value === 'text') {
            me.enterTextMode();
        } else {
            me.leaveTextMode();
        }
    },

    textIsDirty: function () {
        let me = this;
        if (!me.textEditor) {
            return false;
        }
        try {
            return (
                me.textEditor.getValue() !==
                PVE.meta.Codec.originalInLang(me.textOriginal, me.textLang)
            );
        } catch (_err) {
            return true; // cannot tell: assume there is something to lose
        }
    },

    // The stored document rendered in `lang`: what the buffer starts as, the diff's
    // "original" side and the yardstick the dirty check uses.
    textRendered: function (lang) {
        return PVE.meta.Codec.originalInLang(this.textOriginal, lang);
    },

    enterTextMode: function () {
        let me = this;
        me.mode = 'text';
        me.syncButtons();
        me.getLayout().setActiveItem(me.down('#metaText'));
        me.setMask(true);
        me.request({
            url: me.urlFor(me.docId),
            params: me.docParams({ format: 'yaml' }),
            success: function (response) {
                let d = response.result.data || {};
                me.setDigest(me.docId, d.digest);
                // The server's own text, comments and all.
                me.textOriginal = d.text || '';
                me.showTextEditor();
            },
            failure: function (response) {
                me.setMask(false);
                Ext.Msg.alert(gettext('Error'), response.htmlStatus || gettext('Error'));
                me.abortTextMode();
            },
        });
    },

    showTextEditor: function () {
        let me = this;
        PVE.meta.Monaco.load().then(
            function () {
                if (me.isDestroyed || me.mode !== 'text') {
                    return;
                }
                me.setMask(false);
                if (me.textEditor) {
                    me.textEditor.setValue(me.textRendered(me.textLang));
                    me.annotateText();
                    return;
                }
                me.textEditor = PVE.meta.Monaco.create(
                    me.down('#metaTextMount').getEl().dom,
                    me.textRendered(me.textLang),
                );
                // Squiggles describe the text the server sent; typing moves the lines,
                // so they are dropped on the first edit and come back on the next load.
                me.textEditor.onDidChangeModelContent(function () {
                    me.annotateText();
                });
                me.annotateText();
            },
            function (err) {
                me.setMask(false);
                PVE.meta.Utils.alertError(err);
                me.abortTextMode();
            },
        );
    },

    // Text mode could not be entered: fall back to the tree without asking.
    abortTextMode: function () {
        let me = this;
        me.mode = 'tree';
        me.setModeButton('tree');
        me.getLayout().setActiveItem(me.down('#metaTree'));
        me.syncButtons();
    },

    // Going back to the tree drops the buffer, so ask first when there is something to lose.
    leaveTextMode: function () {
        let me = this;
        if (!me.textIsDirty()) {
            me.finishLeavingTextMode();
            return;
        }
        Ext.Msg.confirm(
            gettext('Confirm'),
            gettext('Discard the unapplied changes in the text editor?'),
            function (btn) {
                if (btn === 'yes') {
                    me.finishLeavingTextMode();
                } else {
                    me.setModeButton('text');
                }
            },
        );
    },

    // The mode first, then the document: `reload` is the tree's read, and it refuses
    // to run while the mode still says text.
    finishLeavingTextMode: function () {
        let me = this;
        PVE.meta.Monaco.dispose(me.textEditor);
        me.textEditor = null;
        me.mode = 'tree';
        me.setModeButton('tree');
        me.getLayout().setActiveItem(me.down('#metaTree'));
        me.syncButtons();
        me.reload();
    },

    // Presentation only. `textLang` is recorded between convert and render, since
    // rendering fires the change listener that reads it.
    switchTextLang: function (lang) {
        let me = this;
        if (!me.textEditor || lang === me.textLang) {
            return;
        }
        let value = PVE.meta.Buffer.convert(me.textBuffer(), lang, me.down('#textLangBtn'));
        if (value === undefined) {
            return;
        }
        me.textLang = lang;
        PVE.meta.Buffer.render(me.textBuffer(), value);
        me.annotateText();
    },

    // For hand-written YAML that has drifted from the store's own layout. The
    // squiggles are recomputed because the lines moved.
    formatText: function () {
        let me = this;
        if (!me.textEditor) {
            return;
        }
        if (PVE.meta.Buffer.format(me.textBuffer())) {
            me.annotateText();
        }
    },

    // One write of the whole document at the root view, as **text** -- the only way
    // a `#` comment or a reordering reaches the file. `force` retries after a 422.
    applyText: function (force) {
        let me = this;
        if (!me.textEditor) {
            return;
        }
        if (PVE.meta.Buffer.unchanged(me.textBuffer())) {
            return;
        }
        let params = { mode: 'replace', digest: me.digestOf(me.docId) };
        params[me.textLang === 'json' ? 'data' : 'text'] = me.textEditor.getValue();
        if (force) {
            params.force = 1;
        }
        me.write(
            me.docId,
            params,
            // Only a write that landed re-reads: a failed one leaves the buffer
            // alone, since it is still the only copy of what was typed.
            (ok) => (ok ? me.refreshText() : undefined),
            force ? undefined : () => me.applyText(true),
        );
    },

    // Revert: the buffer is the stored document again.
    discardText: function () {
        let me = this;
        if (!me.textIsDirty()) {
            me.refreshText();
            return;
        }
        Ext.Msg.confirm(
            gettext('Confirm'),
            gettext('Discard the unapplied changes in the text editor?'),
            function (btn) {
                if (btn === 'yes') {
                    me.refreshText();
                }
            },
        );
    },

    // Underline what is wrong with the buffer *as it is now* and describe the key on
    // hover. Advisory only, on every keystroke: a YAML syntax error as an Error
    // marker (Monaco validates JSON itself but not YAML), every schema finding
    // (the Shape) as a Warning -- YAML-only, since the line index is a YAML scan.
    annotateText: function () {
        let me = this;
        if (!me.textEditor || !window.monaco) {
            return;
        }
        let model = me.textEditor.getModel();
        if (!model) {
            return;
        }
        let text = me.textEditor.getValue();
        let markers = [];
        let hovers = Object.create(null);

        let parsed = null;
        let parseError = null;
        try {
            parsed = PVE.meta.Codec.parse(text, me.textLang);
        } catch (err) {
            parseError = err;
        }

        if (parseError) {
            if (me.textLang === 'yaml') {
                // A CoreError carries the parser's own 1-based line and column;
                // anything else lands on line 1 rather than nowhere.
                let line = typeof parseError.line === 'number' ? parseError.line : 1;
                line = Math.min(Math.max(line, 1), model.getLineCount());
                markers.push({
                    startLineNumber: line,
                    endLineNumber: line,
                    startColumn: typeof parseError.column === 'number' ? parseError.column : 1,
                    endColumn: model.getLineMaxColumn(line),
                    message: PVE.meta.Utils.errText(parseError),
                    severity: monaco.MarkerSeverity.Error,
                });
            }
        } else if (me.textLang === 'yaml') {
            let shape = me.shapeFor(me.docId);
            if (shape.hasSchema()) {
                let index = PVE.meta.Markers.lineIndex(text);
                markers = PVE.meta.Markers.placed(shape.findings(parsed), index).map(function (f) {
                    return {
                        startLineNumber: f.line,
                        endLineNumber: f.line,
                        startColumn: 1,
                        endColumn: model.getLineMaxColumn(f.line),
                        message: f.message,
                        severity: monaco.MarkerSeverity.Warning,
                    };
                });
                shape.schemaIndex().forEach(function (entry) {
                    let hover = PVE.meta.Markers.hoverText(entry.schema);
                    if (hover && index[entry.path] !== undefined) {
                        hovers[index[entry.path]] = hover;
                    }
                });
            }
        }

        monaco.editor.setModelMarkers(model, 'pve-meta', markers);
        me.textHovers = hovers;
        me.registerTextHover();
    },

    // One hover provider for the language, reading whichever panel owns the model that
    // is asking. Monaco registers providers per-language, not per-editor.
    registerTextHover: function () {
        let me = this;
        if (PVE.meta.textHoverRegistered || !window.monaco || !monaco.languages) {
            return;
        }
        PVE.meta.textHoverRegistered = true;
        monaco.languages.registerHoverProvider('yaml', {
            provideHover: function (model, position) {
                let owner = me.textEditor && me.textEditor.getModel() === model ? me : null;
                let text = owner && owner.textHovers && owner.textHovers[position.lineNumber];
                if (!text) {
                    return null;
                }
                return {
                    range: new monaco.Range(
                        position.lineNumber,
                        1,
                        position.lineNumber,
                        model.getLineMaxColumn(position.lineNumber),
                    ),
                    contents: [{ value: text }],
                };
            },
        });
    },

    // Re-read the document and put it back in the buffer (after Apply, or Revert).
    // `cfg.keepBuffer` re-reads around the buffer instead: the digest and the text
    // the buffer is compared against are updated, what was typed is left alone.
    // `cfg.then` runs once the read has landed.
    refreshText: function (cfg) {
        let me = this;
        let opts = cfg || {};
        me.request({
            url: me.urlFor(me.docId),
            params: me.docParams({ format: 'yaml' }),
            success: function (response) {
                let d = response.result.data || {};
                me.setDigest(me.docId, d.digest);
                me.textOriginal = d.text || '';
                me.clearParseErrorIfSound();
                if (me.textEditor && !opts.keepBuffer) {
                    me.textEditor.setValue(me.textRendered(me.textLang));
                    me.annotateText();
                }
                if (opts.then) {
                    opts.then();
                }
            },
        });
    },

    // Text is where a document that does not parse gets repaired, and this is the
    // only read that sees the repair: `reload` clears `docParseError` but returns
    // early while the mode is text, so without this the Tree segment stayed disabled
    // until the page was reloaded. The document's own text is the answer -- if it
    // parses now, there are rows to show again.
    clearParseErrorIfSound: function () {
        let me = this;
        if (!me.docParseError) {
            return;
        }
        try {
            PVE.meta.Codec.parse(me.textOriginal, 'yaml');
        } catch (_err) {
            return;
        }
        me.docParseError = '';
        me.syncAccessLabel();
    },

    // A 409 while the Text card holds a buffer: somebody else wrote the document
    // since it was read. The buffer is unwritten work, so the re-read takes the
    // fresh digest and original only -- the next Apply carries that digest -- and
    // the diff is the buffer against what is in the file *now*, which is the
    // question a conflict actually raises.
    conflictInText: function (message) {
        let me = this;
        me.refreshText({
            keepBuffer: true,
            then: function () {
                Ext.Msg.alert(gettext('Conflict'), message);
                me.showDiff();
            },
        });
    },
};
