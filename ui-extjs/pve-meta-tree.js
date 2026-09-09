/*
 * pve-meta-tree.js — the native ExtJS implementation of the pve-meta editor.
 *
 * One panel with two cards (DESIGN.md §5, §7, §8):
 *
 *   Tree — an Ext.tree.Panel with columns Key | Value | Description | Access over the
 *     document the caller can see. Rows are the union of the keys present in the
 *     document and the keys the governing namespaces declare (`GET /meta/namespaces`,
 *     matched by prefix and selector against this guest, most-specific first -- schemas
 *     shadow, they never merge); a declared-but-unset key
 *     renders faded with its default, and "setting" it is just editing it. Map rows
 *     carry a folder icon (open when expanded), value rows a document icon, both at the
 *     size and colour of the PVE resource tree. Comment keys (`k__`, and the bare `__`
 *     for the map itself) are not rows — `k__` is the Description of row `k`. Arrays are
 *     one text leaf. Access lists every grant whose prefix covers the row.
 *
 *   Text — a full-document Monaco editor (YAML, with a presentation-only YAML/JSON view
 *     toggle), Apply through a diff dialog and Discard.
 *
 * The Tree|Text segmented button at the right end of the toolbar swaps the body in
 * place; leaving Text with an edited buffer asks first.
 *
 * Editing is a modal row editor (Edit, double-click, or Enter), the field chosen from
 * the grammar type and falling back to the value's own type; editability is per row from
 * `GET /meta/access`. A commit is one minimal write:
 *   PUT /meta/guests/{vmid}?view=<dotted.path>&mode=replace&data=<json>&digest=<d>
 * 409 (digest mismatch) reloads and reports the API's message verbatim. A 5 s poll of
 * `GET /meta/version` refreshes the tree when the content token changed — never while a
 * row editor, the text window or the Text card is open.
 *
 * Monaco has three jobs: "Edit selection as text" on the selected subtree, the Text
 * card on the whole document, and the diff that confirms either one's Apply. Its AMD
 * loader is fetched lazily on first use from /pve2/js/pve-meta-extjs/vs/loader.js
 * (Monaco is vendored into the package by `make ui`, never fetched from a CDN); every
 * editor is disposed when its owner goes away.
 *
 * YAML is js-yaml 4.1.0, vendored in vendor/ and loaded lazily the same way. It is used
 * only for presentation — the YAML/JSON view toggle and the diff. The server stays the
 * authority on YAML: an Apply in YAML view sends the buffer to the API as `text`.
 *
 * pve-ext's page loader loads this file and instantiates `pveMetaTreePanel` as the
 * tab (see README.md), so session, CSRF, dark theme and i18n all come from the PVE
 * UI — none of it is reimplemented here. Plain ES2017, no build step.
 */

Ext.ns('PVE.meta');

// ---------------------------------------------------------------------------
// Helpers: paths, the JSON data model, and YAML in and out.
// ---------------------------------------------------------------------------

// Row icons. Plain FontAwesome classes: ExtJS marks any node that carries an
// `iconCls` with `x-tree-icon-custom`, which is the class PVE's own stylesheet
// sizes and colours for the resource tree (ext6-pve.css: 1.25em, #555; #e6e6e6
// in proxmox-dark), so these come out the same size and muted grey as every
// other PVE tree icon without this file shipping a line of CSS.
PVE.meta.Icons = {
    map: 'fa fa-folder',
    mapExpanded: 'fa fa-folder-open',
    leaf: 'fa fa-file-text-o',
};

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

    // A grammar's `format` is a PVE::JSONSchema format name, and proxmoxlib already
    // ships the matching client-side validator as an ExtJS vtype -- so a format is
    // wired to PVE's own checker, with PVE's own (translated) error message, rather
    // than to a regex of ours. A format with no vtype (or one we do not know) simply
    // does not constrain the field: an unknown constraint must never block an edit.
    // This is exactly the set ui/src/grammar.rs's check_format() implements, so the two
    // UIs accept and reject the same strings; adding a format means adding it in both.
    FORMAT_VTYPES: {
        'ip': 'IP64Address',
        'ipv4': 'IPAddress',
        'ipv6': 'IP6Address',
        'CIDR': 'IP64CIDRAddress',
        'CIDRv4': 'IPCIDRAddress',
        'CIDRv6': 'IP6CIDRAddress',
        'mac-addr': 'MacAddress',
        'dns-name': 'DnsName',
        'address': 'DnsOrIp',
        'email': 'proxmoxMail',
    },

    vtypeFor: function (format) {
        return (format && PVE.meta.Utils.FORMAT_VTYPES[format]) || undefined;
    },

    // The inverse, for a committed row edit: field value -> the JSON value to send.
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

    // The field a row's value is edited with. The grammar's declared type wins over the
    // type inferred from the stored value: it is the operator's statement of what the
    // key means, and the API's JSON view cannot tell a boolean from the integer 1.
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
            return { xtype: 'proxmoxcheckbox' };
        } else if (d.kind === 'number') {
            let f = { xtype: 'numberfield', allowDecimals: true, hideTrigger: true, keyNavEnabled: false };
            if (d.minimum !== undefined && d.minimum !== null) {
                f.minValue = d.minimum;
            }
            if (d.maximum !== undefined && d.maximum !== null) {
                f.maxValue = d.maximum;
            }
            return f;
        }
        let f = { xtype: 'textfield', selectOnFocus: true };
        let vtype = PVE.meta.Utils.vtypeFor(d.format);
        if (vtype) {
            f.vtype = vtype;
        }
        return f;
    },

    // True if `value` is the same document as `yamlText` parses to, key order
    // included. JSON.stringify preserves insertion order, and the document model is
    // ordered maps (DESIGN section 2), so comparing the two encodings is the right
    // test: same keys, same order, same values.
    sameDocument: function (value, yamlText) {
        try {
            return JSON.stringify(value) === JSON.stringify(PVE.meta.Utils.yamlLoad(yamlText));
        } catch (_err) {
            return false;
        }
    },

    // The namespace governing `path`: the one whose prefix is the LONGEST that covers
    // it. Most-specific wins and schemas never merge (DESIGN section 3.1).
    //
    // The single implementation of that rule on this side. It had three call sites --
    // the row builder, the linter and the hover index -- and lived in two of them; the
    // third simply did not prune, so a parent namespace's `properties` reached into a
    // child namespace's subtree and set its row kind. One function, three callers.
    //
    // `namespaces` must be sorted longest-prefix-first, so this is the first match.
    governing: function (path, namespaces) {
        let list = namespaces || [];
        for (let i = 0; i < list.length; i++) {
            if (PVE.meta.Utils.covers(list[i].prefix, path)) {
                return list[i];
            }
        }
        return null;
    },

    // Longest prefix first, then by name: the order `governing` relies on.
    bySpecificity: function (namespaces) {
        return (namespaces || []).slice().sort(function (a, b) {
            let d = PVE.meta.Utils.depth(b.prefix) - PVE.meta.Utils.depth(a.prefix);
            return d !== 0 ? d : String(a.prefix).localeCompare(String(b.prefix));
        });
    },

    // Segment count of a dotted prefix -- how "specific" it is. `''` is 0.
    depth: function (prefix) {
        let p = String(prefix || '');
        return p === '' ? 0 : p.split('.').length;
    },

    // A scalar as the string a grammar's `enum` and a hover compare and show.
    scalarText: function (value) {
        return typeof value === 'string' ? value : Ext.encode(value);
    },

    // Runs the vtype a format maps to, and returns that vtype's own message on failure.
    // Reusing proxmoxlib's validator rather than a second regex of ours is what keeps
    // the marker and the row editor agreeing about the same string -- they are literally
    // the same check. An unmapped format constrains nothing.
    checkFormat: function (format, value) {
        let vtype = PVE.meta.Utils.vtypeFor(format);
        // Defensive down the whole chain: this runs before any form field has been
        // instantiated, so nothing guarantees the VTypes singleton exists yet, and a
        // validator we cannot reach must constrain nothing rather than throw.
        let vtypes = Ext.form && Ext.form.field && Ext.form.field.VTypes;
        if (!vtype || !vtypes || typeof vtypes[vtype] !== 'function') {
            return null;
        }
        if (vtypes[vtype](value)) {
            return null;
        }
        return vtypes[vtype + 'Text'] || gettext('invalid value');
    },

    // Human-readable form of a scope's selector, for the Access tooltip.
    selectorText: function (selector) {
        let sel = selector || {};
        if (sel.tag) {
            return gettext('tag') + ': ' + sel.tag;
        } else if (sel.all) {
            return gettext('all guests');
        }
        return Ext.encode(sel);
    },

    // --- YAML, through the vendored js-yaml (see PVE.meta.Yaml) --------------
    //
    // Presentation only: the YAML/JSON view toggle and the diff's "original" side.
    // `load` uses js-yaml's default schema, which is the safe one (no arbitrary
    // JS types); `dump` is pinned to the block style the store itself emits.

    yamlLib: function () {
        let y = window.jsyaml;
        if (!y || !y.load) {
            throw new Error(gettext('YAML support is not loaded'));
        }
        return y;
    },

    yamlLoad: function (text) {
        let doc = PVE.meta.Utils.yamlLib().load(String(text));
        // An empty document is the empty map, not a null: the model has no nulls.
        return doc === undefined || doc === null ? {} : doc;
    },

    yamlDump: function (value) {
        return PVE.meta.Utils.yamlLib().dump(value, {
            indent: 2,
            lineWidth: -1, // never fold: a folded line is a changed line in the diff
            noRefs: true, // anchors/aliases are not part of the document model
            sortKeys: false, // documents are ordered maps (DESIGN §2)
        });
    },

    errText: (err) => String((err && (err.message || err.msg)) || err),
};

