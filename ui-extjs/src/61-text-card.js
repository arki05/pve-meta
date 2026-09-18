// ---------------------------------------------------------------------------
// The whole-document Monaco card and the Tree | Text mode switch.
//
// Everything about looking at the document as one text buffer instead of
// rows: building the card, entering and leaving text mode, Format/Diff/Apply/
// Revert, and the squiggles and hovers that annotate the buffer. It reads
// and writes through `PVE.meta.Doc` the same way the tree does; it does not
// know a row exists.
//
// This is the one editor that writes **text**: a `#` comment and a key order are
// not part of the document model (DESIGN §2), so they survive only for as long as
// nothing rewrites the file from that model. Apply sends the buffer.
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
        PVE.meta.Buffer.diff(me.textBuffer(), Ext.String.format(gettext('Changes: {0}'), me.docId));
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
        Proxmox.Utils.setErrorMask(me, true);
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
                Proxmox.Utils.setErrorMask(me, false);
                Ext.Msg.alert(gettext('Error'), response.htmlStatus || gettext('Error'));
                me.abortTextMode();
            },
        });
    },

    showTextEditor: function () {
        let me = this;
        PVE.meta.Monaco.load().then(
            function (monaco) {
                if (me.isDestroyed || me.mode !== 'text') {
                    return;
                }
                Proxmox.Utils.setErrorMask(me, false);
                if (me.textEditor) {
                    me.textEditor.setValue(me.textRendered(me.textLang));
                    me.annotateText();
                    return;
                }
                me.textEditor = monaco.editor.create(me.down('#metaTextMount').getEl().dom, {
                    value: me.textRendered(me.textLang),
                    language: 'yaml',
                    theme: PVE.meta.Monaco.theme(),
                    automaticLayout: true,
                    minimap: { enabled: false },
                    scrollBeyondLastLine: false,
                });
                // Squiggles describe the text the server sent; typing moves the lines,
                // so they are dropped on the first edit and come back on the next load.
                me.textEditor.onDidChangeModelContent(function () {
                    me.annotateText();
                });
                me.annotateText();
            },
            function (err) {
                Proxmox.Utils.setErrorMask(me, false);
                Ext.Msg.alert(gettext('Error'), Ext.htmlEncode(PVE.meta.Utils.errText(err)));
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

    // Going back to the tree drops the buffer: the tree shows the stored document and
    // there is nothing else it could show, so a buffer nobody applied is lost. That is
    // worth one question, and only when there is something to lose.
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

    // Presentation only. `textLang` is recorded between convert and render: rendering
    // fires the change listener, which annotates the buffer in whatever language it
    // finds there.
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

    // One write of the whole document at the root view, as **text** -- which is how a
    // `#` comment or a reordering reaches the file at all. `force` is the retry a 422
    // offers (`submit`).
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
            () => me.refreshText(),
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

    // Underline what is wrong with the buffer *as it is now*, and describe the key on
    // each declared line on hover.
    //
    // Two kinds of finding, both advisory -- Apply is never blocked, the server's lint
    // is the authority (DESIGN section 5):
    //
    //   * a YAML syntax error, as one Error marker on the line the parser reports. Monaco
    //     ships a JSON language service that does this for the JSON view already, but
    //     nothing validates YAML, so this is ours.
    //   * every schema finding, as Warning markers (the Shape, placed by PVE.meta.Markers).
    //
    // This runs on every keystroke (onDidChangeModelContent), against the *buffer* --
    // not against the document the server last sent. Parsing is the core on a document
    // that is a few KB at most; if that ever shows up in typing latency, debounce it.
    //
    // Grammar findings are YAML-only: the line index is a YAML scan, so in the JSON
    // view the document still gets Monaco's own syntax validation but no schema
    // squiggles.
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
    refreshText: function () {
        let me = this;
        me.request({
            url: me.urlFor(me.docId),
            params: me.docParams({ format: 'yaml' }),
            success: function (response) {
                let d = response.result.data || {};
                me.setDigest(me.docId, d.digest);
                me.textOriginal = d.text || '';
                if (me.textEditor) {
                    me.textEditor.setValue(me.textRendered(me.textLang));
                    me.annotateText();
                }
            },
        });
    },
};
