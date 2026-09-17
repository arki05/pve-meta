// ---------------------------------------------------------------------------
// The panel: a card layout over the tree and the full-document text editor.
// ---------------------------------------------------------------------------

// PVE.meta.compose, not Ext's `mixins`: the offline smoke harness stubs
// Ext.define as "store the config object on the namespace" and reads
// PVE.meta.TreePanel's members straight off it, so the members have to be
// on the config object *before* Ext.define ever sees it -- composing here
// has no class-loader order to get right, a real mixin merge would.
Ext.define('PVE.meta.TreePanel', PVE.meta.compose({
    extend: 'Ext.panel.Panel',
    xtype: 'pveMetaTreePanel',

    layout: 'card',
    border: false,

    // vmid/node/type/dc arrive as config properties from pve-ext's page loader;
    // pveSelNode is the fallback for anything that adds this panel the PVE way.
    pveSelNode: undefined,
    // Set when this panel is inside a window: its footer then carries the way out,
    // which is also the way to abandon staged edits.
    onClose: undefined,

    initComponent: function () {
        let me = this;
        let sel = (me.pveSelNode && me.pveSelNode.data) || {};

        me.vmid = me.vmid || sel.vmid;
        // This panel is ONE document's editor, named by `docId`: a guest's, or a
        // prefix/permission file's -- they are all documents (DESIGN §6), so the
        // same tree, markers, text editor and diff serve both, and the registry
        // grids open one of these in a window rather than reimplementing any of it.
        //
        // Rows still carry their document's id even though there is only ever one:
        // it is what every write threads through, and a panel that had to remember
        // which document it was on top of which row was selected is how the digest of
        // one document ends up on a write to another.
        me.docId = me.docId || String(me.vmid);
        // A registry document: no tags to resolve, no permissions to apply, and the
        // meta-schema describes it instead of the prefixes.
        me.registryDoc = me.docKind(me.docId) !== 'guest';
        // Start fetching the core now rather than at the first document read: the
        // registry grids open their dialogs before that read lands, and a validator
        // with no core to ask checks nothing.
        // `loadDocument` awaits the same promise and reports its failure.
        PVE.meta.Core.load().catch(Ext.emptyFn);
        me.docState = Object.create(null); // id -> { digest, data }
        // What is staged: the difference between the stored document and the one
        // shown, as `{ path, op: 'set' | 'delete', value }` each. `setPlanned`
        // derives it whenever the shown document changes; a write clears it.
        me.pending = PVE.meta.EditSet.empty();
        me.schemas = {}; // GET /meta/schemas, the shape of a registry document
        me.access = { read: 1, write: 0, scopes: [] };
        me.prefixes = [];
        me.permissions = [];
        me.tags = [];
        me.token = null;
        me.editing = false; // a row editor is open
        me.mode = 'tree';
        me.textLang = 'yaml';
        me.textOriginal = '';

        me.store = Ext.create('Ext.data.TreeStore', {
            model: 'PVE.meta.TreeModel',
            root: { expanded: true, children: [] },
        });

        Ext.apply(me, {
            tbar: me.buildToolbar(),
            bbar: me.buildFooter(),
            items: [me.buildTreeCard(), me.buildTextCard()],
        });
        me.callParent();

        me.tree = me.down('#metaTree');
        me.on('afterrender', function () {
            // The load first: it is asynchronous and cannot be hurt by what follows,
            // whereas a button sync that threw before it used to leave the panel
            // empty for good.
            me.reload();
            me.syncButtons();
        });
        me.pollTask = Ext.TaskManager.start({ run: () => me.poll(), interval: 5000, fireOnStart: false });
        me.on('destroy', function () {
            Ext.TaskManager.stop(me.pollTask);
            if (me.textWindow) {
                me.textWindow.close();
            }
            PVE.meta.Monaco.dispose(me.textEditor);
            me.textEditor = null;
        });
    },

    // --- chrome --------------------------------------------------------------

    buildTreeCard: function () {
        let me = this;
        return {
            xtype: 'treepanel',
            itemId: 'metaTree',
            // A staged row renders on two lines (stored above, pending below), the
            // way proxmoxlib's PendingObjectGrid does.
            variableRowHeight: true,
            store: me.store,
            rootVisible: false,
            scrollable: true,
            border: false,
            animate: false,
            useArrows: true,
            emptyText: gettext('No metadata'),
            viewConfig: { loadMask: false },
            columns: me.buildColumns(),
            listeners: {
                selectionchange: () => me.syncButtons(),
                cellclick: function (view, td, cellIndex, rec, tr, rowIndex, e) {
                    if (e.getTarget('.pve-meta-undo')) {
                        me.discardRow(rec);
                        e.stopEvent();
                    }
                },
                itemdblclick: (view, rec) => me.editRow(rec),
                itemkeydown: function (view, rec, item, index, e) {
                    if (e.getKey() === e.ENTER && rec) {
                        e.stopEvent();
                        me.editRow(rec);
                        return false;
                    }
                    return true;
                },
                // ExtJS has no per-node "expanded icon", so swap the one it has.
                itemexpand: function (node) {
                    if (node.data.expandedCls) {
                        node.set('iconCls', node.data.expandedCls);
                    }
                },
                itemcollapse: function (node) {
                    if (node.data.expandedCls) {
                        node.set('iconCls', PVE.meta.Icons.map);
                    }
                },
            },
        };
    },

    buildToolbar: function () {
        let me = this;
        return [
            {
                // The permission-document twin of Declare Key, and hidden by the same
                // rule: a missing concept elsewhere, not a missing permission.
                text: gettext('Add Rule'),
                itemId: 'ruleBtn',
                iconCls: 'fa fa-key',
                hidden: true,
                handler: () => me.addRule(),
            },
            {
                text: gettext('Add'),
                itemId: 'addBtn',
                iconCls: 'fa fa-plus',
                handler: function () {
                    let t = me.addTarget();
                    if (!t) {
                        return;
                    }
                    if (t.list) {
                        me.addListMember(t.path);
                    } else {
                        me.addKey(t.docId, t.path);
                    }
                },
            },
            {
                text: gettext('Edit'),
                itemId: 'editBtn',
                iconCls: 'fa fa-pencil',
                disabled: true,
                handler: () => me.editRow(me.getSelection()[0]),
            },
            {
                // Explicit, never implicit: a declared default is an offer, and this is
                // the one click that accepts it. Nothing in this system ever writes a
                // default on its own -- an unset key stays unset until someone means it.
                text: gettext('Set to Default'),
                itemId: 'defaultBtn',
                iconCls: 'fa fa-reply',
                disabled: true,
                handler: () => me.setToDefault(me.getSelection()[0]),
            },
            {
                // Only ever shown on a prefix document, where declaring a key is a
                // thing you can do; hidden everywhere else rather than disabled, since
                // on a guest tab it is not a missing permission but a missing concept.
                text: gettext('Declare Key'),
                itemId: 'declareBtn',
                iconCls: 'fa fa-tag',
                hidden: true,
                handler: () => me.declareKey(me.getSelection()[0]),
            },
            {
                text: gettext('Remove'),
                itemId: 'removeBtn',
                iconCls: 'fa fa-trash-o',
                disabled: true,
                handler: () => me.removeKey(me.getSelection()[0]),
            },
            '-',
            {
                text: gettext('Edit selection as text'),
                itemId: 'textSelBtn',
                iconCls: 'fa fa-file-code-o',
                disabled: true,
                handler: () => me.editSelectionAsText(),
            },
            '-',
            { text: gettext('Reload'), itemId: 'reloadBtn', iconCls: 'fa fa-refresh', handler: () => me.reload() },
            '->',
            // Only shown when the caller is restricted (DESIGN §12).
            { xtype: 'tbtext', itemId: 'accessText', cls: 'faded', hidden: true },
        ];
    },

    // The one bar at the bottom, for both cards. Which view you are looking at on
    // the left, what you can do about it on the right; the top toolbar acts on the
    // document's *contents*, which is a different kind of thing from committing.
    buildFooter: function () {
        let me = this;
        return [
            {
                xtype: 'segmentedbutton',
                itemId: 'modeBtn',
                value: 'tree',
                items: [
                    { text: gettext('Tree'), value: 'tree', ui: 'default-toolbar' },
                    { text: gettext('Text'), value: 'text', ui: 'default-toolbar' },
                ],
                listeners: { change: (btn, value) => me.onModeChange(value) },
            },
            PVE.meta.Footer.langToggle({
                itemId: 'textLangBtn',
                hidden: true,
                onChange: (value) => me.switchTextLang(value),
            }),
        ].concat(
            PVE.meta.Footer.actions({
                applyDisabled: true, // nothing staged yet; `syncFooter` decides after
                diff: () => me.showDiff(),
                format: () => me.formatText(),
                apply: () => (me.mode === 'text' ? me.applyText() : me.applyPending()),
                secondary: () => me.footerSecondary(),
                // In a window the exit *is* the discard, so there is one button for
                // both; in a tab there is nothing to close, so it is Revert.
                secondaryText: me.onClose ? gettext('Close') : gettext('Revert'),
            }),
        );
    },

    // Close, or Discard-and-close, or Revert — one button, because in a window the
    // way out and the way to abandon the edits are the same gesture.
    footerSecondary: function () {
        let me = this;
        if (me.mode === 'text') {
            me.discardText();
            return;
        }
        if (me.isDirty()) {
            me.revertPending();
            if (!me.onClose) {
                return;
            }
        }
        if (me.onClose) {
            me.onClose();
        }
    },

    buildColumns: function () {
        let me = this;
        let U = PVE.meta.Utils;
        let fade = (rec, html) => (rec.data.present ? html : '<span class="faded">' + html + '</span>');
        // `html` is already content-encoded; this only escapes it for the attribute.
        let tip = function (meta, html) {
            if (html) {
                meta.tdAttr = 'data-qtip="' + Ext.htmlEncode(html) + '"';
            }
        };
        // The grammar's description is the tooltip of every cell in the row (DESIGN §12)
        // -- unless the row does not match its schema, in which case that is the more
        // urgent thing to say and goes first.
        let rowTip = function (rec, meta) {
            let d = rec.data;
            let parts = [];
            if (d.finding) {
                parts.push(Ext.htmlEncode(d.finding));
            }
            if (d.belowCount) {
                parts.push(
                    Ext.htmlEncode(
                        Ext.String.format(
                            d.belowCount === 1
                                ? gettext('{0} problem below:')
                                : gettext('{0} problems below:'),
                            d.belowCount,
                        ),
                    ) +
                        '<br>' +
                        Ext.htmlEncode(d.belowText).replace(/\n/g, '<br>') +
                        (d.belowCount > 3 ? '<br>...' : ''),
                );
            }
            if (d.stagedBelow) {
                parts.push(
                    Ext.htmlEncode(
                        Ext.String.format(
                            d.stagedBelow === 1
                                ? gettext('{0} unapplied change below')
                                : gettext('{0} unapplied changes below'),
                            d.stagedBelow,
                        ),
                    ),
                );
            }
            if (d.grammarDescription) {
                parts.push(Ext.htmlEncode(d.grammarDescription));
            }
            tip(meta, parts.join('<br>'));
        };
        let unsetText = function (rec) {
            let d = rec.data.defaultValue;
            return d === undefined
                ? gettext('not set')
                : Ext.String.format(
                      gettext('not set (default: {0})'),
                      Ext.htmlEncode(U.displayValue(d, U.kindOf(d))),
                  );
        };
        return [
            {
                xtype: 'treecolumn',
                text: gettext('Key'),
                dataIndex: 'key',
                flex: 2,
                renderer: function (value, meta, rec) {
                    rowTip(rec, meta);
                    let d = rec.data;
                    let out = fade(rec, Ext.htmlEncode(value));
                    // Collapsing a branch must not hide what is inside it. These are
                    // the *branch's* markers -- something beneath this row -- so they
                    // sit in the Key column, next to the thing you would collapse,
                    // rather than in the Value column, which is empty for a map. The
                    // row's own trouble is still shown on its own value.
                    if (d.stagedBelow) {
                        out += ' <i class="fa fa-circle" style="color:darkorange"></i>';
                    }
                    if (d.belowCount) {
                        out += ' <i class="fa fa-exclamation-triangle warning"></i>';
                    }
                    return out;
                },
            },
            {
                text: gettext('Value'),
                dataIndex: 'valueText',
                flex: 3,
                renderer: function (value, meta, rec) {
                    rowTip(rec, meta);
                    let d = rec.data;
                    // A block of text collapses into one unreadable line in a grid
                    // cell. Show its first line and how much more there is; the row's
                    // `valueText` is untouched, so the editor still opens on all of it.
                    let shown = d.present
                        ? Ext.htmlEncode(PVE.meta.Utils.previewText(value))
                        : '<span class="faded">' + unsetText(rec) + '</span>';
                    if (d.kind === 'map' && !d.pending) {
                        return '';
                    }
                    if (d.finding) {
                        // Advisory, like every other schema signal: the row is still
                        // editable, the value is still there, and the message is in
                        // the tooltip. `warning` is proxmoxlib's own class.
                        shown =
                            '<i class="fa fa-exclamation-triangle warning"></i> ' +
                            '<span class="warning">' + shown + '</span>';
                    }
                    if (!d.pending) {
                        return shown;
                    }
                    // Staged, not written. Rendered the way proxmoxlib's own
                    // `PendingObjectGrid` renders a config change that has not taken
                    // effect yet (`proxmoxlib.js`, the Options pages): the stored
                    // value, then the pending one beneath it in `darkorange`, and a
                    // pending removal as the stored value struck through.
                    let stored = Ext.htmlEncode(PVE.meta.Utils.previewText(d.storedText));
                    let after =
                        d.pending === 'delete'
                            ? '<div style="text-decoration: line-through;">' +
                              (stored || '&nbsp;') +
                              '</div>'
                            : shown;
                    // The discard sits on the row it acts on, not in a toolbar, so what
                    // it applies to is never in question -- on a list member in
                    // particular, where an edit applies to the whole list.
                    let undo =
                        ' <i class="fa fa-undo pve-meta-undo" style="cursor:pointer" ' +
                        'data-qtip="' + Ext.htmlEncode(gettext('Discard this change')) + '"></i>';
                    return (
                        (d.pending === 'delete' ? '' : stored) +
                        '<div style="color:darkorange">' + after + undo + '</div>'
                    );
                },
            },
            {
                // Guest documents only, structurally: a registry document's top-level
                // keys are fixed and `deny_unknown_fields` refuses a fourth, so a
                // comment key cannot exist there to describe one (DESIGN §6). An
                // always-empty column is a column that teaches you to ignore columns.
                hidden: me.docKind(me.docId) !== 'guest',
                // The row's own comment key (`k__`) if present, else nothing.
                text: gettext('Description'),
                dataIndex: 'description',
                flex: 3,
                renderer: function (value, meta, rec) {
                    tip(
                        meta,
                        Ext.htmlEncode(rec.data.grammarDescription || value || ''),
                    );
                    return fade(rec, Ext.htmlEncode(value || ''));
                },
            },
            {
                // Permissions reach guest documents only (DESIGN §4), so on anything
                // else this column can only ever be blank.
                hidden: me.docKind(me.docId) !== 'guest',
                text: gettext('Access'),
                dataIndex: 'accessText',
                flex: 2,
                renderer: function (value, meta, rec) {
                    let list = rec.data.accessList || [];
                    if (!list.length) {
                        return '';
                    }
                    tip(
                        meta,
                        list
                            .map((a) =>
                                Ext.htmlEncode(a.name + ' (' + a.mode + ', ' + a.selector + ')'),
                            )
                            .join('<br>'),
                    );
                    return fade(
                        rec,
                        list
                            .map((a) =>
                                a.mode === 'ro'
                                    ? '<span class="faded">' + Ext.htmlEncode(a.name) + ' (ro)</span>'
                                    : Ext.htmlEncode(a.name),
                            )
                            .join(', '),
                    );
                },
            },
        ];
    },

    // Every path in `docId` whose value does not match its schema, by path. The tree
    // shows these on the rows themselves: the text editor already squiggles them, but
    // the tree is the view people actually open, and a value the schema refuses would
    // otherwise look exactly like one it liked. The same Shape answers here,
    // for the text editor's squiggles and for the warning banner Apply shows: one
    // question asked three times, through one implementation.
    findingsFor: function () {
        let out = Object.create(null);
        let shape = this.shapeFor(this.docId);
        if (!shape.hasSchema()) {
            return out;
        }
        // Against the *planned* document: a staged value that the schema refuses is
        // marked the moment it is staged, not after it has been written.
        shape.findings(this.plannedData()).forEach(function (f) {
            out[f.path] = f.msg;
        });
        return out;
    },

    // --- staged edits --------------------------------------------------------

    // The list at `path`, as it currently stands (staged edits included).
    listAt: function (path) {
        let v = PVE.meta.Utils.valueAt(this.plannedData(), path);
        return Array.isArray(v) ? v.slice() : [];
    },

    // Stages the list at `path` with member `index` replaced, or dropped when
    // `value` is undefined. One write of the whole list, because a view addresses
    // through maps only -- the same reason the member rows are not addressable.
    stageListMember: function (path, index, value) {
        let list = this.listAt(path);
        if (index < 0 || index >= list.length) {
            return;
        }
        if (value === undefined) {
            list.splice(index, 1);
        } else {
            list[index] = value;
        }
        this.stage(path, 'set', list);
    },

    // Makes `doc` the document you are looking at. The edit set is not a log of what
    // was done, it is the difference between what is stored and this -- so it is
    // derived here, from the result, and nowhere else. That is what keeps dirty
    // meaning *the document differs from what is stored* rather than *an edit object
    // exists*. Two ways it came to lie, both reported: opening the subtree editor and
    // pressing OK without typing staged a set of the value already there, and editing
    // a row from A to B and back to A left two edits describing no change. Either way
    // Revert appeared and Apply lit up while no row was marked. Not creating a no-op
    // edit would have been the narrower fix, and it misses the second case: A over a
    // pending B *is* a change at that moment. Asking what the document now is catches
    // both, and anything else that gets here by a route nobody thought of.
    //
    // It also makes every staged edit minimal, which the Text card already did on the
    // way out: replacing a whole subtree marks the rows that actually differ, not the
    // whole subtree.
    setPlanned: function (doc) {
        let me = this;
        me.pending = PVE.meta.EditSet.between(me.dataOf(me.docId), doc);
        me.buildTree();
        me.syncButtons();
    },

    // Records one edit: a `set` is `view::replace` at `path` and a `delete` is
    // `view::remove`, applied to the document as it stands -- so a set at `selector`
    // says everything about `selector.tag`, whatever was staged there before. The one
    // door every editor stages through: the row editor, Add Key, Add Rule, Declare
    // Key, Set to Default, the list helpers and the subtree text editor.
    stage: function (path, op, value) {
        let me = this;
        let U = PVE.meta.Utils;
        let edit = { path: path, op: op };
        if (op === 'set') {
            edit.value = value;
        }
        let doc = new PVE.meta.EditSet([edit]).apply(me.plannedData());
        if (op === 'delete') {
            // A delete never leaves behind a container the stored document does not
            // have. `view::remove` takes the key and keeps its parent, which is right
            // for a map the file holds -- an empty map is a stored state -- and wrong
            // for one an earlier edit created on the way to a key that is now gone:
            // set a ghost `docker.compose`, discard it, and `docker: {}` would stay
            // staged, with Apply lit to write a map the guest never asked for.
            let stored = me.dataOf(me.docId);
            let parent = U.parentPath(path);
            while (parent) {
                let now = U.valueAt(doc, parent);
                let isEmptyMap =
                    now && typeof now === 'object' && !Array.isArray(now) && Object.keys(now).length === 0;
                if (!isEmptyMap || U.valueAt(stored, parent) !== undefined) {
                    break;
                }
                doc = new PVE.meta.EditSet([{ path: parent, op: 'delete' }]).apply(doc);
                parent = U.parentPath(parent);
            }
        }
        me.setPlanned(doc);
    },

    isDirty: function () {
        return this.pending.length > 0;
    },

    // Puts the stored value back on one row and leaves the rest alone. Dropping a
    // row's edits and restoring what is stored there are the same thing once the edit
    // set is the difference: a ghost gets its value back, a key that was added goes,
    // and a subtree comes back whole.
    //
    // A list member needs more care: the edit is staged on the *list*, so putting the
    // whole stored list back would throw away every other member's change too. That
    // one member goes back instead, and the list's edit is gone once every member
    // matches again -- with no step of its own, since a list that matches is not a
    // difference.
    discardRow: function (rec) {
        let me = this;
        if (!rec || !rec.data.path) {
            return;
        }
        let d = rec.data;
        if (d.arrayIndex !== undefined && d.arrayIndex !== null) {
            me.discardListMember(d.path, d.arrayIndex);
            return;
        }
        if (d.storedIndex !== undefined && d.storedIndex !== null) {
            // A member the staged list dropped: put it back where it was, or at the
            // end if the list is shorter than that now. Restoring the whole stored
            // list here would throw away every other member's change.
            me.discardListMember(d.path, d.storedIndex);
            return;
        }
        let stored = PVE.meta.Utils.valueAt(me.dataOf(me.docId), d.path);
        me.stage(d.path, stored === undefined ? 'delete' : 'set', stored);
    },

    discardListMember: function (path, index) {
        let me = this;
        let stored = PVE.meta.Utils.valueAt(me.dataOf(me.docId), path);
        if (!Array.isArray(stored)) {
            return;
        }
        let list = me.listAt(path);
        if (index >= stored.length) {
            // An appended member has nothing stored to go back to: it goes.
            me.stageListMember(path, index, undefined);
        } else if (index >= list.length) {
            // A dropped member, on a list now shorter than where it was: back on
            // the end. The only place a member is ever added past the end.
            list.push(stored[index]);
            me.stage(path, 'set', list);
        } else {
            me.stageListMember(path, index, stored[index]);
        }
    },

    // The document as it would be. Everything the tree shows is computed from this,
    // so a staged value is linted, hovered and diffed exactly like a stored one.
    // The document as it would be if the staged edits were applied. `docId` is here
    // for a caller holding a row's own document; a panel shows one document, so it
    // defaults to that one.
    plannedData: function (docId) {
        return this.pending.apply(this.dataOf(docId || this.docId));
    },

    // Fold a whole subtree, as edited somewhere else, back into the edited document.
    // The one way a text buffer becomes staged edits at a view -- what "Edit selection
    // as text" ends with, and the reason that window no longer needs a write path of
    // its own.
    stageFromView: function (view, value) {
        this.stage(view || '', 'set', value);
    },

    // Revert: the document you are looking at is the stored one.
    revertPending: function () {
        let me = this;
        if (!me.isDirty()) {
            return;
        }
        me.setPlanned(me.dataOf(me.docId));
    },

    // The document a row belongs to; the panel's default for anything with no row.
    docOf: function (rec) {
        return (rec && rec.data && rec.data.docId) || this.docId;
    },

    // What describes this document's shape (`PVE.meta.Shape`). A guest document is
    // described by the prefixes that reach it, most-specific first (they shadow); a
    // prefix or permission file by the one meta-schema for its kind, rooted at the
    // document itself. One function, every caller that needs it -- the row builder, the row
    // markers, the text editor's squiggles and hovers, and the warning banner Apply
    // shows -- so they cannot disagree about what describes the document.
    //
    // One Shape per document, kept for as long as its inputs are the panel's
    // current ones: a Shape caches what the core derives from the listing and the
    // tags, and a render asks it three or four times. The cache is checked against
    // the inputs by identity rather than cleared at the right moment -- every load
    // replaces `prefixes`, `tags` or `schemas` with a new object, and a cache that
    // had to be told about each of those is a cache that is stale the first time
    // one is forgotten.
    shapeFor: function (id) {
        let me = this;
        me.shapes = me.shapes || Object.create(null);
        let inputs = me.shapeInputs(id);
        let have = me.shapes[id];
        if (!have || have.inputs.length !== inputs.length || !inputs.every((v, i) => v === have.inputs[i])) {
            have = me.shapes[id] = { inputs: inputs, shape: me.buildShape(id, inputs) };
        }
        return have.shape;
    },

    // What a document's Shape is built from. A guest's: the prefix listing and the
    // guest's tags (the server resolved the selectors it enforces; these tags are for
    // the rendering decisions the client makes on top, and the client only ever
    // matches tags it was given, DESIGN §12). A registry file's: the meta-schema for
    // its kind.
    shapeInputs: function (id) {
        let me = this;
        let kind = me.docKind(id);
        if (kind === 'guest') {
            return [me.prefixes, me.tags];
        }
        if (kind === 'prefix' || kind === 'permission') {
            return [(me.schemas || {})[kind]];
        }
        return [];
    },

    buildShape: function (id, inputs) {
        let kind = this.docKind(id);
        if (kind === 'guest') {
            return PVE.meta.Shape.of(inputs[0], inputs[1]);
        }
        if (kind === 'prefix' || kind === 'permission') {
            return PVE.meta.Shape.rooted(inputs[0]);
        }
        return PVE.meta.Shape.empty();
    },

    // --- selection and buttons ----------------------------------------------

    getRootNode: function () {
        return this.store.getRoot();
    },

    getSelection: function () {
        return this.tree ? this.tree.getSelection() : [];
    },

    setSelection: function (rec) {
        if (this.tree) {
            this.tree.setSelection(rec);
        }
    },

    parentPath: (rec) => (rec.parentNode && rec.parentNode.data.path) || '',

    // Add goes into the selected map, the parent of a selected leaf, or the root.
    // Where a new key goes: into the selected map, beside the selected leaf, or --
    // with nothing selected -- at the root of this panel's document.
    //
    // A list is the exception: `Add` on one, or on a member of one, appends to the
    // list rather than adding a key beside it, because a list has no keys to add.
    addTarget: function () {
        let me = this;
        let rec = me.getSelection()[0];
        if (!rec) {
            return { docId: me.docId, path: '' };
        }
        let d = rec.data;
        if (d.kind === 'array' || d.arrayIndex !== undefined) {
            return { docId: me.docOf(rec), path: d.path, list: true };
        }
        return {
            docId: me.docOf(rec),
            path: d.kind === 'map' ? d.path : me.parentPath(rec),
        };
    },

    syncButtons: function () {
        let me = this;
        let rec = me.getSelection()[0];
        let d = rec ? rec.data : null;
        let U = PVE.meta.Utils;
        let text = me.mode === 'text';
        let set = function (id, disabled) {
            let btn = me.down('#' + id);
            if (btn) {
                btn.setDisabled(disabled);
            }
        };
        let target = me.addTarget();
        // A permission file has three keys and the parser refuses a fourth
        // (`deny_unknown_fields`), so an arbitrary Add can only ever produce a file
        // the loader would skip: the one thing you add to one is a rule, and Add Rule
        // is that. On a prefix definition Add stays, because `schema` holds whatever
        // you declare -- but its *root* keys are fixed the same way, so Add there is
        // disabled until you are somewhere it means something.
        let kind = me.docKind(me.docId);
        let addBtn = me.down('#addBtn');
        if (addBtn) {
            addBtn.setHidden(kind === 'permission');
        }
        let fixedRoot = kind === 'prefix' && target && target.path === '';
        set('addBtn', text || !target || fixedRoot || !me.editableFor(target.path));
        let row = d;
        set('editBtn', text || !row || !row.editable);
        set('removeBtn', text || !row || !row.present || !row.editable);
        let dirty = me.isDirty();
        set('textSelBtn', text || !row);
        set('reloadBtn', text);
        me.syncFooter();
        let dflt = me.down('#defaultBtn');
        if (dflt) {
            // Disabled, not hidden. What varies per *document* may hide (Declare Key
            // is a missing concept on a guest, not a missing permission); what varies
            // per *row* must not, or the buttons beside it shift under the pointer
            // every time the selection changes -- which is how you click Remove and
            // hit something else.
            // Hidden entirely when nothing in this document declares a default --
            // a permission file never can, so the button was pure furniture there.
            // Disabled, not hidden, when the document has defaults but this row is
            // not one of them: that varies per row, and a button that moves under
            // the pointer is how you aim for one thing and hit another.
            //
            // Offered on any row that has a default and is not already at it --
            // not just on unset ones. A default is the answer to "what should
            // this be", and the moment you most want that answer is when the
            // value in front of you is wrong; refusing then meant the only way
            // back to a declared default was to remember it and retype it. It
            // stages like every other edit, so an accidental click is one
            // Discard away and nothing is written until Apply.
            let offers =
                !!row &&
                row.defaultValue !== undefined &&
                !(row.present && U.sameValue(row.rawValue, row.defaultValue));
            dflt.setHidden(!me.hasDefaults);
            dflt.setDisabled(text || !offers || !row.editable);
        }
        // The Text toggle's enabled state depends on staged edits too, and this is
        // the function that runs whenever those change.
        me.syncAccessLabel();
        let rule = me.down('#ruleBtn');
        if (rule) {
            rule.setHidden(me.docKind(me.docId) !== 'permission');
            rule.setDisabled(text || !me.editableFor(''));
        }
        let declare = me.down('#declareBtn');
        if (declare) {
            // Hidden by the *document*, disabled by the *row* -- the rule above. Hidden
            // reads `me.docId`, not the selected row, because the document kind cannot
            // change while you are in the tree, but the selected row does on every click.
            declare.setHidden(me.docKind(me.docId) !== 'prefix');
            declare.setDisabled(text || !row || !row.editable);
        }
    },

    // One place decides what the footer says, in either mode.
    syncFooter: function () {
        let me = this;
        let textMode = me.mode === 'text';
        // `metaDiff` is deliberately not in this list: it is not a text-mode button.
        ['textLangBtn', 'metaFormat'].forEach(function (id) {
            let c = me.down('#' + id);
            if (c) {
                c.setHidden(!textMode);
            }
        });
        PVE.meta.Footer.sync(me, {
            // In text mode the buffer is the edit, and Apply is offered whenever the
            // caller may write at all -- the diff is what decides if it is worth it.
            //
            // "At all" means any rw scope, not full write access. A whole-document
            // write is authorized by what it changes (DESIGN §5), so a principal
            // holding `rw` on one prefix can perfectly well apply a buffer whose only
            // changes are inside it -- and the server refuses the rest, naming the path
            // it refused. Requiring full write here disabled the button for exactly the
            // callers this view is most useful to.
            canApply: textMode ? PVE.meta.Access.hasAnyWrite(me.access) : me.isDirty(),
            // The same count in both views: the buffer is rendered from the planned
            // document, so those staged edits are in it. Showing it only in the tree
            // made switching to Text look like it had dropped them.
            count: me.pending.length,
            dirty: textMode ? true : me.isDirty(),
            dirtyText: me.onClose ? gettext('Discard') : gettext('Revert'),
            cleanText: me.onClose ? gettext('Close') : gettext('Revert'),
            secondaryOnlyWhenDirty: !me.onClose,
        });
    },

    // The label is a restriction notice, so it says nothing at all for a caller with
    // full write access (DESIGN §12).
    syncAccessLabel: function () {
        let me = this;
        let modeBtn = me.down('#modeBtn');
        if (modeBtn && modeBtn.items.getAt(0)) {
            // A document that does not parse has no rows to show, and a tree that
            // showed none would be indistinguishable from an empty document -- one
            // Apply away from replacing the file with nothing. Text is the only
            // view of it until it parses.
            let tree = modeBtn.items.getAt(0);
            tree.setDisabled(!!me.docParseError);
            tree.setTooltip(
                me.docParseError
                    ? Ext.String.format(
                          gettext('This document is not valid YAML and can only be repaired as text: {0}'),
                          me.docParseError,
                      )
                    : undefined,
            );
        }
        if (modeBtn && modeBtn.items.getAt(1)) {
            // The Text card is the whole document at the root view, and a scope-only
            // principal may not read that at all (DESIGN §5): do not offer it. Staged
            // edits are no longer a reason to refuse -- the buffer is rendered from the
            // planned document, so they are *in* it, and switching back turns whatever
            // was typed into staged edits again.
            modeBtn.items.getAt(1).setDisabled(!me.access.read);
        }
        // A Text-mode Apply is a root replace, which needs full write and nothing else
        // (DESIGN §5, `authorize_view_write`). Without this a read-only caller could
        // compose a whole document, open the diff, tick through the schema warning and
        // collect a 403 at the very end -- the server was right, the button was a lie.
        me.syncFooter();
        let label = me.down('#accessText');
        if (!label) {
            return;
        }
        if (me.access.write) {
            label.setVisible(false);
            return;
        }
        // The core's rule, the same one the footer's Apply asks for. Deciding it
        // again here is how a label and a button come to disagree.
        let scoped = PVE.meta.Access.hasAnyWrite(me.access);
        label.setText(scoped ? gettext('Scoped write access') : gettext('Read-only'));
        label.setVisible(true);
    },

    // --- rows ---------------------------------------------------------------

    // `shapeFor` (the prefixes that reach this guest) and `applicablePermissions`
    // (the rules that do) both resolve `selector: { tag: t }` against `me.tags`,
    // which `GET /meta/access` fills in only for a caller with VM.Audit (DESIGN §8).
    // That is not a gap here: this is a guest tab, and a caller without VM.Audit
    // on `/vms/<vmid>` never sees the guest in the resource tree at all
    // (`PVE::API2::Cluster::resources` skips it), so no reachable caller of this
    // panel has tags we cannot read. A scope-only principal is still bound by
    // its permissions -- they are enforced server-side, on the API it actually uses.

    // The permission rules that reach this guest, with the file each came from.
    // Permissions decide *access*, and unlike prefixes they accumulate by
    // containment: a rule on `homelab` covers `homelab.docker` (DESIGN section 4).
    // The core answers from the listing and this guest's tags, the same way the
    // server computes a caller's scopes, and a file that did not load grants nothing.
    applicablePermissions: function () {
        let me = this;
        if (me.registryDoc) {
            return []; // permissions apply to guest documents only
        }
        return PVE.meta.Access.rulesReaching(me.permissions, me.tags);
    },

    // Every rule whose prefix covers this row, `rw` first. Several principals
    // may read a subtree; this is about who writes and who subscribes, not ownership.
    accessFor: function (path, scopes) {
        let U = PVE.meta.Utils;
        let out = [];
        // Registration names are operator-chosen strings, so no plain `{}` here.
        let seen = Object.create(null);
        scopes.forEach(function (s) {
            if (!PVE.meta.Access.covers(s.prefix, path)) {
                return;
            }
            let name = s.name || s.authid || '';
            let mode = s.mode === 'ro' ? 'ro' : 'rw';
            let key = name + '\u0000' + mode;
            if (seen[key]) {
                return;
            }
            seen[key] = true;
            out.push({
                name: name,
                mode: mode,
                selector: U.selectorText(s.selector),
                prefix: s.prefix,
            });
        });
        out.sort((a, b) => (a.mode === b.mode ? 0 : a.mode === 'rw' ? -1 : 1));
        return out;
    },

    accessSummary: (list) => list.map((a) => a.name + (a.mode === 'ro' ? ' (ro)' : '')).join(', '),

    // Nothing is editable before the core has arrived: `syncButtons` runs on
    // render, ahead of the first load, and a row that cannot be judged yet is a
    // row that cannot be edited yet.
    editableFor: function (path) {
        return PVE.meta.Core.loaded() && PVE.meta.Access.canWrite(this.access, path);
    },

    // The document and the grammars are two sources for the same rows, so merge them
    // as plain entries first — much less fiddly than merging Ext node configs.
    entry: function (parent, key, path) {
        parent.children[key] = parent.children[key] || {
            key: key,
            path: path,
            // Object.create(null): `key` is a document key (attacker-chosen, and
            // no key is reserved - DESIGN §2), so a plain `{}` here lets a key
            // like `constructor` or `hasOwnProperty` resolve through the
            // prototype chain instead of being treated as absent.
            children: Object.create(null),
            present: false,
        };
        return parent.children[key];
    },

    addData: function (entry, value) {
        let me = this;
        let U = PVE.meta.Utils;
        entry.present = true;
        entry.kind = 'map';
        Object.keys(value).forEach(function (key) {
            let v = value[key];
            if (U.isComment(key)) {
                let target = U.commentTarget(key);
                if (target === '') {
                    entry.description = String(v); // the bare `__` documents the map
                } else {
                    me.entry(entry, target, U.joinPath(entry.path, target)).description = String(v);
                }
                return;
            }
            let child = me.entry(entry, key, U.joinPath(entry.path, key));
            if (U.kindOf(v) === 'map') {
                me.addData(child, v);
                return;
            }
            child.present = true;
            child.kind = U.kindOf(v);
            child.value = v;
            // **A list is a container, like a map.** Its members are rows, so you can
            // see them, select one and act on it -- which is the whole reason a map
            // is a tree and not a blob of JSON in a cell. A list was the one shape
            // that stayed a blob, for no reason other than that it came second.
            //
            // The member rows are **not addressable**: a view addresses through maps
            // only, so there is no path to `groups[1]` (DESIGN §2) and nothing may try
            // to write one. They carry their index instead, and everything that acts
            // on one rewrites the list it is in -- which is exactly what staging is
            // for (§12), so this needs no new write path.
            if (child.kind === 'array') {
                v.forEach(function (item, i) {
                    let row = me.entry(child, String(i), child.path);
                    row.present = true;
                    row.arrayIndex = i;
                    row.addressable = false;
                    row.rawItem = item;
                    if (item !== null && typeof item === 'object') {
                        // One line for a member with structure of its own; its real
                        // value rides along in `rawItem` for whatever edits it.
                        row.kind = 'string';
                        row.value = U.itemSummary(item);
                    } else {
                        row.kind = U.kindOf(item);
                        row.value = item;
                    }
                });
            }
        });
    },

    // The rows a document's shape declares, on top of what `addData` found in it.
    //
    // Two things, from the one Shape. Every prefix that reaches the document gets a
    // row: declaring a prefix *is* a statement about the document -- "something of
    // mine lives at this key" -- and it is the statement the whole permission model
    // is written in terms of. Hiding the row until someone had already put content
    // there meant a prefix that applied to every guest was invisible on every guest
    // that had not used it yet, which reads as "netbird is missing" rather than
    // "netbird is empty". Then every path a schema declares gets its declared type,
    // default, enum, range, format and description -- from the core's schema index,
    // which is already pruned where a more specific prefix governs, so a parent's
    // `properties` never reach into a child prefix's subtree and rewrite its row
    // kind. Schemas shadow, they never merge (DESIGN section 3); the same rule
    // the findings and the hovers come through, from the same Shape.
    //
    // A registry document's meta-schema is rooted at the document (DESIGN §6):
    // its "prefix" is the empty path, which is the root row itself and gets nothing.
    addShape: function (root, shape) {
        let me = this;
        let U = PVE.meta.Utils;
        let ensure = function (path) {
            let entry = root;
            let at = '';
            if (path) {
                path.split('.').forEach(function (seg) {
                    at = U.joinPath(at, seg);
                    entry = me.entry(entry, seg, at);
                });
            }
            return entry;
        };
        // The entry at `path` if something put one there, without creating it: the
        // read-only half of `ensure`, for a declaration that may only decorate.
        let existing = function (path) {
            let entry = root;
            let segs = path ? path.split('.') : [];
            for (let i = 0; i < segs.length; i++) {
                entry = entry.children[segs[i]];
                if (!entry) {
                    return null;
                }
            }
            return entry;
        };
        shape.declared().forEach(function (d) {
            if (!d.prefix) {
                return;
            }
            let entry = ensure(d.prefix);
            // `||`, not `=`: `addData` ran first, so a value already stored here
            // keeps the kind it actually has. That is what lets a prefix hold a
            // single scalar -- a prefix is a key like any other, and one that needs
            // to say nothing but `true` should not have to grow a subkey to say it.
            // Only an *absent* prefix falls back to a map, which is the shape
            // almost every one of them turns out to have.
            entry.kind = entry.kind || 'map';
            // The prefix's own description, which for a prefix with no schema is
            // the only thing its row can say about itself.
            entry.grammarDescription = entry.grammarDescription || d.description;
        });
        // Two passes over the index. A hidden declaration decorates a row that
        // exists; it never creates one. That is the whole difference: a schema the
        // size of Traefik's is mostly keys nobody sets on a given guest, and every
        // one of them as a greyed row buries what the guest actually says. `addData`
        // ran first, so a hidden key that *is* set already has its row and still
        // gets its type, enum, range and default -- hiding a declaration must never
        // hide data, nor excuse it from its own schema.
        //
        // Rows first, then decoration, because a shown key inside a hidden subtree
        // creates the rows above it on its way in, and the subtree's own node --
        // visited earlier, hidden, with nothing stored there -- would have found no
        // row to decorate and left a bare shell where a typed map should be.
        let index = shape.schemaIndex().filter((ix) => ix.path !== ix.prefix);
        index.forEach(function (ix) {
            if (!ix.hidden) {
                ensure(ix.path);
            }
        });
        index.forEach(function (ix) {
            let ps = ix.schema || {};
            let child = existing(ix.path);
            if (!child) {
                return;
            }
            // The comment key stays the Description column; the grammar's own
            // description is the tooltip (DESIGN §12), so they are two fields.
            child.grammarDescription = child.grammarDescription || ps.description;
            if (ps.type === 'object') {
                child.kind = 'map';
                return;
            }
            // A declared type wins over the type inferred from the stored value:
            // it is the operator's statement of what the key means, and the API's
            // JSON view cannot tell a boolean from the integer 1 anyway.
            child.kind = ps.type ? me.schemaKind(ps) : child.kind || 'string';
            // First writer wins, like `grammarDescription` above: the index lists
            // each path once, under the prefix that governs it, so this is only
            // ever the same declaration twice -- it is here so the six fields
            // cannot disagree if that ever changes.
            if (ps.default !== undefined && child.defaultValue === undefined) {
                child.defaultValue = ps.default;
            }
            if (ps.enum && child.enumValues === undefined) {
                child.enumValues = ps.enum;
            }
            if (ps.minimum !== undefined && child.minimum === undefined) {
                child.minimum = ps.minimum;
            }
            if (ps.maximum !== undefined && child.maximum === undefined) {
                child.maximum = ps.maximum;
            }
            if (ps.format !== undefined && child.format === undefined) {
                child.format = ps.format;
            }
            // One of two extensions to the PVE::JSONSchema dialect (`hidden` is the
            // other): "this string is a block of text". Only a declaration can say so before the key has
            // a value, which is exactly what `Utils.editorKind` cannot see for
            // itself. It is an editor hint and nothing else -- the server neither
            // reads it nor validates against it, like `format` (DESIGN §7).
            if (ps.multiline !== undefined && child.multiline === undefined) {
                child.multiline = !!ps.multiline;
            }
        });
    },

    // The declared type as the kind the editor and `parseValue` speak. One mapping:
    // `Utils.schemaValueKind` had a second copy of it, so adding a type to one and not
    // the other would have made a row's editor disagree with the parser behind it.
    schemaKind: function (schema) {
        return PVE.meta.Utils.schemaValueKind((schema && schema.type) || 'string');
    },

    // The merged rows of ONE document: what is present in it, plus what its grammar
    // declares (DESIGN §12). Two sources, one set of entries.
    documentEntries: function () {
        let me = this;
        let root = { key: '', path: '', children: Object.create(null), present: true, kind: 'map' };
        me.addData(root, me.plannedData());
        // A row staged for deletion is gone from the planned document, but it should
        // not vanish off the screen before it is applied -- you would be looking at a
        // tree that already claims the write happened. It comes back as a ghost.
        // A staged list edit is one write of the whole list, but it is almost never a
        // change to the whole list: show it on the members that actually differ. Any
        // member the edit dropped comes back as a ghost, the same as a deleted key.
        let storedDoc = me.dataOf(me.docId);
        me.pending.edits.forEach(function (e) {
            if (e.op !== 'set' || !Array.isArray(e.value)) {
                return;
            }
            let before = PVE.meta.Utils.valueAt(storedDoc, e.path);
            if (!Array.isArray(before) || before.length <= e.value.length) {
                return;
            }
            let entry = root;
            let path = '';
            e.path.split('.').forEach(function (seg) {
                path = PVE.meta.Utils.joinPath(path, seg);
                entry = me.entry(entry, seg, path);
            });
            before.slice(e.value.length).forEach(function (item, i) {
                let row = me.entry(entry, String(e.value.length + i), e.path);
                row.present = false;
                row.pendingDelete = true;
                row.arrayIndex = null; // gone: there is no member to act on
                row.storedIndex = e.value.length + i; // but there is one to put back
                row.addressable = false;
                row.kind = 'string';
                row.value = PVE.meta.Utils.itemSummary(item);
            });
        });

        me.pending.edits
            .filter((e) => e.op === 'delete')
            .forEach(function (e) {
                let entry = root;
                let path = '';
                e.path.split('.').forEach(function (seg) {
                    path = PVE.meta.Utils.joinPath(path, seg);
                    entry = me.entry(entry, seg, path);
                    entry.kind = entry.kind || 'map';
                });
                entry.pendingDelete = true;
                entry.present = false;
            });
        me.addShape(root, me.shapeFor(me.docId));
        return root;
    },

    buildTree: function () {
        let me = this;
        let I = PVE.meta.Icons;
        // Permissions reach guest documents only, so the Access
        // column is empty on the datacenter tab by construction (DESIGN §4).
        let scopes = me.applicablePermissions();

        let findings = me.findingsFor();
        // What is staged, by path, so a changed row can show `stored -> pending`.
        let staged = Object.create(null);
        me.pending.edits.forEach((e) => (staged[e.path] = e.op));
        let storedDoc = me.dataOf(me.docId);

        // Which *member* of a staged list actually differs. The edit is one write of
        // the whole list -- members are not addressable (§2) -- but wearing the mark on
        // the list said "all of this changed" when one entry did.
        let memberChanged = function (listPath, index, item) {
            let before = PVE.meta.Utils.valueAt(storedDoc, listPath);
            if (!Array.isArray(before) || index >= before.length) {
                return true; // appended
            }
            return !PVE.meta.Utils.sameValue(before[index], item);
        };
        let storedMember = function (listPath, index) {
            let before = PVE.meta.Utils.valueAt(storedDoc, listPath);
            if (!Array.isArray(before) || index >= before.length) {
                return '';
            }
            let v = before[index];
            return v !== null && typeof v === 'object'
                ? PVE.meta.Utils.itemSummary(v)
                : PVE.meta.Utils.displayValue(v, PVE.meta.Utils.kindOf(v));
        };
        // What each branch has to answer for: schema findings beneath it, and staged
        // edits beneath it. Both are invisible once the branch is collapsed.
        let below = PVE.meta.Utils.rollUp(findings);
        let stagedBelow = PVE.meta.Utils.rollUp(
            me.pending.edits.reduce(function (acc, e) {
                acc[e.path] = e.op === 'delete' ? gettext('removed') : gettext('changed');
                return acc;
            }, Object.create(null)),
        );

        let toNodes = function (entry, docId) {
            return Object.keys(entry.children)
                .sort()
                .map(function (key) {
                    let c = entry.children[key];
                    let kind = c.kind || 'string';
                    if (c.defaultValue !== undefined) {
                        me.hasDefaults = true;
                    }
                    let access = me.accessFor(c.path, scopes);
                    let node = {
                        key: key,
                        text: key,
                        docId: docId,
                        path: c.path,
                        kind: kind,
                        present: !!c.present,
                        description: c.description || '',
                        grammarDescription: c.grammarDescription || '',
                        defaultValue: c.defaultValue,
                        enumValues: c.enumValues,
                        minimum: c.minimum,
                        maximum: c.maximum,
                        format: c.format,
                        multiline: c.multiline,
                        rawValue: c.value,
                        arrayIndex: c.arrayIndex,
                        storedIndex: c.storedIndex,
                        addressable: c.addressable !== false,
                        rawItem: c.rawItem,
                        valueText: c.present ? PVE.meta.Utils.displayValue(c.value, kind) : '',
                        accessList: access,
                        accessText: me.accessSummary(access),
                        finding: findings[c.path] || '',
                        pending: (function () {
                            if (c.pendingDelete) {
                                return 'delete';
                            }
                            if (!staged[c.path]) {
                                return '';
                            }
                            // A member carries the mark when it is the one that
                            // changed; the list itself carries only the dot that says
                            // something below it did.
                            if (c.arrayIndex !== undefined && c.arrayIndex !== null) {
                                return memberChanged(c.path, c.arrayIndex, c.rawItem) ? 'set' : '';
                            }
                            return kind === 'array' ? '' : staged[c.path];
                        })(),
                        belowCount: (below[c.path] || {}).count || 0,
                        belowText: ((below[c.path] || {}).messages || []).join('\n'),
                        stagedBelow:
                            (stagedBelow[c.path] || {}).count ||
                            (kind === 'array' && staged[c.path] ? 1 : 0),
                        // Rendered with the row's own kind, not one inferred from the
                        // raw value: the API returns booleans as 1/0 (DESIGN §7), so
                        // inferring would print a struck-through "1" under a row whose
                        // stored value reads "Yes".
                        storedText: (function () {
                            if (c.arrayIndex !== undefined && c.arrayIndex !== null) {
                                return storedMember(c.path, c.arrayIndex);
                            }
                            if (c.pendingDelete && c.value !== undefined) {
                                return c.value; // a ghost carries what was there
                            }
                            let v = PVE.meta.Utils.valueAt(storedDoc, c.path);
                            return v === undefined ? '' : PVE.meta.Utils.displayValue(v, kind);
                        })(),
                        editable: me.editableFor(c.path),
                        leaf: kind !== 'map' && !Object.keys(c.children).length,
                    };
                    if (kind === 'map' || Object.keys(c.children).length) {
                        node.children = toNodes(c, docId);
                        node.expanded = true;
                        node.iconCls = I.mapExpanded;
                        node.expandedCls = I.mapExpanded;
                    } else {
                        node.iconCls = I.leaf;
                    }
                    return node;
                });
        };

        // Does anything in this document declare a default? If not, "Set to default"
        // is furniture -- a permission file can never have one.
        me.hasDefaults = false;
        let children = toNodes(me.documentEntries(), me.docId);

        // Reloading (including from the version poll) must not fold the tree up.
        // Keyed by document *and* path, even though one panel shows one document: a
        // window opened on a prefix file and the tab behind it are two panels with
        // their own stores, and a key that named only the path would be the same
        // string in both.
        let key = (n) => (n.data.docId || '') + '\u0000' + n.data.path + '\u0000' + (n.data.key || '');
        let expanded = Object.create(null);
        let seen = false;
        me.store.getRoot().cascadeBy(function (n) {
            if (n.data.text && !n.isLeaf()) {
                seen = true;
                if (n.isExpanded()) {
                    expanded[key(n)] = true;
                }
            }
        });
        me.store.setRoot({ expanded: true, children: children });
        if (seen) {
            me.store.getRoot().cascadeBy(function (n) {
                if (n.data.text && !n.isLeaf() && !expanded[key(n)]) {
                    n.collapse();
                    n.set('iconCls', PVE.meta.Icons.map);
                }
            });
        }
    },

    // --- editing ------------------------------------------------------------

    // Opens one of this panel's modal editor windows (Edit Value, Add Key, Add Rule,
    // Declare Key) and wires the one thing all seven call sites did by hand: `editing`
    // goes true so a reload or the version poll cannot pull the document out from
    // under an open window, `on`/`handler` is the window's one result event, and
    // `editing` goes false again on `destroy` -- whether the window committed or was
    // cancelled. A copy of this that forgot the `destroy` listener would leave
    // `editing` stuck true and quietly stop this panel from ever reloading again.
    openEditor: function (xtype, cfg, on, handler) {
        let me = this;
        me.editing = true;
        let win = Ext.create(xtype, cfg);
        win.on(on, handler);
        win.on('destroy', function () {
            me.editing = false;
        });
        win.show();
        return win;
    },

    editRow: function (rec) {
        let me = this;
        if (!rec || !rec.data.editable) {
            return;
        }
        if (rec.data.arrayIndex !== undefined && rec.data.arrayIndex !== null) {
            me.editListMember(rec);
            return;
        }
        // A value with structure inside it is edited as text, wherever the request came
        // from (button, double-click, Enter). Before this, all three simply did nothing
        // on a map row.
        // A declaration in a prefix file is edited by the form that made it, not as
        // text: it is the same thing Declare Key writes, so it is the same dialog
        // prefilled. A row of YAML in a Monaco window is a worse way to change
        // `type` than the dropdown that knows the types.
        if (me.editDeclaration(rec)) {
            return;
        }
        if (PVE.meta.Utils.editorKind(rec.data) === 'text') {
            me.editAsText(rec);
            return;
        }
        me.openEditor('PVE.meta.EditValueWindow', { rec: rec }, 'setvalue', (value) =>
            me.stage(rec.data.path, 'set', value),
        );
    },

    addKey: function (docId, parentPath) {
        let me = this;
        me.openEditor(
            'PVE.meta.AddKeyWindow',
            { parentPath: parentPath || '' },
            'addkey',
            (path, value) => me.stage(path, 'set', value),
        );
    },

    // Applies everything staged, as ONE write.
    //
    // That is the whole point: the states in between are the ones the server refuses
    // (a selector with both `all` and `tag`, or neither), so they must never reach it.
    // The write is a `replace` at the narrowest view covering every staged path, with
    // the planned subtree as its content -- for a single row edit that is exactly a
    // one-key write.
    //
    // Apply applies: it stops for the diff only when the planned document would not
    // match the schema (see `confirmAndApply`, which says why there is no `dry_run`
    // pass).
    applyPending: function () {
        let me = this;
        if (!me.isDirty()) {
            return;
        }
        me.confirmAndApply();
    },

    // The dry run, the diff and the write.
    confirmAndApply: function () {
        let me = this;
        let view = me.pending.writeView();
        let planned = me.plannedData();
        let subtree = view === '' ? planned : PVE.meta.Utils.valueAt(planned, view);
        if (subtree === undefined) {
            subtree = {};
        }
        let params = {
            view: view || undefined,
            mode: 'replace',
            data: Ext.encode(subtree),
            digest: me.digestOf(me.docId),
        };
        let stored = view === '' ? me.dataOf(me.docId) : PVE.meta.Utils.valueAt(me.dataOf(me.docId), view);

        // One staged delete is a DELETE, not a replace of its parent.
        //
        // `writeView` steps up a level for a delete, because you cannot remove a key by
        // replacing it -- and for a *top-level* key that step lands on the document
        // root. A root replace is not required for permission reasons -- the server
        // authorizes a write by what it changes -- but the narrower `DELETE
        // ?view=traefik` stays anyway, because it is the smaller write: it names one
        // subtree instead of the whole document, so it collides with less, and it does
        // not require the caller to be able to send back every key it did not touch.
        let onlyDelete =
            me.pending.length === 1 && me.pending.edits[0].op === 'delete' && me.pending.edits[0].path;
        // `force` is what the warned dialog's Apply sends: the "Save anyway" tick.
        // A prefix with `enforce: true` refuses the write without it (DESIGN §7).
        let write = function (force) {
            if (onlyDelete) {
                let q = Ext.Object.toQueryString({
                    view: me.pending.edits[0].path,
                    digest: me.digestOf(me.docId),
                });
                me.submit(
                    { url: me.urlFor(me.docId) + '?' + q, method: 'DELETE' },
                    function () {
                        me.pending = PVE.meta.EditSet.empty();
                    },
                );
                return;
            }
            if (force) {
                params.force = 1;
            }
            me.write(me.docId, params, function () {
                me.pending = PVE.meta.EditSet.empty();
            });
        };

        // Apply applies. It stops to show the diff only when the document would not
        // match the schema, which is the one case where seeing it changes what you
        // decide -- and the tick is what makes storing it anyway a deliberate act
        // rather than a dialog reflex. Otherwise there is nothing to decide: the diff
        // is a button of its own now, for whenever you want to look first.
        //
        // There is deliberately no `dry_run` pass any more. It existed to turn a
        // server refusal into a banner with a "Save anyway" tick -- but a refusal is
        // not advisory: the server refuses the real write for the same reason, tick or
        // no tick. Showing it as an error is honest; showing it as something you can
        // override is not, and it cost every Apply a second request.
        // Unreachable while the Tree card is disabled, and here anyway: the rule is
        // that a document which does not parse is repaired whole, and the planned
        // document the tree would send is built on an empty one.
        if (me.docParseError) {
            Ext.Msg.alert(
                gettext('Error'),
                Ext.htmlEncode(
                    Ext.String.format(
                        gettext('This document is not valid YAML; repair it as text: {0}'),
                        me.docParseError,
                    ),
                ),
            );
            return;
        }

        let warnings = me.applyFindingsFor(planned);
        if (!warnings.length) {
            write();
            return;
        }
        PVE.meta.Monaco.confirmDiff({
            title: Ext.String.format(gettext('Apply: {0}'), me.docId),
            // Documents, not text: `confirmDiff` renders them once it has the YAML
            // codec, so this cannot run before it is loaded.
            originalValue: stored === undefined ? {} : stored,
            modifiedValue: subtree,
            warnings: warnings,
            apply: () => write(true),
        });
    },

    // The schema findings a planned document would *introduce*, as plain messages --
    // what Apply warns about. The same Shape the tree markers use, through the same
    // `shapeFor`, so the banner and the amber rows can never disagree about what is
    // wrong; they differ only in what they are for. The markers show everything
    // wrong with the document, which is honest. The banner asks you to vouch for
    // what this edit did, which is the only thing you can answer for.
    applyFindingsFor: function (planned) {
        let me = this;
        let shape = me.shapeFor(me.docId);
        if (!shape.hasSchema()) {
            return [];
        }
        let stored = me.dataOf(me.docId);
        return PVE.meta.Shape.introduced(
            shape.findings(stored),
            shape.findings(planned),
            PVE.meta.EditSet.changedPaths(stored, planned),
        ).map(PVE.meta.Utils.findingText);
    },

    // Stage a declared default, because someone asked for it. Never on its own: an
    // unset key stays unset, and a set one keeps whatever it was set to, until this
    // click (DESIGN §12).
    //
    // A row already at its default is refused here as well as disabled in the
    // toolbar, so the two cannot drift apart -- the button's state is a hint, this is
    // the rule.
    setToDefault: function (rec) {
        let me = this;
        let U = PVE.meta.Utils;
        if (!rec || !rec.data.docId || rec.data.defaultValue === undefined) {
            return;
        }
        if (rec.data.present && U.sameValue(rec.data.rawValue, rec.data.defaultValue)) {
            return;
        }
        me.stage(rec.data.path, 'set', rec.data.defaultValue);
    },

    // Appending to a list. A rule list gets the rule form, because that is the list
    // worth having a form for; anything else asks for a value, since a member of a
    // list has no name to give it.
    addListMember: function (path) {
        let me = this;
        let list = me.listAt(path);
        if (path === 'rules' && me.docKind(me.docId) === 'permission') {
            me.addRule();
            return;
        }
        me.openEditor('PVE.meta.AddKeyWindow', { parentPath: path, list: true }, 'addkey', function (
            _path,
            value,
        ) {
            me.stage(path, 'set', list.concat([value]));
        });
    },

    // Editing one member of a list. Three cases, in the order they are worth having:
    // a permission rule gets its own form (it is the list anyone actually edits), a
    // scalar gets the ordinary value editor, and anything else with structure gets
    // the text editor on the list it is in -- which is where it was before lists had
    // rows at all, so nothing is lost.
    editListMember: function (rec) {
        let me = this;
        let d = rec.data;
        let item = d.rawItem;
        let isRule =
            item &&
            typeof item === 'object' &&
            Object.prototype.hasOwnProperty.call(item, 'prefix') &&
            Object.prototype.hasOwnProperty.call(item, 'mode');
        if (isRule) {
            me.openEditor(
                'PVE.meta.AddRuleWindow',
                { title: gettext('Edit Rule'), prefixes: me.prefixes, rule: item },
                'addrule',
                function (rules) {
                    // The form appends to what it was given; for an edit it was given
                    // nothing, so the one rule it produced replaces this member.
                    me.stageListMember(d.path, d.arrayIndex, rules[rules.length - 1]);
                },
            );
            return;
        }
        if (item !== null && typeof item === 'object') {
            me.editAsText(rec);
            return;
        }
        me.openEditor('PVE.meta.EditValueWindow', { rec: rec }, 'setvalue', (value) =>
            me.stageListMember(d.path, d.arrayIndex, value),
        );
    },

    // Append one rule to this permission file. The prefix combobox is filled from
    // the declared prefixes, which is the list an administrator is choosing from
    // nine times in ten -- but it stays editable, because a rule and a prefix
    // definition are independent files and neither waits for the other.
    addRule: function () {
        let me = this;
        if (me.docKind(me.docId) !== 'permission') {
            return;
        }
        me.openEditor(
            'PVE.meta.AddRuleWindow',
            { prefixes: me.prefixes, existing: me.plannedData().rules },
            'addrule',
            (rules) => me.stage('rules', 'set', rules),
        );
    },

    // Declare one key of the selected prefix's schema: a view PUT into
    // `schema.properties.<key>` of that prefix document, with its digest. The
    // window builds the declaration; this only decides where it goes.
    // Opens the declaration form on an existing declaration, prefilled. Returns
    // whether it did, so `editRow` can fall through to its other editors.
    editDeclaration: function (rec) {
        let me = this;
        let docId = me.docOf(rec);
        if (me.docKind(docId) !== 'prefix') {
            return false;
        }
        let key = PVE.meta.Utils.declaredKeyAt(rec.data.path);
        if (!key) {
            return false;
        }
        let current = PVE.meta.Utils.valueAt(me.plannedData(docId), rec.data.path);
        if (current === null || typeof current !== 'object' || Array.isArray(current)) {
            return false; // not a declaration after all; let the text editor have it
        }
        me.openEditor(
            'PVE.meta.DeclareKeyWindow',
            { prefix: me.docTitle(docId), declaration: current, keyName: key },
            'declarekey',
            // Laid over the declaration as it is: the form rewrites the fields it
            // asks about and keeps the rest -- a map's nested `properties` above all.
            (_key, schema) => me.stage(rec.data.path, 'set', PVE.meta.DeclareKeyWindow.merged(current, schema)),
        );
        return true;
    },

    declareKey: function (rec) {
        let me = this;
        if (!rec || !rec.data.docId || me.docKind(rec.data.docId) !== 'prefix') {
            return;
        }
        let docId = rec.data.docId;
        me.openEditor(
            'PVE.meta.DeclareKeyWindow',
            { prefix: me.docTitle(docId) },
            'declarekey',
            (key, schema) => me.stage('schema.properties.' + key, 'set', schema),
        );
    },

    // Staged like every other edit, so no confirm: nothing has happened yet, the row
    // shows struck through, and Revert or Apply is the decision.
    removeKey: function (rec) {
        let me = this;
        if (!rec || !rec.data.path) {
            return;
        }
        if (rec.data.arrayIndex !== undefined && rec.data.arrayIndex !== null) {
            me.stageListMember(rec.data.path, rec.data.arrayIndex, undefined);
            return;
        }
        me.stage(rec.data.path, 'delete');
    },

    // "Edit selection as text": Monaco on the selected subtree, in its own window.
    editSelectionAsText: function () {
        let me = this;
        me.editAsText(me.getSelection()[0]);
    },

    // Monaco on one subtree. Which subtree depends on the row: a value that *has* a
    // subtree (a map, an array of maps) is edited as itself, and a scalar is edited
    // through the map that contains it -- selecting `port` and asking for text is a
    // request to see it in context, not to edit `8080` in an editor with a gutter.
    editAsText: function (rec) {
        let me = this;
        if (!rec) {
            return;
        }
        let view =
            PVE.meta.Utils.editorKind(rec.data) === 'text' ? rec.data.path : me.parentPath(rec);
        let docId = me.docOf(rec);

        // Rendered from the *planned* document, not fetched. This window used to read
        // its subtree back from the server, which meant it showed the stored document
        // and silently ignored everything staged -- so you could edit a row, open its
        // subtree as text, and be looking at the value you had just replaced.
        //
        // There is nothing to fetch any more: the editor dumps with the same codec the
        // store writes with (decision 011), so the text here is the text the file would
        // hold, without asking.
        let planned = me.plannedData(docId);
        let subtree = view === '' ? planned : PVE.meta.Utils.valueAt(planned, view);
        me.textWindow = Ext.create('PVE.meta.TextWindow', {
            view: view,
            docId: docId,
            text: PVE.meta.Codec.dump(subtree === undefined ? {} : subtree, 'yaml'),
            tree: me,
        });
        // No reload on close: nothing was written. Reloading would also have asked
        // whether to discard the staged edits this window has just added to.
        me.textWindow.on('destroy', () => (me.textWindow = null));
        me.textWindow.show();
    },
}, PVE.meta.Doc, PVE.meta.TextCard));

