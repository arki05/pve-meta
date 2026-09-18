// ---------------------------------------------------------------------------
// One document's transport and state: the API calls and what came back.
//
// Everything that reads or writes `docState`, or talks to `/meta/*` directly.
// The tree and the Text card each render a document; neither one owns it --
// this is the seam between them, so that `this.request`, `this.reload` and
// `this.write` say which `this` they mean.
// ---------------------------------------------------------------------------

PVE.meta.Doc = {
    // --- documents -----------------------------------------------------------

    // The API path of a document, from its id. Total by construction: a registry id
    // is `prefixes/<name>` -- the path it is served at -- and everything else is a
    // vmid (`api::parse_id`).
    urlFor: function (id) {
        return id.indexOf('/') === -1 ? '/meta/guests/' + id : '/meta/' + id;
    },

    // The parameters of every read and write of a document, with `comments: 1`.
    // Without it the server leaves the comment keys out of a read and keeps the
    // stored ones through a replace (DESIGN §2, §7); this editor shows them as the
    // description column and edits them, so it always asks for them, and what it
    // writes is the subtree with its notes.
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

    // The file name, which for a prefix is the prefix: whatever follows the last
    // `/`, since a file name never contains one.
    docTitle: function (id) {
        return id.slice(id.lastIndexOf('/') + 1);
    },

    // The digest to send with a write, and the parsed document to build rows from.
    // Per document, because a compare-and-swap is per document: one shared `digest`
    // field would have sent a prefix's digest with a write to another document.
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

    // Every load is a chain of these. A failure the caller did not handle masks
    // the panel with it.
    request: function (opts) {
        let me = this;
        PVE.meta.request(
            me,
            Ext.apply(
                {
                    failure: (response) =>
                        Proxmox.Utils.setErrorMask(me, response.htmlStatus || gettext('Error')),
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
        Proxmox.Utils.setErrorMask(me, true);
        // Cleared here and set only by `loadDocument`: it describes the document
        // this load is about to read, not the one the panel used to hold.
        me.docParseError = '';
        me.loadPrefixes(() =>
            me.loadAccess(() =>
                me.loadSchemas(() => me.loadDocument(() => Proxmox.Utils.setErrorMask(me, false))),
            ),
        );
    },

    // A failure here doesn't break the page: the schema-declared rows stay empty,
    // rather than the page.
    loadPrefixes: function (next) {
        let me = this;
        me.request({
            url: '/meta/prefixes',
            params: me.prefixParams(),
            success: function (response) {
                // The listing as served, failures included (the registry grid shows
                // them). Which of these reach a guest, and in what order, is the
                // Shape's question, answered by the core from this list every time.
                me.prefixes = response.result.data || [];
                next();
            },
            failure: function () {
                me.prefixes = [];
                next();
            },
        });
    },

    // What `loadPrefixes` asks for. A guest's rows come from the set the server has
    // already resolved for it -- selector and node override applied, most-specific
    // first (DESIGN §3) -- so the reload a migration's node change causes gets the
    // new set. A registry document's rule picker wants every file, as it is.
    prefixParams: function () {
        return this.registryDoc ? {} : { id: this.docId };
    },

    loadAccess: function (next) {
        let me = this;
        me.request({
            url: '/meta/access',
            // Ask about the document this panel is actually showing: a guest's read
            // is VM.Audit, a registry file's is open to every authenticated user, so
            // asking about the wrong kind gives the wrong access answer (DESIGN §6).
            params: { id: me.docId },
            success: function (response) {
                me.access = response.result.data || { read: 0, write: 0 };
                me.syncAccessLabel();
                next();
            },
        });
    },

    // The meta-schema, once per load and only where it is used: it describes a
    // registry document, and a guest tab never shows one.
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
            // An older API has no /meta/schemas: the registry documents still show
            // as trees, just without declared rows or hovers.
            failure: () => next(),
        });
    },

    // This panel's one document.
    // Reads the document as YAML, for two reasons. `format=json` renders it as a
    // native Perl hash, which turns booleans into 1/0 and loses the file's key
    // order; the YAML text keeps both. Order is not a value (DESIGN §2), but a write
    // at the root view sends the document back, and keeping the order the file
    // already has is what stops that from churning it.
    //
    // The core is loaded first rather than assumed: it is lazy, and calling into it
    // before it is there is a bug this editor has already had once (with js-yaml).
    //
    // One narrower loss remains and cannot be fixed here: JavaScript objects order
    // integer-like keys first, so a document with keys `2` and `1` cannot round
    // trip through any client built on plain objects. That is a property of the
    // language, and it is a far smaller hole than the one it replaces.
    loadDocument: function (next) {
        let me = this;
        PVE.meta.Core.load().then(
            function () {
                me.request({
                    url: me.urlFor(me.docId),
                    params: me.docParams({ format: 'yaml' }),
                    success: function (response) {
                        let d = response.result.data || {};
                        // The server could read the bytes but they are not a
                        // document. The tree cannot show one -- there are no rows --
                        // and it must not pretend the document is empty, because a
                        // write from it would then replace the file with what was
                        // edited on top of nothing.
                        //
                        // So hand it to the text editor, which is the one place a
                        // document that is not a document can still be worked on, and
                        // is where DESIGN §7 says the repair happens: a root replace
                        // with a full document. Monaco already puts the parser's own
                        // complaint on the offending line, so nothing here has to
                        // explain what is wrong. `docParseError` is what keeps the
                        // tree out of reach until it parses again.
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
                            Proxmox.Utils.setErrorMask(me, Ext.htmlEncode(PVE.meta.Utils.errText(err)));
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
                // Without the core this panel cannot read a document faithfully, and
                // reading it unfaithfully is what this whole path exists to stop.
                Proxmox.Utils.setErrorMask(me, Ext.htmlEncode(PVE.meta.Utils.errText(err)));
            },
        );
    },

    // --- writes -------------------------------------------------------------

    // The one request an edit is (DESIGN §8), built and not sent: a pure function, so
    // "which write does this edit produce" is a question with a tested answer rather
    // than a shape assembled at four call sites. An edit is
    // `{ path, op: 'set' | 'delete', value }`. A set is `view::replace` at its own
    // path, which creates the maps above it, so a new key at `a.b.c` needs no
    // ancestor of its own; the root view is the whole document and is named by
    // leaving `view` out. A delete is `DELETE ?view=`, which takes the key's note
    // with it. `force` is the "Save anyway" an enforcing prefix's 422 offers.
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

    write: function (docId, params, onSuccess, retry) {
        this.submit(
            { url: this.urlFor(docId), method: 'PUT', params: this.docParams(params) },
            onSuccess,
            retry,
        );
    },

    // `retry` is what the "Save anyway" tick calls: the same write again, with
    // `force=1`. Without one a 422 is an ordinary error.
    submit: function (opts, onSuccess, retry) {
        let me = this;
        Proxmox.Utils.API2Request(
            Ext.apply(
                {
                    waitMsgTarget: me,
                    success: function () {
                        if (onSuccess) {
                            onSuccess();
                        }
                        me.reload();
                    },
                    failure: function (response) {
                        // The API's message, verbatim. A 409 means somebody else wrote the
                        // document since we read it: reload first, then say so.
                        let status = String((response.result || {}).status);
                        let text =
                            response.htmlStatus || Proxmox.Utils.getResponseErrorMessage(response);
                        if (status === '409') {
                            if (me.mode === 'text') {
                                me.refreshText();
                            } else {
                                me.reload();
                            }
                            Ext.Msg.alert(gettext('Conflict'), text);
                            return;
                        }
                        // An enforcing prefix refused this write, naming the paths
                        // (DESIGN §5). The tick is what makes storing it anyway a
                        // deliberate act: the server's lint decides what is storable,
                        // and an operator whose schema has drifted must not be able to
                        // lock the administrator out of editing.
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
                                    }
                                },
                            });
                            return;
                        }
                        Ext.Msg.alert(gettext('Error'), text);
                    },
                },
                opts,
            ),
        );
    },
};

