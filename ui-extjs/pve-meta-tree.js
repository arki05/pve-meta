/*
 * pve-meta-tree.js — the native ExtJS implementation of the pve-meta editor.
 *
 * One Ext.tree.Panel with columns Key | Value | Owner over the document the caller
 * can see (DESIGN.md §5, §7, §8). Rows are the union of the keys present in the
 * document and the keys the applicable grammars declare (`GET /meta/operators`,
 * matched by scope prefix and selector against this guest); a declared-but-unset key
 * renders faded with its default and description, and "setting" it is just editing
 * its Value cell. Comment keys (`k__`, and the bare `__` for the map itself) are not
 * rows — they are the description/tooltip of the row they document. Arrays are one
 * text leaf. Owner is the registration whose scope covers the row.
 *
 * Editing is inline (Ext.grid.plugin.CellEditing), the editor chosen from the grammar
 * type and falling back to the value's own type; editability is per row from
 * `GET /meta/access`. A commit is one minimal write:
 *   PUT /meta/guests/{vmid}?view=<dotted.path>&mode=replace&data=<json>&digest=<d>
 * 409 (digest mismatch) reloads and reports the API's message verbatim. A 5 s poll of
 * `GET /meta/version` refreshes the tree when the content token changed — never while
 * a cell editor or the text window is open.
 *
 * Monaco has exactly two jobs: "Edit as Text" on the selected subtree (YAML, with a
 * presentation-only YAML/JSON toggle), and the diff that confirms that window's
 * Apply. Its AMD loader is fetched lazily on first use from
 * /pve2/js/pve-meta-ui/vs/loader.js (shipped by the pve-meta UI package); every
 * editor is disposed when its window closes.
 *
 * pve-ext's page loader loads this file and instantiates `pveMetaTreePanel` as the
 * tab (see README.md), so session, CSRF, dark theme and i18n all come from the PVE
 * UI — none of it is reimplemented here. Plain ES2017, no build step.
 */

Ext.ns('PVE.meta');

// ---------------------------------------------------------------------------
// Helpers: paths, the JSON data model, and YAML in and out.
// ---------------------------------------------------------------------------

