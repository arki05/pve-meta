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
    // is `prefixes/<name>` or `nodes/<node>/prefixes/<name>` -- the path it is served
    // at -- and everything else is a vmid (`api::parse_id`).
    urlFor: function (id) {
        return id.indexOf('/') === -1 ? '/meta/guests/' + id : '/meta/' + id;
    },

    // The document id of a registry file: `<kind>/<name>`, or for a node's prefix
    // file `nodes/<node>/prefixes/<name>` -- its own document, never the cluster file
    // of the same name (`api::parse_id`).
    registryId: function (kind, name, node) {
        return node ? 'nodes/' + node + '/prefixes/' + name : kind + '/' + name;
    },

    // What kind of document an id names -- which decides what governs its rows. A
    // node's prefix file is a prefix, described by the same meta-schema.
    docKind: function (id) {
        if (id.indexOf('prefixes/') === 0 || /^nodes\/[^/]+\/prefixes\//.test(id)) {
            return 'prefix';
        }
        if (id.indexOf('permissions/') === 0) {
            return 'permission';
        }
        return 'guest';
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
        // Staged edits are the reason a reload is not free any more: re-reading the
        // document is fine, but the overlay on top of it would be describing changes
        // against content that has moved. Ask, the same way leaving Text mode dirty
        // does.
        if (me.isDirty()) {
            Ext.Msg.confirm(
                gettext('Confirm'),
                gettext('Discard the unapplied changes and reload?'),
                function (btn) {
                    if (btn === 'yes') {
                        me.pending = PVE.meta.EditSet.empty();
                        me.reload();
                    }
                },
            );
            return;
        }
        Proxmox.Utils.setErrorMask(me, true);
        // Cleared here and set only by `loadDocument`: it describes the document
        // this load is about to read, not the one the panel used to hold.
        me.docParseError = '';
        me.loadPrefixes(() =>
            me.loadPermissions(() =>
                me.loadAccess(() =>
                    me.loadSchemas(() =>
                        me.loadDocument(() => Proxmox.Utils.setErrorMask(me, false)),
                    ),
                ),
            ),
        );
    },

    // A failure here doesn't break the page: the Access column and the
    // schema-declared rows stay empty, rather than the page.
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

    // What `loadPrefixes` asks for. A guest's rows come from the set in effect for
    // it: the packaged and cluster files with its current node's on top, one per
    // name, resolved by the server from the guest's id (DESIGN §3), so the reload a
    // migration's token change causes gets the new node's set. A registry
    // document's rule picker wants every file.
    prefixParams: function () {
        return this.registryDoc ? { all: 1 } : { id: this.docId };
    },

    loadPermissions: function (next) {
        let me = this;
        me.request({
            url: '/meta/permissions',
            success: function (response) {
                me.permissions = response.result.data || [];
                next();
            },
            failure: function () {
                me.permissions = [];
                next();
            },
        });
    },

    // There is no separate tag request: `GET /meta/access` returns the guest's
    // tags with the access answer (see `loadAccess`), instead of a `GET
    // /meta/guests` that would read, parse and digest every document in the
    // cluster just to learn one guest's tags, on every open of every guest tab
    // that has a `tag:` selector anywhere in the registry, which the shipped
    // traefik prefix has.

    loadAccess: function (next) {
        let me = this;
        me.request({
            url: '/meta/access',
            // Ask about the document this panel is actually showing: a guest's read
            // is VM.Audit, a registry file's is open to every authenticated user, so
            // asking about the wrong kind gives the wrong access answer (DESIGN §6).
            params: { id: me.docId },
            success: function (response) {
                me.access = response.result.data || { read: 0, write: 0, scopes: [], tags: [] };
                // The server resolved the selectors it enforces; these tags are
                // for the *rendering* decisions the client makes on top -- which
                // prefixes apply to this guest. Same tags, same authority, one
                // request instead of two.
                me.tags = me.access.tags || [];
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
    // order; the YAML text keeps both. Order is not a value (DESIGN §2), but an
    // Apply at the root view writes `plannedData()` back, and keeping the order
    // the file already has is what stops that from churning it.
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
                    params: { format: 'yaml' },
                    success: function (response) {
                        let d = response.result.data || {};
                        // The server could read the bytes but they are not a
                        // document. The tree cannot show one -- there are no rows --
                        // and it must not pretend the document is empty, because an
                        // Apply would then replace the file with whatever was staged
                        // on top of nothing.
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

    poll: function () {
        let me = this;
        if (
            !me.rendered ||
            me.isDestroyed ||
            me.editing ||
            me.textWindow ||
            me.mode === 'text' ||
            // Never pull the document out from under staged edits. The token keeps
            // moving; the next tick after Apply or Revert picks the change up.
            me.isDirty()
        ) {
            return;
        }
        Proxmox.Utils.API2Request({
            url: '/meta/version',
            method: 'GET',
            // Scoped to the document this panel shows. Unscoped, every tick of
            // every open tab read and hashed every document *and every snapshot
            // copy* in the cluster to answer a question about one guest, and any
            // guest changing anywhere reloaded every open editor. The scoped
            // token still covers the prefix and permission directories, so a
            // registry change reloads this panel the way it always did.
            params: { id: me.docId },
            failure: Ext.emptyFn, // transient; the next tick tries again
            success: function (response) {
                let token = (response.result.data || {}).token;
                if (me.token === null) {
                    me.token = token;
                } else if (token && token !== me.token) {
                    // Re-check: an edit (or a text editor) may have started while
                    // this request was in flight. Do *not* advance me.token here -
                    // leaving it stale means the next 5 s tick sees the same change
                    // and retries, instead of the reload being lost silently.
                    if (me.isDestroyed || me.editing || me.textWindow || me.mode === 'text') {
                        return;
                    }
                    me.token = token;
                    me.reload();
                }
            },
        });
    },

    // --- writes -------------------------------------------------------------

    write: function (docId, params, onSuccess) {
        this.submit({ url: this.urlFor(docId), method: 'PUT', params: params }, onSuccess);
    },

    submit: function (opts, onSuccess) {
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
                        let conflict = String((response.result || {}).status) === '409';
                        if (conflict) {
                            if (me.mode === 'text') {
                                me.refreshText();
                            } else {
                                me.reload();
                            }
                        }
                        Ext.Msg.alert(
                            conflict ? gettext('Conflict') : gettext('Error'),
                            response.htmlStatus || Proxmox.Utils.getResponseErrorMessage(response),
                        );
                    },
                },
                opts,
            ),
        );
    },
};