// ---------------------------------------------------------------------------
// js-yaml, vendored under vendor/ and loaded lazily on first use.
// ---------------------------------------------------------------------------

PVE.meta.Yaml = {
    SRC: '/pve2/js/pve-meta-extjs/vendor/js-yaml.min.js',
    promise: null,

    load: function () {
        let me = PVE.meta.Yaml;
        me.promise =
            me.promise ||
            new Promise(function (resolve, reject) {
                if (window.jsyaml && window.jsyaml.load) {
                    resolve(window.jsyaml);
                    return;
                }
                // js-yaml ships a UMD bundle: if an AMD `define` is present it
                // registers as an anonymous module instead of setting window.jsyaml.
                // Monaco's loader installs exactly such a `define`, so hide it for
                // the duration of this one script load and put it back afterwards.
                // PVE.meta.Monaco.load() waits for this promise first, so the two
                // never overlap.
                let prevDefine = window.define;
                let restore = (fn) =>
                    function (arg) {
                        window.define = prevDefine;
                        fn(arg);
                    };
                window.define = undefined;
                let script = document.createElement('script');
                script.src = me.SRC;
                script.onload = restore(function () {
                    if (window.jsyaml && window.jsyaml.load) {
                        resolve(window.jsyaml);
                    } else {
                        reject(new Error('js-yaml did not register (' + me.SRC + ')'));
                    }
                });
                script.onerror = restore(() => reject(new Error('failed to load ' + me.SRC)));
                document.head.appendChild(script);
            });
        return me.promise;
    },
};

// ---------------------------------------------------------------------------
// Grammar findings for the text editor: what is wrong, and which line to underline.
//
// Mirrors ui/src/lint.rs exactly -- same rules, same messages, same line scan -- so
// the two implementations say the same thing about the same document. Changing one
// means changing the other.
//
// Two halves, kept apart on purpose. `findings()` answers *what is wrong*, from the
// parsed document the panel already holds; `lineIndex()` answers *where to draw it*,
// by scanning the YAML the server returned. The pairing only holds while the buffer
// still is what the server sent, so the caller clears both once it is dirty.
// ---------------------------------------------------------------------------

