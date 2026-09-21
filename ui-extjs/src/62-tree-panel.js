// ---------------------------------------------------------------------------
// The panel: a card layout over the tree and the full-document text editor.
// ---------------------------------------------------------------------------

// PVE.meta.compose, not Ext's `mixins`: the offline smoke harness stubs Ext.define
// to read members straight off the config object, so they must be on it before
// Ext.define ever sees it.
Ext.define('PVE.meta.TreePanel', PVE.meta.compose({
    extend: 'Ext.panel.Panel',
    xtype: 'pveMetaTreePanel',

    layout: 'card',
    border: false,

    // vmid/node/type/dc arrive as config properties from pve-ext's page loader;
    // pveSelNode is the fallback for anything that adds this panel the PVE way.
    pveSelNode: undefined,
    // Set when this panel is inside a window: its footer then carries the way out.
    onClose: undefined,

    initComponent: function () {
        let me = this;
        let sel = (me.pveSelNode && me.pveSelNode.data) || {};

        me.vmid = me.vmid || sel.vmid;
        // This panel is ONE document's editor, named by `docId`: a guest's or a
        // prefix file's -- both documents (DESIGN §3), so the same tree, markers,
        // text editor and diff serve both. Rows carry their document's id too,
        // since that is what every write threads through.
        me.docId = me.docId || String(me.vmid);
        // A registry document: no tags to resolve, the meta-schema describes it.
        me.registryDoc = me.docKind(me.docId) !== 'guest';
        // Start fetching the core now: the registry grids open their dialogs before
        // the first document read, and a validator with no core checks nothing.
        // `loadDocument` awaits the same promise and reports its own failure.
        PVE.meta.Core.load().catch(Ext.emptyFn);
        me.docState = Object.create(null); // id -> { digest, data }
        me.schemas = {}; // GET /meta/schemas, the shape of a registry document
        me.access = { read: 1, write: 0 };
        me.prefixes = [];
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
            // The load first: asynchronous, so a throw in the button sync cannot
            // leave it un-started.
            me.reload();
            me.syncButtons();
        });
        me.on('destroy', function () {
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
                // on a guest tab it is not disallowed but a missing concept.
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
            // Only shown when the caller is restricted (DESIGN §8).
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
                applyDisabled: true, // there is no buffer yet; `syncFooter` decides after
                diff: () => me.showDiff(),
                format: () => me.formatText(),
                apply: () => me.applyText(),
                secondary: () => me.footerSecondary(),
                secondaryText: me.onClose ? gettext('Close') : gettext('Revert'),
            }),
        );
    },

    // Revert in Text, and in a window the way out. In the tree there is nothing
    // unwritten to revert: an edit is a write (DESIGN §6).
    footerSecondary: function () {
        let me = this;
        if (me.mode === 'text') {
            me.discardText();
            return;
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
        // The grammar's description is the tooltip of every cell in the row (DESIGN §8),
        // unless the row does not match its schema, which is more urgent and goes first.
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
                    // The *branch's* marker (something beneath this row, invisible once
                    // collapsed) sits in the Key column, since Value is empty for a map.
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
                    if (d.kind === 'map') {
                        return '';
                    }
                    // Show the first line and how much more there is; the row's own
                    // `valueText` is untouched, so the editor still opens on all of it.
                    let shown = d.present
                        ? Ext.htmlEncode(PVE.meta.Utils.previewText(value))
                        : '<span class="faded">' + unsetText(rec) + '</span>';
                    if (d.finding) {
                        // Advisory: the row stays editable, the message is in the
                        // tooltip. `warning` is proxmoxlib's own class.
                        shown =
                            '<i class="fa fa-exclamation-triangle warning"></i> ' +
                            '<span class="warning">' + shown + '</span>';
                    }
                    return shown;
                },
            },
            {
                // Guest documents only: a registry document's top-level keys are fixed
                // (`deny_unknown_fields`), so no comment key can describe one (DESIGN §3).
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
        ];
    },

    // Every path in `docId` whose value does not match its schema, by path -- shown
    // on the rows themselves, from the same Shape the text editor's squiggles use.
    // Advisory, always: an enforcing prefix is the server's 422, not a pre-write
    // step (DESIGN §4).
    findingsFor: function () {
        let out = Object.create(null);
        let shape = this.shapeFor(this.docId);
        if (!shape.hasSchema()) {
            return out;
        }
        shape.findings(this.dataOf(this.docId)).forEach(function (f) {
            out[f.path] = f.msg;
        });
        return out;
    },

    // --- writes --------------------------------------------------------------

    // Sends one edit as one write, and reloads. Every editor here goes through this.
    sendEdit: function (edit, force) {
        let me = this;
        me.submit(
            me.writeFor(me.docId, edit, me.digestOf(me.docId), force),
            undefined,
            force ? undefined : () => me.sendEdit(edit, true),
        );
    },

    // The list at `path`, as stored.
    listAt: function (path) {
        let v = PVE.meta.Utils.valueAt(this.dataOf(this.docId), path);
        return Array.isArray(v) ? v.slice() : [];
    },

    // Writes the list at `path` with member `index` replaced, or dropped when `value`
    // is undefined. One `replace` of the whole list: a view addresses through maps only.
    writeListMember: function (path, index, value) {
        let list = this.listAt(path);
        if (index < 0 || index >= list.length) {
            return;
        }
        if (value === undefined) {
            list.splice(index, 1);
        } else {
            list[index] = value;
        }
        this.sendEdit({ path: path, op: 'set', value: list });
    },

    // A whole subtree, as edited somewhere else: one `replace` at that view, which
    // says everything about what is inside it. What "Edit selection as text" ends
    // with.
    writeSubtree: function (view, value) {
        this.sendEdit({ path: view || '', op: 'set', value: value });
    },

    // The document a row belongs to; the panel's default for anything with no row.
    docOf: function (rec) {
        return (rec && rec.data && rec.data.docId) || this.docId;
    },

    // What describes this document's shape (`PVE.meta.Shape`): every caller that
    // needs one goes through here, so the row builder, markers and text-editor
    // squiggles cannot disagree. One Shape per document, kept as long as its
    // inputs (`shapeInputs`) are the ones currently in memory -- checked by
    // identity each call, rather than invalidated by hand on every load.
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

    // What a document's Shape is built from. A guest's: the prefix listing, already
    // resolved for it by the server (DESIGN §3). A registry file's: the
    // meta-schema for its kind.
    shapeInputs: function (id) {
        let me = this;
        let kind = me.docKind(id);
        if (kind === 'guest') {
            return [me.prefixes];
        }
        if (kind === 'prefix') {
            return [(me.schemas || {})[kind]];
        }
        return [];
    },

    buildShape: function (id, inputs) {
        let kind = this.docKind(id);
        if (kind === 'guest') {
            return PVE.meta.Shape.of(inputs[0]);
        }
        if (kind === 'prefix') {
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

    // Where a new key goes: into the selected map, beside a selected leaf, or --
    // nothing selected -- the document root. A list is the exception: `Add` on one,
    // or on a member of one, appends to it instead, since a list has no keys to add.
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
        // On a prefix definition Add is disabled at its root: the document's own
        // top-level keys are fixed, even though `schema` holds whatever you declare.
        let kind = me.docKind(me.docId);
        let fixedRoot = kind === 'prefix' && target && target.path === '';
        set('addBtn', text || !target || fixedRoot || !me.editableFor());
        let row = d;
        set('editBtn', text || !row || !row.editable);
        set('removeBtn', text || !row || !row.present || !row.editable);
        set('textSelBtn', text || !row);
        set('reloadBtn', text);
        me.syncFooter();
        let dflt = me.down('#defaultBtn');
        if (dflt) {
            // Hidden when nothing in the document declares a default (a per-document
            // fact); disabled per row, never hidden per row, so buttons beside it do
            // not shift under the pointer as the selection changes. Offered on any
            // row with a default it is not already at -- not just unset ones, since
            // the value most worth resetting is the one already there and wrong.
            let offers =
                !!row &&
                row.defaultValue !== undefined &&
                !(row.present && U.sameValue(row.rawValue, row.defaultValue));
            dflt.setHidden(!me.hasDefaults);
            dflt.setDisabled(text || !offers || !row.editable);
        }
        me.syncAccessLabel();
        let declare = me.down('#declareBtn');
        if (declare) {
            // Hidden by the document (its kind cannot change mid-tree), disabled by the row.
            declare.setHidden(me.docKind(me.docId) !== 'prefix');
            declare.setDisabled(text || !row || !row.editable);
        }
    },

    // One place decides what the footer says, in either mode. Everything on it but
    // the mode switch belongs to the Text card now: in the tree an edit is its own
    // write, so there is nothing to apply, revert or diff.
    syncFooter: function () {
        let me = this;
        let textMode = me.mode === 'text';
        ['textLangBtn', 'metaFormat', 'metaDiff', 'metaApply'].forEach(function (id) {
            let c = me.down('#' + id);
            if (c) {
                c.setHidden(!textMode);
            }
        });
        let apply = me.down('#metaApply');
        if (apply) {
            // The buffer is the edit, and Apply is offered whenever the document is
            // writable (DESIGN §4); the diff is what decides if it is worth it.
            apply.setDisabled(!me.access.write);
        }
        let second = me.down('#metaSecondary');
        if (second) {
            // Revert in Text; in a window, the way out. A tab's tree has neither.
            second.setHidden(!textMode && !me.onClose);
            second.setIconCls(textMode ? 'fa fa-undo' : 'fa fa-times');
            second.setText(textMode ? gettext('Revert') : gettext('Close'));
        }
    },

    // The label is a read-only notice, so it says nothing at all for a caller who
    // may write (DESIGN §4).
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
            modeBtn.items.getAt(1).setDisabled(!me.access.read);
        }
        me.syncFooter();
        let label = me.down('#accessText');
        if (!label) {
            return;
        }
        if (me.access.write) {
            label.setVisible(false);
            return;
        }
        label.setText(gettext('Read-only'));
        label.setVisible(true);
    },

    // --- rows ---------------------------------------------------------------

    // Rows are editable iff the document is: PVE's ACLs are the whole access model
    // now (DESIGN §4), so nothing is computed per path.
    editableFor: function () {
        return !!(this.access && this.access.write);
    },

    // The document and the grammars are two sources for the same rows, so merge them
    // as plain entries first — much less fiddly than merging Ext node configs.
    entry: function (parent, key, path) {
        parent.children[key] = parent.children[key] || {
            key: key,
            path: path,
            // Object.create(null): a document key is unrestricted (DESIGN §2), so a
            // plain `{}` would let `constructor` or `hasOwnProperty` resolve through
            // the prototype chain instead of being treated as absent.
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
            // A list is a container, like a map: its members are rows you can select
            // and act on. They are **not addressable** (a view addresses through maps
            // only, DESIGN §2) so they carry their index instead, and everything that
            // acts on one rewrites the list it is in.
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

    // The rows a document's shape declares, on top of what `addData` found: every
    // reaching prefix gets a row, and every schema-declared path gets its type,
    // default, enum, range, format and description from the core's already-pruned
    // schema index (schemas shadow, never merge, DESIGN §3).
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
            // `||`, not `=`: `addData` ran first, so a value already stored here keeps
            // its actual kind; only an absent prefix falls back to a map.
            entry.kind = entry.kind || 'map';
            entry.grammarDescription = entry.grammarDescription || d.description;
        });
        // Two passes: a hidden declaration decorates a row that exists, never
        // creates one, so a schema nobody has set does not bury real data under
        // greyed rows. Rows first, then decoration, so a shown key inside a hidden
        // subtree still creates the rows above it.
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
            // description is the tooltip (DESIGN §8), so they are two fields.
            child.grammarDescription = child.grammarDescription || ps.description;
            if (ps.type === 'object') {
                child.kind = 'map';
                return;
            }
            // A declared type wins over the type inferred from the stored value.
            child.kind = ps.type ? me.schemaKind(ps) : child.kind || 'string';
            // First writer wins: the index lists each path once, under the prefix
            // that governs it, so these six fields cannot disagree.
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
            // One of two dialect extensions (`hidden` the other): an editor hint the
            // server neither reads nor validates, like `format` (DESIGN §5).
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
    // declares (DESIGN §8). Two sources, one set of entries.
    documentEntries: function () {
        let me = this;
        let root = { key: '', path: '', children: Object.create(null), present: true, kind: 'map' };
        me.addData(root, me.dataOf(me.docId));
        me.addShape(root, me.shapeFor(me.docId));
        return root;
    },

    buildTree: function () {
        let me = this;
        let I = PVE.meta.Icons;
        let findings = me.findingsFor();
        // What each branch has to answer for: the schema findings beneath it, which
        // are invisible once the branch is collapsed.
        let below = PVE.meta.Utils.rollUp(findings);

        let toNodes = function (entry, docId) {
            return Object.keys(entry.children)
                .sort()
                .map(function (key) {
                    let c = entry.children[key];
                    let kind = c.kind || 'string';
                    if (c.defaultValue !== undefined) {
                        me.hasDefaults = true;
                    }
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
                        addressable: c.addressable !== false,
                        rawItem: c.rawItem,
                        valueText: c.present ? PVE.meta.Utils.displayValue(c.value, kind) : '',
                        finding: findings[c.path] || '',
                        belowCount: (below[c.path] || {}).count || 0,
                        belowText: ((below[c.path] || {}).messages || []).join('\n'),
                        editable: me.editableFor(),
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
        // is furniture.
        me.hasDefaults = false;
        let children = toNodes(me.documentEntries(), me.docId);

        // Reloading must not fold the tree up. Keyed by document and path, since two
        // panels (a window and the tab behind it) can each hold a different document.
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

    // Opens one of this panel's modal editor windows. `editing` goes true so a
    // reload cannot pull the document out from under it, and false again on
    // `destroy`, committed or cancelled.
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
        // A value with structure inside it is edited as text (button, double-click or
        // Enter alike). A declaration is a map like any other, edited as YAML on
        // `schema.properties.<key>` (DESIGN §3).
        if (PVE.meta.Utils.editorKind(rec.data) === 'text') {
            me.editAsText(rec);
            return;
        }
        me.openEditor('PVE.meta.EditValueWindow', { rec: rec }, 'setvalue', (value) =>
            me.sendEdit({ path: rec.data.path, op: 'set', value: value }),
        );
    },

    // A new key is a `replace` at its own path: `view::replace` creates the maps
    // above it, so a dotted path into a subtree nobody has written yet needs no
    // second write to make room for it.
    addKey: function (docId, parentPath) {
        let me = this;
        me.openEditor(
            'PVE.meta.AddKeyWindow',
            { parentPath: parentPath || '' },
            'addkey',
            (path, value) => me.sendEdit({ path: path, op: 'set', value: value }),
        );
    },

    // Write a declared default, only on this click: an unset key stays unset
    // otherwise. Refused here, as well as disabled in the toolbar, if already at it.
    setToDefault: function (rec) {
        let me = this;
        let U = PVE.meta.Utils;
        if (!rec || !rec.data.docId || rec.data.defaultValue === undefined) {
            return;
        }
        if (rec.data.present && U.sameValue(rec.data.rawValue, rec.data.defaultValue)) {
            return;
        }
        me.sendEdit({ path: rec.data.path, op: 'set', value: rec.data.defaultValue });
    },

    // Appending to a list. A member of a list has no name to give it, so this asks
    // for a value.
    addListMember: function (path) {
        let me = this;
        let list = me.listAt(path);
        me.openEditor('PVE.meta.AddKeyWindow', { parentPath: path, list: true }, 'addkey', function (
            _path,
            value,
        ) {
            me.sendEdit({ path: path, op: 'set', value: list.concat([value]) });
        });
    },

    // Editing one member of a list: a scalar gets the ordinary value editor, and
    // anything else with structure gets the text editor on the list it is in --
    // which is where it was before lists had rows at all, so nothing is lost.
    editListMember: function (rec) {
        let me = this;
        let d = rec.data;
        let item = d.rawItem;
        if (item !== null && typeof item === 'object') {
            me.editAsText(rec);
            return;
        }
        me.openEditor('PVE.meta.EditValueWindow', { rec: rec }, 'setvalue', (value) =>
            me.writeListMember(d.path, d.arrayIndex, value),
        );
    },

    // "Declare Key": there is no form of its own any more -- `schema.properties` is
    // a map like any other, so this opens it as text (DESIGN §3). An empty map
    // gets a hint instead of a blank buffer, so the first key is not typed from
    // nothing.
    declareKey: function (rec) {
        let me = this;
        if (!rec || me.docKind(me.docId) !== 'prefix') {
            return;
        }
        let props = PVE.meta.Utils.valueAt(me.dataOf(me.docId), 'schema.properties');
        if (!props || !Object.keys(props).length) {
            props = { key_name: { type: 'string' } };
        }
        me.textWindow = Ext.create('PVE.meta.TextWindow', {
            view: 'schema.properties',
            text: PVE.meta.Codec.dump(props, 'yaml'),
            tree: me,
        });
        me.textWindow.on('destroy', () => (me.textWindow = null));
        me.textWindow.show();
    },

    // `DELETE ?view=<path>`, which takes the key's note with it (`view::remove`). A
    // member of a list has no path of its own, so removing one is a write of the list
    // without it.
    removeKey: function (rec) {
        let me = this;
        if (!rec || !rec.data.path) {
            return;
        }
        if (rec.data.arrayIndex !== undefined && rec.data.arrayIndex !== null) {
            me.writeListMember(rec.data.path, rec.data.arrayIndex, undefined);
            return;
        }
        me.sendEdit({ path: rec.data.path, op: 'delete' });
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

        // Rendered from the document this panel holds, not fetched: the editor dumps
        // with the same codec the store writes with (decision 011), so the text here
        // is the text the file holds, without asking for it again. `me.docId`, since
        // that is the document OK writes back to.
        let stored = me.dataOf(me.docId);
        let subtree = view === '' ? stored : PVE.meta.Utils.valueAt(stored, view);
        me.textWindow = Ext.create('PVE.meta.TextWindow', {
            view: view,
            text: PVE.meta.Codec.dump(subtree === undefined ? {} : subtree, 'yaml'),
            tree: me,
        });
        // No reload on close: the write, if there was one, reloads on its own.
        me.textWindow.on('destroy', () => (me.textWindow = null));
        me.textWindow.show();
    },
}, PVE.meta.Doc, PVE.meta.TextCard));

