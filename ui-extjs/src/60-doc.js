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

    // The file name, which for a prefix is the prefix: whatever follows the last `/`.
    docTitle: function (id) {
        return id.slice(id.lastIndexOf('/') + 1);
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
        if (!me.rendered || me.isDestroyed || me.editing || me.textWindow || me.mode === 'text') {
            return;
        }
        me.setMask(true);
        // Cleared here and set only by `loadDocument`: describes the load in
        // progress, not the document the panel used to hold.
        me.docParseError = '';
        me.loadPrefixes(() =>
            me.loadAccess(() => me.loadSchemas(() => me.loadDocument(() => me.setMask(false)))),
        );
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
                me.request({
                    url: me.urlFor(me.docId),
                    params: me.docParams({ format: 'yaml' }),
                    success: function (response) {
                        let d = response.result.data || {};
                        // The bytes read but do not parse: hand off to Text, the one
                        // place a repair happens, a root replace with a full document
                        // (DESIGN §5). `docParseError` keeps the tree out of reach until then.
                        if (d.parse_error) {
                            me.docParseError = d.parse_error;
                            me.docState[me.docId] = { digest: d.digest || '', data: {} };
                            me.buildTree();
                            me.syncButtons();
                            next();
                            me.setModeButton('text');
                            me.enterTextMode();
                            return;
                        }
                        let data;
                        try {
                            data = PVE.meta.Codec.parse(d.text || '', 'yaml');
                        } catch (err) {
                            me.setMask(Ext.htmlEncode(PVE.meta.Utils.errText(err)));
                            return;
                        }
                        me.docState[me.docId] = { digest: d.digest || '', data: data };
                        me.buildTree();
                        me.syncButtons();
                        next();
                    },
                });
            },
            function (err) {
                // Without the core this panel cannot read a document faithfully.
                me.setMask(Ext.htmlEncode(PVE.meta.Utils.errText(err)));
            },
        );
    },

    // --- writes -------------------------------------------------------------

    // The one request an edit is (DESIGN §6): a pure function, tested once rather
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
    // one a 422 is an ordinary error.
    submit: function (opts, onDone, retry) {
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
                        done(true);
                        me.reload();
                    },
                    failure: function (response) {
                        // The API's message, verbatim. A 409 means somebody else
                        // wrote the document since we read it: reload, then say so.
                        let status = String((response.result || {}).status);
                        let text =
                            response.htmlStatus || Proxmox.Utils.getResponseErrorMessage(response);
                        if (status === '409') {
                            if (me.mode === 'text') {
                                // The buffer is unwritten text and the only copy of
                                // it: the Text card re-reads around it rather than
                                // over it, and says so itself once it has.
                                me.conflictInText(text);
                            } else {
                                me.reload();
                                Ext.Msg.alert(gettext('Conflict'), text);
                            }
                            done(false);
                            return;
                        }
                        // An enforcing prefix refused this write, naming the paths
                        // (DESIGN §4); the tick makes storing it anyway a deliberate act.
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
};