PVE.meta.Utils = {
    // A key ending in `__` documents its sibling; a bare `__` documents the map.
    isComment: (key) => key.length >= 2 && key.slice(-2) === '__',
    commentTarget: (key) => key.slice(0, -2),
    joinPath: (prefix, key) => (prefix ? prefix + '.' + key : key),

    // A scope on prefix `p` covers the subtree `p` and the sibling comment key `p__`.
    // That is the only comment-key rule (DESIGN §3).
    covers: (p, path) => path === p || path === p + '__' || path.indexOf(p + '.') === 0,

    kindOf: function (value) {
        if (Ext.isArray(value)) {
            return 'array';
        } else if (value !== null && typeof value === 'object') {
            return 'map';
        } else if (typeof value === 'boolean') {
            return 'boolean';
        } else if (typeof value === 'number') {
            return 'number';
        }
        return 'string';
    },

    // What the Value column shows for a leaf. Arrays are one text leaf (DESIGN §8).
    displayValue: function (value, kind) {
        if (kind === 'map') {
            return '';
        } else if (kind === 'boolean') {
            // The API's JSON view returns YAML booleans as 1/0 (a Perl JSON artifact),
            // so render whatever arrived the way PVE renders every other boolean.
            return Proxmox.Utils.format_boolean(value);
        }
        return kind === 'string' ? String(value) : Ext.encode(value);
    },

    // The inverse, for a committed cell edit: field value -> the JSON value to send.
    parseValue: function (text, kind) {
        if (kind === 'boolean') {
            return text === true || text === 'true' || text === 1 || text === '1';
        } else if (kind === 'number') {
            let n = Number(text);
            if (text === '' || isNaN(n)) {
                throw new Error(gettext('Not a number') + ': ' + text);
            }
            return n;
        } else if (kind === 'array') {
            let t = String(text).trim();
            if (t.charAt(0) === '[') {
                return Ext.decode(t); // throws on malformed JSON, which is what we want
            }
            return t === '' ? [] : t.split(',').map((s) => s.trim());
        }
        return String(text);
    },

    // --- YAML, for the text window's presentation-only YAML/JSON toggle -----
    //
    // The store's own canonical dump is the shape this pair round-trips: block maps,
    // block sequences, plain and quoted scalars. Anything richer (anchors, block
    // scalars, multi-document, non-empty flow collections) throws rather than being
    // silently mangled — the server stays the authority on YAML, and Apply in YAML
    // mode sends the buffer untouched.

    yamlScalar: function (value) {
        if (typeof value !== 'string') {
            return Ext.encode(value);
        }
        let needsQuotes =
            value === '' ||
            /^\s|\s$|^[-?:,[\]{}#&*!|>'"%@`]|:\s|\s#|\n/.test(value) ||
            /^(true|false|null|yes|no|on|off|~)$/i.test(value) ||
            /^[-+]?[0-9.]+([eE][-+]?[0-9]+)?$/.test(value);
        if (!needsQuotes) {
            return value;
        }
        return '"' + value.replace(/\\/g, '\\\\').replace(/"/g, '\\"').replace(/\n/g, '\\n') + '"';
    },

    yamlDump: function (value, indent) {
        let U = PVE.meta.Utils;
        let pad = ' '.repeat((indent = indent || 0));
        let kind = U.kindOf(value);
        if (kind === 'map') {
            let keys = Object.keys(value);
            return keys.length
                ? keys
                      .map(function (k) {
                          let v = value[k];
                          let vk = U.kindOf(v);
                          let deep = (vk === 'map' && Object.keys(v).length) || (vk === 'array' && v.length);
                          if (!deep) {
                              return pad + U.yamlScalar(k) + ': ' + U.yamlDump(v, 0).trim() + '\n';
                          }
                          // Sequences sit at the key's own indentation, as the store dumps them.
                          return (
                              pad + U.yamlScalar(k) + ':\n' + U.yamlDump(v, vk === 'map' ? indent + 2 : indent)
                          );
                      })
                      .join('')
                : pad + '{}\n';
        } else if (kind === 'array') {
            return value.length
                ? value
                      .map(function (v) {
                          let vk = U.kindOf(v);
                          return vk === 'map' || vk === 'array'
                              ? pad + '-\n' + U.yamlDump(v, indent + 2)
                              : pad + '- ' + U.yamlDump(v, 0).trim() + '\n';
                      })
                      .join('')
                : pad + '[]\n';
        }
        return pad + U.yamlScalar(value) + '\n';
    },

    yamlPlain: function (text) {
        let t = text.trim();
        let q = t.charAt(0);
        if (q === '"' || q === "'") {
            if (t.length < 2 || t.charAt(t.length - 1) !== q) {
                throw new Error(gettext('unterminated quoted scalar') + ': ' + t);
            }
            let body = t.slice(1, -1);
            return q === '"'
                ? body.replace(/\\(.)/g, (m, c) => (c === 'n' ? '\n' : c === 't' ? '\t' : c))
                : body.replace(/''/g, "'");
        }
        if (t === '{}') {
            return {};
        } else if (t === '[]') {
            return [];
        } else if (/^[[{]/.test(t)) {
            throw new Error(gettext('flow collections are not supported') + ': ' + t);
        } else if (/^[&*|>]/.test(t)) {
            // anchors, aliases and block scalars: never guess at these
            throw new Error(gettext('unsupported YAML syntax') + ': ' + t);
        } else if (t === 'true' || t === 'false') {
            return t === 'true';
        } else if (/^[-+]?(\d+(\.\d*)?|\.\d+)([eE][-+]?\d+)?$/.test(t)) {
            return Number(t);
        } else if (/^(null|~)$/.test(t)) {
            throw new Error(gettext('the document model has no nulls'));
        }
        return t;
    },

    // The first content line at or after state.i, as { col, seq }.
    yamlPeek: function (lines, state) {
        for (let j = state.i; j < lines.length; j++) {
            if (!lines[j].trim() || /^\s*#/.test(lines[j])) {
                continue;
            }
            let col = lines[j].length - lines[j].replace(/^ +/, '').length;
            let body = lines[j].slice(col);
            return { col: col, seq: body === '-' || body.slice(0, 2) === '- ' };
        }
        return null;
    },

    // Indentation-driven parse of the lines at and below state.i that are indented at
    // least `indent` columns. Returns a map, an array or a scalar. `seqOnly` marks the
    // block as a sequence living at its parent key's own indentation — the shape the
    // store dumps — so it must stop at the first line that is not a sequence item.
    yamlBlock: function (lines, state, indent, seqOnly) {
        let U = PVE.meta.Utils;
        let result;
        while (state.i < lines.length) {
            let line = lines[state.i];
            if (!line.trim() || /^\s*#/.test(line)) {
                state.i++;
                continue;
            }
            if (/^ *\t/.test(line)) {
                throw new Error(gettext('tabs are not valid YAML indentation') + ' (line ' + (state.i + 1) + ')');
            }
            let col = line.length - line.replace(/^ +/, '').length;
            if (col < indent) {
                break;
            }
            let body = line.slice(col);
            let isItem = body.charAt(0) === '-' && (body.length === 1 || body.charAt(1) === ' ');
            if (seqOnly && !isItem && col === indent) {
                break;
            }
            state.i++;

            if (isItem) {
                result = result === undefined ? [] : result;
                if (!Ext.isArray(result)) {
                    throw new Error(gettext('sequence item inside a mapping') + ' (line ' + state.i + ')');
                }
                let rest = body.slice(1).trim();
                if (rest === '') {
                    result.push(U.yamlBlock(lines, state, col + 1));
                } else if (/^("(?:[^"\\]|\\.)*"|'(?:[^']|'')*'|[^\s:][^:]*?)\s*:(\s|$)/.test(rest)) {
                    lines[--state.i] = ' '.repeat(col + 2) + rest; // re-read it as a mapping
                    result.push(U.yamlBlock(lines, state, col + 2));
                } else {
                    result.push(U.yamlPlain(rest));
                }
                continue;
            }

            let m = body.match(/^("(?:[^"\\]|\\.)*"|'(?:[^']|'')*'|[^:]+?)\s*:(?:\s+(.*))?$/);
            if (!m) {
                if (result === undefined) {
                    return U.yamlPlain(body); // a bare scalar document
                }
                throw new Error(gettext('cannot parse YAML line') + ' ' + state.i + ': ' + line);
            }
            result = result === undefined ? {} : result;
            if (Ext.isArray(result)) {
                throw new Error(gettext('mapping key inside a sequence') + ' (line ' + state.i + ')');
            }
            let key = String(U.yamlPlain(m[1]));
            let inline = m[2] === undefined ? '' : m[2].trim();
            if (inline && inline.charAt(0) !== '"' && inline.charAt(0) !== "'") {
                inline = inline.replace(/\s+#.*$/, '').trim(); // a trailing comment
            }
            if (inline !== '') {
                result[key] = U.yamlPlain(inline);
                continue;
            }
            // A block sequence may sit at its key's own indentation, not below it.
            let next = U.yamlPeek(lines, state);
            let flat = !!next && next.col === col && next.seq;
            result[key] = U.yamlBlock(lines, state, flat ? col : col + 1, flat);
        }
        return result === undefined ? {} : result;
    },

    yamlParse: (text) => PVE.meta.Utils.yamlBlock(String(text).split('\n'), { i: 0 }, 0),

    errText: (err) => String((err && (err.message || err.msg)) || err),
};

// ---------------------------------------------------------------------------
// Monaco, loaded lazily on first use from the tree the pve-meta UI package ships.
// ---------------------------------------------------------------------------

PVE.meta.Monaco = {
    VS: '/pve2/js/pve-meta-ui/vs',
    promise: null,

    load: function () {
        let me = PVE.meta.Monaco;
        me.promise =
            me.promise ||
            new Promise(function (resolve, reject) {
                if (window.monaco && window.monaco.editor) {
                    resolve(window.monaco);
                    return;
                }
                // Absolute, because a language worker resolves its own scripts against
                // this and has no page URL to make a root-relative path absolute with.
                let vs = window.location.origin + me.VS;
                window.MonacoEnvironment = { baseUrl: vs };
                let script = document.createElement('script');
                script.src = vs + '/loader.js';
                script.onload = function () {
                    try {
                        window.require.config({ paths: { vs: vs } });
                        window.require(['vs/editor/editor.main'], () => resolve(window.monaco), reject);
                    } catch (err) {
                        reject(err);
                    }
                };
                script.onerror = () => reject(new Error('failed to load ' + script.src));
                document.head.appendChild(script);
            });
        return me.promise;
    },

    // Monaco does not inherit the page's CSS, so pick its built-in theme from the
    // cookie PVE's own theme picker writes (proxmoxlib's ThemeEditWindow); anything
    // else follows the browser preference, exactly like PVE's charts do.
    theme: function () {
        let cookie = '';
        try {
            cookie = Ext.util.Cookies.get('PVEThemeCookie') || '';
        } catch (_e) {
            cookie = '';
        }
        if (cookie === 'proxmox-dark') {
            return 'vs-dark';
        } else if (cookie === 'crisp') {
            return 'vs';
        }
        return window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches
            ? 'vs-dark'
            : 'vs';
    },

    dispose: function (editor) {
        if (!editor) {
            return;
        }
        let model = editor.getModel();
        if (model && model.original) {
            // A diff editor does not own its two models, so they have to be detached
            // before they are disposed - disposing them under a live DiffEditorWidget
            // is what "TextModel got disposed before ... model got reset" complains
            // about. A standalone editor created with a `value` owns its model and
            // disposes it itself.
            editor.setModel(null);
            model.original.dispose();
            model.modified.dispose();
        }
        editor.dispose();
    },
};

// ---------------------------------------------------------------------------
// The row model.
// ---------------------------------------------------------------------------

Ext.define('PVE.meta.TreeModel', {
    extend: 'Ext.data.TreeModel',
    fields: [
        { name: 'key', type: 'string' },
        { name: 'path', type: 'string' }, // dotted; this is the `view` of a write
        { name: 'valueText', type: 'string' },
        { name: 'ownerText', type: 'string' },
        { name: 'description', type: 'string' },
        { name: 'kind', type: 'string' }, // map | array | string | number | boolean
        { name: 'present', type: 'boolean' },
        { name: 'editable', type: 'boolean' },
        { name: 'defaultValue' },
        { name: 'enumValues' },
        { name: 'rawValue' },
    ],
});

// ---------------------------------------------------------------------------
// "Add Key" — its own window because the key is a path and the type picks the editor.
// ---------------------------------------------------------------------------

Ext.define('PVE.meta.AddKeyWindow', {
    extend: 'Ext.window.Window',
    xtype: 'pveMetaAddKeyWindow',

    title: gettext('Add Key'),
    modal: true,
    width: 480,
    layout: 'fit',
    parentPath: '', // dotted path of the map the key goes into ('' = the document root)

    initComponent: function () {
        let me = this;
        Ext.apply(me, {
            items: [
                {
                    xtype: 'form',
                    reference: 'form',
                    bodyPadding: 10,
                    border: false,
                    defaults: { anchor: '100%', labelWidth: 110 },
                    items: [
                        {
                            xtype: 'displayfield',
                            fieldLabel: gettext('Under'),
                            value: Ext.htmlEncode(me.parentPath || gettext('(document root)')),
                        },
                        {
                            xtype: 'textfield',
                            name: 'key',
                            allowBlank: false,
                            fieldLabel: gettext('Key'),
                            emptyText: gettext('key, or a dotted path'),
                        },
                        {
                            xtype: 'proxmoxKVComboBox',
                            name: 'kind',
                            fieldLabel: gettext('Type'),
                            value: 'string',
                            comboItems: [
                                ['string', gettext('String')],
                                ['number', gettext('Number')],
                                ['boolean', gettext('Boolean')],
                                ['array', gettext('Array')],
                                ['map', gettext('Map')],
                            ],
                            listeners: {
                                change: (f, v) => me.down('[name=value]').setDisabled(v === 'map'),
                            },
                        },
                        { xtype: 'textfield', name: 'value', fieldLabel: gettext('Value') },
                    ],
                },
            ],
            buttons: [
                { text: gettext('Add'), handler: () => me.submit() },
                { text: gettext('Cancel'), handler: () => me.close() },
            ],
        });
        me.callParent();
    },

    submit: function () {
        let me = this;
        let form = me.down('form').getForm();
        if (!form.isValid()) {
            return;
        }
        let v = form.getValues();
        let key = String(v.key).replace(/^\.+|\.+$/g, '');
        try {
            if (!key) {
                throw new Error(gettext('Key must not be empty'));
            }
            let value = v.kind === 'map' ? {} : PVE.meta.Utils.parseValue(v.value || '', v.kind);
            me.fireEvent('addkey', PVE.meta.Utils.joinPath(me.parentPath, key), value);
            me.close();
        } catch (err) {
            Ext.Msg.alert(gettext('Error'), Ext.htmlEncode(PVE.meta.Utils.errText(err)));
        }
    },
});

// ---------------------------------------------------------------------------
// "Edit as Text" — Monaco on one subtree, with a diff-confirmed Apply.
// ---------------------------------------------------------------------------

Ext.define('PVE.meta.TextWindow', {
    extend: 'Ext.window.Window',
    xtype: 'pveMetaTextWindow',

    modal: true,
    width: 900,
    height: 640,
    layout: 'fit',
    referenceHolder: true, // lookupReference('mount'/'langbtn') needs this
    lang: 'yaml',
    // configs: view (dotted path, '' = root), text (YAML), tree (the owning panel)

    initComponent: function () {
        let me = this;
        me.original = me.text || '';
        me.title = Ext.String.format(
            gettext('Edit as Text: {0}'),
            Ext.htmlEncode(me.view || gettext('(whole document)')),
        );

        Ext.apply(me, {
            items: [{ xtype: 'component', reference: 'mount', style: 'height:100%;width:100%' }],
            tbar: [
                {
                    xtype: 'segmentedbutton',
                    reference: 'langbtn',
                    value: 'yaml',
                    items: [
                        { text: 'YAML', value: 'yaml' },
                        { text: 'JSON', value: 'json' },
                    ],
                    listeners: { change: (btn, value) => me.switchLang(value) },
                },
            ],
            buttons: [
                { text: gettext('Apply'), handler: () => me.showDiff() },
                { text: gettext('Cancel'), handler: () => me.close() },
            ],
        });
        me.callParent();

        me.on('afterrender', function () {
            Proxmox.Utils.setErrorMask(me, true);
            PVE.meta.Monaco.load().then(
                function (monaco) {
                    Proxmox.Utils.setErrorMask(me, false);
                    me.editor = monaco.editor.create(me.lookupReference('mount').getEl().dom, {
                        value: me.original,
                        language: 'yaml',
                        theme: PVE.meta.Monaco.theme(),
                        automaticLayout: true,
                        minimap: { enabled: false },
                        scrollBeyondLastLine: false,
                    });
                },
                (err) => Proxmox.Utils.setErrorMask(me, Ext.htmlEncode(PVE.meta.Utils.errText(err))),
            );
        });

        // Monaco leaks a ResizeObserver and its models otherwise, and a stale buffer
        // is one that can be applied to the wrong path.
        me.on('destroy', function () {
            PVE.meta.Monaco.dispose(me.diffEditor);
            PVE.meta.Monaco.dispose(me.editor);
            me.editor = me.diffEditor = null;
        });
    },

    // Presentation only: re-render the same value in the other syntax. If we cannot
    // parse the buffer we say so and stay put; the server remains the YAML authority.
    switchLang: function (lang) {
        let me = this;
        let btn = me.lookupReference('langbtn');
        if (!me.editor || lang === me.lang) {
            return;
        }
        let value;
        try {
            value =
                me.lang === 'json'
                    ? Ext.decode(me.editor.getValue())
                    : PVE.meta.Utils.yamlParse(me.editor.getValue());
        } catch (err) {
            Ext.Msg.alert(
                gettext('Error'),
                Ext.String.format(
                    gettext('Cannot convert to {0}: {1}'),
                    lang.toUpperCase(),
                    Ext.htmlEncode(PVE.meta.Utils.errText(err)),
                ),
            );
            btn.suspendEvents();
            btn.setValue(me.lang);
            btn.resumeEvents();
            return;
        }
        me.lang = lang;
        window.monaco.editor.setModelLanguage(me.editor.getModel(), lang);
        me.editor.setValue(
            lang === 'json' ? JSON.stringify(value, null, 2) : PVE.meta.Utils.yamlDump(value),
        );
    },

    // Monaco's second job: original vs edited, side by side, as the confirm step.
    showDiff: function () {
        let me = this;
        if (!me.editor) {
            return;
        }
        let lang = me.lang;
        let edited = me.editor.getValue();
        let original = me.original;
        if (lang === 'json') {
            try {
                original = JSON.stringify(PVE.meta.Utils.yamlParse(me.original), null, 2);
            } catch (_err) {
                lang = 'yaml'; // cannot render the original as JSON; diff the YAML
            }
        }
        if (edited === original) {
            Ext.Msg.alert(gettext('Notice'), gettext('No changes.'));
            return;
        }

        let win = Ext.create('Ext.window.Window', {
            title: gettext('Confirm') + ': ' + Ext.htmlEncode(me.view || gettext('(whole document)')),
            itemId: 'pveMetaDiffWindow',
            modal: true,
            width: 1000,
            height: 620,
            layout: 'fit',
            referenceHolder: true,
            items: [{ xtype: 'component', reference: 'diff', style: 'height:100%;width:100%' }],
            buttons: [
                {
                    text: gettext('Apply'),
                    handler: function () {
                        win.close();
                        me.apply(edited, me.lang);
                    },
                },
                { text: gettext('Back'), handler: () => win.close() },
            ],
        });
        win.on('afterrender', function () {
            let monaco = window.monaco;
            me.diffEditor = monaco.editor.createDiffEditor(win.lookupReference('diff').getEl().dom, {
                theme: PVE.meta.Monaco.theme(),
                automaticLayout: true,
                readOnly: true,
                renderSideBySide: true,
                minimap: { enabled: false },
            });
            me.diffEditor.setModel({
                original: monaco.editor.createModel(original, lang),
                modified: monaco.editor.createModel(edited, lang),
            });
        });
        win.on('destroy', function () {
            PVE.meta.Monaco.dispose(me.diffEditor);
            me.diffEditor = null;
        });
        win.show();
    },

    apply: function (text, lang) {
        let me = this;
        // JSON is a subset of YAML, but `data` is the parameter that says "this is the
        // JSON data model", so use it when the user is editing JSON.
        let params = { view: me.view, mode: 'replace', digest: me.tree.digest };
        params[lang === 'json' ? 'data' : 'text'] = text;
        me.tree.write(params, () => me.close());
    },
});

// ---------------------------------------------------------------------------
// The panel.
// ---------------------------------------------------------------------------

Ext.define('PVE.meta.TreePanel', {
    extend: 'Ext.tree.Panel',
    xtype: 'pveMetaTreePanel',

    rootVisible: false,
    scrollable: true,
    border: false,
    animate: false,
    useArrows: true,
    emptyText: gettext('No metadata'),
    viewConfig: { loadMask: false },

    // vmid/node/type/dc arrive as config properties from pve-ext's page loader;
    // pveSelNode is the fallback for anything that adds this panel the PVE way.
    pveSelNode: undefined,

    initComponent: function () {
        let me = this;
        let sel = (me.pveSelNode && me.pveSelNode.data) || {};

        me.vmid = me.vmid || sel.vmid;
        me.dc = !me.vmid;
        me.baseUrl = me.dc ? '/meta/datacenter' : '/meta/guests/' + me.vmid;
        me.digest = '';
        me.access = { read: 1, write: 0, scopes: [] };
        me.registrations = [];
        me.tags = [];
        me.token = null;
        me.editing = false;

        me.store = Ext.create('Ext.data.TreeStore', {
            model: 'PVE.meta.TreeModel',
            root: { expanded: true, children: [] },
        });
        me.cellEditing = Ext.create('Ext.grid.plugin.CellEditing', { clicksToEdit: 1 });

        Ext.apply(me, {
            plugins: [me.cellEditing],
            columns: me.buildColumns(),
            tbar: me.buildToolbar(),
            listeners: {
                beforeedit: (editor, e) => me.onBeforeEdit(e),
                edit: (editor, e) => me.onEdit(e),
                canceledit: () => {
                    me.editing = false;
                },
            },
        });
        me.callParent();

        me.on('afterrender', () => me.reload());
        me.pollTask = Ext.TaskManager.start({ run: () => me.poll(), interval: 5000, fireOnStart: false });
        me.on('destroy', function () {
            Ext.TaskManager.stop(me.pollTask);
            if (me.textWindow) {
                me.textWindow.close();
            }
        });
    },

    buildColumns: function () {
        let me = this;
        let U = PVE.meta.Utils;
        let fade = (rec, html) => (rec.data.present ? html : '<span class="faded">' + html + '</span>');
        // The comment key that documents this row becomes its tooltip (DESIGN §8).
        let qtip = function (rec, meta) {
            if (rec.data.description) {
                meta.tdAttr = 'data-qtip="' + Ext.htmlEncode(Ext.htmlEncode(rec.data.description)) + '"';
            }
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
                    qtip(rec, meta);
                    return fade(rec, Ext.htmlEncode(value));
                },
            },
            {
                text: gettext('Value'),
                dataIndex: 'valueText',
                flex: 3,
                // A placeholder, only so CellEditing considers the column editable;
                // onBeforeEdit() replaces it with the editor this row actually wants.
                editor: { xtype: 'textfield' },
                renderer: function (value, meta, rec) {
                    qtip(rec, meta);
                    if (rec.data.kind === 'map') {
                        return '';
                    }
                    return rec.data.present
                        ? Ext.htmlEncode(value)
                        : '<span class="faded">' + unsetText(rec) + '</span>';
                },
            },
            {
                text: gettext('Owner'),
                dataIndex: 'ownerText',
                flex: 1,
                renderer: (value, meta, rec) => fade(rec, Ext.htmlEncode(value || '')),
            },
            {
                xtype: 'actioncolumn',
                width: 60,
                align: 'center',
                items: [
                    {
                        tooltip: gettext('Add key here'),
                        getClass: (v, meta, rec) =>
                            rec.data.kind === 'map' && rec.data.editable
                                ? 'fa fa-plus-circle'
                                : 'x-hidden-display',
                        handler: (view, r, c, item, e, rec) => me.addKey(rec.data.path),
                    },
                    {
                        tooltip: gettext('Remove'),
                        getClass: (v, meta, rec) =>
                            rec.data.present && rec.data.editable ? 'fa fa-trash-o' : 'x-hidden-display',
                        handler: (view, r, c, item, e, rec) => me.removeKey(rec),
                    },
                ],
            },
        ];
    },

    buildToolbar: function () {
        let me = this;
        return [
            { text: gettext('Reload'), iconCls: 'fa fa-refresh', handler: () => me.reload() },
            '-',
            {
                text: gettext('Add'),
                iconCls: 'fa fa-plus',
                handler: function () {
                    let rec = me.getSelection()[0];
                    me.addKey(rec ? (rec.data.kind === 'map' ? rec.data.path : me.parentPath(rec)) : '');
                },
            },
            {
                xtype: 'proxmoxButton',
                text: gettext('Remove'),
                iconCls: 'fa fa-trash-o',
                disabled: true,
                // Proxmox.button.Button picks up the tree's selection model itself
                // through parentXType; asking for it here would be too early.
                parentXType: 'treepanel',
                enableFn: (rec) => rec.data.present && rec.data.editable,
                handler: (btn, e, rec) => me.removeKey(rec),
            },
            '-',
            { text: gettext('Edit as Text'), iconCls: 'fa fa-file-code-o', handler: () => me.editAsText() },
            '->',
            { xtype: 'tbtext', reference: 'accessText' },
        ];
    },

    parentPath: (rec) => (rec.parentNode && rec.parentNode.data.path) || '',

    // --- loading -----------------------------------------------------------

    // Every load is a chain of these. A tab switch destroys the panel while requests
    // are still in flight, so no callback may touch a destroyed one.
    request: function (opts) {
        let me = this;
        let guard = (fn) => (fn ? (...args) => (me.isDestroyed ? undefined : fn(...args)) : undefined);
        Proxmox.Utils.API2Request({
            method: 'GET',
            url: opts.url,
            params: opts.params,
            success: guard(opts.success),
            failure: guard(
                opts.failure ||
                    ((response) =>
                        Proxmox.Utils.setErrorMask(me, response.htmlStatus || gettext('Error'))),
            ),
        });
    },

    reload: function () {
        let me = this;
        if (!me.rendered || me.isDestroyed || me.editing || me.textWindow) {
            return;
        }
        Proxmox.Utils.setErrorMask(me, true);
        me.loadOperators(() =>
            me.loadTags(() =>
                me.loadAccess(() => me.loadDocument(() => Proxmox.Utils.setErrorMask(me, false))),
            ),
        );
    },

    // /meta/operators is revision 5; against an older API it simply fails and the
    // Owner column and the grammar-declared rows stay empty, rather than the page.
    loadOperators: function (next) {
        let me = this;
        me.request({
            url: '/meta/operators',
            success: function (response) {
                me.registrations = response.result.data || [];
                next();
            },
            failure: function () {
                me.registrations = [];
                next();
            },
        });
    },

    // Only needed to resolve `selector: { tag: t }`, so only fetched when one exists.
    loadTags: function (next) {
        let me = this;
        me.tags = [];
        let needed = me.registrations.some((r) =>
            (r.scopes || []).some((s) => s.selector && s.selector.tag),
        );
        if (me.dc || !needed) {
            next();
            return;
        }
        me.request({
            url: '/meta/guests',
            success: function (response) {
                let row = (response.result.data || []).find((g) => String(g.vmid) === String(me.vmid));
                me.tags = (row && row.tags) || [];
                next();
            },
            failure: () => next(),
        });
    },

    loadAccess: function (next) {
        let me = this;
        me.request({
            url: '/meta/access',
            params: me.dc ? { dc: 1 } : { vmid: me.vmid },
            success: function (response) {
                me.access = response.result.data || { read: 0, write: 0, scopes: [] };
                let scoped = (me.access.scopes || []).some((s) => s.mode === 'rw');
                let label = me.down('[reference=accessText]');
                (label || { setText: Ext.emptyFn }).setText(
                    me.access.write
                        ? gettext('Full write access')
                        : scoped
                          ? gettext('Scoped write access')
                          : me.access.read
                            ? gettext('Read only')
                            : gettext('No access'),
                );
                next();
            },
        });
    },

    loadDocument: function (next) {
        let me = this;
        me.request({
            url: me.baseUrl,
            success: function (response) {
                let d = response.result.data || {};
                me.digest = d.digest || '';
                me.buildTree(d.data || {});
                next();
            },
        });
    },

    poll: function () {
        let me = this;
        if (!me.rendered || me.isDestroyed || me.editing || me.textWindow) {
            return;
        }
        Proxmox.Utils.API2Request({
            url: '/meta/version',
            method: 'GET',
            failure: Ext.emptyFn, // transient; the next tick tries again
            success: function (response) {
                let token = (response.result.data || {}).token;
                if (me.token === null) {
                    me.token = token;
                } else if (token && token !== me.token) {
                    // Re-check: an edit (or the text window) may have started while
                    // this request was in flight. Do *not* advance me.token here -
                    // leaving it stale means the next 5 s tick sees the same change
                    // and retries, instead of the reload being lost silently.
                    if (me.isDestroyed || me.editing || me.textWindow) {
                        return;
                    }
                    me.token = token;
                    me.reload();
                }
            },
        });
    },

    // --- rows ---------------------------------------------------------------

    // `true` if `/meta/access` (server-resolved, not gated on VM.Audit) already
    // told us this scope applies to us: same prefix and mode as one of our own
    // resolved scopes.
    resolvedScopeApplies: function (scope) {
        let me = this;
        return (me.access.scopes || []).some(
            (s) => s.prefix === scope.prefix && s.mode === scope.mode,
        );
    },

    // The scopes of every registration whose selector matches this guest. Scopes
    // apply to guest documents only (DESIGN §3), so the datacenter gets none.
    //
    // A `tag` selector is normally resolved against `me.tags` (from
    // `GET /meta/guests`), but that field is only populated for a caller with
    // VM.Audit (DESIGN §5). A scope-only principal never has it, so also accept
    // a scope `/meta/access` already resolved for us: that endpoint resolves
    // selectors server-side without requiring VM.Audit, so it still surfaces our
    // own declared rows and Owner label even when `me.tags` is empty. The
    // registration is still the source of the label (name, selector text).
    applicableScopes: function () {
        let me = this;
        let out = [];
        if (me.dc) {
            return out;
        }
        me.registrations.forEach(function (reg) {
            (reg.scopes || []).forEach(function (scope) {
                let sel = scope.selector || {};
                let matches =
                    scope.prefix &&
                    (sel.all ||
                        (sel.tag && me.tags.indexOf(sel.tag) !== -1) ||
                        me.resolvedScopeApplies(scope));
                if (matches) {
                    out.push(Ext.apply({ registration: reg }, scope));
                }
            });
        });
        return out;
    },

    ownerFor: function (path, scopes) {
        let best = null;
        scopes.forEach(function (s) {
            if (PVE.meta.Utils.covers(s.prefix, path) && (!best || s.prefix.length > best.prefix.length)) {
                best = s;
            }
        });
        if (!best) {
            return '';
        }
        let sel = best.selector || {};
        return (
            (best.registration.name || best.registration.authid || '') +
            (sel.tag ? ' (tag: ' + sel.tag + ')' : '') +
            (best.mode === 'ro' ? ' [ro]' : '')
        );
    },

    editableFor: function (path) {
        let me = this;
        return (
            !!me.access.write ||
            (me.access.scopes || []).some(
                (s) => s.mode === 'rw' && PVE.meta.Utils.covers(s.prefix, path),
            )
        );
    },

    // The document and the grammars are two sources for the same rows, so merge them
    // as plain entries first — much less fiddly than merging Ext node configs.
    entry: function (parent, key, path) {
        parent.children[key] = parent.children[key] || {
            key: key,
            path: path,
            // Object.create(null): `key` is a document key (attacker-chosen, and
            // no key is reserved - DESIGN §4), so a plain `{}` here lets a key
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
            } else {
                child.present = true;
                child.kind = U.kindOf(v);
                child.value = v;
            }
        });
    },

    // A grammar is a PVE::JSONSchema object rooted at its scope's prefix.
    addGrammar: function (root, prefix, schema) {
        let me = this;
        let U = PVE.meta.Utils;
        let entry = root;
        let path = '';
        prefix.split('.').forEach(function (seg) {
            path = U.joinPath(path, seg);
            entry = me.entry(entry, seg, path);
            entry.kind = entry.kind || 'map';
        });
        let walk = function (node, sch) {
            if (!sch || sch.type !== 'object' || !sch.properties) {
                return;
            }
            node.kind = node.kind || 'map';
            Object.keys(sch.properties).forEach(function (key) {
                let ps = sch.properties[key] || {};
                let child = me.entry(node, key, U.joinPath(node.path, key));
                child.description = child.description || ps.description;
                if (ps.type === 'object') {
                    child.kind = 'map';
                    walk(child, ps);
                    return;
                }
                // A declared type wins over the type inferred from the stored value:
                // it is the operator's statement of what the key means, and the API's
                // JSON view cannot tell a boolean from the integer 1 anyway.
                child.kind = ps.type ? me.schemaKind(ps) : child.kind || 'string';
                if (ps.default !== undefined) {
                    child.defaultValue = ps.default;
                }
                if (ps.enum) {
                    child.enumValues = ps.enum;
                }
            });
        };
        walk(entry, schema);
    },

    schemaKind: function (schema) {
        let t = (schema && schema.type) || 'string';
        if (t === 'integer' || t === 'number') {
            return 'number';
        }
        return t === 'boolean' || t === 'array' ? t : 'string';
    },

    buildTree: function (data) {
        let me = this;
        let scopes = me.applicableScopes();
        let root = { key: '', path: '', children: Object.create(null), present: true, kind: 'map' };
        me.addData(root, data);
        scopes.forEach((s) => (s.grammar ? me.addGrammar(root, s.prefix, s.grammar) : undefined));

        let toNodes = (entry) =>
            Object.keys(entry.children)
                .sort()
                .map(function (key) {
                    let c = entry.children[key];
                    let kind = c.kind || 'string';
                    let node = {
                        key: key,
                        text: key,
                        path: c.path,
                        kind: kind,
                        present: !!c.present,
                        description: c.description || '',
                        defaultValue: c.defaultValue,
                        enumValues: c.enumValues,
                        rawValue: c.value,
                        valueText: c.present ? PVE.meta.Utils.displayValue(c.value, kind) : '',
                        ownerText: me.ownerFor(c.path, scopes),
                        editable: me.editableFor(c.path),
                        iconCls: 'x-hidden-display',
                        leaf: kind !== 'map',
                    };
                    if (kind === 'map') {
                        node.children = toNodes(c);
                        node.expanded = true;
                    }
                    return node;
                });

        // Reloading (including from the version poll) must not fold the tree up.
        // Keyed by document path, so it gets the same treatment as `children`.
        let expanded = Object.create(null);
        let seen = false;
        me.getRootNode().cascadeBy(function (n) {
            if (n.data.path && !n.isLeaf()) {
                seen = true;
                if (n.isExpanded()) {
                    expanded[n.data.path] = true;
                }
            }
        });
        me.setRootNode({ expanded: true, children: toNodes(root) });
        if (seen) {
            me.getRootNode().cascadeBy(function (n) {
                if (n.data.path && !n.isLeaf() && !expanded[n.data.path]) {
                    n.collapse();
                }
            });
        }
    },

    // --- editing ------------------------------------------------------------

    onBeforeEdit: function (e) {
        let me = this;
        let d = e.record.data;
        if (e.field !== 'valueText' || d.kind === 'map' || !d.editable) {
            return false;
        }
        // setColumnField also evicts CellEditing's per-column editor cache, which
        // plain column.setEditor() does not - without it every row after the first
        // would reuse the first row's editor.
        let field = me.editorFor(d);
        if (Ext.isFunction(me.cellEditing.setColumnField)) {
            me.cellEditing.setColumnField(e.column, field);
        } else {
            e.column.setEditor(field);
        }
        if (d.present) {
            e.value = d.kind === 'boolean' || d.kind === 'number' ? d.rawValue : d.valueText;
        } else if (d.defaultValue !== undefined) {
            e.value = d.defaultValue;
        } else {
            e.value = d.kind === 'boolean' ? false : '';
        }
        me.editing = true;
        return true;
    },

    editorFor: function (d) {
        if (d.enumValues) {
            return {
                xtype: 'combobox',
                store: d.enumValues.map((v) => String(v)),
                queryMode: 'local',
                editable: false,
                forceSelection: true,
            };
        } else if (d.kind === 'boolean') {
            return { xtype: 'checkbox' };
        } else if (d.kind === 'number') {
            return { xtype: 'numberfield', allowDecimals: true, hideTrigger: true, keyNavEnabled: false };
        }
        return { xtype: 'textfield', selectOnFocus: true };
    },

    onEdit: function (e) {
        let me = this;
        me.editing = false;
        let d = e.record.data;
        let value;
        try {
            value = PVE.meta.Utils.parseValue(e.value, d.kind);
        } catch (err) {
            Ext.Msg.alert(gettext('Error'), Ext.htmlEncode(PVE.meta.Utils.errText(err)));
            me.reload();
            return;
        }
        if (d.present && Ext.encode(value) === Ext.encode(d.rawValue)) {
            return; // nothing actually changed
        }
        me.write({ view: d.path, mode: 'replace', data: Ext.encode(value), digest: me.digest });
    },

    addKey: function (parentPath) {
        let me = this;
        let win = Ext.create('PVE.meta.AddKeyWindow', { parentPath: parentPath || '' });
        win.on('addkey', (path, value) =>
            me.write({ view: path, mode: 'replace', data: Ext.encode(value), digest: me.digest }),
        );
        win.show();
    },

    removeKey: function (rec) {
        let me = this;
        if (!rec) {
            return;
        }
        Ext.Msg.confirm(
            gettext('Confirm'),
            Ext.String.format(gettext('Remove "{0}"?'), Ext.htmlEncode(rec.data.path)),
            function (btn) {
                if (btn === 'yes') {
                    let q = Ext.Object.toQueryString({ view: rec.data.path, digest: me.digest });
                    me.submit({ url: me.baseUrl + '?' + q, method: 'DELETE' });
                }
            },
        );
    },

    editAsText: function () {
        let me = this;
        let rec = me.getSelection()[0];
        let view = rec ? (rec.data.kind === 'map' ? rec.data.path : me.parentPath(rec)) : '';
        me.request({
            url: me.baseUrl,
            params: { view: view, format: 'yaml' },
            success: function (response) {
                let d = response.result.data || {};
                me.digest = d.digest || me.digest;
                me.textWindow = Ext.create('PVE.meta.TextWindow', {
                    view: view,
                    text: d.text || '',
                    tree: me,
                });
                me.textWindow.on('destroy', function () {
                    me.textWindow = null;
                    me.reload();
                });
                me.textWindow.show();
            },
        });
    },

    // --- writes -------------------------------------------------------------

    write: function (params, onSuccess) {
        this.submit({ url: this.baseUrl, method: 'PUT', params: params }, onSuccess);
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
                            me.reload();
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
});
