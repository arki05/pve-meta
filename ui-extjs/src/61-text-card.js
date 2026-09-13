// ---------------------------------------------------------------------------
// The whole-document Monaco card and the Tree | Text mode switch.
//
// Everything about looking at the document as one text buffer instead of
// rows: building the card, entering and leaving text mode, Format/Apply/
// Discard, and the squiggles and hovers that annotate the buffer. It reads
// and writes through `PVE.meta.Doc` the same way the tree does; it does not
// know a row exists.
// ---------------------------------------------------------------------------

PVE.meta.TextCard = {
    buildTextCard: function () {
        let me = this;
        return {
            xtype: 'panel',
            itemId: 'metaText',
            layout: 'fit',
            border: false,
            items: [{ xtype: 'component', itemId: 'metaTextMount', style: 'height:100%;width:100%' }],
        };
    },

    // The **stored** document is the baseline, not the planned one. `textRendered`
    // renders the planned document -- staged edits included, which is the point of
    // it -- so comparing the buffer with that would answer "nothing changed" for
    // exactly the case where something did: stage an edit in the tree, switch to
    // Text, Apply. It said "No changes." and wrote nothing.
    textBuffer: function () {
        return { editor: this.textEditor, lang: this.textLang, original: this.textOriginal };
    },

    // The buffer against the file, without committing to anything. Text mode's Apply
    // shows the same diff, but only as the last step before writing -- and wanting to
    // see what you changed is not the same as wanting to write it.
    // What Apply would write, against what is stored -- in whichever view you are in.
    // In Text that is the buffer; in the tree it is the planned document, which is the
    // same question asked of the other view. It used to be offered in Text only, which
    // made "check before you commit" a thing you could do only after switching views.
    showDiff: function () {
        let me = this;
        let title = Ext.String.format(gettext('Changes: {0}'), me.docId);
        if (me.mode === 'text') {
            if (!me.textEditor) {
                return;
            }
            PVE.meta.Buffer.diff(me.textBuffer(), title);
            return;
        }
        PVE.meta.Monaco.confirmDiff({
            title: Ext.String.format(gettext('Changes: {0}'), me.docId),
            originalValue: me.dataOf(me.docId),
            modifiedValue: me.plannedData(),
            // No `apply`: this is the view, not the decision.
        });
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
            // The stored document, for the same reason `applyText` uses it: comparing
            // against the planned one would call a staged edit "not dirty".
            return (
                me.textEditor.getValue() !==
                PVE.meta.Codec.originalInLang(me.textOriginal, me.textLang)
            );
        } catch (_err) {
            return true; // cannot tell: assume there is something to lose
        }
    },

    // The loaded document rendered in `lang`; the diff's "original" side and the
    // yardstick the dirty check uses. `Utils.originalInLang` is shared with the
    // subtree window's own diff rather than a second inline conversion there.
    // What the buffer should show: the **planned** document -- stored plus whatever is
    // staged -- in `lang`. With nothing staged this is the server's own text, comments
    // and all, because `renderBuffer` prefers the original when the document is
    // unchanged. That is what makes Tree and Text two views of one thing rather than
    // two editors that have to be kept apart.
    textRendered: function (lang) {
        let me = this;
        if (!me.isDirty()) {
            return PVE.meta.Codec.originalInLang(me.textOriginal, lang);
        }
        return PVE.meta.Codec.render(me.plannedData(), lang, me.textOriginal);
    },

    enterTextMode: function () {
        let me = this;
        me.mode = 'text';
        me.syncButtons();
        me.getLayout().setActiveItem(me.down('#metaText'));
        Proxmox.Utils.setErrorMask(me, true);
        me.request({
            url: me.urlFor(me.docId),
            params: { format: 'yaml' },
            success: function (response) {
                let d = response.result.data || {};
                me.setDigest(me.docId, d.digest);
                // The server's own text, comments and all -- but what the buffer shows
                // is the *planned* document, so staged edits are there too. With
                // nothing staged the two are the same text (`renderBuffer` prefers the
                // original when the document is unchanged), so opening text mode on an
                // untouched document still shows the file as it was written.
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

    // Going back to the tree keeps whatever was typed: the buffer is turned into
    // staged edits on rows, so the tree shows which keys changed and to what, and one
    // Apply writes them. Switching views is not a decision about your work.
    //
    // The one thing that can stop it is a buffer that does not parse: there is no
    // document to show as a tree, and guessing at one would lose what was typed. So it
    // says so and stays put.
    leaveTextMode: function () {
        let me = this;
        let parsed;
        try {
            parsed = PVE.meta.Codec.parse(me.textEditor.getValue(), me.textLang);
        } catch (err) {
            me.setModeButton('text');
            Ext.Msg.alert(
                gettext('Cannot show this as a tree'),
                Ext.htmlEncode(PVE.meta.Utils.errText(err)) +
                    '<br><br>' +
                    Ext.htmlEncode(gettext('Fix the text, or Discard it, and try again.')),
            );
            return;
        }
        // Whatever you typed becomes staged rows, and the tree then shows it -- so
        // switching needs no confirmation and gets none. The exception is a change
        // the tree has no way to show, and that one asks.
        if (me.layoutOnlyChange(parsed)) {
            Ext.Msg.show({
                title: gettext('Switch to the tree?'),
                message: gettext(
                    'This changes how the document is laid out, not what it says -- ' +
                        'key order, indentation. The tree shows the document, so it ' +
                        'cannot carry that and switching will drop it. Apply from Text ' +
                        'first to keep it.',
                ),
                buttons: Ext.Msg.YESNO,
                buttonText: { yes: gettext('Switch and drop it'), no: gettext('Stay in Text') },
                icon: Ext.Msg.QUESTION,
                fn: function (btn) {
                    if (btn === 'yes') {
                        me.finishLeavingTextMode(parsed);
                    } else {
                        me.setModeButton('text');
                    }
                },
            });
            return;
        }
        me.finishLeavingTextMode(parsed);
    },

    // The buffer changed and the document did not: key order and layout are text,
    // not document (decision 007), so a buffer that reorders keys or reindents them
    // parses to the document it started as and stages nothing. Switching to the
    // tree would drop it with no row to mark and nothing to say what happened.
    // Apply *from* Text keeps it, because that path sends the buffer rather than
    // the model. The edit is real; it is just not one the other view can hold.
    layoutOnlyChange: function (parsed) {
        let me = this;
        return PVE.meta.Core.call('same', me.dataOf(me.docId), parsed) && me.textIsDirty();
    },

    // The mode first, then the document: `setPlanned` syncs the buttons, and they
    // are the tree's only once the mode says so.
    finishLeavingTextMode: function (parsed) {
        let me = this;
        PVE.meta.Monaco.dispose(me.textEditor);
        me.textEditor = null;
        me.mode = 'tree';
        me.setModeButton('tree');
        me.getLayout().setActiveItem(me.down('#metaTree'));
        me.setPlanned(parsed);
    },

    // Presentation only, exactly like the selection window's toggle. `textLang` is
    // recorded between convert and render: rendering fires the change listener,
    // which annotates the buffer in whatever language it finds there.
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

    applyText: function () {
        let me = this;
        if (!me.textEditor) {
            return;
        }
        let buffer = me.textBuffer();
        if (PVE.meta.Buffer.unchanged(buffer)) {
            return;
        }
        let edited = me.textEditor.getValue();
        // The whole document, at the root view, as **text** -- not as a dump of the
        // planned document. That is the one thing this path does that the tree's Apply
        // cannot: a `#` comment is not part of the document model (DESIGN §2), so it
        // survives only for as long as nothing rewrites the file from the model.
        // Sending the buffer keeps what was typed, comments included.
        //
        // The buffer already contains whatever was staged in the tree -- it is rendered
        // from the planned document -- so this applies all of it, and the staged edits
        // are spent.
        let write = function (force) {
            let params = { mode: 'replace', digest: me.digestOf(me.docId) };
            params[me.textLang === 'json' ? 'data' : 'text'] = edited;
            if (force) {
                params.force = 1; // the "Save anyway" tick, see the tree's Apply
            }
            me.submit({ url: me.urlFor(me.docId), method: 'PUT', params: params }, function () {
                me.pending = PVE.meta.EditSet.empty();
                me.refreshText();
            });
        };

        // Same rule as the tree's Apply: stop only when the document would not match
        // the schema, because that is the one case where seeing the diff changes what
        // you decide. The tick keeps storing it anyway a deliberate act -- the server's
        // lint decides what is *storable* (DESIGN §7), and an operator whose schema has
        // drifted must not be able to lock the administrator out of editing.
        let warnings = me.textFindings();
        if (!warnings.length) {
            write();
            return;
        }
        let base = PVE.meta.Buffer.baseline(buffer);
        PVE.meta.Monaco.confirmDiff({
            title: gettext('(whole document)'),
            original: base.text,
            modified: edited,
            lang: base.lang,
            warnings: warnings,
            apply: () => write(true),
        });
    },

    // Drops everything unapplied -- the buffer's edits and the staged ones behind it,
    // which are the same set: the buffer is rendered from the planned document.
    discardText: function () {
        let me = this;
        if (!me.textIsDirty() && !me.isDirty()) {
            me.refreshText();
            return;
        }
        Ext.Msg.confirm(
            gettext('Confirm'),
            gettext('Discard the unapplied changes in the text editor?'),
            function (btn) {
                if (btn !== 'yes') {
                    return;
                }
                me.pending = PVE.meta.EditSet.empty();
                me.buildTree();
                me.syncButtons();
                me.refreshText();
            },
        );
    },

    // Re-read the document and put it back in the buffer (after Apply, or Discard).
    // Underline what is wrong with the buffer *as it is now*, and describe the key on
    // each declared line on hover.
    //
    // Two kinds of finding, both advisory -- Apply is never blocked, the server's lint
    // is the authority (DESIGN section 7):
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
            let shape = me.textShape();
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

    // The grammar findings the current buffer would *introduce*, as plain messages --
    // what Apply warns about before it writes. Empty when the buffer does not parse
    // (the write will fail on its own) or when no grammar applies. Same rule as the
    // tree's Apply, through the same `introducedFindings`: a violation the document
    // already had, on a path this buffer did not change, is not this edit's to vouch
    // for. Reordering keys therefore stops demanding a tick, since a reordering
    // changes no value at all.
    textShape: function () {
        return this.shapeFor(this.docId);
    },

    textFindings: function () {
        let me = this;
        if (!me.textEditor) {
            return [];
        }
        let shape = me.textShape();
        if (!shape.hasSchema()) {
            return [];
        }
        try {
            let value = PVE.meta.Codec.parse(me.textEditor.getValue(), me.textLang);
            let stored = me.dataOf(me.docId);
            return PVE.meta.Shape.introduced(
                shape.findings(stored),
                shape.findings(value),
                PVE.meta.EditSet.changedPaths(stored, value),
            ).map(PVE.meta.Utils.findingText);
        } catch (_err) {
            return [];
        }
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

    refreshText: function () {
        let me = this;
        me.request({
            url: me.urlFor(me.docId),
            params: { format: 'yaml' },
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

