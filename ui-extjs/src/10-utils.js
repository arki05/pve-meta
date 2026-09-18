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
    // One finding as the warning banner lists it. An enforced one -- its prefix says
    // `enforce: true`, so the server refuses the write unless saved anyway -- says so
    // first, since that is the difference between "you have been told" and "it will
    // not go through".
    findingText: function (f) {
        return (f.enforced ? gettext('enforced') + ': ' : '') + f.path + ': ' + f.msg;
    },

    // A key ending in `__` documents its sibling; a bare `__` documents the map.
    isComment: (key) => key.length >= 2 && key.slice(-2) === '__',
    commentTarget: (key) => key.slice(0, -2),
    joinPath: (prefix, key) => (prefix ? prefix + '.' + key : key),
    // The path above `path`, or '' for a top-level key (and for the root itself).
    parentPath: (path) => (path && path.indexOf('.') !== -1 ? path.slice(0, path.lastIndexOf('.')) : ''),

    // The key a path declares, if it is a declaration inside a prefix file's schema
    // -- `schema.properties.host`, or `schema.properties.spec.properties.host` --
    // and `null` otherwise. What makes one is the segment before the last: a
    // declaration is always a child of some `properties`.
    declaredKeyAt: function (path) {
        let segs = String(path || '').split('.');
        if (segs.length < 3 || segs[0] !== 'schema' || segs[segs.length - 2] !== 'properties') {
            return null;
        }
        return segs[segs.length - 1];
    },

    // Two document values are the same value. Maps compare as sets: key order is
    // kept on disk as a courtesy and is not a value (DESIGN §2), so the keys are
    // sorted before the two are encoded and compared.
    // Two documents are the same value. The core's rule (`same`), not a local
    // one: this was a hand-rolled canonical `JSON.stringify` with sorted keys,
    // which is the same rule spelled a second time -- and a third copy of it
    // nearby had no sort, so a list member whose keys arrived in a different
    // order rendered as changed when nothing about it had changed.
    sameValue: (a, b) => PVE.meta.Core.call('same', a, b),

    // Why a key name is not one, or `null` if it is fine. A dotted path is
    // accepted and checked segment by segment, since the Key field takes one.
    //
    // The rule is the core's (`path::is_valid_segment`, through `key_path_check`);
    // this only puts it into words, naming the character it refused, so the answer
    // arrives in the field rather than as `invalid path: homelab.bad key (400)`
    // after a round trip. The server still refuses on its own, with the same rule.
    keyPathError: function (text) {
        if (!PVE.meta.Core.loaded()) {
            return null; // no early answer, then; the server's is still the answer
        }
        let why = PVE.meta.Core.call('key_path_check', String(text === undefined || text === null ? '' : text));
        if (!why) {
            return null;
        }
        if (why.reason === 'empty') {
            return gettext('Key must not be empty');
        }
        if (why.reason === 'empty_segment') {
            return gettext('A dotted path must not have an empty segment');
        }
        return Ext.String.format(
            gettext("'{0}' is not allowed in a key ({1}). Keys are letters, digits, _ - @ ! -- and '.' separates them."),
            why.char === ' ' ? gettext('space') : why.char,
            why.segment,
        );
    },

    // Why a registry file name is not one (`registry::is_valid_file_name`: a dotted
    // prefix, which is what the file name is), or `null`.
    fileNameError: function (text) {
        if (!PVE.meta.Core.loaded()) {
            return null; // as `keyPathError`
        }
        return PVE.meta.Core.call('file_name_valid', String(text || ''))
            ? null
            : gettext("A name is one or more key segments joined by '.', e.g. homelab.docker");
    },

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

    // What the Value column shows for a leaf. Arrays are one text leaf (DESIGN §12).
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
    // prefix's `schema` through verbatim and never validates `format` (DESIGN §7 --
    // the lint is the authority, a schema is an affordance), and the shared core's
    // schema findings hand a `format` back here to be checked (`Shape.findings`)
    // rather than carrying a third implementation of what `ipv4` means.
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

    // The value at a dotted path, through maps only -- `undefined` where a segment
    // is missing or the path runs into a list or a scalar. A render-time lookup;
    // the same addressing rule a view has (DESIGN §2).
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

    // One element of a list, on one line. Presentation only -- like `format`, it
    // describes nothing and constrains nothing; it is there so a list of maps reads
    // as something other than JSON in a grid cell. There is no shape this recognises
    // beyond a scalar, so a map member falls back to its JSON.
    itemSummary: function (v) {
        if (!v || typeof v !== 'object' || Array.isArray(v)) {
            return PVE.meta.Utils.scalarText(v);
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
    // for (docs/DESIGN.md §12).
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
    // key means, so a stored `1` under a declared `boolean` still gets the checkbox.
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

    // Human-readable form of a prefix's selector, for the "Applies to" column.
    selectorText: function (selector) {
        let sel = selector || {};
        if (sel.tag) {
            return gettext('tag') + ': ' + sel.tag;
        } else if (sel.all) {
            return gettext('all guests');
        }
        return Ext.encode(sel);
    },

    errText: (err) => String((err && (err.message || err.msg)) || err),
};