PVE.meta.Lint = {
    // The namespaces that carry a schema, longest prefix first. Shape comes from
    // namespaces, never from grants (DESIGN section 3.1).
    applicable: function (namespaces) {
        return PVE.meta.Utils.bySpecificity(
            (namespaces || []).filter((ns) => ns && ns.schema && ns.prefix),
        );
    },

    valueAt: function (data, path) {
        let cur = data;
        if (!path) {
            return cur;
        }
        let parts = path.split('.');
        for (let i = 0; i < parts.length; i++) {
            if (!cur || typeof cur !== 'object' || Array.isArray(cur)) {
                return undefined;
            }
            cur = Object.prototype.hasOwnProperty.call(cur, parts[i]) ? cur[parts[i]] : undefined;
        }
        return cur;
    },

    findings: function (data, applicable) {
        let out = [];
        let list = applicable || [];
        list.forEach(function (ns) {
            let value = PVE.meta.Lint.valueAt(data, ns.prefix);
            if (value !== undefined) {
                PVE.meta.Lint.walk(value, ns.schema, ns.prefix, out, list, ns);
            }
        });
        out.sort((a, b) => (a.path < b.path ? -1 : a.path > b.path ? 1 : 0));
        return out;
    },

    // `list`/`owner`, when given, enforce most-specific-wins: the walk stops where a
    // *different* namespace governs, so a parent's schema never reaches into a child
    // namespace's subtree. Schemas shadow, they do not merge (DESIGN section 3.1).
    walk: function (value, schema, path, out, list, owner) {
        let message = PVE.meta.Lint.checkValue(schema, value);
        if (message) {
            out.push({ path: path, message: message });
            return; // a value of the wrong shape says nothing useful about its children
        }
        let props = schema && schema.properties;
        if (!props || !value || typeof value !== 'object' || Array.isArray(value)) {
            return;
        }
        Object.keys(value).forEach(function (key) {
            if (Object.prototype.hasOwnProperty.call(props, key)) {
                let child = path ? path + '.' + key : key;
                if (owner && PVE.meta.Utils.governing(child, list) !== owner) {
                    return; // a more specific namespace owns this subtree
                }
                PVE.meta.Lint.walk(value[key], props[key], child, out, list, owner);
            }
        });
    },

    // Only what the row editor also enforces, so the two never disagree.
    checkValue: function (schema, value) {
        if (!schema) {
            return null;
        }
        if (schema.enum) {
            let shown = PVE.meta.Utils.scalarText(value);
            let allowed = schema.enum.map((v) => String(v));
            return allowed.indexOf(shown) === -1
                ? gettext('expected one of') + ': ' + allowed.join(', ')
                : null;
        }
        if (schema.type && !PVE.meta.Lint.typeMatches(schema.type, value)) {
            return gettext('expected') + ' ' + schema.type;
        }
        if (typeof value === 'number') {
            if (schema.minimum !== undefined && value < schema.minimum) {
                return gettext('must be at least') + ' ' + schema.minimum;
            }
            if (schema.maximum !== undefined && value > schema.maximum) {
                return gettext('must be at most') + ' ' + schema.maximum;
            }
        }
        if (typeof value === 'string' && schema.format) {
            return PVE.meta.Utils.checkFormat(schema.format, value);
        }
        return null;
    },

    // A boolean arriving as 1/0 is the API's own wire convention (DESIGN section 4);
    // flagging it would put a warning on every boolean in the store.
    typeMatches: function (declared, value) {
        switch (declared) {
            case 'string':
                return typeof value === 'string';
            case 'integer':
                return typeof value === 'number' && Number.isInteger(value);
            case 'number':
                return typeof value === 'number';
            case 'boolean':
                return typeof value === 'boolean' || value === 0 || value === 1;
            case 'object':
                return !!value && typeof value === 'object' && !Array.isArray(value);
            case 'array':
                return Array.isArray(value);
            default:
                return true;
        }
    },

    // A scan, not a parser: the store dumps canonically (block style, two-space indent,
    // one mapping key per line), so an indent stack resolves every key's path. Sequence
    // items are not indexed (a view addresses through maps only) and a block scalar's
    // body is skipped, so prose that reads `foo: bar` is never taken for a key.
    lineIndex: function (yaml) {
        let out = Object.create(null);
        let stack = []; // [indent, key]
        let blockAt = null;
        String(yaml || '')
            .split('\n')
            .forEach(function (raw, i) {
                let line = raw.trim();
                let indent = raw.length - raw.replace(/^\s+/, '').length;
                if (blockAt !== null) {
                    if (line === '' || indent > blockAt) {
                        return;
                    }
                    blockAt = null;
                }
                if (line === '' || line.charAt(0) === '#' || line === '---' || line === '...') {
                    return;
                }
                if (line === '-' || line.slice(0, 2) === '- ') {
                    return;
                }
                let split = PVE.meta.Lint.splitKey(line);
                if (!split) {
                    return;
                }
                while (stack.length && stack[stack.length - 1][0] >= indent) {
                    stack.pop();
                }
                stack.push([indent, split.key]);
                out[stack.map((e) => e[1]).join('.')] = i + 1;
                let value = split.rest.trim();
                if (value.charAt(0) === '|' || value.charAt(0) === '>') {
                    blockAt = indent;
                }
            });
        return out;
    },

    splitKey: function (line) {
        if (line.charAt(0) === '"') {
            let key = '';
            for (let i = 1; i < line.length; i++) {
                let c = line.charAt(i);
                if (c === '\\') {
                    key += line.charAt(++i);
                } else if (c === '"') {
                    let after = line.slice(i + 1);
                    return after.charAt(0) === ':' ? { key: key, rest: after.slice(1) } : null;
                } else {
                    key += c;
                }
            }
            return null;
        }
        let at = line.indexOf(':');
        if (at <= 0) {
            return null;
        }
        let key = line.slice(0, at).replace(/\s+$/, '');
        return key ? { key: key, rest: line.slice(at + 1) } : null;
    },

    // Findings paired with the line to underline; one whose path the text does not
    // carry is dropped, because a marker on the wrong line is worse than none.
    placed: function (findings, index) {
        let out = [];
        (findings || []).forEach(function (f) {
            if (index[f.path] !== undefined) {
                out.push({ line: index[f.path], message: f.message });
            }
        });
        return out;
    },

    // Every schema node the grammars declare, by document path -- the hover index.
    // Every schema node by document path -- the hover index. Pruned the same way as
    // `findings`, so a path covered by two namespaces resolves to the more specific
    // one rather than to whichever was collected last.
    schemaIndex: function (applicable) {
        let out = Object.create(null);
        let list = applicable || [];
        let collect = function (schema, path, owner) {
            if (owner && PVE.meta.Utils.governing(path, list) !== owner) {
                return;
            }
            out[path] = schema;
            let props = schema && schema.properties;
            if (!props) {
                return;
            }
            Object.keys(props).forEach((k) =>
                collect(props[k], path ? path + '.' + k : k, owner),
            );
        };
        list.forEach((ns) => collect(ns.schema, ns.prefix, ns));
        return out;
    },

    hoverText: function (schema) {
        if (!schema) {
            return null;
        }
        let parts = [];
        if (schema.type) {
            parts.push(schema.format ? schema.type + ' (' + schema.format + ')' : schema.type);
        }
        if (schema.enum) {
            parts.push(gettext('one of') + ': ' + schema.enum.map((v) => String(v)).join(', '));
        }
        if (schema.minimum !== undefined && schema.maximum !== undefined) {
            parts.push(schema.minimum + '..' + schema.maximum);
        } else if (schema.minimum !== undefined) {
            parts.push(gettext('at least') + ' ' + schema.minimum);
        } else if (schema.maximum !== undefined) {
            parts.push(gettext('at most') + ' ' + schema.maximum);
        }
        if (schema.default !== undefined) {
            parts.push(gettext('default') + ': ' + PVE.meta.Utils.scalarText(schema.default));
        }
        if (schema.description) {
            parts.push(schema.description);
        }
        return parts.length ? parts.join(' \u00b7 ') : null;
    },
};

// ---------------------------------------------------------------------------
// Monaco, loaded lazily on first use from the tree the pve-meta UI package ships.
// ---------------------------------------------------------------------------

