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
 * `GET /meta/version?id=<docid>` — this document plus the registry, never the whole
 * store — refreshes the tree when the content token changed, and never while a row
 * editor, the text window or the Text card is open.
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

    // Two document values are the same value. Key order is data (DESIGN section 2)
    // and JSON.stringify preserves insertion order, so encoding both and comparing
    // the text is the right test rather than a shortcut.
    sameValue: (a, b) => JSON.stringify(a) === JSON.stringify(b),

    // Why a key name is not one, or `null` if it is fine. A dotted path is
    // accepted and checked segment by segment, since the Key field takes one.
    //
    // A deliberate mirror of `path::is_valid_segment`, and the second one of
    // those in this UI: the registry name field mirrors `is_valid_file_name` the
    // same way. Both exist for the same reason -- the server's answer is right
    // but arrives as `invalid path: homelab.bad key (400)` after a round trip,
    // which reads like a bug in the editor rather than a typo in the field. The
    // server stays the authority: this only refuses earlier and in words, and it
    // never lets anything through that the server would refuse, because a
    // charset is the one kind of rule you can restate without restating a
    // judgement.
    keyPathError: function (text) {
        let path = String(text === undefined || text === null ? '' : text);
        if (!path) {
            return gettext('Key must not be empty');
        }
        let bad = null;
        let empty = false;
        path.split('.').forEach(function (seg) {
            if (!seg) {
                empty = true;
            } else if (bad === null && !/^[A-Za-z0-9_@!-]+$/.test(seg)) {
                bad = seg;
            }
        });
        if (empty) {
            return gettext('A dotted path must not have an empty segment');
        }
        if (bad !== null) {
            let ch = bad.split('').find((c) => !/[A-Za-z0-9_@!-]/.test(c));
            return Ext.String.format(
                gettext("'{0}' is not allowed in a key ({1}). Keys are letters, digits, _ - @ ! -- and '.' separates them."),
                ch === ' ' ? gettext('space') : ch,
                bad,
            );
        }
        return null;
    },

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
            // The empty path is the *document*, not a key called "". `setAtPath` walks
            // segments and `''.split('.')` is `['']`, so without this a whole-document
            // edit -- what `diffDocuments` falls back to when it cannot express a
            // change as paths -- would land under a key nobody can address.
            if (p.path === '') {
                out = p.op === 'delete' ? {} : JSON.parse(JSON.stringify(p.value));
                return;
            }
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

    // Every key path at which two documents differ in *value*.
    //
    // Not the edits needed to turn one into the other -- that is `diffDocuments`,
    // which has to be exact and falls back to replacing the document whole when it
    // cannot be. This one has no fallback on purpose: a pure key reordering changes
    // no value anywhere, and its one caller must not be told that reordering a
    // document touched every path in it.
    //
    // Lists are compared whole, like everywhere else: a view addresses through maps
    // only (DESIGN section 2), so a changed member reports the list's own path.
    changedPaths: function (was, now) {
        let U = PVE.meta.Utils;
        let out = [];
        let isMap = (v) => v !== null && typeof v === 'object' && !Array.isArray(v);
        let same = (a, b) => JSON.stringify(a) === JSON.stringify(b);
        let walk = function (a, b, path) {
            if (same(a, b)) {
                return;
            }
            if (!isMap(a) || !isMap(b)) {
                out.push(path);
                return;
            }
            let keys = Object.keys(a);
            Object.keys(b).forEach(function (k) {
                if (keys.indexOf(k) === -1) {
                    keys.push(k);
                }
            });
            keys.forEach((k) => walk(a[k], b[k], U.joinPath(path, k)));
        };
        walk(was, now, '');
        return out;
    },

    // Of the findings a planned document has, the ones this edit is answerable for:
    // the ones the stored document did not already have, plus any sitting on a path
    // the edit changed.
    //
    // Before this, one bad value anywhere in a document made *every* later edit
    // anywhere else in it stop at a "Save anyway" tick -- forever, and for something
    // the edit had not done. Warning about a pre-existing violation on a path the
    // edit never touched says nothing the amber row markers do not already say, and
    // it trains the tick into a reflex, which is the one thing that tick must not be.
    //
    // Overlap counts in both directions: writing a bad `homelab.port` answers for a
    // finding at `homelab.port`, and replacing `homelab` answers for one beneath it.
    // So an edit that puts a *differently* wrong value on an already-wrong path still
    // warns -- it was ours this time.
    //
    // Findings are `{ path, message }` here and only become text at the banner, so
    // that this never has to take a formatted string apart.
    introducedFindings: function (before, after, changed) {
        let had = Object.create(null);
        (before || []).forEach(function (f) {
            had[f.path + '\u0000' + f.message] = true;
        });
        let touched = function (path) {
            return (changed || []).some(function (p) {
                return (
                    p === path ||
                    (path !== '' && p.indexOf(path + '.') === 0) ||
                    (p !== '' && path.indexOf(p + '.') === 0)
                );
            });
        };
        return (after || []).filter(
            (f) => !had[f.path + '\u0000' + f.message] || touched(f.path),
        );
    },

    // What would have to be staged to turn `stored` into `edited`, as the same
    // `{ path, op, value }` entries a row edit produces.
    //
    // This is what makes text an ordinary way to edit rather than a second editing
    // model: whatever you type there comes back as edits *on rows*, so the tree shows
    // which keys changed and to what, a key you deleted shows struck through, and one
    // Apply writes the lot. Before this, text mode had its own buffer, its own apply
    // and its own write, and the two models had to be kept apart by rules -- "you may
    // not open text while edits are staged" and the rest.
    //
    // Lists are compared whole, because their members are not addressable (§2). Maps
    // recurse, so a one-key change stays a one-key edit.
    diffDocuments: function (stored, edited) {
        let U = PVE.meta.Utils;
        let out = [];
        let isMap = (v) => v !== null && typeof v === 'object' && !Array.isArray(v);
        let same = (a, b) => JSON.stringify(a) === JSON.stringify(b);
        let walk = function (was, now, path) {
            Object.keys(now).forEach(function (k) {
                let at = U.joinPath(path, k);
                let before = Object.prototype.hasOwnProperty.call(was, k) ? was[k] : undefined;
                if (before === undefined) {
                    out.push({ path: at, op: 'set', value: now[k] });
                } else if (isMap(before) && isMap(now[k])) {
                    walk(before, now[k], at);
                } else if (!same(before, now[k])) {
                    out.push({ path: at, op: 'set', value: now[k] });
                }
            });
            Object.keys(was).forEach(function (k) {
                if (!Object.prototype.hasOwnProperty.call(now, k)) {
                    out.push({ path: U.joinPath(path, k), op: 'delete' });
                }
            });
        };
        if (!isMap(stored) || !isMap(edited)) {
            return same(stored, edited) ? [] : [{ path: '', op: 'set', value: edited }];
        }
        walk(stored, edited, '');

        // A self-check, because a diff that quietly loses something is worse than no
        // diff at all: key *order* is data in this model (§2) and a pure reordering
        // produces no per-key entries, so replay the edits and fall back to replacing
        // the document whole if the result is not what was typed.
        if (!same(U.applyPending(stored, out), edited)) {
            return [{ path: '', op: 'set', value: edited }];
        }
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

    // One element of a list, on one line. Presentation only -- like `format`, it
    // describes nothing and constrains nothing; it is there so a list of maps reads
    // as something other than JSON in a grid cell.
    //
    // A permission rule gets its own shape because that is the list people actually
    // look at, and `{"prefix":"traefik","mode":"rw","selector":{"tag":"traefik"}}`
    // is not a thing anyone reads twice.
    itemSummary: function (v) {
        if (!v || typeof v !== 'object' || Array.isArray(v)) {
            return PVE.meta.Utils.scalarText(v);
        }
        let has = (k) => Object.prototype.hasOwnProperty.call(v, k);
        if (has('prefix') && has('mode')) {
            return (
                v.prefix + ' (' + v.mode +
                (has('selector') ? ', ' + PVE.meta.Utils.selectorText(v.selector) : '') + ')'
            );
        }
        return Ext.encode(v);
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

    // --- the two editor buffers (TextWindow, TreePanel's Text card) --------
    //
    // Both editors carry a Monaco buffer with a presentation-only YAML/JSON toggle
    // and a Format button, and both used to parse and re-dump it inline -- six
    // copies of "decode JSON or load YAML" and four of "stringify JSON or dump
    // YAML" between them, one of which (TextWindow's own toggle) was missing the
    // round-trip fix its sibling got. One set of functions now; see `renderBuffer`.

    // How the buffer's own language reads it back. Used by Format, the JSON/YAML
    // toggle and the live grammar squiggles -- everything that needs the parsed
    // value rather than the text.
    parseBuffer: function (text, lang) {
        return lang === 'json' ? Ext.decode(text) : PVE.meta.Utils.yamlLoad(text);
    },

    // The inverse: `value` as text in `lang`. Always re-dumps -- what Format wants,
    // since a re-indent is the point of clicking it.
    dumpBuffer: function (value, lang) {
        return lang === 'json' ? JSON.stringify(value, null, 2) : PVE.meta.Utils.yamlDump(value);
    },

    // As `dumpBuffer`, but for a switch *into* YAML prefers `originalYaml` when it is
    // the same document. js-yaml's dump lays a document out differently from
    // serde_yaml_ng's (indentation of nested sequences, quoting), so unconditionally
    // re-dumping on every toggle made a round trip through JSON look like an edit --
    // a diff of pure whitespace, offered as something to Apply. That was fixed once,
    // in the Text card's toggle; the subtree window's own toggle inlined the same
    // ternary without it, so the identical no-op could open its diff with Apply
    // enabled. Both call this now.
    renderBuffer: function (value, lang, originalYaml) {
        if (
            lang !== 'json' &&
            originalYaml !== undefined &&
            PVE.meta.Utils.sameDocument(value, originalYaml)
        ) {
            return originalYaml;
        }
        return PVE.meta.Utils.dumpBuffer(value, lang);
    },

    // The document a buffer's own editor last loaded (its `original`/`textOriginal`),
    // rendered in `lang`. Throws if it cannot be read as YAML -- callers fall back to
    // 'yaml' rather than lose the comparison. TextWindow's diff used to inline this
    // exact conversion rather than share TreePanel's `textRendered`.
    originalInLang: function (originalYaml, lang) {
        return lang === 'json'
            ? JSON.stringify(PVE.meta.Utils.yamlLoad(originalYaml), null, 2)
            : originalYaml;
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
            // Fitted to the viewport, not fixed: at 620px tall on a shorter window it
            // pushed its own Apply and Back buttons off the bottom of the screen with
            // nothing to scroll, so the only way out was Escape -- which nothing says.
            width: Math.min(1000, Ext.Element.getViewportWidth() - 40),
            height: Math.min(620, Ext.Element.getViewportHeight() - 40),
            maxHeight: Ext.Element.getViewportHeight() - 40,
            constrain: true,
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
                // Without an `apply` this window is just a look at the difference --
                // the same view, without the decision. Being able to see the diff
                // without committing to it is the point of offering it outside Apply.
                {
                    text: gettext('Apply'),
                    itemId: 'diffApplyBtn',
                    hidden: !cfg.apply,
                    disabled: !!warnings.length,
                    handler: function () {
                        win.close();
                        cfg.apply();
                    },
                },
                { text: cfg.apply ? gettext('Back') : gettext('Close'), handler: () => win.close() },
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
// The editor footer — one bar, three editors.
//
// There are three places you edit a document here: the tree, the text card behind
// the Tree | Text toggle, and the text window over one subtree. They had grown
// three different chromes — the subtree window put its view switch on *top* and had
// no Format button at all, the tree put Apply and Revert on top, and a document
// window's Close sat at the bottom while the Apply for the same document sat at the
// top of the panel inside it. Nothing about the three is different enough to
// justify that.
//
// So: **which view you are looking at goes bottom-left, what you can do about it
// goes bottom-right**, and every editor builds both halves from here. The top
// toolbar is left for acting on the document's *contents* (Add, Edit, Remove,
// Declare Key, Add Rule), which is a different kind of thing from committing.
// ---------------------------------------------------------------------------

Ext.define('PVE.meta.Footer', {
    singleton: true,

    // cfg: { format: handler?, apply: handler, secondary: handler, secondaryText }
    actions: function (cfg) {
        let out = [];
        if (cfg.format) {
            out.push({
                text: gettext('Format'),
                itemId: 'metaFormat',
                iconCls: 'fa fa-indent',
                tooltip: gettext('Re-indent the buffer canonically'),
                handler: cfg.format,
            });
        }
        if (cfg.diff) {
            out.push({
                text: gettext('Diff'),
                itemId: 'metaDiff',
                iconCls: 'fa fa-exchange',
                tooltip: gettext('Show this buffer against the stored document'),
                handler: cfg.diff,
            });
        }
        out.push('->');
        out.push({
            text: gettext('Apply'),
            itemId: 'metaApply',
            iconCls: 'fa fa-check',
            // Stated by the caller, never defaulted. Defaulting it to `disabled` meant
            // a caller that never called `sync` got a button that looked ordinary and
            // did nothing at all -- no click, no request, no message -- which is
            // exactly what happened to the subtree window. A shared builder whose
            // default only one of its callers undoes is a rule with two meanings,
            // which is the thing extracting it was meant to stop.
            disabled: !!cfg.applyDisabled,
            handler: cfg.apply,
        });
        out.push({
            text: cfg.secondaryText || gettext('Revert'),
            itemId: 'metaSecondary',
            iconCls: 'fa fa-undo',
            handler: cfg.secondary,
        });
        return out;
    },

    // `count` on the button it acts on rather than in a label beside it: a label is
    // the first thing clipped when an editor opens in a window, and a counter you
    // cannot read is not one.
    sync: function (owner, state) {
        let apply = owner.down('#metaApply');
        if (apply) {
            apply.setDisabled(!state.canApply);
            apply.setText(
                state.count
                    ? Ext.String.format(gettext('Apply ({0})'), state.count)
                    : gettext('Apply'),
            );
        }
        let second = owner.down('#metaSecondary');
        if (second) {
            // The icon has to agree with the word: an undo arrow on a button that says
            // Close is a button that looks like it will throw your work away.
            second.setIconCls(state.dirty ? 'fa fa-undo' : 'fa fa-times');
            // A window's Close becomes Discard once there is something to lose, which
            // is the one moment the difference matters.
            second.setText(state.dirty ? state.dirtyText : state.cleanText);
            second.setDisabled(!!state.secondaryOnlyWhenDirty && !state.dirty);
        }
    },
});

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
        { name: 'arrayIndex' }, // this row is member N of the list at `path`
        { name: 'addressable', type: 'boolean' }, // false: no view path names this row
        { name: 'rawItem' }, // a list member's real value, whatever it is
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
    list: false, // appending to a list instead: a member has no name to give it

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
                            fieldLabel: me.list ? gettext('Append to') : gettext('Under'),
                            value: Ext.htmlEncode(me.parentPath || gettext('(document root)')),
                        },
                        {
                            xtype: 'textfield',
                            name: 'key',
                            allowBlank: me.list,
                            hidden: me.list,
                            fieldLabel: gettext('Key'),
                            emptyText: gettext('key, or a dotted path'),
                            // Leading and trailing dots are stripped on submit, so
                            // do not fail the field for them while it is being typed.
                            validator: (v) =>
                                me.list && !v
                                    ? true
                                    : PVE.meta.Utils.keyPathError(
                                          String(v || '').replace(/^\.+|\.+$/g, ''),
                                      ) || true,
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
        let key = String(v.key || '').replace(/^\.+|\.+$/g, '');
        try {
            if (!key && !me.list) {
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
// "Add Rule" — one entry of a permission file, as a form.
//
// The same shape as Declare Key, one document over: a permission file's `rules`
// is the whole point of the file, and leaving it to the text editor made the
// interesting part of it the one part with no affordance.
//
// It appends rather than edits, and it stages like everything else. Appending
// replaces the whole `rules` array, because a view addresses through maps only —
// there is no path to `rules[1]` (DESIGN §2). Changing or removing a rule is
// still the text editor; adding one is what you do a hundred times more often.
// ---------------------------------------------------------------------------

Ext.define('PVE.meta.AddRuleWindow', {
    extend: 'Ext.window.Window',
    xtype: 'pveMetaAddRuleWindow',

    title: gettext('Add Rule'),
    modal: true,
    width: 520,
    layout: 'fit',
    prefixes: null, // the declared prefixes, for the combobox
    existing: null, // the rules already in the file (appending)
    rule: null, // the rule being edited, if this is an edit

    initComponent: function () {
        let me = this;
        let declared = (me.prefixes || []).map((p) => [p.prefix, p.prefix]);
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
                            // Editable on purpose: a rule may name a prefix nobody has
                            // declared yet. The list is a convenience, not a
                            // constraint -- permissions and prefix definitions are
                            // independent files and neither waits for the other.
                            xtype: 'combobox',
                            name: 'prefix',
                            fieldLabel: gettext('Prefix'),
                            allowBlank: false,
                            store: declared,
                            queryMode: 'local',
                            editable: true,
                            forceSelection: false,
                            emptyText: gettext('a declared prefix, or any key path'),
                        },
                        {
                            xtype: 'proxmoxKVComboBox',
                            name: 'mode',
                            fieldLabel: gettext('Mode'),
                            value: 'ro',
                            comboItems: [
                                ['ro', gettext('Read only')],
                                ['rw', gettext('Read and write')],
                            ],
                        },
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
                        { xtype: 'textfield', name: 'tag', fieldLabel: gettext('Tag'), hidden: true },
                    ],
                },
            ],
            buttons: [
                { text: me.rule ? gettext('OK') : gettext('Add'), handler: () => me.submit() },
                { text: gettext('Cancel'), handler: () => me.close() },
            ],
        });
        me.callParent();
        me.on('show', function () {
            if (me.rule) {
                let sel = me.rule.selector || {};
                me.down('form').getForm().setValues({
                    prefix: me.rule.prefix,
                    mode: me.rule.mode,
                    selector: sel.tag ? 'tag' : 'all',
                    tag: sel.tag || '',
                });
            }
            me.down('[name=prefix]').focus(true, 50);
        });
    },

    statics: {
        // The `rules` list this form would produce. Pure, so the offline suite can
        // check the arithmetic without a DOM.
        rulesWith: function (existing, v) {
            let rule = {
                prefix: String(v.prefix).trim(),
                mode: v.mode === 'rw' ? 'rw' : 'ro',
                selector: v.selector === 'tag' ? { tag: String(v.tag).trim() } : { all: true },
            };
            return (Array.isArray(existing) ? existing : []).concat([rule]);
        },
    },

    submit: function () {
        let me = this;
        let form = me.down('form').getForm();
        if (!form.isValid()) {
            return;
        }
        let v = form.getValues();
        if (v.selector === 'tag' && !String(v.tag).trim()) {
            Ext.Msg.alert(gettext('Error'), gettext('A tag selector needs a tag'));
            return;
        }
        me.fireEvent('addrule', PVE.meta.AddRuleWindow.rulesWith(me.existing, v));
        me.close();
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
                            // A declared key becomes a real key in a real document,
                            // so it lives under the same charset -- and a schema that
                            // declares an unwritable key is worse than a rejected
                            // form, because nothing rejects it until someone tries.
                            validator: (v) => PVE.meta.Utils.keyPathError(v) || true,
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
                            // `none`, not `''`: a KVComboBox whose key is the empty
                            // string hands back the store record's internal id
                            // (`KeyValue-1`) instead of the key, and it renders blank
                            // rather than showing its own default. Caught in a browser
                            // -- this wrote `format: KeyValue-1` into a schema, and the
                            // same shape broke Create Service Token outright.
                            value: 'none',
                            comboItems: [['none', gettext('none')]].concat(
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
        if (v.type === 'string' && v.format && v.format !== 'none') {
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
            bbar: [
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
            ].concat(
                PVE.meta.Footer.actions({
                    // Stated, like the panel states its own: there is no buffer until
                    // Monaco loads, and `syncFooter` turns it on when there is.
                    applyDisabled: true,
                    diff: () => me.showBufferDiff(),
                    format: () => me.formatBuffer(),
                    apply: () => me.showDiff(),
                    secondary: () => me.close(),
                    secondaryText: gettext('Close'),
                }),
            ),
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
                    // Without these the footer never learns there is a buffer:
                    // `syncFooter` was written and never called, so the secondary
                    // button kept an undo icon over the word Close whatever you did.
                    me.syncFooter();
                    me.editor.onDidChangeModelContent(() => me.syncFooter());
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

    // The buffer against what was loaded, without committing to it -- the same view
    // Apply ends with, offered on its own.
    showBufferDiff: function () {
        let me = this;
        if (!me.editor) {
            return;
        }
        let lang = me.lang;
        let original;
        try {
            original = PVE.meta.Utils.originalInLang(me.original, lang);
        } catch (_err) {
            lang = 'yaml';
            original = me.original;
        }
        PVE.meta.Monaco.confirmDiff({
            title: me.view || gettext('(whole document)'),
            original: original,
            modified: me.editor.getValue(),
            lang: lang,
        });
    },

    // Apply is live as soon as there is a buffer; the diff decides whether there is
    // anything in it worth writing. Close becomes Discard once the buffer differs from
    // what was loaded -- the same rule the panel's footer follows, and for the same
    // reason: that is the moment there is something to lose.
    syncFooter: function () {
        let me = this;
        PVE.meta.Footer.sync(me, {
            canApply: !!me.editor,
            count: 0,
            dirty: !!me.editor && me.editor.getValue() !== me.original,
            dirtyText: gettext('Discard'),
            cleanText: gettext('Close'),
        });
    },

    // Presentation only: re-render the same value in the other syntax. If we cannot
    // parse the buffer we say so and stay put; the server remains the YAML authority.
    // The same canonical re-dump the text card's Format does, on this window's own
    // editor -- through `Utils.parseBuffer`/`dumpBuffer`, so the two Format buttons
    // cannot drift. It refuses a buffer that does not parse rather than mangling it.
    formatBuffer: function () {
        let me = this;
        let ed = me.editor;
        if (!ed) {
            return;
        }
        let text = ed.getValue();
        try {
            let U = PVE.meta.Utils;
            let formatted = U.dumpBuffer(U.parseBuffer(text, me.lang), me.lang);
            if (formatted !== text) {
                ed.setValue(formatted);
            }
        } catch (err) {
            Ext.Msg.alert(
                gettext('Cannot format'),
                Ext.htmlEncode(PVE.meta.Utils.errText(err)),
            );
        }
    },

    switchLang: function (lang) {
        let me = this;
        let btn = me.lookupReference('langbtn');
        if (!me.editor || lang === me.lang) {
            return;
        }
        let value;
        try {
            value = PVE.meta.Utils.parseBuffer(me.editor.getValue(), me.lang);
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
        // `renderBuffer`, not a bare dump: switching JSON -> YAML must not turn a
        // no-op round trip into a whitespace diff (see the comment on that function).
        // This editor used to dump unconditionally here, which the Text card's own
        // toggle did not.
        me.editor.setValue(PVE.meta.Utils.renderBuffer(value, lang, me.original));
    },

    // Apply applies, and the window closes when the write lands. The diff is a button
    // of its own now, so stopping to show it again was asking twice for one decision.
    showDiff: function () {
        let me = this;
        if (!me.editor) {
            return;
        }
        let edited = me.editor.getValue();
        let original = me.original;
        try {
            original = PVE.meta.Utils.originalInLang(me.original, me.lang);
        } catch (_err) {
            original = me.original;
        }
        if (edited === original) {
            Ext.Msg.alert(gettext('Notice'), gettext('No changes.'));
            return;
        }
        me.apply(edited, me.lang);
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
    // Set when this panel is inside a window: its footer then carries the way out,
    // which is also the way to abandon staged edits.
    onClose: undefined,

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
            bbar: me.buildFooter(),
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
            // Only shown when the caller is restricted (DESIGN §8).
            { xtype: 'tbtext', itemId: 'accessText', cls: 'faded', hidden: true },
        ];
    },

    // The buffer against the file, without committing to anything. Text mode's Apply
    // shows the same diff, but only as the last step before writing -- and wanting to
    // see what you changed is not the same as wanting to write it.
    showTextDiff: function () {
        let me = this;
        if (!me.textEditor) {
            return;
        }
        let lang = me.textLang;
        let original;
        try {
            original = PVE.meta.Utils.originalInLang(me.textOriginal, lang);
        } catch (_err) {
            lang = 'yaml'; // cannot render the stored text as JSON; diff the YAML
            original = me.textOriginal;
        }
        PVE.meta.Monaco.confirmDiff({
            title: Ext.String.format(gettext('Changes: {0}'), me.textDocId || me.docId),
            original: original,
            modified: me.textEditor.getValue(),
            lang: lang,
            // No `apply`: this is the view, not the decision.
        });
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
            {
                xtype: 'segmentedbutton',
                itemId: 'textLangBtn',
                hidden: true,
                value: 'yaml',
                items: [
                    { text: 'YAML', value: 'yaml', ui: 'default-toolbar' },
                    { text: 'JSON', value: 'json', ui: 'default-toolbar' },
                ],
                listeners: { change: (btn, value) => me.switchTextLang(value) },
            },
        ].concat(
            PVE.meta.Footer.actions({
                applyDisabled: true, // nothing staged yet; `syncFooter` decides after
                diff: () => me.showTextDiff(),
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
                    // The discard sits on the row it acts on. It used to be a button
                    // in the top toolbar, which meant reading the toolbar to find out
                    // what it would apply to -- and on a list member it applied to the
                    // whole list, which was worse than unclear.
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
                // comment key cannot exist there to describe one (DESIGN §3.5). An
                // always-empty column is a column that teaches you to ignore columns.
                hidden: me.docKind(me.docId) !== 'guest' && me.docKind(me.docId) !== 'datacenter',
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
                // Permissions reach guest documents only (DESIGN §3.3), so on anything
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
    // One function, every caller that needs it -- the row markers, the text editor's
    // squiggles, and the warning banner Apply shows. They are the same question asked
    // three times, and a rule with more than one implementation is one waiting to
    // disagree with itself.
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

    // The list at `path`, as it currently stands (staged edits included).
    listAt: function (path) {
        let v = PVE.meta.Lint.valueAt(this.plannedData(), path);
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

    // Records one edit. A staged path replaces any earlier entry for itself *and*
    // for everything under it: staging `selector` after `selector.tag` means the
    // subtree was replaced wholesale, and keeping the older, narrower entry would
    // re-apply it on top of the new value.
    stage: function (path, op, value) {
        let me = this;
        // `containsPath` plus one case it does not have: the empty path is the
        // *document*, and replacing that replaces everything -- including edits staged
        // under keys the new document does not have.
        let under = (p) => path === '' || PVE.meta.Utils.containsPath(path, p);
        me.pending = me.pending.filter((e) => !under(e.path));
        me.pending.push({ path: path, op: op, value: value });
        me.buildTree();
        me.syncButtons();
    },

    isDirty: function () {
        return this.pending.length > 0;
    },

    // The staged edits at `path` or under it -- the same subsumption `stage()` uses,
    // so "what would Discard drop" and "what did staging replace" are one rule.
    pendingUnder: function (path) {
        return this.pending.filter((e) => PVE.meta.Utils.containsPath(path, e.path));
    },

    // Drops the staged edits on one row, leaving the rest alone.
    //
    // A list member needs more care than "drop what is staged at this path": the edit
    // is staged on the *list*, so dropping it would throw away every other member's
    // change too. Put that one member back to what the document says instead, and
    // drop the whole staged edit only once the list matches again.
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
        let drop = me.pendingUnder(d.path);
        if (!drop.length) {
            return;
        }
        me.pending = me.pending.filter((e) => drop.indexOf(e) === -1);
        me.buildTree();
        me.syncButtons();
    },

    discardListMember: function (path, index) {
        let me = this;
        let stored = PVE.meta.Lint.valueAt(me.dataOf(me.docId), path);
        let list = me.listAt(path);
        if (!Array.isArray(stored)) {
            return;
        }
        if (index < stored.length) {
            list[index] = JSON.parse(JSON.stringify(stored[index]));
        } else {
            list.splice(index, 1); // it was appended; putting it back means removing it
        }
        if (JSON.stringify(list) === JSON.stringify(stored)) {
            // Nothing of this list's edit is left to keep.
            me.pending = me.pending.filter((e) => e.path !== path);
            me.buildTree();
            me.syncButtons();
            return;
        }
        me.stage(path, 'set', list);
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
            // Hidden by the *document*, disabled by the *row* -- the rule above. It
            // used to read `row.docId`, so with nothing selected it hid itself and
            // reappeared on the next click: a button that flickers as you move
            // through a tree, for a fact that cannot change while you are in it.
            declare.setHidden(me.docKind(me.docId) !== 'prefix');
            declare.setDisabled(text || !row || !row.editable);
        }
    },

    // One place decides what the footer says, in either mode.
    syncFooter: function () {
        let me = this;
        let textMode = me.mode === 'text';
        ['textLangBtn', 'metaFormat', 'metaDiff'].forEach(function (id) {
            let c = me.down('#' + id);
            if (c) {
                c.setHidden(!textMode);
            }
        });
        PVE.meta.Footer.sync(me, {
            // In text mode the buffer is the edit, and Apply is offered whenever the
            // caller may write at all -- the diff is what decides if it is worth it.
            canApply: textMode ? !!me.access.write : me.isDirty(),
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
                me.loadAccess(() =>
                    me.loadSchemas(() =>
                        me.loadDocument(() => Proxmox.Utils.setErrorMask(me, false)),
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

    // There is no separate tag request: `GET /meta/access` returns the guest's
    // tags with the access answer (see `loadAccess`). It used to be
    // `GET /meta/guests`, which reads, parses and digests every document in the
    // cluster -- to learn one guest's tags, on every open of every guest tab
    // that has a `tag:` selector anywhere in the registry, which the shipped
    // traefik prefix has.

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

    // The label is a restriction notice, so it says nothing at all for a caller with
    // full write access (DESIGN §8).
    syncAccessLabel: function () {
        let me = this;
        let modeBtn = me.down('#modeBtn');
        if (modeBtn && modeBtn.items.getAt(1)) {
            // The Text card is the whole document at the root view, and a scope-only
            // principal may not read that at all (DESIGN §3): do not offer it. Staged
            // edits are no longer a reason to refuse -- the buffer is rendered from the
            // planned document, so they are *in* it, and switching back turns whatever
            // was typed into staged edits again.
            modeBtn.items.getAt(1).setDisabled(!me.access.read);
        }
        // A Text-mode Apply is a root replace, which needs full write and nothing else
        // (DESIGN §3.4, `authorize_view_write`). Without this a read-only caller could
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

    // --- rows ---------------------------------------------------------------

    // Both lists below resolve `selector: { tag: t }` against `me.tags`, which
    // `GET /meta/access` fills in only for a caller with VM.Audit (DESIGN §5).
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
            // for (§8), so this needs no new write path.
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

    // A grammar is a PVE::JSONSchema object rooted at its scope's prefix.
    // `prefixes`/`owner`, when given, enforce most-specific-wins: the walk stops
    // where a *different* prefix governs, so a parent's `properties` never reach
    // into a child prefix's subtree and rewrite its row kind. The same rule
    // `Lint.findings` and `Lint.schemaIndex` apply, through the same
    // `Utils.governing` -- it lived in two of the three and this was the one that
    // silently merged (DESIGN section 3.1).
    addGrammar: function (root, prefix, schema, prefixes, owner, description) {
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
                // `||`, not `=`: `addData` ran first, so a value already stored here
                // keeps the kind it actually has. That is what lets a prefix hold a
                // single scalar -- a prefix is a key like any other, and one that
                // needs to say nothing but `true` should not have to grow a subkey to
                // say it. Only an *absent* prefix falls back to a map, which is the
                // shape almost every one of them turns out to have.
                entry.kind = entry.kind || 'map';
            });
            // The prefix's own description, which for a prefix with no schema is the
            // only thing its row can say about itself.
            entry.grammarDescription = entry.grammarDescription || description;
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
        me.addData(root, me.plannedData());
        // A row staged for deletion is gone from the planned document, but it should
        // not vanish off the screen before it is applied -- you would be looking at a
        // tree that already claims the write happened. It comes back as a ghost.
        // A staged list edit is one write of the whole list, but it is almost never a
        // change to the whole list: show it on the members that actually differ. Any
        // member the edit dropped comes back as a ghost, the same as a deleted key.
        let storedDoc = me.dataOf(me.docId);
        me.pending.forEach(function (e) {
            if (e.op !== 'set' || !Array.isArray(e.value)) {
                return;
            }
            let before = PVE.meta.Lint.valueAt(storedDoc, e.path);
            if (!Array.isArray(before) || before.length <= e.value.length) {
                return;
            }
            let list = PVE.meta.Lint.valueAt(me.plannedData(), e.path);
            let entry = root;
            let path = '';
            e.path.split('.').forEach(function (seg) {
                path = PVE.meta.Utils.joinPath(path, seg);
                entry = me.entry(entry, seg, path);
            });
            before.slice(Array.isArray(list) ? list.length : 0).forEach(function (item, i) {
                let row = me.entry(entry, String((list || []).length + i), e.path);
                row.present = false;
                row.pendingDelete = true;
                row.arrayIndex = null; // gone: there is no member to act on
                row.addressable = false;
                row.kind = 'string';
                row.value = PVE.meta.Utils.itemSummary(item);
            });
        });

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
            // A prefix with no schema still gets its row. Declaring a prefix *is* a
            // statement about the document -- "something of mine lives at this key" --
            // and it is the statement the whole permission model is written in terms
            // of. Hiding the row until someone had already put content there meant a
            // prefix that applied to every guest was invisible on every guest that had
            // not used it yet, which reads as "netbird is missing" rather than "netbird
            // is empty". The rows differ in what they can say, not in whether they
            // exist: a schema paints declared children and types, and without one there
            // is just the key, its description, and whatever is stored under it.
            //
            // The empty prefix is not a prefix at all: it is a registry document's
            // meta-schema rooted at the document (DESIGN §3.6), so with no schema there
            // is nothing to add.
            if (!ns.prefix) {
                if (ns.schema) {
                    me.addGrammar(root, '', ns.schema, [], null);
                }
                return;
            }
            // `prefixes`/`owner` are the most-specific-wins prune, and only guest
            // documents have more than one prefix to shadow between. A registry
            // document has exactly one schema, rooted at the document, so it is
            // walked with no owner and nothing is pruned.
            me.addGrammar(root, ns.prefix, ns.schema, grammar, ns, ns.description);
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

        // Which *member* of a staged list actually differs. The edit is one write of
        // the whole list -- members are not addressable (§2) -- but wearing the mark on
        // the list said "all of this changed" when one entry did.
        let memberChanged = function (listPath, index, item) {
            let before = PVE.meta.Lint.valueAt(storedDoc, listPath);
            if (!Array.isArray(before) || index >= before.length) {
                return true; // appended
            }
            return JSON.stringify(before[index]) !== JSON.stringify(item);
        };
        let storedMember = function (listPath, index) {
            let before = PVE.meta.Lint.valueAt(storedDoc, listPath);
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
                        // raw value: the API returns booleans as 1/0 (DESIGN §4), so
                        // inferring would print a struck-through "1" under a row whose
                        // stored value reads "Yes".
                        storedText: (function () {
                            if (c.arrayIndex !== undefined && c.arrayIndex !== null) {
                                return storedMember(c.path, c.arrayIndex);
                            }
                            if (c.pendingDelete && c.value !== undefined) {
                                return c.value; // a ghost carries what was there
                            }
                            let v = PVE.meta.Lint.valueAt(storedDoc, c.path);
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
    // the planned subtree as its content -- for a single row edit that is exactly the
    // one-key write this used to send immediately.
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

        // One staged delete is a DELETE, not a replace of its parent.
        //
        // `writeView` steps up a level for a delete, because you cannot remove a key by
        // replacing it -- but for a *top-level* key that step lands on the document
        // root, and a root write needs full write access (DESIGN §3.4). So a principal
        // holding `rw` on `traefik` could stage Remove on the `traefik` row, see it
        // struck through, and get "writing the whole document requires full write
        // access" on Apply -- for a delete the server would have taken as
        // `DELETE ?view=traefik`, which is what this panel sent before staging existed.
        let onlyDelete =
            me.pending.length === 1 && me.pending[0].op === 'delete' && me.pending[0].path;
        let write = function () {
            if (onlyDelete) {
                let q = Ext.Object.toQueryString({
                    view: me.pending[0].path,
                    digest: me.digestOf(me.docId),
                });
                me.submit(
                    { url: me.urlFor(me.docId) + '?' + q, method: 'DELETE' },
                    function () {
                        me.pending = [];
                    },
                );
                return;
            }
            me.submit({ url: me.urlFor(me.docId), method: 'PUT', params: params }, function () {
                me.pending = [];
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
        let warnings = me.textFindingsFor(planned);
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
            apply: write,
        });
    },

    // The schema findings a planned document would *introduce*, as plain messages --
    // what Apply warns about. The same `Lint.findings` the tree markers use, through
    // the same `grammarSplit`, so the banner and the amber rows can never disagree
    // about what is wrong; they differ only in what they are for. The markers show
    // everything wrong with the document, which is honest. The banner asks you to
    // vouch for what this edit did, which is the only thing you can answer for.
    textFindingsFor: function (planned) {
        let me = this;
        let U = PVE.meta.Utils;
        let g = me.grammarSplit(me.docId);
        if (!g.withSchema.length) {
            return [];
        }
        let stored = me.dataOf(me.docId);
        return U.introducedFindings(
            PVE.meta.Lint.findings(stored, g.withSchema, g.all),
            PVE.meta.Lint.findings(planned, g.withSchema, g.all),
            U.changedPaths(stored, planned),
        ).map((f) => f.path + ': ' + f.message);
    },

    // Stage a declared default, because someone asked for it. Never on its own: an
    // unset key stays unset, and a set one keeps whatever it was set to, until this
    // click (DESIGN §8).
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
            // The stored document, for the same reason `applyText` uses it: comparing
            // against the planned one would call a staged edit "not dirty".
            return (
                me.textEditor.getValue() !==
                PVE.meta.Utils.originalInLang(me.textOriginal, me.textLang)
            );
        } catch (_err) {
            return true; // cannot tell: assume there is something to lose
        }
    },

    // The loaded document rendered in `lang`; the diff's "original" side and the
    // yardstick the dirty check uses. `Utils.originalInLang` -- the subtree
    // window's own diff used to inline the same conversion rather than share this.
    // What the buffer should show: the **planned** document -- stored plus whatever is
    // staged -- in `lang`. With nothing staged this is the server's own text, comments
    // and all, because `renderBuffer` prefers the original when the document is
    // unchanged. That is what makes Tree and Text two views of one thing rather than
    // two editors that have to be kept apart.
    textRendered: function (lang) {
        let me = this;
        if (!me.isDirty()) {
            return PVE.meta.Utils.originalInLang(me.textOriginal, lang);
        }
        return PVE.meta.Utils.renderBuffer(me.plannedData(), lang, me.textOriginal);
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
    // Apply writes them. Switching views is not a decision about your work any more --
    // it used to ask you to discard it, which is why it felt like a trap.
    //
    // The one thing that can stop it is a buffer that does not parse: there is no
    // document to show as a tree, and guessing at one would lose what was typed. So it
    // says so and stays put.
    leaveTextMode: function () {
        let me = this;
        let parsed;
        try {
            parsed = PVE.meta.Utils.parseBuffer(me.textEditor.getValue(), me.textLang);
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
        me.pending = PVE.meta.Utils.diffDocuments(me.dataOf(me.textDocId), parsed);
        PVE.meta.Monaco.dispose(me.textEditor);
        me.textEditor = null;
        me.mode = 'tree';
        me.setModeButton('tree');
        me.getLayout().setActiveItem(me.down('#metaTree'));
        me.buildTree();
        me.syncButtons();
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
            value = PVE.meta.Utils.parseBuffer(me.textEditor.getValue(), me.textLang);
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

        // `renderBuffer`: switching back to YAML prefers the server's own text when
        // the document is unchanged (see the comment on that function) rather than
        // re-dumping unconditionally.
        me.textEditor.setValue(PVE.meta.Utils.renderBuffer(value, lang, me.textOriginal));
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
            let U = PVE.meta.Utils;
            let formatted = U.dumpBuffer(U.parseBuffer(text, me.textLang), me.textLang);
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
        // Against the **stored** document, not the planned one. `textRendered` renders
        // the planned document -- staged edits included, which is the point of it --
        // so comparing the buffer with that answers "nothing changed" for exactly the
        // case where something did: stage an edit in the tree, switch to Text, Apply.
        // It said "No changes." and wrote nothing.
        let original;
        try {
            original = PVE.meta.Utils.originalInLang(me.textOriginal, lang);
        } catch (_err) {
            lang = 'yaml'; // cannot render the stored text as JSON; diff the YAML
            original = me.textOriginal;
        }
        if (edited === original) {
            Ext.Msg.alert(gettext('Notice'), gettext('No changes.'));
            return;
        }
        // The whole document, at the root view, as **text** -- not as a dump of the
        // planned document. That is the one thing this path does that the tree's Apply
        // cannot: a `#` comment is not part of the document model (DESIGN §2), so it
        // survives only for as long as nothing rewrites the file from the model.
        // Sending the buffer keeps what was typed, comments included.
        //
        // The buffer already contains whatever was staged in the tree -- it is rendered
        // from the planned document -- so this applies all of it, and the staged edits
        // are spent.
        let write = function () {
            let params = { mode: 'replace', digest: me.digestOf(me.textDocId) };
            params[me.textLang === 'json' ? 'data' : 'text'] = edited;
            me.submit({ url: me.urlFor(me.textDocId), method: 'PUT', params: params }, function () {
                me.pending = [];
                me.refreshText();
            });
        };

        // Same rule as the tree's Apply: stop only when the document would not match
        // the schema, because that is the one case where seeing the diff changes what
        // you decide. The tick keeps storing it anyway a deliberate act -- the server's
        // lint decides what is *storable* (DESIGN §4), and an operator whose schema has
        // drifted must not be able to lock the administrator out of editing.
        let warnings = me.textFindings();
        if (!warnings.length) {
            write();
            return;
        }
        PVE.meta.Monaco.confirmDiff({
            title: gettext('(whole document)'),
            original: original,
            modified: edited,
            lang: lang,
            warnings: warnings,
            apply: write,
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
                me.pending = [];
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
            parsed = PVE.meta.Utils.parseBuffer(text, me.textLang);
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

    // The grammar findings the current buffer would *introduce*, as plain messages --
    // what Apply warns about before it writes. Empty when the buffer does not parse
    // (the write will fail on its own) or when no grammar applies. Same rule as the
    // tree's Apply, through the same `introducedFindings`: a violation the document
    // already had, on a path this buffer did not change, is not this edit's to vouch
    // for. Reordering keys therefore stops demanding a tick, since a reordering
    // changes no value at all.
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
            let U = PVE.meta.Utils;
            let value = U.parseBuffer(me.textEditor.getValue(), me.textLang);
            let stored = me.dataOf(me.textDocId || me.docId);
            return U.introducedFindings(
                PVE.meta.Lint.findings(stored, applicable, all),
                PVE.meta.Lint.findings(value, applicable, all),
                U.changedPaths(stored, value),
            ).map((f) => f.path + ': ' + f.message);
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
                    // The panel's footer is the only bar: a window with Close at the
                    // bottom and the Apply for the same document at the top of the
                    // panel inside it was the worst of the three chromes.
                    onClose: () => me.close(),
                },
            ],
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
    width: 620,
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
            // One dialog, not two. "Add" and "Create Service Token" were the same act
            // -- write a permission file for a principal -- differing only in whether
            // the principal exists yet, and a second button for that is a question the
            // dialog can just ask.
            let toggle = function () {
                let fresh = me.down('[name=principal]').getValue() === 'new';
                ['authid'].forEach((n) => me.down('[name=' + n + ']').setHidden(fresh));
                ['user', 'tokenid', 'role'].forEach((n) =>
                    me.down('[name=' + n + ']').setHidden(!fresh),
                );
                me.down('#tokenNote').setHidden(!fresh);
                me.down('[name=authid]').allowBlank = fresh;
                me.down('[name=user]').allowBlank = !fresh;
                me.down('[name=tokenid]').allowBlank = !fresh;
            };
            items.push(
                {
                    xtype: 'proxmoxKVComboBox',
                    name: 'principal',
                    fieldLabel: gettext('For'),
                    value: 'new',
                    comboItems: [
                        ['new', gettext('A new service token')],
                        ['existing', gettext('An existing user or token')],
                    ],
                    listeners: { change: toggle },
                },
                {
                    // Editable: a permission file may name a principal that does not
                    // exist yet, and the parser only checks the *shape* of an authid.
                    xtype: 'combobox',
                    name: 'authid',
                    fieldLabel: gettext('Auth ID'),
                    hidden: true,
                    allowBlank: true,
                    store: [],
                    queryMode: 'local',
                    editable: true,
                    forceSelection: false,
                    emptyText: gettext('user@realm, or user@realm!tokenid'),
                },
                {
                    xtype: 'textfield',
                    name: 'user',
                    fieldLabel: gettext('User'),
                    value: '@pve',
                    emptyText: 'traefik@pve',
                    regex: /^[^\s@]+@[A-Za-z0-9-]+$/,
                    regexText: gettext('A user id is user@realm'),
                    listeners: {
                        change: function (f, v) {
                            let name = me.down('[name=name]');
                            if (!name.isDirty()) {
                                name.setValue(String(v).split('@')[0]);
                            }
                        },
                    },
                },
                {
                    xtype: 'fieldcontainer',
                    fieldLabel: gettext('Token ID'),
                    layout: 'hbox',
                    items: [
                        {
                            xtype: 'textfield',
                            name: 'tokenid',
                            flex: 1,
                            value: 'meta',
                            regex: /^[A-Za-z0-9_-]+$/,
                            regexText: gettext('Letters, digits, - and _'),
                        },
                        {
                            xtype: 'button',
                            text: gettext('Generate'),
                            margin: '0 0 0 5',
                            handler: () =>
                                me.down('[name=tokenid]').setValue(PVE.meta.ServiceToken.randomTokenId()),
                        },
                    ],
                },
                {
                    xtype: 'proxmoxKVComboBox',
                    name: 'role',
                    fieldLabel: gettext('Guest access'),
                    // `none`, not `''`: a KVComboBox whose key is the empty string
                    // hands back the store record's internal id (`KeyValue-1`).
                    value: 'none',
                    comboItems: [
                        ['none', gettext('None — metadata only')],
                        ['PVEAuditor', gettext('Read guest configs (PVEAuditor on /vms)')],
                        ['PVEVMAdmin', gettext('Manage guests (PVEVMAdmin on /vms)')],
                    ],
                },
                {
                    xtype: 'displayfield',
                    itemId: 'tokenNote',
                    userCls: 'faded',
                    value: Ext.htmlEncode(
                        gettext(
                            'A new service token is a pve-realm user that cannot log in, with one token on ' +
                                'it. Guest access is a PVE role on /vms, covering guests created later too — ' +
                                'and it also lets this principal read ALL metadata on those guests, since ' +
                                'VM.Audit is full read. Writes stay inside the rules you give it.',
                        ),
                    ),
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
        me.on('show', function () {
            me.down('[name=name]').focus(true, 50);
            let box = me.down('[name=authid]');
            if (!box) {
                return;
            }
            // Users and their tokens in one call, so the picker costs one request.
            Proxmox.Utils.API2Request({
                url: '/access/users',
                params: { full: 1 },
                method: 'GET',
                failure: Ext.emptyFn, // a picker that did not load is still typable
                success: function (response) {
                    let out = [];
                    (response.result.data || []).forEach(function (u) {
                        out.push(u.userid);
                        (u.tokens || []).forEach((t) => out.push(u.userid + '!' + t.tokenid));
                    });
                    box.setStore(out);
                },
            });
        });
    },

    statics: {
        // Everything the dialog will do, as data: the file to write, and -- when the
        // principal does not exist yet -- the PVE objects to make first. Pure, so the
        // offline suite can check the order and the shape without a browser.
        //
        // The permission file must name the **token**, not the user. Naming the user
        // produces a file that parses, loads, and grants the token nothing.
        planFrom: function (kind, v) {
            let out = { file: String(v.name || '').trim(), content: {} };
            if (kind !== 'permissions') {
                if (v.description) {
                    out.content.description = v.description;
                }
                out.content.selector = v.selector === 'tag' ? { tag: v.tag } : { all: true };
                return out;
            }
            if (v.principal === 'new') {
                out.user = String(v.user || '').trim();
                out.tokenid = String(v.tokenid || '').trim();
                out.authid = out.user + '!' + out.tokenid;
                if (v.role && v.role !== 'none') {
                    out.acl = { path: '/vms', role: v.role, propagate: 1 };
                }
            } else {
                out.authid = String(v.authid || '').trim();
            }
            out.content.authid = out.authid;
            if (v.description) {
                out.content.description = v.description;
            }
            // No rules on purpose: it permits nothing until an administrator says what.
            out.content.rules = [];
            return out;
        },
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
        let plan = PVE.meta.NewRegistryWindow.planFrom(me.kind, v);
        if (me.kind === 'permissions' && !plan.authid) {
            Ext.Msg.alert(gettext('Error'), gettext('A permission needs an auth id'));
            return;
        }
        me.fireEvent('create', plan);
        me.close();
    },
});

// ---------------------------------------------------------------------------
// Creating a service principal, for the "New" dialog's second half.
//
// Two things about PVE's model shape this, both verified on the lab:
//
// * **A `pve`-realm user with no password cannot log in at all** (`/access/ticket`
//   answers "authentication failure"), while its token keeps working. That is the
//   closest thing PVE has to a service principal: there is no userless API key, so
//   every token hangs off a user, and this makes that user a dead end.
// * **Privilege separation is an intersection.** With privsep on, a token's rights
//   are its own ACLs *and* its user's, so granting the user a role later would
//   silently do nothing. This user exists only to carry this token, so privsep off
//   is what makes "add a role later" behave the way anyone would expect.
//
// The optional role goes on `/vms`, not per-guest and not `/`: per-guest silently
// misses guests created later, and `PVEAuditor` on `/` would also hand over
// `Sys.Audit`, which is the datacenter document's own read permission.
// ---------------------------------------------------------------------------

Ext.define('PVE.meta.ServiceToken', {
    singleton: true,

    // A token id with no meaning, for when you do not want to invent one. Not a
    // secret -- PVE generates that itself and shows it once -- just a name.
    randomTokenId: function () {
        let out = '';
        for (let i = 0; i < 6; i++) {
            out += 'abcdefghijklmnopqrstuvwxyz0123456789'.charAt(Math.floor(Math.random() * 36));
        }
        return 't-' + out;
    },

    // The secret, once. PVE never shows it again and it cannot be recovered, so
    // this is a copyable field rather than a message: the one moment it exists.
    showSecret: function (plan, secret) {
        Ext.create('Ext.window.Window', {
            title: gettext('Service Token Created'),
            modal: true,
            width: 620,
            bodyPadding: 10,
            items: [
                {
                    xtype: 'form',
                    border: false,
                    defaults: { anchor: '100%', labelWidth: 120 },
                    items: [
                        {
                            xtype: 'displayfield',
                            fieldLabel: gettext('Token ID'),
                            value: Ext.htmlEncode(plan.authid),
                        },
                        {
                            xtype: 'textfield',
                            fieldLabel: gettext('Secret'),
                            value: secret || '',
                            editable: false,
                            selectOnFocus: true,
                        },
                        {
                            xtype: 'displayfield',
                            userCls: 'faded',
                            value: Ext.htmlEncode(
                                gettext(
                                    'Copy it now — PVE does not show it again. Use it as the header ' +
                                        'Authorization: PVEAPIToken=<id>=<secret>. It can touch nothing ' +
                                        'until you add rules to its permission file.',
                                ),
                            ),
                        },
                    ],
                },
            ],
            buttons: [{ text: gettext('Close'), handler: function () { this.up('window').close(); } }],
        }).show();
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

    // "New" for both lists, and for a permission file both of the things that used
    // to be two buttons: name an existing principal, or make one.
    //
    // When it makes one, the order is chosen so a failure leaves the least behind: a
    // user with no token is inert, a token with no permission file grants nothing at
    // all, and only the last step makes anything true. Each failure says which step
    // it was and what already exists, because "create failed" with three PVE objects
    // half-made is not a message anyone can act on.
    createOne: function () {
        let me = this;
        let win = Ext.create('PVE.meta.NewRegistryWindow', { kind: me.kind });
        win.on('create', function (plan) {
            let made = [];
            let fail = (step) => (response) =>
                Ext.Msg.alert(
                    gettext('Error'),
                    Ext.htmlEncode(step) + ': ' +
                        (response.htmlStatus || Proxmox.Utils.getResponseErrorMessage(response)) +
                        (made.length
                            ? '<br><br>' +
                              Ext.htmlEncode(
                                  Ext.String.format(
                                      gettext('Already created: {0}. Remove it from Datacenter → Permissions, or run this again to reuse it.'),
                                      made.join(', '),
                                  ),
                              )
                            : ''),
                );
            let req = (opts) => Proxmox.Utils.API2Request(Ext.apply({ waitMsgTarget: me }, opts));

            let writeFile = function (secret) {
                req({
                    url: '/meta/' + me.kind + '/' + encodeURIComponent(plan.file),
                    method: 'PUT',
                    // `digest: ''` is "this file must not exist yet", so two
                    // administrators creating the same name is a 409 rather than one
                    // silently overwriting the other.
                    params: { data: Ext.encode(plan.content), mode: 'replace', digest: '' },
                    failure: fail(gettext('writing the file')),
                    success: function () {
                        me.reload();
                        if (secret) {
                            PVE.meta.ServiceToken.showSecret(plan, secret);
                            return;
                        }
                        me.editOne({ data: { id: me.kind + '/' + plan.file } });
                    },
                });
            };
            if (!plan.user) {
                writeFile(null);
                return;
            }
            let addAcl = function (secret) {
                if (!plan.acl) {
                    writeFile(secret);
                    return;
                }
                req({
                    url: '/access/acl',
                    method: 'PUT',
                    params: {
                        path: plan.acl.path,
                        roles: plan.acl.role,
                        propagate: plan.acl.propagate,
                        users: plan.user,
                    },
                    failure: fail(gettext('granting guest access')),
                    success: () => writeFile(secret),
                });
            };
            let addToken = function () {
                req({
                    url: '/access/users/' + encodeURIComponent(plan.user) + '/token/' +
                        encodeURIComponent(plan.tokenid),
                    method: 'POST',
                    // privsep off: this user exists only to carry this token, and with
                    // it on, a role added to the user later would silently not apply.
                    params: { privsep: 0 },
                    failure: fail(gettext('creating the token')),
                    success: function (response) {
                        made.push(plan.authid);
                        addAcl((response.result.data || {}).value);
                    },
                });
            };
            req({
                url: '/access/users',
                method: 'POST',
                // No password: this user cannot log in, only its token can act.
                params: { userid: plan.user, comment: 'pve-meta service principal' },
                failure: function (response) {
                    // An existing user is the normal case for a second token on the
                    // same principal, not an error to stop on.
                    if (String(response.htmlStatus || '').indexOf('already exists') !== -1) {
                        addToken();
                        return;
                    }
                    fail(gettext('creating the user'))(response);
                },
                success: function () {
                    made.push(plan.user);
                    addToken();
                },
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
