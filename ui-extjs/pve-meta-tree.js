/*
 * pve-meta-tree.js — the native ExtJS implementation of the pve-meta editor.
 *
 * One panel with two cards (DESIGN.md §5, §7, §8):
 *
 *   Tree — an Ext.tree.Panel with columns Key | Value | Description | Access over the
 *     document the caller can see. Rows are the union of the keys present in the
 *     document and the keys the governing prefixes declare (`GET /meta/prefixes`,
 *     matched by prefix and selector against this guest, most-specific first -- schemas
 *     shadow, they never merge); a declared-but-unset key
 *     renders faded with its default, and "setting" it is just editing it. Map rows
 *     carry a folder icon (open when expanded), value rows a document icon, both at the
 *     size and colour of the PVE resource tree. Comment keys (`k__`, and the bare `__`
 *     for the map itself) are not rows — `k__` is the Description of row `k`. Arrays are
 *     one text leaf. Access lists every permission whose prefix covers the row.
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
    // This is the ONLY implementation of the format set anywhere: the server passes a
    // prefix's `schema` through verbatim and never validates `format` (DESIGN §4 --
    // the lint is the authority, a schema is an affordance). It used to be mirrored in
    // a second UI that no longer exists (git tag pwt-ui-removed), so there is nothing
    // to keep in sync; adding a format is a change here and in DESIGN §8's list.
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

    // --- staged edits ------------------------------------------------------
    //
    // A row edit used to be a write. That works until a document has a rule spanning
    // two keys, and then it does not work at all: a prefix definition's selector is
    // *exactly one of* `all` or `tag` (DESIGN §3.1), so changing `{all: true}` into
    // `{tag: web}` has no legal one-key step. Dropping `all` first is refused, adding
    // `tag` first is refused, and the row editor could only ever do one at a time --
    // the field was uneditable from the tree, with no error that said why.
    //
    // So edits are staged and applied together, the way the text editor has always
    // worked: the tree shows what the document *would* be, and one Apply writes it.
    // Anything with a cross-key rule needs this; nothing loses by it.

    // Sets `path` in `doc`, creating the maps along the way.
    //
    // `defineProperty`, not assignment: keys are document data and no key is
    // reserved (DESIGN §4), so `doc['__proto__'] = v` would set the prototype rather
    // than a key. The same reason `entry()` builds with `Object.create(null)`.
    setAtPath: function (doc, path, value) {
        let segs = String(path).split('.');
        let cur = doc;
        for (let i = 0; i < segs.length - 1; i++) {
            let k = segs[i];
            let next = Object.prototype.hasOwnProperty.call(cur, k) ? cur[k] : undefined;
            if (!next || typeof next !== 'object' || Array.isArray(next)) {
                next = {};
                Object.defineProperty(cur, k, {
                    value: next,
                    writable: true,
                    enumerable: true,
                    configurable: true,
                });
            }
            cur = next;
        }
        Object.defineProperty(cur, segs[segs.length - 1], {
            value: value,
            writable: true,
            enumerable: true,
            configurable: true,
        });
    },

    deleteAtPath: function (doc, path) {
        let segs = String(path).split('.');
        let cur = doc;
        for (let i = 0; i < segs.length - 1; i++) {
            let k = segs[i];
            if (!cur || typeof cur !== 'object' || !Object.prototype.hasOwnProperty.call(cur, k)) {
                return;
            }
            cur = cur[k];
        }
        if (cur && typeof cur === 'object') {
            delete cur[segs[segs.length - 1]];
        }
    },

    // The document as it would be once the staged edits are applied. This is what
    // the tree renders, what the schema markers are computed from, and what Apply
    // writes -- one planned document, so what you see is what is sent.
    applyPending: function (data, pending) {
        let U = PVE.meta.Utils;
        let out = JSON.parse(JSON.stringify(data || {}));
        (pending || []).forEach(function (p) {
            if (p.op === 'delete') {
                U.deleteAtPath(out, p.path);
            } else {
                U.setAtPath(out, p.path, p.value);
            }
        });
        return out;
    },

    // Rolls a set of `path -> message` facts up to every ancestor path.
    //
    // A collapsed branch hides everything under it, so a marker that only ever sits
    // on the offending row is a marker you cannot see: collapse `homelab` and the
    // amber `port` disappears along with the fact that something is wrong. Ancestors
    // therefore carry a count of what is beneath them, and the first few messages,
    // which is what their tooltip says.
    rollUp: function (byPath) {
        let out = Object.create(null);
        Object.keys(byPath || {}).forEach(function (path) {
            let segs = path.split('.');
            for (let i = 1; i < segs.length; i++) {
                let ancestor = segs.slice(0, i).join('.');
                let at = out[ancestor] || (out[ancestor] = { count: 0, messages: [] });
                at.count++;
                if (at.messages.length < 3) {
                    at.messages.push(path + ': ' + byPath[path]);
                }
            }
        });
        return out;
    },

    // The narrowest view that covers every staged path -- the write Apply sends.
    //
    // One write, because the whole point is that the intermediate states are the
    // ones the server refuses. Narrow, because a root write needs full write access
    // while a scoped principal may hold only its own prefix (DESIGN §3.4), and
    // because a write that names less is a write that can collide with less.
    //
    // A delete cannot be expressed by replacing the thing being deleted, so a staged
    // delete at the common ancestor moves the write one level up: the parent is
    // replaced with a copy that no longer has the key.
    writeView: function (pending) {
        let list = pending || [];
        if (!list.length) {
            return null;
        }
        let common = list[0].path.split('.');
        list.slice(1).forEach(function (p) {
            let segs = p.path.split('.');
            let i = 0;
            while (i < common.length && i < segs.length && common[i] === segs[i]) {
                i++;
            }
            common = common.slice(0, i);
        });
        let view = common.join('.');
        if (view !== '' && list.some((p) => p.op === 'delete' && p.path === view)) {
            let segs = view.split('.');
            segs.pop();
            view = segs.join('.');
        }
        return view;
    },

    // A schema `type` as the kind `parseValue` speaks: both integers and numbers are
    // parsed as numbers, and a map or a list of one is not a scalar to parse at all.
    schemaValueKind: function (type) {
        if (type === 'integer' || type === 'number') {
            return 'number';
        }
        return type === 'boolean' || type === 'array' ? type : 'string';
    },

    // Which editor a row's *shape* calls for: 'inline' (the one-line row editor),
    // 'multiline' (a textarea) or 'text' (Monaco on that subtree).
    //
    // One function, because four places have to agree on it -- the Edit button's
    // enabled state, the double-click, the Enter key and the window that opens -- and
    // when they did not, a map row was simply not editable by any of the three
    // gestures while the toolbar's "Edit selection as text" quietly did the job.
    //
    // The rule is the value's shape, not a declaration: **a value with structure
    // inside it is edited as text**. A map is nested YAML and belongs in Monaco; so is
    // an array of maps. An array of scalars stays a one-line leaf (`[lan, wan]` reads
    // and edits fine). A string is one line unless it has newlines in it -- or the
    // schema said so with `multiline`, which is the only way to know before the
    // first value exists. Adding a `nested yaml` *type* instead would be the string
    // blob wearing a hat: it costs the per-key rows, diffs and writes that nesting is
    // for (docs/DESIGN.md §8).
    editorKind: function (d) {
        if (!d) {
            return 'none';
        }
        if (d.kind === 'map') {
            return 'text';
        }
        if (d.kind === 'array') {
            let list = Array.isArray(d.rawValue) ? d.rawValue : [];
            return list.some((v) => v !== null && typeof v === 'object') ? 'text' : 'inline';
        }
        if (d.kind === 'string') {
            let raw = typeof d.rawValue === 'string' ? d.rawValue : '';
            return d.multiline || raw.indexOf('\n') !== -1 ? 'multiline' : 'inline';
        }
        return 'inline';
    },

    // What the Value column shows for a value that does not fit on a line. The row
    // keeps its exact `valueText` -- the editor opens on that, so truncating it here
    // and nowhere else is the difference between a summary and data loss.
    previewText: function (text) {
        let s = String(text === undefined || text === null ? '' : text);
        let nl = s.indexOf('\n');
        if (nl === -1) {
            return s;
        }
        let rest = s.slice(nl + 1).replace(/\n+$/, '');
        let more = rest === '' ? 0 : rest.split('\n').length;
        let first = s.slice(0, nl);
        return more ? first + ' ' + Ext.String.format(gettext('(+{0} lines)'), more) : first;
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
        if (PVE.meta.Utils.editorKind(d) === 'multiline') {
            // No `format` here: a PVE format validates one line (an address, a name),
            // and none of them describe a block of text.
            return { xtype: 'textarea', height: 260, grow: false };
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

    // The prefix governing `path`: the one whose prefix is the LONGEST that covers
    // it. Most-specific wins and schemas never merge (DESIGN section 3.1).
    //
    // The single implementation of that rule on this side. It had three call sites --
    // the row builder, the linter and the hover index -- and lived in two of them; the
    // third simply did not prune, so a parent prefix's `properties` reached into a
    // child prefix's subtree and set its row kind. One function, three callers.
    //
    // `prefixes` must be sorted longest-prefix-first, so this is the first match.
    governing: function (path, prefixes) {
        let list = prefixes || [];
        for (let i = 0; i < list.length; i++) {
            if (PVE.meta.Utils.containsPath(list[i].prefix, path)) {
                return list[i];
            }
        }
        return null;
    },

    // Plain containment: `p` itself, or anything under `p.`. Deliberately NOT
    // `covers`, which additionally aliases the sibling comment key `p__` -- that is a
    // *permission* rule (a scope on `p` may write the note about `p`), and it does not
    // belong here. With prefixes `a` and `a__` both declared, `covers` would have
    // said `a` governs the whole `a__` prefix; Rust's `registry::governing` uses
    // plain containment and would have said `a__`. Two predicates, two jobs.
    containsPath: (p, path) => path === p || path.indexOf(p + '.') === 0,

    // Longest prefix first, then by name: the order `governing` relies on.
    bySpecificity: function (prefixes) {
        return (prefixes || []).slice().sort(function (a, b) {
            let d = PVE.meta.Utils.depth(b.prefix) - PVE.meta.Utils.depth(a.prefix);
            // Byte order, matching Rust's `String::cmp` -- `localeCompare` orders
            // `@`, `!`, `_` and mixed case differently, and a mirror that sorts
            // differently is a mirror that will eventually decide differently.
            return d !== 0 ? d : (a.prefix < b.prefix ? -1 : a.prefix > b.prefix ? 1 : 0);
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
        // JSON_SCHEMA, not js-yaml's DEFAULT_SCHEMA. The default resolves implicit
        // timestamps, so `2020-01-01` parses to a JS Date and re-serialises as
        // "2020-01-01T00:00:00.000Z" -- a *presentation-only* view toggle, or the
        // Format button, would silently rewrite the stored value. The server keeps it
        // a string (`serde_yaml_ng`, verified), and this must agree with the server
        // about what a document *is*.
        //
        // js-yaml still accepts anchors, aliases and explicit tags, which the store
        // refuses (format.rs's YAML safety scan). That divergence is one the server
        // catches -- an Apply carrying them is a 400 -- so it is left alone rather
        // than reimplementing that scan here.
        let doc = PVE.meta.Utils.yamlLib().load(String(text), {
            schema: PVE.meta.Utils.yamlLib().JSON_SCHEMA,
        });
        // An empty document is the empty map, not a null: the model has no nulls.
        return doc === undefined || doc === null ? {} : doc;
    },

    yamlDump: function (value) {
        return PVE.meta.Utils.yamlLib().dump(value, {
            schema: PVE.meta.Utils.yamlLib().JSON_SCHEMA, // as yamlLoad, for the same reason
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
// The only implementation of these rules: the second UI this used to mirror is gone
// (git tag pwt-ui-removed). What it must still agree with is the *server* -- the value
// model (PVE.meta.Utils.yamlLoad uses JSON_SCHEMA so a bare date stays a string, as
// the store keeps it) and the coverage rule (testdata/covers-cases.json, read by this
// suite and by scopes.rs). Findings themselves are advisory: DESIGN §4 makes the
// server's single lint the authority on what is storable.
//
// Two halves, kept apart on purpose. `findings()` answers *what is wrong*, from the
// parsed document the panel already holds; `lineIndex()` answers *where to draw it*,
// by scanning the YAML the server returned. The pairing only holds while the buffer
// still is what the server sent, so the caller clears both once it is dirty.
// ---------------------------------------------------------------------------

PVE.meta.Lint = {
    // The prefixes that carry a schema, longest prefix first. Shape comes from
    // prefixes, never from permissions (DESIGN section 3.1).
    applicable: function (prefixes) {
        return PVE.meta.Utils.bySpecificity(
            (prefixes || []).filter((ns) => ns && ns.schema && ns.prefix),
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

    // `withSchema` is what to walk; `all` is what *shadows*, which is every applicable
    // prefix whether or not it carries a schema. Two jobs, two lists: a prefix
    // with a selector and no schema (the lab's `netbird`) still governs its subtree, so
    // reusing the filtered list for pruning let a parent's schema reach into a
    // schema-less child. Defaults to `withSchema` only for callers that have no
    // schema-less prefixes to worry about.
    findings: function (data, withSchema, all) {
        let out = [];
        let list = withSchema || [];
        let shadow = all && all.length ? PVE.meta.Utils.bySpecificity(all) : list;
        list.forEach(function (ns) {
            let value = PVE.meta.Lint.valueAt(data, ns.prefix);
            if (value === undefined) {
                return;
            }
            if (ns.prefix) {
                PVE.meta.Lint.walk(value, ns.schema, ns.prefix, out, shadow, ns);
            } else {
                // The empty prefix is the document itself -- a registry document's
                // meta-schema (DESIGN §3.6). It governs everything and shadows
                // nothing, so it is walked with no owner: `governing` answers about
                // prefixes, the empty prefix is not one, and passing an owner here
                // would prune every path in the document.
                PVE.meta.Lint.walk(value, ns.schema, '', out, [], null);
            }
        });
        out.sort((a, b) => (a.path < b.path ? -1 : a.path > b.path ? 1 : 0));
        return out;
    },

    // `list`/`owner`, when given, enforce most-specific-wins: the walk stops where a
    // *different* prefix governs, so a parent's schema never reaches into a child
    // prefix's subtree. Schemas shadow, they do not merge (DESIGN section 3.1).
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
                    return; // a more specific prefix owns this subtree
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

    // Every schema node by document path -- the hover index. Pruned the same way as
    // `findings`, so a path covered by two prefixes resolves to the more specific
    // one rather than to whichever was collected last.
    schemaIndex: function (withSchema, all) {
        let out = Object.create(null);
        let list = withSchema || [];
        let shadow = all && all.length ? PVE.meta.Utils.bySpecificity(all) : list;
        let collect = function (schema, path, owner) {
            if (owner && PVE.meta.Utils.governing(path, shadow) !== owner) {
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
        // `ns.prefix || null` as the owner: the same root-schema case as `findings`.
        list.forEach((ns) => collect(ns.schema, ns.prefix, ns.prefix ? ns : null));
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
    // before anything is written. Shared by the selection window, the Text card and
    // the tree's Apply.
    //
    // cfg: { title, original, modified, lang, warnings, apply }, or the same with
    // `originalValue`/`modifiedValue` -- documents, rendered here as YAML.
    //
    // **It loads Monaco itself.** It cannot draw without it, every caller had to
    // remember to, and the one that forgot threw "YAML support is not loaded" before
    // it could reach the server -- with edits staged and no way to apply them. A
    // function that needs a thing should get the thing; `Monaco.load()` is a cached
    // promise, so callers that already awaited it pay nothing.
    confirmDiff: function (cfg) {
        PVE.meta.Monaco.load().then(
            () => PVE.meta.Monaco.showDiffWindow(cfg),
            (err) => Ext.Msg.alert(gettext('Error'), Ext.htmlEncode(PVE.meta.Utils.errText(err))),
        );
    },

    showDiffWindow: function (cfg) {
        let state = {};
        // Rendering a document rather than a buffer: only reachable from here, which
        // is after the YAML codec is loaded.
        if (cfg.originalValue !== undefined || cfg.modifiedValue !== undefined) {
            cfg = Ext.apply({}, cfg);
            cfg.original = PVE.meta.Utils.yamlDump(cfg.originalValue);
            cfg.modified = PVE.meta.Utils.yamlDump(cfg.modifiedValue);
            cfg.lang = 'yaml';
        }
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
        { name: 'docId', type: 'string' }, // which document this row belongs to
        { name: 'path', type: 'string' }, // dotted; this is the `view` of a write
        { name: 'finding', type: 'string' }, // this row does not match its schema
        { name: 'belowCount', type: 'int' }, // findings somewhere beneath this row
        { name: 'belowText', type: 'string' }, // the first few of them, for the tooltip
        { name: 'stagedBelow', type: 'int' }, // staged edits somewhere beneath this row
        { name: 'multiline', type: 'boolean' }, // grammar `multiline` -> a text box
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
// "Declare Key" — the small form behind a prefix's `schema.properties.<key>`.
//
// A prefix definition's schema is a document like any other now, so this window writes one
// property of it with an ordinary view PUT; there is no second write path. It covers
// the seven things the editor actually consumes (type, description, optional,
// default, enum, minimum/maximum, format) plus `multiline`. Anything beyond that --
// nested `properties`, a shape this form has no field for -- is what "Edit as text"
// is for, and the schema row opens Monaco on itself.
//
// The key is not validated here on purpose: `schema.properties.<key>` is a document
// path like any other, so the server's one lint decides what a key may be and says
// so (DESIGN §4). A *dotted* key is refused, because that would silently declare a
// nested property rather than the one the form is asking about.
//
// There is deliberately **no Optional field**. Every key of a guest document is
// optional: nothing in pve-meta ever requires one, so `optional: 0` would be a claim
// no code reads and no write enforces. A missing value is a legitimate state -- an
// operator fills it in, or there is a reason it is not there -- and a `default` is an
// offer the row makes ("Set to default"), never something written behind your back.
// (`optional` survives in the meta-schema, DESIGN §3.6, because *those* files really do
// have required fields: a permission file without an `authid` is refused on the way in.)
// ---------------------------------------------------------------------------

Ext.define('PVE.meta.DeclareKeyWindow', {
    extend: 'Ext.window.Window',
    xtype: 'pveMetaDeclareKeyWindow',

    title: gettext('Declare Key'),
    modal: true,
    width: 520,
    layout: 'fit',
    prefix: '', // the prefix this declaration is for, shown in the header

    initComponent: function () {
        let me = this;
        let numeric = () => ['integer', 'number'].indexOf(me.down('[name=type]').getValue()) !== -1;
        let isString = () => me.down('[name=type]').getValue() === 'string';
        let sync = function () {
            let type = me.down('[name=type]').getValue();
            ['minimum', 'maximum'].forEach((n) => me.down('[name=' + n + ']').setDisabled(!numeric()));
            me.down('[name=format]').setDisabled(!isString());
            me.down('[name=multiline]').setDisabled(!isString());
            // A boolean picks its default from a list; a map has no default the
            // editor would ever read (`addGrammar` stops at an object and walks into
            // it), so offering one would be a field that does nothing.
            me.down('[name=defaultBool]').setHidden(type !== 'boolean');
            let plain = me.down('[name=default]');
            plain.setHidden(type === 'boolean');
            plain.setDisabled(type === 'object');
        };
        Ext.apply(me, {
            items: [
                {
                    xtype: 'form',
                    reference: 'form',
                    bodyPadding: 10,
                    border: false,
                    defaults: { anchor: '100%', labelWidth: 120 },
                    items: [
                        {
                            xtype: 'displayfield',
                            fieldLabel: gettext('Prefix'),
                            value: Ext.htmlEncode(me.prefix),
                        },
                        {
                            xtype: 'textfield',
                            name: 'key',
                            allowBlank: false,
                            fieldLabel: gettext('Key'),
                            emptyText: gettext('the key this declares'),
                        },
                        {
                            xtype: 'proxmoxKVComboBox',
                            name: 'type',
                            fieldLabel: gettext('Type'),
                            value: 'string',
                            comboItems: [
                                ['string', gettext('String')],
                                ['integer', gettext('Integer')],
                                ['number', gettext('Number')],
                                ['boolean', gettext('Boolean')],
                                ['object', gettext('Map')],
                                ['array', gettext('Array')],
                            ],
                            listeners: { change: sync },
                        },
                        { xtype: 'textfield', name: 'description', fieldLabel: gettext('Description') },
                        // Two fields, one label: a boolean default typed into a text
                        // box is a trap. `parseValue` reads truth the way the row
                        // editor's checkbox produces it -- `true`/`1` -- so "True" or
                        // "yes" would have been accepted and stored as `false`, the
                        // opposite of what was typed, into a cluster-wide file.
                        { xtype: 'textfield', name: 'default', fieldLabel: gettext('Default') },
                        {
                            xtype: 'proxmoxKVComboBox',
                            name: 'defaultBool',
                            fieldLabel: gettext('Default'),
                            hidden: true,
                            value: '',
                            comboItems: [
                                ['', gettext('none')],
                                ['true', gettext('Yes')],
                                ['false', gettext('No')],
                            ],
                        },
                        {
                            xtype: 'textfield',
                            name: 'enum',
                            fieldLabel: gettext('Values'),
                            emptyText: gettext('comma-separated; leave empty for any'),
                        },
                        { xtype: 'numberfield', name: 'minimum', fieldLabel: gettext('Minimum'), hideTrigger: true },
                        { xtype: 'numberfield', name: 'maximum', fieldLabel: gettext('Maximum'), hideTrigger: true },
                        {
                            xtype: 'proxmoxKVComboBox',
                            name: 'format',
                            fieldLabel: gettext('Format'),
                            value: '',
                            comboItems: [['', gettext('none')]].concat(
                                Object.keys(PVE.meta.Utils.FORMAT_VTYPES).map((f) => [f, f]),
                            ),
                        },
                        {
                            xtype: 'proxmoxcheckbox',
                            name: 'multiline',
                            fieldLabel: gettext('Multi-line'),
                            boxLabel: gettext('edit this string in a text box'),
                        },
                    ],
                },
            ],
            buttons: [
                { text: gettext('Declare'), handler: () => me.submit() },
                { text: gettext('Cancel'), handler: () => me.close() },
            ],
        });
        me.callParent();
        me.on('show', function () {
            sync();
            me.down('[name=key]').focus(true, 50);
        });
    },

    // The declaration, in the order a reader wants it, with every empty field left
    // out entirely: a schema full of nulls describes nothing and would not pass the
    // document lint anyway.
    schemaFrom: function (v) {
        let U = PVE.meta.Utils;
        let out = { type: v.type };
        if (v.description) {
            out.description = v.description;
        }
        let dflt = v.type === 'boolean' ? v.defaultBool : v.type === 'object' ? '' : v.default;
        if (dflt !== undefined && dflt !== '') {
            out.default = U.parseValue(dflt, U.schemaValueKind(v.type));
        }
        if (v.enum) {
            out.enum = String(v.enum)
                .split(',')
                .map((x) => x.trim())
                .filter((x) => x !== '');
        }
        ['minimum', 'maximum'].forEach(function (n) {
            if (['integer', 'number'].indexOf(v.type) !== -1 && v[n] !== undefined && v[n] !== '') {
                out[n] = Number(v[n]);
            }
        });
        if (v.type === 'string' && v.format) {
            out.format = v.format;
        }
        if (v.type === 'string' && v.multiline) {
            out.multiline = 1;
        }
        return out;
    },

    submit: function () {
        let me = this;
        let form = me.down('form').getForm();
        if (!form.isValid()) {
            return;
        }
        let v = form.getValues();
        let key = String(v.key).trim();
        try {
            if (key.indexOf('.') !== -1) {
                throw new Error(
                    gettext('A key is one name; edit the schema as text to nest one inside another'),
                );
            }
            me.fireEvent('declarekey', key, me.schemaFrom(v));
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
        if (U.editorKind(d) === 'multiline') {
            me.width = 640;
        }
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
        let params = {
            view: me.view || undefined,
            mode: 'replace',
            digest: me.tree.digestOf(me.docId),
        };
        params[lang === 'json' ? 'data' : 'text'] = text;
        me.tree.write(me.docId, params, () => me.close());
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
        // This panel is ONE document's editor, named by `docId`: a guest's, the
        // datacenter's, or a prefix/permission file's -- they are all documents (DESIGN
        // §3.5), so the same tree, markers, text editor and diff serve all three, and
        // the registry grids open one of these in a window rather than reimplementing
        // any of it.
        //
        // Rows still carry their document's id even though there is only ever one:
        // it is what every write threads through, and a panel that had to remember
        // which document it was on top of which row was selected is how the digest of
        // one document ends up on a write to another.
        me.docId = me.docId || (me.dc ? 'datacenter' : String(me.vmid));
        me.docState = Object.create(null); // id -> { digest, data }
        // Edits accumulate here until Apply, in the order they were made:
        // `{ path, op: 'set' | 'delete', value }`, at most one entry per path.
        me.pending = [];
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
                handler: function () {
                    let t = me.addTarget();
                    if (t) {
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
            {
                // The one write. Disabled until something is staged, so the tree has
                // exactly the shape the text editor has always had: edit freely,
                // then decide.
                text: gettext('Apply'),
                itemId: 'applyBtn',
                iconCls: 'fa fa-check',
                disabled: true,
                handler: () => me.applyPending(),
            },
            {
                text: gettext('Revert'),
                itemId: 'revertBtn',
                iconCls: 'fa fa-undo',
                disabled: true,
                handler: () => me.revertPending(),
            },
            { text: gettext('Reload'), itemId: 'reloadBtn', iconCls: 'fa fa-refresh', handler: () => me.reload() },
            '->',
            // How many edits are waiting. Shown only when there are any -- it is the
            // answer to "why is this row orange".
            { xtype: 'tbtext', itemId: 'pendingText', cls: 'warning', hidden: true },
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
        // The grammar's description is the tooltip of every cell in the row (DESIGN §8)
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
                    return (
                        (d.pending === 'delete' ? '' : stored) +
                        '<div style="color:darkorange">' + after + '</div>'
                    );
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

    // --- documents -----------------------------------------------------------

    // The API path of a document, from its id. Total by construction: a registry id
    // is `prefixes/<name>` -- the path it is served at -- and everything else is a
    // vmid or the literal `datacenter` (`api::parse_id`).
    urlFor: function (id) {
        if (id === 'datacenter') {
            return '/meta/datacenter';
        }
        return id.indexOf('/') === -1 ? '/meta/guests/' + id : '/meta/' + id;
    },

    // What kind of document an id names -- which decides what governs its rows.
    docKind: function (id) {
        if (id === 'datacenter') {
            return 'datacenter';
        }
        if (id.indexOf('prefixes/') === 0) {
            return 'prefix';
        }
        if (id.indexOf('permissions/') === 0) {
            return 'permission';
        }
        return 'guest';
    },

    docTitle: function (id) {
        let cut = id.indexOf('/');
        return cut === -1 ? id : id.slice(cut + 1);
    },

    // What describes one document, in the two lists `Lint.findings` wants: what to
    // walk, and what shadows. A guest document has prefixes (which shadow each
    // other); a registry document has one schema at its root (which shadows nothing,
    // so it is its own list); the datacenter document has neither.
    //
    // One function, two callers -- the tree's row markers and the text editor's --
    // because they are the same question asked twice, and the last four wrong-result
    // bugs in this file were all a rule with two implementations.
    grammarSplit: function (docId) {
        let all = this.grammarFor(docId);
        let rooted = all.filter((ns) => ns.schema && !ns.prefix);
        if (rooted.length) {
            return { all: rooted, withSchema: rooted };
        }
        return { all: all, withSchema: PVE.meta.Lint.applicable(all) };
    },

    // Every path in `docId` whose value does not match its schema, by path. The tree
    // shows these on the rows themselves: the text editor has squiggled them since
    // revision 6, but the tree is the view people actually open, and a value the
    // schema refuses looked exactly like one it liked.
    findingsFor: function () {
        let out = Object.create(null);
        let g = this.grammarSplit(this.docId);
        if (!g.withSchema.length) {
            return out;
        }
        // Against the *planned* document: a staged value that the schema refuses is
        // marked the moment it is staged, not after it has been written.
        PVE.meta.Lint.findings(this.plannedData(), g.withSchema, g.all).forEach(function (f) {
            out[f.path] = f.message;
        });
        return out;
    },

    // The digest to send with a write, and the parsed document to build rows from.
    // Per document, because a compare-and-swap is per document: one shared `digest`
    // field would have sent a prefix's digest with a write to the datacenter.
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

    // --- staged edits --------------------------------------------------------

    // Records one edit. A staged path replaces any earlier entry for itself *and*
    // for everything under it: staging `selector` after `selector.tag` means the
    // subtree was replaced wholesale, and keeping the older, narrower entry would
    // re-apply it on top of the new value.
    stage: function (path, op, value) {
        let me = this;
        let under = (p) => p === path || p.indexOf(path + '.') === 0;
        me.pending = me.pending.filter((e) => !under(e.path));
        me.pending.push({ path: path, op: op, value: value });
        me.buildTree();
        me.syncButtons();
    },

    isDirty: function () {
        return this.pending.length > 0;
    },

    // The document as it would be. Everything the tree shows is computed from this,
    // so a staged value is linted, hovered and diffed exactly like a stored one.
    plannedData: function () {
        return PVE.meta.Utils.applyPending(this.dataOf(this.docId), this.pending);
    },

    revertPending: function () {
        let me = this;
        if (!me.isDirty()) {
            return;
        }
        me.pending = [];
        me.buildTree();
        me.syncButtons();
    },

    // The document a row belongs to; the panel's default for anything with no row.
    docOf: function (rec) {
        return (rec && rec.data && rec.data.docId) || this.docId;
    },

    // What describes this document's shape. A guest document is described by the
    // prefixes that reach it, most-specific first (they shadow); a prefix or
    // permission file by the one meta-schema for its kind, rooted at the document itself;
    // the datacenter document by nothing at all -- prefixes are guest-only
    // (DESIGN §3.3), which is what keeps a prefix from painting rows onto it.
    grammarFor: function (id) {
        let me = this;
        let kind = me.docKind(id);
        if (kind === 'guest') {
            return me.applicablePrefixes();
        }
        if (kind !== 'prefix' && kind !== 'permission') {
            return []; // the datacenter document: nothing describes its shape
        }
        let schema = kind === 'prefix' ? me.schemas.prefix : me.schemas.permission;
        // A pseudo-prefix at the root. Its prefix is empty, so it governs the
        // whole document and there is nothing for it to shadow -- which is why it is
        // never passed as the shadowing list: `governing` answers about prefixes, and
        // the empty prefix is not one.
        return schema ? [{ prefix: '', schema: schema }] : [];
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
    addTarget: function () {
        let me = this;
        let rec = me.getSelection()[0];
        if (!rec) {
            return { docId: me.docId, path: '' };
        }
        return {
            docId: me.docOf(rec),
            path: rec.data.kind === 'map' ? rec.data.path : me.parentPath(rec),
        };
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
        let target = me.addTarget();
        set('addBtn', text || !target || !me.editableFor(target.path));
        let row = d;
        set('editBtn', text || !row || !row.editable);
        set('removeBtn', text || !row || !row.present || !row.editable);
        let dirty = me.isDirty();
        // The text editors write immediately -- "edit as text" *is* an apply. With
        // edits staged they would be showing the stored document while the tree
        // shows the planned one, so they wait until this is settled either way.
        set('textSelBtn', text || !row || dirty);
        set('reloadBtn', text);
        set('applyBtn', text || !dirty);
        set('revertBtn', text || !dirty);
        let count = me.down('#pendingText');
        if (count) {
            count.setHidden(!dirty);
            count.setText(
                dirty
                    ? Ext.String.format(
                          me.pending.length === 1
                              ? gettext('{0} unapplied change')
                              : gettext('{0} unapplied changes'),
                          me.pending.length,
                      )
                    : '',
            );
        }
        let dflt = me.down('#defaultBtn');
        if (dflt) {
            // Disabled, not hidden. What varies per *document* may hide (Declare Key
            // is a missing concept on a guest, not a missing permission); what varies
            // per *row* must not, or the buttons beside it shift under the pointer
            // every time the selection changes -- which is how you click Remove and
            // hit something else.
            let offers = !!row && !row.present && row.defaultValue !== undefined;
            dflt.setDisabled(text || !offers || !row.editable);
        }
        // The Text toggle's enabled state depends on staged edits too, and this is
        // the function that runs whenever those change.
        me.syncAccessLabel();
        let declare = me.down('#declareBtn');
        if (declare) {
            // Hidden by the *document*, disabled by the *row* -- the rule above. It
            // used to read `row.docId`, so with nothing selected it hid itself and
            // reappeared on the next click: a button that flickers as you move
            // through a tree, for a fact that cannot change while you are in it.
            declare.setHidden(me.docKind(me.docId) !== 'prefix');
            declare.setDisabled(text || !row || !row.editable);
        }
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
                        me.pending = [];
                        me.reload();
                    }
                },
            );
            return;
        }
        Proxmox.Utils.setErrorMask(me, true);
        me.loadPrefixes(() =>
            me.loadPermissions(() =>
                me.loadTags(() =>
                    me.loadAccess(() =>
                        me.loadSchemas(() =>
                            me.loadDocument(() => Proxmox.Utils.setErrorMask(me, false)),
                        ),
                    ),
                ),
            ),
        );
    },

    // /meta/prefixes and /meta/permissions are revision 6; against an older API they
    // simply fail and the Access column and the schema-declared rows stay empty,
    // rather than the page.
    loadPrefixes: function (next) {
        let me = this;
        me.request({
            url: '/meta/prefixes',
            success: function (response) {
                // Served most-specific first (DESIGN section 3.1) -- the order
                // `Utils.governing` relies on. Sorted again here so the UI does not
                // depend on the server's ordering for correctness.
                me.prefixes = PVE.meta.Utils.bySpecificity(response.result.data || []);
                next();
            },
            failure: function () {
                me.prefixes = [];
                next();
            },
        });
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

    // Only needed to resolve `selector: { tag: t }`, so only fetched when one exists.
    loadTags: function (next) {
        let me = this;
        me.tags = [];
        let hasTagSelector = (list, key) =>
            (list || []).some((e) => (e[key] || []).some((x) => x.selector && x.selector.tag));
        let needed =
            (me.prefixes || []).some((n) => n.selector && n.selector.tag) ||
            hasTagSelector(me.permissions, 'rules');
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
            // Ask about the document this panel is actually showing. `dc: 1` used to
            // stand in for "not a guest", which stopped being true the moment a
            // prefix or permission file could be the document: those are readable by every
            // authenticated user, and asking about the datacenter document instead
            // answered with Sys.Audit -- disabling Text mode on a file the caller may
            // certainly read (DESIGN §3.5).
            params: { id: me.docId },
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
            // principal may not read that at all (DESIGN §3): do not offer it. Nor
            // while edits are staged, which the tree is showing and the text card
            // would not be.
            modeBtn.items.getAt(1).setDisabled(!me.access.read || me.isDirty());
        }
        // A Text-mode Apply is a root replace, which needs full write and nothing else
        // (DESIGN §3.4, `authorize_view_write`). Without this a read-only caller could
        // compose a whole document, open the diff, tick through the schema warning and
        // collect a 403 at the very end -- the server was right, the button was a lie.
        let applyBtn = me.down('#textApplyBtn');
        if (applyBtn) {
            applyBtn.setDisabled(!me.access.write);
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

    // The meta-schema, once per load and only where it is used: it describes a
    // registry document, and a guest tab never shows one.
    loadSchemas: function (next) {
        let me = this;
        if (!me.dc) {
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
    loadDocument: function (next) {
        let me = this;
        me.request({
            url: me.urlFor(me.docId),
            success: function (response) {
                let d = response.result.data || {};
                me.docState[me.docId] = { digest: d.digest || '', data: d.data || {} };
                me.buildTree();
                me.syncButtons();
                next();
            },
        });
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

    // Both lists below resolve `selector: { tag: t }` against `me.tags`, which
    // `GET /meta/guests` fills in only for a caller with VM.Audit (DESIGN §5).
    // That is not a gap here: this is a guest tab, and a caller without VM.Audit
    // on `/vms/<vmid>` never sees the guest in the resource tree at all
    // (`PVE::API2::Cluster::resources` skips it), so no reachable caller of this
    // panel has tags we cannot read. A scope-only principal is still bound by
    // its permissions -- they are enforced server-side, on the API it actually uses.

    // The prefixes that reach this guest, most-specific first. Prefixes decide
    // *shape*: which declared-but-unset rows appear and which schema governs a path.
    applicablePrefixes: function () {
        let me = this;
        if (me.dc) {
            return []; // prefixes apply to guest documents only (DESIGN section 3.3)
        }
        return (me.prefixes || []).filter(function (ns) {
            let sel = ns.selector || {};
            return sel.all || (sel.tag && me.tags.indexOf(sel.tag) !== -1);
        });
    },

    // The permission rules that reach this guest. Permissions decide *access*, and
    // unlike prefixes they accumulate by containment: a rule on `homelab` covers
    // `homelab.docker` (DESIGN section 3.2).
    applicablePermissions: function () {
        let me = this;
        let out = [];
        if (me.dc) {
            return out; // permissions apply to guest documents only
        }
        (me.permissions || []).forEach(function (file) {
            (file.rules || []).forEach(function (entry) {
                let sel = entry.selector || {};
                let matches =
                    entry.prefix && (sel.all || (sel.tag && me.tags.indexOf(sel.tag) !== -1));
                if (matches) {
                    out.push(Ext.apply({ file: file }, entry));
                }
            });
        });
        return out;
    },

    // Every rule whose prefix covers this row, `rw` first. Several principals
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
            let name = s.file.name || s.file.authid || '';
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
    // `prefixes`/`owner`, when given, enforce most-specific-wins: the walk stops
    // where a *different* prefix governs, so a parent's `properties` never reach
    // into a child prefix's subtree and rewrite its row kind. The same rule
    // `Lint.findings` and `Lint.schemaIndex` apply, through the same
    // `Utils.governing` -- it lived in two of the three and this was the one that
    // silently merged (DESIGN section 3.1).
    addGrammar: function (root, prefix, schema, prefixes, owner) {
        let me = this;
        let U = PVE.meta.Utils;
        let entry = root;
        let path = '';
        // The empty prefix is the document root itself -- how a registry document's
        // meta-schema is rooted (DESIGN §3.6). `''.split('.')` is `['']`, which would
        // otherwise create a child with an empty key.
        if (prefix) {
            prefix.split('.').forEach(function (seg) {
                path = U.joinPath(path, seg);
                entry = me.entry(entry, seg, path);
                entry.kind = entry.kind || 'map';
            });
        }
        let walk = function (node, sch) {
            if (!sch || sch.type !== 'object' || !sch.properties) {
                return;
            }
            node.kind = node.kind || 'map';
            Object.keys(sch.properties).forEach(function (key) {
                let childPath = U.joinPath(node.path, key);
                if (owner && U.governing(childPath, prefixes) !== owner) {
                    return; // a more specific prefix owns this subtree
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
                // First writer wins, like `grammarDescription` above: prefixes are
                // walked most-specific first, so the closest one should win. The
                // governing prune makes this unobservable today -- it is here so the
                // six fields cannot disagree if that prune is ever loosened.
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
                // The one extension to the PVE::JSONSchema dialect: "this string is
                // a block of text". Only a declaration can say so before the key has
                // a value, which is exactly what `Utils.editorKind` cannot see for
                // itself. It is an editor hint and nothing else -- the server neither
                // reads it nor validates against it, like `format` (DESIGN §4).
                if (ps.multiline !== undefined && child.multiline === undefined) {
                    child.multiline = !!ps.multiline;
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

    // The merged rows of ONE document: what is present in it, plus what its grammar
    // declares (DESIGN §8). Two sources, one set of entries.
    documentEntries: function () {
        let me = this;
        let root = { key: '', path: '', children: Object.create(null), present: true, kind: 'map' };
        me.addData(root, me.plannedData());
        // A row staged for deletion is gone from the planned document, but it should
        // not vanish off the screen before it is applied -- you would be looking at a
        // tree that already claims the write happened. It comes back as a ghost.
        me.pending
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
        let grammar = me.grammarFor(me.docId);
        grammar.forEach(function (ns) {
            if (!ns.schema) {
                return;
            }
            // `prefixes`/`owner` are the most-specific-wins prune, and only guest
            // documents have more than one prefix to shadow between. A registry
            // document has exactly one schema, rooted at the document, so it is
            // walked with no owner and nothing is pruned.
            if (ns.prefix) {
                me.addGrammar(root, ns.prefix, ns.schema, grammar, ns);
            } else {
                me.addGrammar(root, '', ns.schema, [], null);
            }
        });
        return root;
    },

    buildTree: function () {
        let me = this;
        let I = PVE.meta.Icons;
        // Permissions reach guest documents only, so the Access
        // column is empty on the datacenter tab by construction (DESIGN §3.3).
        let scopes = me.applicablePermissions();

        let findings = me.findingsFor();
        // What is staged, by path, so a changed row can show `stored -> pending`.
        let staged = Object.create(null);
        me.pending.forEach((e) => (staged[e.path] = e.op));
        let storedDoc = me.dataOf(me.docId);
        // What each branch has to answer for: schema findings beneath it, and staged
        // edits beneath it. Both are invisible once the branch is collapsed.
        let below = PVE.meta.Utils.rollUp(findings);
        let stagedBelow = PVE.meta.Utils.rollUp(
            me.pending.reduce(function (acc, e) {
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
                        valueText: c.present ? PVE.meta.Utils.displayValue(c.value, kind) : '',
                        accessList: access,
                        accessText: me.accessSummary(access),
                        finding: findings[c.path] || '',
                        pending: staged[c.path] || '',
                        belowCount: (below[c.path] || {}).count || 0,
                        belowText: ((below[c.path] || {}).messages || []).join('\n'),
                        stagedBelow: (stagedBelow[c.path] || {}).count || 0,
                        // Rendered with the row's own kind, not one inferred from the
                        // raw value: the API returns booleans as 1/0 (DESIGN §4), so
                        // inferring would print a struck-through "1" under a row whose
                        // stored value reads "Yes".
                        storedText: (function () {
                            let v = PVE.meta.Lint.valueAt(storedDoc, c.path);
                            return v === undefined ? '' : PVE.meta.Utils.displayValue(v, kind);
                        })(),
                        editable: me.editableFor(c.path),
                        leaf: kind !== 'map',
                    };
                    if (kind === 'map') {
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

    editRow: function (rec) {
        let me = this;
        if (!rec || !rec.data.editable) {
            return;
        }
        // A value with structure inside it is edited as text, wherever the request came
        // from (button, double-click, Enter). Before this, all three simply did nothing
        // on a map row.
        if (PVE.meta.Utils.editorKind(rec.data) === 'text') {
            me.editAsText(rec);
            return;
        }
        me.editing = true;
        let win = Ext.create('PVE.meta.EditValueWindow', { rec: rec });
        win.on('setvalue', (value) => me.stage(rec.data.path, 'set', value));
        win.on('destroy', function () {
            me.editing = false;
        });
        win.show();
    },

    addKey: function (docId, parentPath) {
        let me = this;
        me.editing = true;
        let win = Ext.create('PVE.meta.AddKeyWindow', { parentPath: parentPath || '' });
        win.on('addkey', (path, value) => me.stage(path, 'set', value));
        win.on('destroy', function () {
            me.editing = false;
        });
        win.show();
    },

    // Applies everything staged, as ONE write.
    //
    // That is the whole point: the states in between are the ones the server refuses
    // (a selector with both `all` and `tag`, or neither), so they must never reach it.
    // The write is a `replace` at the narrowest view covering every staged path, with
    // the planned subtree as its content -- for a single row edit that is exactly the
    // one-key write this used to send immediately.
    //
    // The server is asked twice: once with `dry_run=1`, whose complaints become the
    // diff dialog's warning banner, and then for real. That is how a rule the client
    // cannot know -- "exactly one of all/tag" is not expressible in the schema
    // dialect (DESIGN §3.6) -- still gets said before the write rather than after.
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
        let U = PVE.meta.Utils;
        let view = U.writeView(me.pending);
        let planned = me.plannedData();
        let subtree = view === '' ? planned : PVE.meta.Lint.valueAt(planned, view);
        if (subtree === undefined) {
            subtree = {};
        }
        let params = {
            view: view || undefined,
            mode: 'replace',
            data: Ext.encode(subtree),
            digest: me.digestOf(me.docId),
        };
        let stored = view === '' ? me.dataOf(me.docId) : PVE.meta.Lint.valueAt(me.dataOf(me.docId), view);

        Proxmox.Utils.API2Request({
            url: me.urlFor(me.docId),
            method: 'PUT',
            waitMsgTarget: me,
            params: Ext.apply({ dry_run: 1 }, params),
            // A refusal is not a failure to report and stop on: it is the banner. The
            // administrator still gets the diff and an explicit "apply anyway", the
            // same as a schema mismatch in the text editor -- and the server refuses
            // it again for real if it really is unstorable.
            callback: function (options, success, response) {
                let warnings = me.textFindingsFor(planned);
                if (!success) {
                    // `htmlStatus` is already HTML -- PVE encodes it -- and the banner
                    // encodes every warning again, so the server's own quotes and
                    // angle brackets arrived as `&#39;` and `&lt;` on screen. Back to
                    // plain text here; the banner does the one encoding.
                    let msg = response.htmlStatus || Proxmox.Utils.getResponseErrorMessage(response);
                    msg = Ext.util.Format.htmlDecode(Ext.util.Format.stripTags(String(msg)));
                    warnings = [msg.replace(/\s+/g, ' ').trim()].concat(warnings);
                }
                PVE.meta.Monaco.confirmDiff({
                    title: Ext.String.format(gettext('Apply: {0}'), me.docId),
                    // Documents, not text: `confirmDiff` renders them once it has the
                    // YAML codec, so this cannot run before it is loaded.
                    originalValue: stored === undefined ? {} : stored,
                    modifiedValue: subtree,
                    warnings: warnings,
                    apply: function () {
                        me.submit(
                            { url: me.urlFor(me.docId), method: 'PUT', params: params },
                            function () {
                                me.pending = [];
                            },
                        );
                    },
                });
            },
        });
    },

    // The schema findings for a planned document, as plain messages -- what Apply
    // warns about. The same `Lint.findings` the tree markers use, through the same
    // `grammarSplit`, so the banner and the amber rows can never disagree.
    textFindingsFor: function (planned) {
        let me = this;
        let g = me.grammarSplit(me.docId);
        if (!g.withSchema.length) {
            return [];
        }
        return PVE.meta.Lint.findings(planned, g.withSchema, g.all).map(
            (f) => f.path + ': ' + f.message,
        );
    },

    // Write a declared default into the document, because someone asked for it.
    setToDefault: function (rec) {
        let me = this;
        if (!rec || !rec.data.docId || rec.data.present || rec.data.defaultValue === undefined) {
            return;
        }
        me.stage(rec.data.path, 'set', rec.data.defaultValue);
    },

    // Declare one key of the selected prefix's schema: a view PUT into
    // `schema.properties.<key>` of that prefix document, with its digest. The
    // window builds the declaration; this only decides where it goes.
    declareKey: function (rec) {
        let me = this;
        if (!rec || !rec.data.docId || me.docKind(rec.data.docId) !== 'prefix') {
            return;
        }
        let docId = rec.data.docId;
        me.editing = true;
        let win = Ext.create('PVE.meta.DeclareKeyWindow', { prefix: me.docTitle(docId) });
        win.on('declarekey', (key, schema) =>
            me.stage('schema.properties.' + key, 'set', schema),
        );
        win.on('destroy', function () {
            me.editing = false;
        });
        win.show();
    },

    // Staged like every other edit, so no confirm: nothing has happened yet, the row
    // shows struck through, and Revert or Apply is the decision.
    removeKey: function (rec) {
        let me = this;
        if (!rec || !rec.data.path) {
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
        me.request({
            url: me.urlFor(docId),
            // The document root is addressed by *omitting* `view`, not by sending an
            // empty one -- which is what a document row on the datacenter tab is.
            params: { view: view || undefined, format: 'yaml' },
            success: function (response) {
                let d = response.result.data || {};
                me.setDigest(docId, d.digest);
                me.textWindow = Ext.create('PVE.meta.TextWindow', {
                    view: view,
                    docId: docId,
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
        // Which document the Text card shows. On a guest tab there is only one; on
        // the datacenter tab it is the one the selection is in, so switching to Text
        // with a prefix row selected edits that prefix -- the alternative was
        // a full-document editor that could only ever mean one of several documents.
        let rec = me.getSelection()[0];
        me.textDocId = (rec && rec.data.docId) || me.docId;
        me.syncButtons();
        me.getLayout().setActiveItem(me.down('#metaText'));
        Proxmox.Utils.setErrorMask(me, true);
        me.request({
            url: me.urlFor(me.textDocId),
            params: { format: 'yaml' },
            success: function (response) {
                let d = response.result.data || {};
                me.setDigest(me.textDocId, d.digest);
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
                let params = { mode: 'replace', digest: me.digestOf(me.textDocId) };
                params[me.textLang === 'json' ? 'data' : 'text'] = edited;
                me.submit(
                    { url: me.urlFor(me.textDocId), method: 'PUT', params: params },
                    () => me.refreshText(),
                );
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
            let g = me.textGrammar();
            let all = g.all;
            let applicable = g.withSchema;
            if (applicable.length) {
                let index = PVE.meta.Lint.lineIndex(text);
                markers = PVE.meta.Lint.placed(
                    PVE.meta.Lint.findings(parsed, applicable, all),
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
                let schemas = PVE.meta.Lint.schemaIndex(applicable, all);
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
    textGrammar: function () {
        return this.grammarSplit(this.textDocId || this.docId);
    },

    textFindings: function () {
        let me = this;
        if (!me.textEditor) {
            return [];
        }
        let g = me.textGrammar();
        let all = g.all;
        let applicable = g.withSchema;
        if (!applicable.length) {
            return [];
        }
        try {
            let value =
                me.textLang === 'json'
                    ? Ext.decode(me.textEditor.getValue())
                    : PVE.meta.Utils.yamlLoad(me.textEditor.getValue());
            return PVE.meta.Lint.findings(value, applicable, all).map((f) => f.path + ': ' + f.message);
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
            url: me.urlFor(me.textDocId),
            params: { format: 'yaml' },
            success: function (response) {
                let d = response.result.data || {};
                me.setDigest(me.textDocId, d.digest);
                me.textOriginal = d.text || '';
                if (me.textEditor) {
                    me.textEditor.setValue(me.textRendered(me.textLang));
                    me.annotateText();
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
});

// ---------------------------------------------------------------------------
// One document in a window — what a registry grid opens.
//
// It is the ordinary editor panel, unchanged: tree, row editors, markers, the
// Tree | Text toggle, the diff. A prefix definition or a permission file is a document
// (DESIGN §3.5), so "edit one" was never a thing that needed its own editor.
// ---------------------------------------------------------------------------

Ext.define('PVE.meta.DocumentWindow', {
    extend: 'Ext.window.Window',
    xtype: 'pveMetaDocumentWindow',

    modal: true,
    width: 860,
    height: 560,
    layout: 'fit',
    // configs: docId ('prefixes/<name>' or 'permissions/<name>')

    initComponent: function () {
        let me = this;
        me.title = Ext.String.format(gettext('Edit: {0}'), Ext.htmlEncode(me.docId));
        Ext.apply(me, {
            items: [
                {
                    xtype: 'pveMetaTreePanel',
                    // `dc: true` says "this is not a guest": no tags to resolve
                    // and no permissions to apply. Which ACL answers apply is decided by
                    // `docId`, which `loadAccess` sends as-is.
                    dc: true,
                    docId: me.docId,
                    border: false,
                },
            ],
            buttons: [{ text: gettext('Close'), handler: () => me.close() }],
        });
        me.callParent();
    },
});

// ---------------------------------------------------------------------------
// "New" — the two fields a registry file cannot be created without.
//
// Everything else about a prefix definition is optional and is filled in by
// editing the document it creates; this only gets far enough that the file parses,
// because a file the loader would skip is refused on the way in (DESIGN §3.5).
// ---------------------------------------------------------------------------

Ext.define('PVE.meta.NewRegistryWindow', {
    extend: 'Ext.window.Window',
    xtype: 'pveMetaNewRegistryWindow',

    modal: true,
    width: 460,
    layout: 'fit',
    kind: 'prefixes',

    initComponent: function () {
        let me = this;
        let isPrefix = me.kind === 'prefixes';
        me.title = isPrefix ? gettext('New Prefix') : gettext('New Permission');
        let items = [
            {
                xtype: 'textfield',
                name: 'name',
                allowBlank: false,
                fieldLabel: isPrefix ? gettext('Prefix') : gettext('Name'),
                // The file name *is* the prefix, so a nested one is dotted and this
                // field is the whole identity of what is being created.
                emptyText: isPrefix ? gettext('e.g. homelab.docker') : gettext('file name'),
            },
        ];
        if (isPrefix) {
            items.push(
                {
                    xtype: 'proxmoxKVComboBox',
                    name: 'selector',
                    fieldLabel: gettext('Applies to'),
                    value: 'all',
                    comboItems: [
                        ['all', gettext('Every guest')],
                        ['tag', gettext('Guests with a tag')],
                    ],
                    listeners: {
                        change: (f, v) => me.down('[name=tag]').setHidden(v !== 'tag'),
                    },
                },
                {
                    xtype: 'textfield',
                    name: 'tag',
                    fieldLabel: gettext('Tag'),
                    hidden: true,
                },
                { xtype: 'textfield', name: 'description', fieldLabel: gettext('Description') },
            );
        } else {
            items.push(
                {
                    xtype: 'textfield',
                    name: 'authid',
                    allowBlank: false,
                    fieldLabel: gettext('Auth ID'),
                    emptyText: gettext('user@realm, or user@realm!tokenid'),
                },
                { xtype: 'textfield', name: 'description', fieldLabel: gettext('Description') },
            );
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
                { text: gettext('Create'), handler: () => me.submit() },
                { text: gettext('Cancel'), handler: () => me.close() },
            ],
        });
        me.callParent();
        me.on('show', () => me.down('[name=name]').focus(true, 50));
    },

    // The smallest file the loader will read back. A permission file is created with
    // no rules on purpose: it permits nothing until an administrator says what.
    contentFrom: function (v) {
        if (this.kind === 'permissions') {
            let out = { authid: v.authid };
            if (v.description) {
                out.description = v.description;
            }
            out.rules = [];
            return out;
        }
        let out = {};
        if (v.description) {
            out.description = v.description;
        }
        out.selector = v.selector === 'tag' ? { tag: v.tag } : { all: true };
        return out;
    },

    submit: function () {
        let me = this;
        let form = me.down('form').getForm();
        if (!form.isValid()) {
            return;
        }
        let v = form.getValues();
        if (me.kind === 'prefixes' && v.selector === 'tag' && !String(v.tag).trim()) {
            Ext.Msg.alert(gettext('Error'), gettext('A tag selector needs a tag'));
            return;
        }
        me.fireEvent('create', String(v.name).trim(), me.contentFrom(v));
        me.close();
    },
});

// ---------------------------------------------------------------------------
// The two registry lists.
//
// A grid rather than a tree, because the interesting facts about these files are
// *columns*: which guests a prefix reaches, whether it carries a schema, and
// whether what you are looking at is a package's file or your own on top of one.
// A tree could show none of that, and showed a packaged definition and a cluster
// override as the same thing.
// ---------------------------------------------------------------------------

Ext.define('PVE.meta.RegistryGrid', {
    extend: 'Ext.grid.Panel',
    xtype: 'pveMetaRegistryGrid',

    kind: 'prefixes', // or 'permissions'
    border: false,
    emptyText: gettext('No entries'),

    // The row a list entry becomes. Kept out of initComponent so the offline suite
    // can check the mapping without a DOM.
    statics: {
        rowsFrom: function (kind, list) {
            let U = PVE.meta.Utils;
            return (list || []).map(function (e) {
                if (kind === 'permissions') {
                    return {
                        name: e.name,
                        id: 'permissions/' + e.name,
                        authid: e.authid || '',
                        description: e.description || '',
                        // What it actually permits, in one line: prefix, mode and the
                        // selector that decides which guests it reaches.
                        summary: (e.rules || [])
                            .map((g) => g.prefix + ' (' + g.mode + ', ' + U.selectorText(g.selector) + ')')
                            .join(', '),
                        origin: e.origin || 'cluster',
                        overrides: !!e.overrides,
                    };
                }
                return {
                    name: e.prefix,
                    id: 'prefixes/' + e.prefix,
                    description: e.description || '',
                    selector: U.selectorText(e.selector),
                    schema: e.schema ? gettext('yes') : '',
                    origin: e.origin || 'cluster',
                    overrides: !!e.overrides,
                };
            });
        },

        // What the Origin column says. Three states, not two: a cluster file that
        // displaced a package's is the one where Remove does not remove anything --
        // it reverts to what the package ships.
        originText: function (row) {
            if (row.origin === 'packaged') {
                return gettext('packaged');
            }
            return row.overrides ? gettext('cluster (overrides packaged)') : gettext('cluster');
        },
    },

    initComponent: function () {
        let me = this;
        let isPrefix = me.kind === 'prefixes';
        me.store = Ext.create('Ext.data.Store', {
            fields: ['name', 'id', 'authid', 'description', 'selector', 'schema', 'summary', 'origin', 'overrides'],
            data: [],
            sorters: [{ property: 'name' }],
        });
        me.access = { write: 0 };

        let columns = [
            {
                text: isPrefix ? gettext('Prefix') : gettext('Name'),
                dataIndex: 'name',
                flex: 2,
                renderer: Ext.htmlEncode,
            },
        ];
        if (isPrefix) {
            columns.push(
                { text: gettext('Applies to'), dataIndex: 'selector', flex: 1, renderer: Ext.htmlEncode },
                { text: gettext('Schema'), dataIndex: 'schema', width: 90, renderer: Ext.htmlEncode },
            );
        } else {
            columns.push(
                { text: gettext('Auth ID'), dataIndex: 'authid', flex: 2, renderer: Ext.htmlEncode },
                { text: gettext('Rules'), dataIndex: 'summary', flex: 3, renderer: Ext.htmlEncode },
            );
        }
        columns.push(
            { text: gettext('Description'), dataIndex: 'description', flex: 2, renderer: Ext.htmlEncode },
            {
                text: gettext('Origin'),
                dataIndex: 'origin',
                width: 200,
                renderer: (v, meta, rec) =>
                    Ext.htmlEncode(PVE.meta.RegistryGrid.originText(rec.data)),
            },
        );

        Ext.apply(me, {
            columns: columns,
            tbar: [
                {
                    text: gettext('Add'),
                    itemId: 'addBtn',
                    iconCls: 'fa fa-plus',
                    disabled: true,
                    handler: () => me.createOne(),
                },
                {
                    text: gettext('Edit'),
                    itemId: 'editBtn',
                    iconCls: 'fa fa-pencil',
                    disabled: true,
                    handler: () => me.editOne(me.getSelection()[0]),
                },
                {
                    text: gettext('Remove'),
                    itemId: 'removeBtn',
                    iconCls: 'fa fa-trash-o',
                    disabled: true,
                    handler: () => me.removeOne(me.getSelection()[0]),
                },
                '->',
                { text: gettext('Reload'), iconCls: 'fa fa-refresh', handler: () => me.reload() },
            ],
            listeners: {
                itemdblclick: (view, rec) => me.editOne(rec),
                selectionchange: () => me.syncButtons(),
            },
        });
        me.callParent();
        me.on('afterrender', () => me.reload());
    },

    syncButtons: function () {
        let me = this;
        let rec = me.getSelection()[0];
        let may = !!me.access.write;
        let set = function (id, disabled) {
            let btn = me.down('#' + id);
            if (btn) {
                btn.setDisabled(disabled);
            }
        };
        set('addBtn', !may);
        // A packaged file is editable: the write creates the cluster override rather
        // than touching the package's copy (DESIGN §3.5). Removing one is not, since
        // there would be nothing of ours to remove.
        set('editBtn', !rec || !may);
        set('removeBtn', !rec || !may || rec.data.origin === 'packaged');
    },

    request: function (opts) {
        let me = this;
        let guard = (fn) => (fn ? (...args) => (me.isDestroyed ? undefined : fn(...args)) : undefined);
        Proxmox.Utils.API2Request(
            Ext.apply({ method: 'GET', success: guard(opts.success), failure: guard(opts.failure) }, opts),
        );
    },

    reload: function () {
        let me = this;
        me.request({
            url: '/meta/access',
            // The datacenter document's answer, and this grid only reads `write`
            // from it -- which is Sys.Modify on `/` for both (DESIGN §3.5). The
            // *read* bits differ (a registry file is readable by everyone), which is
            // why the document editor asks by id instead; a list needs neither the
            // read bit nor a file to ask about.
            params: { dc: 1 },
            success: function (response) {
                me.access = response.result.data || { write: 0 };
                me.syncButtons();
            },
            failure: Ext.emptyFn,
        });
        me.request({
            url: '/meta/' + me.kind,
            success: function (response) {
                me.store.setData(PVE.meta.RegistryGrid.rowsFrom(me.kind, response.result.data || []));
                me.syncButtons();
            },
            failure: (response) =>
                Proxmox.Utils.setErrorMask(me, response.htmlStatus || gettext('Error')),
        });
    },

    editOne: function (rec) {
        let me = this;
        if (!rec) {
            return;
        }
        let win = Ext.create('PVE.meta.DocumentWindow', { docId: rec.data.id });
        win.on('destroy', () => me.reload());
        win.show();
    },

    createOne: function () {
        let me = this;
        let win = Ext.create('PVE.meta.NewRegistryWindow', { kind: me.kind });
        win.on('create', function (name, content) {
            Proxmox.Utils.API2Request({
                url: '/meta/' + me.kind + '/' + encodeURIComponent(name),
                method: 'PUT',
                waitMsgTarget: me,
                // `digest: ''` is "this file must not exist yet" (DESIGN §5), so two
                // administrators creating the same name is a 409 rather than one
                // silently overwriting the other.
                params: { data: Ext.encode(content), mode: 'replace', digest: '' },
                success: function () {
                    me.reload();
                    let win2 = Ext.create('PVE.meta.DocumentWindow', {
                        docId: me.kind + '/' + name,
                    });
                    win2.on('destroy', () => me.reload());
                    win2.show();
                },
                failure: (response) =>
                    Ext.Msg.alert(gettext('Error'), response.htmlStatus || gettext('Error')),
            });
        });
        win.show();
    },

    removeOne: function (rec) {
        let me = this;
        if (!rec || rec.data.origin === 'packaged') {
            return;
        }
        // Say which of the two things this is. Removing an override does not remove
        // the prefix -- the packaged file underneath comes back.
        let question = rec.data.overrides
            ? Ext.String.format(
                  gettext('Remove the cluster file for "{0}"? The packaged one takes over again.'),
                  Ext.htmlEncode(rec.data.name),
              )
            : Ext.String.format(gettext('Remove "{0}"?'), Ext.htmlEncode(rec.data.name));
        Ext.Msg.confirm(gettext('Confirm'), question, function (btn) {
            if (btn !== 'yes') {
                return;
            }
            Proxmox.Utils.API2Request({
                url: '/meta/' + rec.data.id,
                method: 'DELETE',
                waitMsgTarget: me,
                success: () => me.reload(),
                failure: (response) =>
                    Ext.Msg.alert(gettext('Error'), response.htmlStatus || gettext('Error')),
            });
        });
    },
});

// ---------------------------------------------------------------------------
// The datacenter tab: the datacenter document, and the two registry lists.
//
// Three sub-tabs rather than one tree of everything. They are three different
// kinds of thing -- one document, a list of prefix definitions, a list of permissions --
// and drawing them as branches of a single tree claimed a relationship they do
// not have, while hiding the columns that make a list worth reading.
// ---------------------------------------------------------------------------

Ext.define('PVE.meta.DatacenterPanel', {
    extend: 'Ext.tab.Panel',
    xtype: 'pveMetaDatacenterPanel',

    border: false,
    defaults: { border: false },

    initComponent: function () {
        let me = this;
        Ext.apply(me, {
            items: [
                {
                    title: gettext('Document'),
                    iconCls: 'fa fa-file-text-o',
                    xtype: 'pveMetaTreePanel',
                    dc: true,
                },
                {
                    title: gettext('Prefixes'),
                    iconCls: 'fa fa-sitemap',
                    xtype: 'pveMetaRegistryGrid',
                    kind: 'prefixes',
                },
                {
                    title: gettext('Permissions'),
                    iconCls: 'fa fa-key',
                    xtype: 'pveMetaRegistryGrid',
                    kind: 'permissions',
                },
            ],
        });
        me.callParent();
    },
});