PVE.meta.Monaco = {
    VS: '/pve2/js/pve-meta-extjs/vs',
    promise: null,

    load: function () {
        let me = PVE.meta.Monaco;
        me.promise =
            me.promise ||
            // js-yaml first, deliberately: its UMD bundle and Monaco's AMD loader
            // both want the global `define`, and every caller of Monaco here also
            // needs the YAML codec.
            PVE.meta.Yaml.load().then(
                () =>
                    new Promise(function (resolve, reject) {
                        if (window.monaco && window.monaco.editor) {
                            resolve(window.monaco);
                            return;
                        }
                        // Absolute, because a language worker resolves its own scripts
                        // against this and has no page URL to make a root-relative path
                        // absolute with.
                        let vs = window.location.origin + me.VS;
                        window.MonacoEnvironment = { baseUrl: vs };
                        let script = document.createElement('script');
                        script.src = vs + '/loader.js';
                        script.onload = function () {
                            try {
                                window.require.config({ paths: { vs: vs } });
                                window.require(
                                    ['vs/editor/editor.main'],
                                    () => resolve(window.monaco),
                                    reject,
                                );
                            } catch (err) {
                                reject(err);
                            }
                        };
                        script.onerror = () => reject(new Error('failed to load ' + script.src));
                        document.head.appendChild(script);
                    }),
            );
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

    // Monaco's other job: original vs edited, side by side, as the confirm step
    // before anything is written. Shared by the selection window and the Text card.
    // cfg: { title, original, modified, lang, apply }
    confirmDiff: function (cfg) {
        let state = {};
        // `cfg.warnings` (grammar findings) turns this into the warned form: a banner
        // above the diff and an Apply gated on an explicit tick.
        let warnings = cfg.warnings || [];
        let win = Ext.create('Ext.window.Window', {
            title: gettext('Confirm') + ': ' + Ext.htmlEncode(cfg.title),
            itemId: 'pveMetaDiffWindow',
            modal: true,
            width: 1000,
            height: 620,
            layout: 'border',
            referenceHolder: true,
            items: [
                // The schema warning lives *in* the confirm step rather than in a
                // dialog before it: one decision, with the diff that decision is
                // about visible underneath it, instead of an alert to dismiss and
                // then a second window to read.
                {
                    xtype: 'panel',
                    region: 'north',
                    hidden: !warnings.length,
                    bodyPadding: 8,
                    border: false,
                    cls: 'pve-meta-diff-warning',
                    style: 'border-bottom:1px solid var(--pwt-color-outline,#c0c0c0)',
                    html:
                        '<div style="display:flex;gap:8px;align-items:flex-start">' +
                        '<i class="fa fa-exclamation-triangle" style="color:#e6a23c;margin-top:2px"></i>' +
                        '<div><b>' +
                        Ext.htmlEncode(gettext('This does not match the schema the operators declare')) +
                        '</b><ul style="margin:4px 0 0 0;padding-left:18px">' +
                        warnings.slice(0, 8).map((w) => '<li>' + Ext.htmlEncode(w) + '</li>').join('') +
                        '</ul>' +
                        (warnings.length > 8
                            ? '<div>' +
                              Ext.htmlEncode(
                                  Ext.String.format(gettext('... and {0} more.'), warnings.length - 8),
                              ) +
                              '</div>'
                            : '') +
                        '</div></div>',
                },
                {
                    xtype: 'component',
                    region: 'center',
                    reference: 'diff',
                    style: 'height:100%;width:100%',
                },
            ],
            buttons: [
                {
                    xtype: 'proxmoxcheckbox',
                    itemId: 'diffAckBox',
                    hidden: !warnings.length,
                    boxLabel: gettext('Save anyway'),
                    // Advisory, not a gate: the server's lint decides what is storable
                    // (DESIGN section 4). The tick is here so a mismatch is a deliberate
                    // act rather than a dialog reflex -- never to make it impossible.
                    listeners: {
                        change: (box, value) => win.down('#diffApplyBtn').setDisabled(!value),
                    },
                },
                '->',
                {
                    text: gettext('Apply'),
                    itemId: 'diffApplyBtn',
                    disabled: !!warnings.length,
                    handler: function () {
                        win.close();
                        cfg.apply();
                    },
                },
                { text: gettext('Back'), handler: () => win.close() },
            ],
        });
        win.on('afterrender', function () {
            let monaco = window.monaco;
            state.editor = monaco.editor.createDiffEditor(win.lookupReference('diff').getEl().dom, {
                theme: PVE.meta.Monaco.theme(),
                automaticLayout: true,
                readOnly: true,
                renderSideBySide: true,
                minimap: { enabled: false },
                // Monaco defaults this to true, which hides indentation-only changes --
                // exactly what a YAML -> JSON -> YAML round trip produces. A confirm
                // dialog that shows nothing while Apply is enabled is worse than no
                // dialog, so show them.
                ignoreTrimWhitespace: false,
            });
            state.editor.setModel({
                original: monaco.editor.createModel(cfg.original, cfg.lang),
                modified: monaco.editor.createModel(cfg.modified, cfg.lang),
            });
        });
        win.on('destroy', function () {
            PVE.meta.Monaco.dispose(state.editor);
            state.editor = null;
        });
        win.show();
        return win;
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
        { name: 'description', type: 'string' }, // the comment key `k__`, if present
        { name: 'grammarDescription', type: 'string' }, // the grammar's, shown as tooltip
        { name: 'accessText', type: 'string' }, // plain-text summary of accessList
        { name: 'accessList' }, // [{ name, mode, selector, prefix }]
        { name: 'kind', type: 'string' }, // map | array | string | number | boolean
        { name: 'present', type: 'boolean' },
        { name: 'editable', type: 'boolean' },
        { name: 'expandedCls', type: 'string' }, // iconCls while this map row is open
        { name: 'defaultValue' },
        { name: 'enumValues' },
        { name: 'minimum' }, // grammar `minimum`, honoured by the number editor
        { name: 'maximum' }, // grammar `maximum`, honoured by the number editor
        { name: 'format', type: 'string' }, // grammar `format` -> an ExtJS vtype
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
// The row editor — opened by Edit, double-click or Enter (DESIGN §8).
// ---------------------------------------------------------------------------

Ext.define('PVE.meta.EditValueWindow', {
    extend: 'Ext.window.Window',
    xtype: 'pveMetaEditValueWindow',

    modal: true,
    width: 480,
    layout: 'fit',
    defaultButton: 'okBtn',
    // configs: rec (the tree record being edited)

    initComponent: function () {
        let me = this;
        let U = PVE.meta.Utils;
        let d = me.rec.data;
        me.title = Ext.String.format(gettext('Edit: {0}'), Ext.htmlEncode(d.path));

        let value;
        if (d.present) {
            value = d.kind === 'boolean' || d.kind === 'number' ? d.rawValue : d.valueText;
        } else if (d.defaultValue !== undefined) {
            value = d.defaultValue;
        } else {
            value = d.kind === 'boolean' ? false : '';
        }

        let items = [
            {
                xtype: 'displayfield',
                fieldLabel: gettext('Key'),
                value: Ext.htmlEncode(d.path),
            },
        ];
        let note = d.description || d.grammarDescription;
        if (note) {
            items.push({
                xtype: 'displayfield',
                fieldLabel: gettext('Description'),
                value: Ext.htmlEncode(note),
            });
        }
        items.push(
            Ext.apply(
                {
                    name: 'value',
                    itemId: 'valueField',
                    fieldLabel: gettext('Value'),
                    value: value,
                },
                U.editorFor(d),
            ),
        );
        if (!d.present && d.defaultValue !== undefined) {
            items.push({
                xtype: 'displayfield',
                fieldLabel: gettext('Default'),
                value: Ext.htmlEncode(U.displayValue(d.defaultValue, U.kindOf(d.defaultValue))),
            });
        }

        Ext.apply(me, {
            items: [
                {
                    xtype: 'form',
                    reference: 'form',
                    bodyPadding: 10,
                    border: false,
                    defaults: { anchor: '100%', labelWidth: 110 },
                    items: items,
                },
            ],
            buttons: [
                { text: gettext('OK'), itemId: 'okBtn', handler: () => me.submit() },
                { text: gettext('Cancel'), handler: () => me.close() },
            ],
        });
        me.callParent();
        me.on('show', () => me.down('#valueField').focus(true, 50));
    },

    submit: function () {
        let me = this;
        let form = me.down('form').getForm();
        if (!form.isValid()) {
            return;
        }
        let d = me.rec.data;
        let value;
        try {
            value = PVE.meta.Utils.parseValue(me.down('#valueField').getValue(), d.kind);
        } catch (err) {
            Ext.Msg.alert(gettext('Error'), Ext.htmlEncode(PVE.meta.Utils.errText(err)));
            return;
        }
        if (d.present && Ext.encode(value) === Ext.encode(d.rawValue)) {
            me.close(); // nothing actually changed
            return;
        }
        me.fireEvent('setvalue', value);
        me.close();
    },
});

// ---------------------------------------------------------------------------
// "Edit selection as text" — Monaco on one subtree, with a diff-confirmed Apply.
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
    // configs: view (dotted path), text (YAML), tree (the owning panel)

    initComponent: function () {
        let me = this;
        me.original = me.text || '';
        me.title = Ext.String.format(
            gettext('Edit selection as text: {0}'),
            Ext.htmlEncode(me.view || gettext('(whole document)')),
        );

        Ext.apply(me, {
            items: [{ xtype: 'component', reference: 'mount', style: 'height:100%;width:100%' }],
            tbar: [
                {
                    xtype: 'segmentedbutton',
                    reference: 'langbtn',
                    value: 'yaml',
                    // `ui` per item, not the container's `defaultUI`: the latter only
                    // reaches a child that has no `ui` of its own, and the theme's
                    // plain `default` is PVE's blue primary button - far too loud for
                    // a view switch sitting in a toolbar of grey buttons.
                    items: [
                        { text: 'YAML', value: 'yaml', ui: 'default-toolbar' },
                        { text: 'JSON', value: 'json', ui: 'default-toolbar' },
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
            PVE.meta.Monaco.dispose(me.editor);
            me.editor = null;
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
                    : PVE.meta.Utils.yamlLoad(me.editor.getValue());
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
                original = JSON.stringify(PVE.meta.Utils.yamlLoad(me.original), null, 2);
            } catch (_err) {
                lang = 'yaml'; // cannot render the original as JSON; diff the YAML
            }
        }
        if (edited === original) {
            Ext.Msg.alert(gettext('Notice'), gettext('No changes.'));
            return;
        }
        PVE.meta.Monaco.confirmDiff({
            title: me.view || gettext('(whole document)'),
            original: original,
            modified: edited,
            lang: lang,
            apply: () => me.apply(edited, me.lang),
        });
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
// The panel: a card layout over the tree and the full-document text editor.
// ---------------------------------------------------------------------------

Ext.define('PVE.meta.TreePanel', {
    extend: 'Ext.panel.Panel',
    xtype: 'pveMetaTreePanel',

    layout: 'card',
    border: false,

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
        me.namespaces = [];
        me.grants = [];
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
            items: [me.buildTreeCard(), me.buildTextCard()],
        });
        me.callParent();

        me.tree = me.down('#metaTree');
        me.on('afterrender', function () {
            me.syncButtons();
            me.reload();
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
                    if (e.getKey() === e.ENTER && rec && rec.data.kind !== 'map') {
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

    buildTextCard: function () {
        let me = this;
        return {
            xtype: 'panel',
            itemId: 'metaText',
            layout: 'fit',
            border: false,
            items: [{ xtype: 'component', itemId: 'metaTextMount', style: 'height:100%;width:100%' }],
            bbar: [
                {
                    xtype: 'segmentedbutton',
                    itemId: 'textLangBtn',
                    value: 'yaml',
                    items: [
                        { text: 'YAML', value: 'yaml', ui: 'default-toolbar' },
                        { text: 'JSON', value: 'json', ui: 'default-toolbar' },
                    ],
                    listeners: { change: (btn, value) => me.switchTextLang(value) },
                },
                '->',
                {
                    text: gettext('Format'),
                    itemId: 'textFormatBtn',
                    iconCls: 'fa fa-indent',
                    tooltip: gettext('Re-indent the buffer canonically'),
                    handler: () => me.formatText(),
                },
                {
                    text: gettext('Apply'),
                    itemId: 'textApplyBtn',
                    iconCls: 'fa fa-check',
                    handler: () => me.applyText(),
                },
                {
                    text: gettext('Discard'),
                    itemId: 'textDiscardBtn',
                    iconCls: 'fa fa-undo',
                    handler: () => me.discardText(),
                },
            ],
        };
    },

    buildToolbar: function () {
        let me = this;
        return [
            {
                text: gettext('Add'),
                itemId: 'addBtn',
                iconCls: 'fa fa-plus',
                handler: () => me.addKey(me.addTarget()),
            },
            {
                text: gettext('Edit'),
                itemId: 'editBtn',
                iconCls: 'fa fa-pencil',
                disabled: true,
                handler: () => me.editRow(me.getSelection()[0]),
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
        ];
    },

    buildColumns: function () {
        let U = PVE.meta.Utils;
        let fade = (rec, html) => (rec.data.present ? html : '<span class="faded">' + html + '</span>');
        // `html` is already content-encoded; this only escapes it for the attribute.
        let tip = function (meta, html) {
            if (html) {
                meta.tdAttr = 'data-qtip="' + Ext.htmlEncode(html) + '"';
            }
        };
        // The grammar's description is the tooltip of every cell in the row (DESIGN §8).
        let rowTip = (rec, meta) => tip(meta, Ext.htmlEncode(rec.data.grammarDescription || ''));
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
                    return fade(rec, Ext.htmlEncode(value));
                },
            },
            {
                text: gettext('Value'),
                dataIndex: 'valueText',
                flex: 3,
                renderer: function (value, meta, rec) {
                    rowTip(rec, meta);
                    if (rec.data.kind === 'map') {
                        return '';
                    }
                    return rec.data.present
                        ? Ext.htmlEncode(value)
                        : '<span class="faded">' + unsetText(rec) + '</span>';
                },
            },
            {
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
    addTarget: function () {
        let rec = this.getSelection()[0];
        return rec ? (rec.data.kind === 'map' ? rec.data.path : this.parentPath(rec)) : '';
    },

    syncButtons: function () {
        let me = this;
        let rec = me.getSelection()[0];
        let d = rec ? rec.data : null;
        let text = me.mode === 'text';
        let set = function (id, disabled) {
            let btn = me.down('#' + id);
            if (btn) {
                btn.setDisabled(disabled);
            }
        };
        set('addBtn', text || !me.editableFor(me.addTarget()));
        set('editBtn', text || !d || d.kind === 'map' || !d.editable);
        set('removeBtn', text || !d || !d.present || !d.editable);
        set('textSelBtn', text || !d);
        set('reloadBtn', text);
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
        if (!me.rendered || me.isDestroyed || me.editing || me.textWindow || me.mode === 'text') {
            return;
        }
        Proxmox.Utils.setErrorMask(me, true);
        me.loadNamespaces(() =>
            me.loadGrants(() =>
                me.loadTags(() =>
                    me.loadAccess(() =>
                        me.loadDocument(() => Proxmox.Utils.setErrorMask(me, false)),
                    ),
                ),
            ),
        );
    },

    // /meta/namespaces and /meta/grants are revision 6; against an older API they
    // simply fail and the Access column and the schema-declared rows stay empty,
    // rather than the page.
    loadNamespaces: function (next) {
        let me = this;
        me.request({
            url: '/meta/namespaces',
            success: function (response) {
                // Served most-specific first (DESIGN section 3.1) -- the order
                // `Utils.governing` relies on. Sorted again here so the UI does not
                // depend on the server's ordering for correctness.
                me.namespaces = PVE.meta.Utils.bySpecificity(response.result.data || []);
                next();
            },
            failure: function () {
                me.namespaces = [];
                next();
            },
        });
    },

    loadGrants: function (next) {
        let me = this;
        me.request({
            url: '/meta/grants',
            success: function (response) {
                me.grants = response.result.data || [];
                next();
            },
            failure: function () {
                me.grants = [];
                next();
            },
        });
    },

    // Only needed to resolve `selector: { tag: t }`, so only fetched when one exists.
    loadTags: function (next) {
        let me = this;
        me.tags = [];
        let hasTagSelector = (list, key) =>
            (list || []).some((e) => (e[key] || []).some((x) => x.selector && x.selector.tag));
        let needed =
            (me.namespaces || []).some((n) => n.selector && n.selector.tag) ||
            hasTagSelector(me.grants, 'grants');
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
                me.syncAccessLabel();
                next();
            },
        });
    },

    // The label is a restriction notice, so it says nothing at all for a caller with
    // full write access (DESIGN §8).
    syncAccessLabel: function () {
        let me = this;
        let modeBtn = me.down('#modeBtn');
        if (modeBtn && modeBtn.items.getAt(1)) {
            // The Text card is the whole document at the root view, and a scope-only
            // principal may not read that at all (DESIGN §3): do not offer it.
            modeBtn.items.getAt(1).setDisabled(!me.access.read);
        }
        let label = me.down('#accessText');
        if (!label) {
            return;
        }
        if (me.access.write) {
            label.setVisible(false);
            return;
        }
        let scoped = (me.access.scopes || []).some((s) => s.mode === 'rw');
        label.setText(scoped ? gettext('Scoped write access') : gettext('Read-only'));
        label.setVisible(true);
    },

    loadDocument: function (next) {
        let me = this;
        me.request({
            url: me.baseUrl,
            success: function (response) {
                let d = response.result.data || {};
                me.digest = d.digest || '';
                me.buildTree(d.data || {});
                me.syncButtons();
                next();
            },
        });
    },

    poll: function () {
        let me = this;
        if (!me.rendered || me.isDestroyed || me.editing || me.textWindow || me.mode === 'text') {
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

    // The grant entries whose selector matches this guest. Grants
    // apply to guest documents only (DESIGN §3), so the datacenter gets none.
    //
    // A `tag` selector is normally resolved against `me.tags` (from
    // `GET /meta/guests`), but that field is only populated for a caller with
    // VM.Audit (DESIGN §5). A scope-only principal never has it, so also accept
    // a scope `/meta/access` already resolved for us: that endpoint resolves
    // selectors server-side without requiring VM.Audit, so it still surfaces our
    // own declared rows and Access entries even when `me.tags` is empty. The
    // grant file is still the source of the label (name, selector text).
    // The namespaces that reach this guest, most-specific first. Namespaces decide
    // *shape*: which declared-but-unset rows appear and which schema governs a path.
    applicableNamespaces: function () {
        let me = this;
        if (me.dc) {
            return []; // namespaces apply to guest documents only (DESIGN section 3.3)
        }
        return (me.namespaces || []).filter(function (ns) {
            let sel = ns.selector || {};
            return sel.all || (sel.tag && me.tags.indexOf(sel.tag) !== -1);
        });
    },

    // The grant entries that reach this guest. Grants decide *access*, and unlike
    // namespaces they accumulate by containment: a grant on `homelab` covers
    // `homelab.docker` (DESIGN section 3.2).
    applicableGrants: function () {
        let me = this;
        let out = [];
        if (me.dc) {
            return out; // grants apply to guest documents only
        }
        (me.grants || []).forEach(function (grant) {
            (grant.grants || []).forEach(function (entry) {
                let sel = entry.selector || {};
                let matches =
                    entry.prefix &&
                    (sel.all ||
                        (sel.tag && me.tags.indexOf(sel.tag) !== -1) ||
                        me.resolvedScopeApplies(entry));
                if (matches) {
                    out.push(Ext.apply({ grant: grant }, entry));
                }
            });
        });
        return out;
    },

    // Every grant whose prefix covers this row, `rw` first. Several principals
    // may read a subtree; this is about who writes and who subscribes, not ownership.
    accessFor: function (path, scopes) {
        let U = PVE.meta.Utils;
        let out = [];
        // Registration names are operator-chosen strings, so no plain `{}` here.
        let seen = Object.create(null);
        scopes.forEach(function (s) {
            if (!U.covers(s.prefix, path)) {
                return;
            }
            let name = s.grant.name || s.grant.authid || '';
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
    // `namespaces`/`owner`, when given, enforce most-specific-wins: the walk stops
    // where a *different* namespace governs, so a parent's `properties` never reach
    // into a child namespace's subtree and rewrite its row kind. The same rule
    // `Lint.findings` and `Lint.schemaIndex` apply, through the same
    // `Utils.governing` -- it lived in two of the three and this was the one that
    // silently merged (DESIGN section 3.1).
    addGrammar: function (root, prefix, schema, namespaces, owner) {
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
                let childPath = U.joinPath(node.path, key);
                if (owner && U.governing(childPath, namespaces) !== owner) {
                    return; // a more specific namespace owns this subtree
                }
                let ps = sch.properties[key] || {};
                let child = me.entry(node, key, childPath);
                // The comment key stays the Description column; the grammar's own
                // description is the tooltip (DESIGN §8), so they are two fields.
                child.grammarDescription = child.grammarDescription || ps.description;
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
                if (ps.minimum !== undefined) {
                    child.minimum = ps.minimum;
                }
                if (ps.maximum !== undefined) {
                    child.maximum = ps.maximum;
                }
                if (ps.format !== undefined) {
                    child.format = ps.format;
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
        let I = PVE.meta.Icons;
        // Kept for text mode's grammar findings (annotateText): the parsed document the
        // server returned, so nothing has to re-read the YAML to know what is in it.
        me.docData = data;
        // Namespaces decide shape, grants decide access -- two lists, two rules
        // (DESIGN section 3). Declared rows come from namespaces only, and only from the
        // one governing each prefix: most-specific wins, schemas never merge.
        let namespaces = me.applicableNamespaces();
        let scopes = me.applicableGrants();
        let root = { key: '', path: '', children: Object.create(null), present: true, kind: 'map' };
        me.addData(root, data);
        namespaces.forEach(function (ns) {
            if (ns.schema) {
                me.addGrammar(root, ns.prefix, ns.schema, namespaces, ns);
            }
        });

        let toNodes = (entry) =>
            Object.keys(entry.children)
                .sort()
                .map(function (key) {
                    let c = entry.children[key];
                    let kind = c.kind || 'string';
                    let access = me.accessFor(c.path, scopes);
                    let node = {
                        key: key,
                        text: key,
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
                        rawValue: c.value,
                        valueText: c.present ? PVE.meta.Utils.displayValue(c.value, kind) : '',
                        accessList: access,
                        accessText: me.accessSummary(access),
                        editable: me.editableFor(c.path),
                        leaf: kind !== 'map',
                    };
                    if (kind === 'map') {
                        node.children = toNodes(c);
                        node.expanded = true;
                        node.iconCls = I.mapExpanded;
                        node.expandedCls = I.mapExpanded;
                    } else {
                        node.iconCls = I.leaf;
                    }
                    return node;
                });

        // Reloading (including from the version poll) must not fold the tree up.
        // Keyed by document path, so it gets the same treatment as `children`.
        let expanded = Object.create(null);
        let seen = false;
        me.store.getRoot().cascadeBy(function (n) {
            if (n.data.path && !n.isLeaf()) {
                seen = true;
                if (n.isExpanded()) {
                    expanded[n.data.path] = true;
                }
            }
        });
        me.store.setRoot({ expanded: true, children: toNodes(root) });
        if (seen) {
            me.store.getRoot().cascadeBy(function (n) {
                if (n.data.path && !n.isLeaf() && !expanded[n.data.path]) {
                    n.collapse();
                    n.set('iconCls', PVE.meta.Icons.map);
                }
            });
        }
    },

    // --- editing ------------------------------------------------------------

    editRow: function (rec) {
        let me = this;
        if (!rec || rec.data.kind === 'map' || !rec.data.editable) {
            return;
        }
        me.editing = true;
        let win = Ext.create('PVE.meta.EditValueWindow', { rec: rec });
        win.on('setvalue', (value) =>
            me.write({ view: rec.data.path, mode: 'replace', data: Ext.encode(value), digest: me.digest }),
        );
        win.on('destroy', function () {
            me.editing = false;
        });
        win.show();
    },

    addKey: function (parentPath) {
        let me = this;
        me.editing = true;
        let win = Ext.create('PVE.meta.AddKeyWindow', { parentPath: parentPath || '' });
        win.on('addkey', (path, value) =>
            me.write({ view: path, mode: 'replace', data: Ext.encode(value), digest: me.digest }),
        );
        win.on('destroy', function () {
            me.editing = false;
        });
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

    // "Edit selection as text": Monaco on the selected subtree, in its own window.
    editSelectionAsText: function () {
        let me = this;
        let rec = me.getSelection()[0];
        if (!rec) {
            return;
        }
        let view = rec.data.kind === 'map' ? rec.data.path : me.parentPath(rec);
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
            return me.textEditor.getValue() !== me.textRendered(me.textLang);
        } catch (_err) {
            return true; // cannot tell: assume there is something to lose
        }
    },

    // The loaded document rendered in `lang`; the diff's "original" side and the
    // yardstick the dirty check uses.
    textRendered: function (lang) {
        let me = this;
        if (lang !== 'json') {
            return me.textOriginal;
        }
        return JSON.stringify(PVE.meta.Utils.yamlLoad(me.textOriginal), null, 2);
    },

    enterTextMode: function () {
        let me = this;
        me.mode = 'text';
        me.syncButtons();
        me.getLayout().setActiveItem(me.down('#metaText'));
        Proxmox.Utils.setErrorMask(me, true);
        me.request({
            url: me.baseUrl,
            params: { view: '', format: 'yaml' },
            success: function (response) {
                let d = response.result.data || {};
                me.digest = d.digest || me.digest;
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
                    value: me.textOriginal,
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

    leaveTextMode: function () {
        let me = this;
        let finish = function () {
            PVE.meta.Monaco.dispose(me.textEditor);
            me.textEditor = null;
            me.mode = 'tree';
            me.setModeButton('tree');
            me.getLayout().setActiveItem(me.down('#metaTree'));
            me.syncButtons();
            me.reload();
        };
        if (!me.textIsDirty()) {
            finish();
            return;
        }
        me.setModeButton('text'); // stay put until the question is answered
        Ext.Msg.confirm(
            gettext('Confirm'),
            gettext('Discard the unapplied changes in the text editor?'),
            (btn) => (btn === 'yes' ? finish() : undefined),
        );
    },

    // Presentation only, exactly like the selection window's toggle.
    switchTextLang: function (lang) {
        let me = this;
        let btn = me.down('#textLangBtn');
        if (!me.textEditor || lang === me.textLang) {
            return;
        }
        let value;
        try {
            value =
                me.textLang === 'json'
                    ? Ext.decode(me.textEditor.getValue())
                    : PVE.meta.Utils.yamlLoad(me.textEditor.getValue());
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
            btn.setValue(me.textLang);
            btn.resumeEvents();
            return;
        }
        me.textLang = lang;
        window.monaco.editor.setModelLanguage(me.textEditor.getModel(), lang);

        let rendered;
        if (lang === 'json') {
            rendered = JSON.stringify(value, null, 2);
        } else {
            // Back to YAML: prefer the server's own text when the document is
            // unchanged. js-yaml and serde_yaml lay the same document out
            // differently (indentation of nested sequences, quoting), so re-dumping
            // here made a *presentation* toggle report unsaved changes and offer an
            // Apply whose only content was whitespace.
            rendered = PVE.meta.Utils.sameDocument(value, me.textOriginal)
                ? me.textOriginal
                : PVE.meta.Utils.yamlDump(value);
        }
        me.textEditor.setValue(rendered);
        me.annotateText();
    },

    // Re-dump the buffer canonically in whichever language is showing: two-space
    // indent, no folding, key order preserved. For hand-written YAML that has drifted
    // from the store's own layout, and it is the same dumper the JSON/YAML toggle uses,
    // so formatting then toggling is a no-op.
    //
    // Refuses on a buffer that does not parse rather than mangling it -- the squiggle
    // already says where.
    formatText: function () {
        let me = this;
        if (!me.textEditor) {
            return;
        }
        let text = me.textEditor.getValue();
        try {
            let value =
                me.textLang === 'json' ? Ext.decode(text) : PVE.meta.Utils.yamlLoad(text);
            let formatted =
                me.textLang === 'json'
                    ? JSON.stringify(value, null, 2)
                    : PVE.meta.Utils.yamlDump(value);
            if (formatted !== text) {
                me.textEditor.setValue(formatted);
                me.annotateText();
            }
        } catch (err) {
            Ext.Msg.alert(
                gettext('Cannot format'),
                Ext.htmlEncode(PVE.meta.Utils.errText(err)),
            );
        }
    },

    applyText: function () {
        let me = this;
        if (!me.textEditor) {
            return;
        }
        let lang = me.textLang;
        let edited = me.textEditor.getValue();
        let original;
        try {
            original = me.textRendered(lang);
        } catch (_err) {
            lang = 'yaml'; // cannot render the original as JSON; diff the YAML
            original = me.textOriginal;
        }
        if (edited === original) {
            Ext.Msg.alert(gettext('Notice'), gettext('No changes.'));
            return;
        }
        PVE.meta.Monaco.confirmDiff({
            title: gettext('(whole document)'),
            original: original,
            modified: edited,
            lang: lang,
            // Advisory: the banner and the tick make a schema mismatch a deliberate
            // act, they do not forbid it. The server's lint decides what is storable
            // (DESIGN section 4), and an operator whose grammar has drifted from what
            // a document legitimately holds must not be able to lock the administrator
            // out of editing it.
            warnings: me.textFindings(),
            apply: function () {
                // The whole document, at the root view. JSON is a subset of YAML, but
                // `data` is the parameter that says "this is the JSON data model".
                let params = { view: '', mode: 'replace', digest: me.digest };
                params[me.textLang === 'json' ? 'data' : 'text'] = edited;
                me.submit({ url: me.baseUrl, method: 'PUT', params: params }, () => me.refreshText());
            },
        });
    },

    discardText: function () {
        let me = this;
        if (!me.textIsDirty()) {
            me.refreshText();
            return;
        }
        Ext.Msg.confirm(
            gettext('Confirm'),
            gettext('Discard the unapplied changes in the text editor?'),
            (btn) => (btn === 'yes' ? me.refreshText() : undefined),
        );
    },

    // Re-read the document and put it back in the buffer (after Apply, or Discard).
    // Underline what is wrong with the buffer *as it is now*, and describe the key on
    // each declared line on hover.
    //
    // Two kinds of finding, both advisory -- Apply is never blocked, the server's lint
    // is the authority (DESIGN section 4):
    //
    //   * a YAML syntax error, as one Error marker on the line js-yaml reports. Monaco
    //     ships a JSON language service that does this for the JSON view already, but
    //     nothing validates YAML, so this is ours.
    //   * every grammar finding, as Warning markers (PVE.meta.Lint).
    //
    // This runs on every keystroke (onDidChangeModelContent), against the *buffer* --
    // not against the document the server last sent. Parsing is js-yaml on a document
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
            parsed = me.textLang === 'json' ? Ext.decode(text) : PVE.meta.Utils.yamlLoad(text);
        } catch (err) {
            parseError = err;
        }

        if (parseError) {
            if (me.textLang === 'yaml') {
                // js-yaml's YAMLException carries a 0-based mark; anything else lands
                // on line 1 rather than nowhere.
                let mark = parseError.mark || {};
                let line = typeof mark.line === 'number' ? mark.line + 1 : 1;
                line = Math.min(Math.max(line, 1), model.getLineCount());
                markers.push({
                    startLineNumber: line,
                    endLineNumber: line,
                    startColumn: typeof mark.column === 'number' ? mark.column + 1 : 1,
                    endColumn: model.getLineMaxColumn(line),
                    message: parseError.reason || PVE.meta.Utils.errText(parseError),
                    severity: monaco.MarkerSeverity.Error,
                });
            }
        } else if (me.textLang === 'yaml') {
            let applicable = PVE.meta.Lint.applicable(me.applicableNamespaces());
            if (applicable.length) {
                let index = PVE.meta.Lint.lineIndex(text);
                markers = PVE.meta.Lint.placed(
                    PVE.meta.Lint.findings(parsed, applicable),
                    index,
                ).map(function (f) {
                    return {
                        startLineNumber: f.line,
                        endLineNumber: f.line,
                        startColumn: 1,
                        endColumn: model.getLineMaxColumn(f.line),
                        message: f.message,
                        severity: monaco.MarkerSeverity.Warning,
                    };
                });
                let schemas = PVE.meta.Lint.schemaIndex(applicable);
                Object.keys(schemas).forEach(function (path) {
                    let hover = PVE.meta.Lint.hoverText(schemas[path]);
                    if (hover && index[path] !== undefined) {
                        hovers[index[path]] = hover;
                    }
                });
            }
        }

        monaco.editor.setModelMarkers(model, 'pve-meta', markers);
        me.textHovers = hovers;
        me.registerTextHover();
    },

    // The grammar findings for the current buffer, as plain messages -- what Apply
    // warns about before it writes. Empty when the buffer does not parse (the write
    // will fail on its own) or when no grammar applies.
    textFindings: function () {
        let me = this;
        if (!me.textEditor) {
            return [];
        }
        let applicable = PVE.meta.Lint.applicable(me.applicableNamespaces());
        if (!applicable.length) {
            return [];
        }
        try {
            let value =
                me.textLang === 'json'
                    ? Ext.decode(me.textEditor.getValue())
                    : PVE.meta.Utils.yamlLoad(me.textEditor.getValue());
            return PVE.meta.Lint.findings(value, applicable).map((f) => f.path + ': ' + f.message);
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
            url: me.baseUrl,
            params: { view: '', format: 'yaml' },
            success: function (response) {
                let d = response.result.data || {};
                me.digest = d.digest || me.digest;
                me.textOriginal = d.text || '';
                if (me.textEditor) {
                    me.textEditor.setValue(me.textRendered(me.textLang));
                    me.annotateText();
                }
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
});
