// ---------------------------------------------------------------------------
// The whole-document Monaco card and the Tree | Text mode switch: the document as
// one text buffer instead of rows. The one editor that writes **text**, so a `#`
// comment or key order (outside the document model, DESIGN §2) survives.
// ---------------------------------------------------------------------------

// What each Monaco model's lines mean, by model: the hover provider is registered
// once per *language*, so it cannot belong to one panel. A guest tab and a prefix
// file's window each have a buffer, and the panel the provider happened to be built
// with may be long destroyed. Written by `annotateText`, dropped with the editor.
PVE.meta.TextHovers = new Map();

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
        me.flushAnnotate();
        PVE.meta.Buffer.diff(me.textBuffer(), me.docId);
    },

    // Is there anything in the buffer that the file does not have? One predicate,
    // `Buffer.unchanged`, which the subtree window asks too -- this used to be a
    // second copy of it that compared against `textOriginal` by hand.
    textIsDirty: function () {
        let me = this;
        return !!me.textEditor && !PVE.meta.Buffer.unchanged(me.textBuffer());
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
                    me.scheduleAnnotate();
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

    // The buffer goes, and what the hover provider knows about its model with it.
    disposeTextEditor: function () {
        let me = this;
        me.cancelAnnotate();
        let model = me.textEditor && me.textEditor.getModel();
        if (model) {
            PVE.meta.TextHovers.delete(model);
        }
        PVE.meta.Monaco.dispose(me.textEditor);
        me.textEditor = null;
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
        me.disposeTextEditor();
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
        me.flushAnnotate();
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
    // hover. Advisory only, and after a pause in the typing: a YAML syntax error as
    // an Error marker (Monaco validates JSON itself but not YAML), every schema
    // finding (the Shape) as a Warning -- YAML-only, since the line index is a YAML
    // scan.
    // Every keystroke moves every line, so the answer is only worth having once
    // typing pauses: the parser runs over the whole buffer and the findings come
    // from the core. Anything that acts on the buffer flushes first.
    ANNOTATE_DELAY: 150,

    scheduleAnnotate: function () {
        let me = this;
        me.cancelAnnotate();
        me.annotateTimer = setTimeout(function () {
            me.annotateTimer = null;
            if (!me.isDestroyed) {
                me.annotateText();
            }
        }, me.ANNOTATE_DELAY);
    },

    cancelAnnotate: function () {
        let me = this;
        if (me.annotateTimer) {
            clearTimeout(me.annotateTimer);
            me.annotateTimer = null;
        }
    },

    // What a pending annotation owes the buffer, now: Apply and Diff both ask what
    // is wrong with the text as it stands, not as it stood 150 ms ago.
    flushAnnotate: function () {
        let me = this;
        if (me.annotateTimer) {
            me.cancelAnnotate();
            me.annotateText();
        }
    },

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
        PVE.meta.TextHovers.set(model, hovers);
        me.registerTextHover();
    },

    // One hover provider for the language, answering for whichever model is asking.
    // Monaco registers providers per-language, not per-editor, so this one is
    // registered once per page and closes over nothing.
    registerTextHover: function () {
        if (PVE.meta.textHoverRegistered || !window.monaco || !monaco.languages) {
            return;
        }
        PVE.meta.textHoverRegistered = true;
        monaco.languages.registerHoverProvider('yaml', {
            provideHover: function (model, position) {
                let hovers = PVE.meta.TextHovers.get(model);
                let text = hovers && hovers[position.lineNumber];
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
