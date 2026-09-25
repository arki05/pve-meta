// ---------------------------------------------------------------------------
// One document's transport and state: everything that reads or writes `docState`
// or talks to `/meta/*` directly -- the seam PVE.meta.TreePanel composes in.
// ---------------------------------------------------------------------------

PVE.meta.Doc = {
    // --- documents -----------------------------------------------------------

    // The API path of a document, from its id: a registry id is `prefixes/<name>`,
    // everything else a vmid (`api::parse_id`).
    urlFor: function (id) {
        return id.indexOf('/') === -1 ? '/meta/guests/' + id : '/meta/' + id;
    },

    // The parameters of every read and write of a document, with `comments: 1` --
    // this editor always wants the comment keys, as the description column (DESIGN §2, §5).
    docParams: function (params) {
        return Ext.apply({ comments: 1 }, params);
    },

    // The document id of a prefix file: `prefixes/<name>` (`api::parse_id`).
    registryId: function (name) {
        return 'prefixes/' + name;
    },

    // What kind of document an id names -- which decides what governs its rows.
    docKind: function (id) {
        return id.indexOf('prefixes/') === 0 ? 'prefix' : 'guest';
    },

    // The digest to send with a write, and the parsed document to build rows from.
    // Per document: a shared `digest` field would send one document's digest with
    // a write to another.
    digestOf: function (id) {
        return (this.docState[id] || {}).digest || '';
    },

    dataOf: function (id) {
        return (this.docState[id] || {}).data || {};
    },

    setDigest: function (id, digest) {
        if (digest) {
            this.docState[id] = this.docState[id] || { data: {} };
            this.docState[id].digest = digest;
        }
    },

    // --- loading -----------------------------------------------------------

    // The load mask goes over the body, never `me.el`: the panel's element includes
    // its docked footer, and the Tree|Text switch under a mask swallows the click --
    // which is why the switch needed a second one after every fresh render.
    setMask: function (msg) {
        Proxmox.Utils.setErrorMask({ el: this.body || this.el }, msg);
    },

    // Every load is a chain of these. A failure the caller did not handle masks
    // the panel with it.
    request: function (opts) {
        let me = this;
        PVE.meta.request(
            me,
            Ext.apply(
                {
                    failure: (response) =>
                        me.setMask(response.htmlStatus || gettext('Error')),
                },
                opts,
            ),
        );
    },

    reload: function () {
        let me = this;
        if (!me.rendered || me.isDestroyed || me.mode === 'text') {
            return;
        }
        if (me.editing || me.textWindow) {
            // Not under a modal editor: a rebuilt tree is the document pulled out
            // from under it. Owed instead, and paid by `editorClosed`.
            me.reloadPending = true;
            return;
        }
        me.reloadPending = false;
        me.setMask(true);
        // Cleared here and set only by `loadDocument`: describes the load in
        // progress, not the document the panel used to hold.
        me.docParseError = '';
        me.loadPrefixes(() =>
            me.loadAccess(() => me.loadSchemas(() => me.loadDocument(() => me.setMask(false)))),
        );
    },

    // What every editor's `destroy` tells the panel: a reload that came due while
    // it was open -- a write that landed, a conflict re-read -- reaches the rows now.
    editorClosed: function () {
        let me = this;
        if (me.reloadPending) {
            me.reload();
        }
    },

    // A failure here doesn't break the page: the schema-declared rows stay empty.
    loadPrefixes: function (next) {
        let me = this;
        me.request({
            url: '/meta/prefixes',
            params: me.prefixParams(),
            success: function (response) {
                // Which of these reach a guest, and in what order, is the Shape's
                // question, answered by the core from this list every time.
                me.prefixes = response.result.data || [];
                next();
            },
            failure: function () {
                me.prefixes = [];
                next();
            },
        });
    },

    // What `loadPrefixes` asks for: a guest's rows come from the server's already
    // resolved set (DESIGN §3); a registry document's rule picker wants every file.
    prefixParams: function () {
        return this.registryDoc ? {} : { id: this.docId };
    },

    loadAccess: function (next) {
        let me = this;
        me.request({
            url: '/meta/access',
            // Ask about the document this panel actually shows: a guest's read is
            // VM.Audit, a registry file's is open to everyone (DESIGN §4).
            params: { id: me.docId },
            success: function (response) {
                me.access = response.result.data || { read: 0, write: 0 };
                me.syncAccessLabel();
                next();
            },
        });
    },

    // The meta-schema, once per load and only where used: a guest tab never shows one.
    loadSchemas: function (next) {
        let me = this;
        if (!me.registryDoc) {
            next();
            return;
        }
        me.request({
            url: '/meta/schemas',
            success: function (response) {
                me.schemas = response.result.data || {};
                next();
            },
            // An older API has no /meta/schemas: still shows as a tree, just
            // without declared rows or hovers.
            failure: () => next(),
        });
    },

    // This panel's one document, read as YAML: `format=json` turns booleans into
    // 1/0 and loses key order, which a root-view write would then churn. The core
    // is loaded first rather than assumed, since calling into it too early is a
    // bug this editor has already had once.
    loadDocument: function (next) {
        let me = this;
        PVE.meta.Core.load().then(
            function () {
                me.readDocument(function (read) {
                    me.docState[me.docId] = { digest: read.digest, data: read.data };
                    // The bytes read but do not parse: hand off to Text, the one
                    // place a repair happens, a root replace with a full document
                    // (DESIGN §5). `docParseError` keeps the tree out of reach until then.
                    if (read.parseError) {
                        me.docParseError = read.parseError;
                        me.buildTree();
                        me.syncButtons();
                        next();
                        me.setModeButton('text');
                        me.enterTextMode();
                        return;
                    }
                    me.buildTree();
                    me.syncButtons();
                    next();
                });
            },
            function (err) {
                // Without the core this panel cannot read a document faithfully.
                me.setMask(Ext.htmlEncode(PVE.meta.Utils.errText(err)));
            },
        );
    },

    // One read of this panel's document, parsed: `onRead({ digest, data,
    // parseError })`, with `data` empty when the text does not parse on the
    // server. What a load and a conflict re-read share; what to do with the
    // answer is the caller's. Needs the core, which every caller has by now.
    readDocument: function (onRead) {
        let me = this;
        me.request({
            url: me.urlFor(me.docId),
            params: me.docParams({ format: 'yaml' }),
            success: function (response) {
                let d = response.result.data || {};
                let read = { digest: d.digest || '', data: {}, parseError: d.parse_error || '' };
                if (!read.parseError) {
                    try {
                        read.data = PVE.meta.Codec.parse(d.text || '', 'yaml');
                    } catch (err) {
                        me.setMask(Ext.htmlEncode(PVE.meta.Utils.errText(err)));
                        return;
                    }
                }
                onRead(read);
            },
        });
    },

    // --- writes -------------------------------------------------------------

    // The one request an edit is (decision 026): a pure function, tested once rather
    // than assembled at four call sites. An edit is `{ path, op: 'set' | 'delete',
    // value }`; a set is `view::replace` at its own path (which creates the maps
    // above it), the root view named by leaving `view` out; a delete is `DELETE
    // ?view=`, taking the key's note with it. `force` is "Save anyway" on a 422.
    writeFor: function (docId, edit, digest, force) {
        let url = PVE.meta.Doc.urlFor(docId);
        if (edit.op === 'delete') {
            let query = { view: edit.path, digest: digest };
            if (force) {
                query.force = 1;
            }
            return { url: url + '?' + Ext.Object.toQueryString(query), method: 'DELETE' };
        }
        let params = { mode: 'replace', data: Ext.encode(edit.value), digest: digest };
        if (edit.path) {
            params.view = edit.path;
        }
        if (force) {
            params.force = 1;
        }
        return { url: url, method: 'PUT', params: PVE.meta.Doc.docParams(params) };
    },

    write: function (docId, params, onDone, retry) {
        this.submit(
            { url: this.urlFor(docId), method: 'PUT', params: this.docParams(params) },
            onDone,
            retry,
        );
    },

    // `onDone(ok)` is how a write answers the editor that started it: true once the
    // server has it, false on every failure it is left to deal with -- which is what
    // keeps a window open with what was typed instead of closing over a 400. A 422
    // that offers "Save anyway" answers nothing until that question is settled: the
    // retry carries the same `onDone`, so the editor stays masked and open across it.
    // `retry` is what the tick calls: the same write again, with `force=1`. Without
    // one a 422 is an ordinary error. `edit` is the edit this write is, when it is
    // one (`sendEdit`): what a 409 under an open editor is answered about.
    submit: function (opts, onDone, retry, edit) {
        let me = this;
        let done = function (ok) {
            if (onDone) {
                onDone(ok);
            }
        };
        Proxmox.Utils.API2Request(
            Ext.apply(
                {
                    waitMsgTarget: me,
                    success: function () {
                        // The reload first: under an editor it is only owed, and
                        // the answer is what closes that editor and pays it.
                        me.reload();
                        done(true);
                    },
                    failure: function (response) {
                        // The API's message, verbatim.
                        let status = String((response.result || {}).status);
                        let text =
                            response.htmlStatus || Proxmox.Utils.getResponseErrorMessage(response);
                        if (status === '409') {
                            me.conflict(text, edit);
                            done(false);
                            return;
                        }
                        // An enforcing prefix refused this write, naming the paths
                        // (DESIGN §5); the tick makes storing it anyway a deliberate act.
                        if (status === '422' && retry) {
                            Ext.Msg.show({
                                title: gettext('This does not match the schema the operators declare'),
                                message: text,
                                buttons: Ext.Msg.YESNO,
                                buttonText: { yes: gettext('Save anyway'), no: gettext('Cancel') },
                                icon: Ext.Msg.QUESTION,
                                fn: function (btn) {
                                    if (btn === 'yes') {
                                        retry();
                                    } else {
                                        done(false);
                                    }
                                },
                            });
                            return;
                        }
                        Ext.Msg.alert(gettext('Error'), text);
                        done(false);
                    },
                },
                opts,
            ),
        );
    },

    // A 409: somebody else wrote the document since it was read. Whichever copy
    // of unwritten work is at stake decides what happens to it: the Text card's
    // buffer (`conflictInText`), an open editor's input (`conflictInEditor`), or
    // none -- then the tree is reloaded and the message shown, as everywhere in PVE.
    conflict: function (message, edit) {
        let me = this;
        if (me.mode === 'text') {
            me.conflictInText(message);
        } else if (edit && (me.editing || me.textWindow)) {
            me.conflictInEditor(edit, message);
        } else {
            me.reload();
            Ext.Msg.alert(gettext('Conflict'), message);
        }
    },

    // A 409 under an open editor. `reload` would answer it by pulling the
    // document out from under what was typed, and it refused to; so OK gave 409
    // for ever. Refreshing the digest alone would answer it by writing over the
    // other writer's change without a word. So the document is re-read *around*
    // the editor: the digest and data the next OK needs now, the rows once the
    // editor closes (`reloadPending`). Then the one question a conflict raises:
    // is the value this editor is about to replace still the one it opened on?
    // If so, the edit can go again as it is. If not, both values are named, and
    // OK is stated to write over the newer one. The editor stays open either way,
    // with what was typed in it.
    conflictInEditor: function (edit, message) {
        let me = this;
        let U = PVE.meta.Utils;
        let path = edit.path || '';
        let before = U.valueAt(me.openedOn || me.dataOf(me.docId), path);
        me.readDocument(function (read) {
            me.docState[me.docId] = { digest: read.digest, data: read.data };
            me.reloadPending = true;
            let after = U.valueAt(read.data, path);
            let same =
                before === undefined || after === undefined ? before === after : U.sameValue(before, after);
            let where = Ext.htmlEncode(path || gettext('(whole document)'));
            let maps = U.kindOf(before) === 'map' && U.kindOf(after) === 'map';
            let lines = [message];
            if (read.parseError) {
                lines.push(gettext('The document no longer parses as YAML and can only be repaired as text.'));
            } else if (same) {
                lines.push(
                    Ext.String.format(
                        gettext('The value at {0} is still the one this editor opened on: press OK to send the edit again as it is.'),
                        where,
                    ),
                );
            } else if (maps) {
                // No line of YAML says how two maps differ; the diff does.
                lines.push(
                    Ext.String.format(
                        gettext('The map at {0} was changed as well; the diff shows how. Pressing OK writes the text in this editor over it.'),
                        where,
                    ),
                );
            } else {
                lines.push(
                    Ext.String.format(
                        gettext('The value at {0} was changed as well: it was {1} and is now {2}. Pressing OK writes the value in this editor over the new one.'),
                        where,
                        me.storedValueText(before),
                        me.storedValueText(after),
                    ),
                );
            }
            // The subtree window compares against the view as stored now from here
            // on, and shows the diff once the alert is dismissed, not on top of it.
            let win = !same && me.textWindow;
            if (win) {
                win.conflict(after);
            }
            Ext.Msg.alert(gettext('Conflict'), lines.join('<br>'), function () {
                if (win && !win.isDestroyed) {
                    win.showDiff();
                }
            });
        });
    },

    // A stored value in one line of an alert, encoded. A map is said to be one: its
    // first line of YAML and "(+N lines)" told nobody anything, and what is in it is
    // the diff window's to show.
    storedValueText: function (value) {
        let U = PVE.meta.Utils;
        if (value === undefined) {
            return Ext.htmlEncode(gettext('not set'));
        }
        let kind = U.kindOf(value);
        if (kind === 'map') {
            return Ext.htmlEncode(gettext('a map'));
        }
        return Ext.htmlEncode(U.previewText(U.displayValue(value, kind)));
    },
};
